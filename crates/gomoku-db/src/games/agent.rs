//! MCP agents: the seat behind a token and the cheap read a waiting `get_state` repeats once a second.

use gomoku_core::clock::{ClockState, seat_out_of_time};
use gomoku_core::paper::clock_seat;
use gomoku_core::rules::seat_on_turn;
use gomoku_core::types::{GameStatus, GameVariant, Seat};
use sqlx::Row;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use super::{GameError, seat_of, to_ms};
use crate::Database;

/// The seat an agent token opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentSeat {
    pub player_id: Uuid,
    pub game_id: Uuid,
    pub variant: GameVariant,
    pub origin_is_show: bool,
}

/// What a waiting `get_state` looks at: no transaction, no locks.
#[derive(Debug, Clone, Copy)]
pub struct AgentPeek {
    pub status: GameStatus,
    pub black_seat: Option<Seat>,
    pub seat: Seat,
    pub move_count: usize,
    pub clock_ms: [i64; 2],
    pub time_control_ms: i64,
    pub turn_started_at_ms: Option<i64>,
    pub auto_seat: Option<Seat>,
    pub pending_auto_at_ms: Option<i64>,
    /// The newest chat sequence number written by somebody else than the agent.
    pub opponent_last_seq: Option<i64>,
}

impl AgentPeek {
    /// `true` when the wait is over: it is the agent's turn, the game ended, a clock ran out, the paper automat's stone is
    /// due, or (only with a cursor) somebody else wrote newer than `after_message`.
    pub fn is_ready(&self, now_ms: i64, after_message: Option<i64>) -> bool {
        let (GameStatus::Active, Some(black)) = (self.status, self.black_seat) else {
            return true;
        };
        if let (Some(cursor), Some(last)) = (after_message, self.opponent_last_seq)
            && last > cursor
        {
            return true;
        }
        let automat_for_me = self.auto_seat == Some(self.seat);
        if seat_on_turn(self.move_count, black) == self.seat && !automat_for_me {
            return true;
        }
        if self.pending_auto_at_ms.is_some_and(|at| at <= now_ms) {
            return true;
        }
        let running = clock_seat(self.move_count, black, self.auto_seat);
        let clock = ClockState {
            clock_ms: self.clock_ms,
            turn_started_at_ms: self.turn_started_at_ms,
            turn_seat: Some(running),
            time_control_ms: Some(self.time_control_ms),
        };
        seat_out_of_time(&clock, now_ms).is_some()
    }
}

impl Database {
    /// The seat of an agent token, if the token is still valid: while the game is active, and for an hour after it ended
    /// (for a game never played, an hour after its invitation expired). The same `None` answers "unknown" and "expired".
    pub async fn find_agent_seat(
        &self,
        token_hash: &str,
        now: OffsetDateTime,
    ) -> Result<Option<AgentSeat>, GameError> {
        let cutoff =
            now - Duration::milliseconds(gomoku_core::constants::GUEST_TOKEN_GRACE_MS as i64);
        let row = sqlx::query(
            "SELECT p.id, p.game_id, g.variant, g.origin FROM game_players p JOIN games g ON g.id = p.game_id \
             WHERE p.agent_token_hash = $1 AND (g.status = 'active' OR (g.status IN ('finished', 'waiting', 'cancelled') \
               AND (CASE WHEN g.finished_at IS NOT NULL THEN g.finished_at \
                         ELSE (SELECT max(i.expires_at) FROM invitations i WHERE i.game_id = g.id AND i.consumed_at IS NULL) END) > $2))",
        )
        .bind(token_hash)
        .bind(cutoff)
        .fetch_optional(self.pool())
        .await?;
        let Some(row) = row else { return Ok(None) };
        Ok(Some(AgentSeat {
            player_id: row.try_get("id")?,
            game_id: row.try_get("game_id")?,
            variant: GameVariant::parse(&row.try_get::<String, _>("variant")?)
                .ok_or(GameError::Corrupt)?,
            origin_is_show: row.try_get::<String, _>("origin")? == "show",
        }))
    }

    /// One cheap read for a waiting `get_state`; `None` when the game vanished (cleanup).
    pub async fn agent_peek(&self, seat: &AgentSeat) -> Result<Option<AgentPeek>, GameError> {
        let row = sqlx::query(
            "SELECT g.status, g.black_seat, p.seat, COALESCE(s.move_count, g.move_count::int4) AS move_count, \
               g.clock_ms_1, g.clock_ms_2, g.time_control_ms, g.turn_started_at, s.auto_seat, s.pending_auto_at, \
               (SELECT max(m.seq) FROM chat_messages m WHERE m.game_id = g.id AND m.sender_id IS DISTINCT FROM p.id)::int8 AS opponent_last_seq \
             FROM games g JOIN game_players p ON p.game_id = g.id LEFT JOIN paper_game_states s ON s.game_id = g.id \
             WHERE g.id = $1 AND p.id = $2",
        )
        .bind(seat.game_id)
        .bind(seat.player_id)
        .fetch_optional(self.pool())
        .await?;
        let Some(row) = row else { return Ok(None) };
        let status: String = row.try_get("status")?;
        let seat_of_row = |name: &str| -> Result<Option<Seat>, GameError> {
            Ok(seat_of(row.try_get::<Option<i16>, _>(name)?))
        };
        Ok(Some(AgentPeek {
            status: GameStatus::parse(&status).ok_or(GameError::Corrupt)?,
            black_seat: seat_of_row("black_seat")?,
            seat: seat_of_row("seat")?.ok_or(GameError::Corrupt)?,
            move_count: usize::try_from(row.try_get::<i32, _>("move_count")?)
                .map_err(|_| GameError::Corrupt)?,
            clock_ms: [
                i64::from(row.try_get::<i32, _>("clock_ms_1")?),
                i64::from(row.try_get::<i32, _>("clock_ms_2")?),
            ],
            time_control_ms: i64::from(row.try_get::<i32, _>("time_control_ms")?),
            turn_started_at_ms: row
                .try_get::<Option<OffsetDateTime>, _>("turn_started_at")?
                .map(to_ms),
            auto_seat: seat_of_row("auto_seat")?,
            pending_auto_at_ms: row
                .try_get::<Option<OffsetDateTime>, _>("pending_auto_at")?
                .map(to_ms),
            opponent_last_seq: row.try_get("opponent_last_seq")?,
        }))
    }
}
