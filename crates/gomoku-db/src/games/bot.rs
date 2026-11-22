//! What the bot needs from the database: whether a move is due, from which position, with how much clock, and how many
//! games with a bot are in progress. The move itself is made through `play_move`, like a human's.

use gomoku_core::board::decode_moves;
use gomoku_core::clock::{ClockState, remaining_ms};
use gomoku_core::rules::seat_on_turn;
use gomoku_core::types::{GameStatus, GameVariant, Seat};
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

use super::{GameError, read_game, to_ms};
use crate::Database;

/// A bot move that is due, with everything the engine needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BotTurn {
    pub game_id: Uuid,
    pub bot_player_id: Uuid,
    pub level: i16,
    pub memory_mb: Option<i64>,
    pub depth_cap: Option<i32>,
    /// The cells played so far, in order (black on even indices).
    pub moves: Vec<usize>,
    /// The bot's clock at `now`.
    pub remaining_ms: i64,
    /// A show: the move waits for a pause counted from the start of the turn, and searches with show limits.
    pub is_show: bool,
    /// The bot's stored clock at the start of its turn.
    pub clock_at_turn_start_ms: i64,
    /// Time since the turn started (`None` before the clock started).
    pub elapsed_ms: Option<i64>,
}

/// Why a read found nothing to do, or the due move.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BotTurnRead {
    /// No bot in the game, not its turn, not started, finished or full.
    Idle,
    Due(Box<BotTurn>),
}

impl Database {
    /// An unlocked read of whether the bot of the game is on turn and may move now.
    pub async fn bot_turn_due(
        &self,
        game_id: Uuid,
        now: OffsetDateTime,
    ) -> Result<BotTurnRead, GameError> {
        let mut conn = self.pool().acquire().await?;
        let Some(game) = read_game(&mut conn, game_id).await? else {
            return Ok(BotTurnRead::Idle);
        };
        let bots = sqlx::query(
            "SELECT id, seat, bot_level, bot_memory_mb, bot_depth_cap FROM game_players WHERE game_id = $1 AND kind = 'bot' ORDER BY seat",
        )
        .bind(game_id)
        .fetch_all(&mut *conn)
        .await?;
        let Some(black) = game.black_seat else {
            return Ok(BotTurnRead::Idle);
        };
        // Of the bots of the game (a show has two) the one on turn.
        let on_turn = seat_on_turn(game.move_count, black);
        let mut found = None;
        for row in bots {
            let seat = Seat::from_number(
                u8::try_from(row.try_get::<i16, _>("seat")?).map_err(|_| GameError::Corrupt)?,
            )
            .ok_or(GameError::Corrupt)?;
            if seat == on_turn {
                found = Some((row, seat));
            }
        }
        let Some((bot, seat)) = found else {
            return Ok(BotTurnRead::Idle);
        };
        let due = game.status == GameStatus::Active
            && game.variant == GameVariant::Classic
            && game.turn_started_at.is_some()
            && game.move_count < 225;
        if !due {
            return Ok(BotTurnRead::Idle);
        }
        let state: ClockState = game.clock_state();
        Ok(BotTurnRead::Due(Box::new(BotTurn {
            game_id,
            bot_player_id: bot.try_get("id")?,
            level: bot.try_get("bot_level")?,
            memory_mb: bot
                .try_get::<Option<i16>, _>("bot_memory_mb")?
                .map(i64::from),
            depth_cap: bot
                .try_get::<Option<i16>, _>("bot_depth_cap")?
                .map(i32::from),
            moves: decode_moves(&game.moves).map_err(|_| GameError::Corrupt)?,
            remaining_ms: remaining_ms(&state, seat, to_ms(now)),
            is_show: game.origin == "show",
            clock_at_turn_start_ms: game.clock_ms[seat.index()],
            elapsed_ms: game
                .turn_started_at
                .map(|at| (to_ms(now) - to_ms(at)).max(0)),
        })))
    }

    /// Games with a bot in progress whose deadline has not passed: they hold a slot of the bot's capacity.
    pub async fn count_active_bot_games(&self, now: OffsetDateTime) -> Result<i64, GameError> {
        let sql = concat!(
            "SELECT count(*) FROM games WHERE status = 'active' \
             AND EXISTS (SELECT 1 FROM game_players p WHERE p.game_id = games.id AND p.kind = 'bot') AND ",
            game_deadline_sql!("games"),
            " > $1"
        );
        Ok(sqlx::query_scalar(sql)
            .bind(now)
            .fetch_one(self.pool())
            .await?)
    }
}
