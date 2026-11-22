//! Show mode: a private game without a human seat, created, read and ended by the administrator. Two bots play each
//! other, or a bot waits for an agent that joins over MCP.

use gomoku_core::admin::ShowLayout;
use gomoku_core::constants::INITIAL_CLOCK_MS;
use gomoku_core::types::Seat;
use sqlx::{PgConnection, Row};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use super::{
    GameError, GameRow, InvitationRow, PLAYER_SELECT, PlayerRow, Scope, expire_waiting, read_game,
    release_slots, settle_expired,
};
use crate::Database;

/// The frozen strength of one show bot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShowBot {
    pub level: i16,
    pub memory_mb: Option<i16>,
    pub depth_cap: Option<i16>,
}

/// The show that is open (waiting for its agent, or in progress).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShowOpen {
    pub game_id: Uuid,
    pub waiting: bool,
    pub layout: ShowLayout,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShowCreated {
    pub game_id: Uuid,
    pub layout: ShowLayout,
    /// Storage form of the invitation code (bot vs agent only).
    pub code: Option<String>,
    pub expires_at: Option<OffsetDateTime>,
}

/// A show as the administrator reads it.
#[derive(Debug, Clone)]
pub struct ShowRead {
    pub game: GameRow,
    pub players: Vec<PlayerRow>,
    /// While the show waits for its agent.
    pub invitation: Option<InvitationRow>,
    /// For a cancelled show: `"ended"` (by the administrator) or `"expired"` (the agent did not come).
    pub closed_reason: Option<&'static str>,
    /// How long the bot on turn has been on turn, with its level (`None` unless a bot is on turn).
    pub bot_on_turn: Option<(i16, i64)>,
}

/// The open shows after their lazy closing, newest first.
async fn open_shows(
    conn: &mut PgConnection,
    now: OffsetDateTime,
) -> Result<Vec<ShowOpen>, GameError> {
    let select = "SELECT g.id, g.status, (SELECT count(*) FROM game_players p WHERE p.game_id = g.id AND p.kind = 'bot') AS bots \
                  FROM games g WHERE g.origin = 'show' AND g.status IN ('waiting', 'active') ORDER BY g.created_at DESC, g.id";
    let ids: Vec<Uuid> = sqlx::query(select)
        .fetch_all(&mut *conn)
        .await?
        .iter()
        .map(|r| r.try_get("id"))
        .collect::<Result<_, _>>()?;
    for id in ids {
        expire_waiting(conn, now, Scope::Game(id)).await?;
        settle_expired(conn, id, now).await?;
    }
    let rows = sqlx::query(select).fetch_all(&mut *conn).await?;
    rows.iter()
        .map(|row| {
            let status: String = row.try_get("status")?;
            Ok(ShowOpen {
                game_id: row.try_get("id")?,
                waiting: status == "waiting",
                layout: ShowLayout::of_bots(row.try_get("bots")?),
            })
        })
        .collect()
}

async fn insert_bot(
    conn: &mut PgConnection,
    game_id: Uuid,
    seat: i16,
    bot: ShowBot,
    now: OffsetDateTime,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO game_players (game_id, seat, kind, is_active, joined_at, seen_at, bot_level, bot_memory_mb, bot_depth_cap) \
         VALUES ($1, $2, 'bot', true, $3, $3, $4, $5, $6)",
    )
    .bind(game_id)
    .bind(seat)
    .bind(now)
    .bind(bot.level)
    .bind(bot.memory_mb)
    .bind(bot.depth_cap)
    .execute(conn)
    .await?;
    Ok(())
}

impl Database {
    /// The open show, after the lazy closing of an expired invitation and of a clock that ran out.
    pub async fn show_current(&self, now: OffsetDateTime) -> Result<Option<ShowOpen>, GameError> {
        let mut tx = self.pool().begin().await?;
        let open = open_shows(&mut tx, now).await?.into_iter().next();
        tx.commit().await?;
        Ok(open)
    }

    /// Creates a show. Of two simultaneous creations exactly one wins: the other gets `ShowRunning` (returned after the
    /// commit, so the lazy closings are kept).
    pub async fn show_create(
        &self,
        bots: &[ShowBot],
        black_seat: Seat,
        code: &mut (dyn FnMut() -> String + Send),
        invitation_ttl_ms: i64,
        now: OffsetDateTime,
    ) -> Result<ShowCreated, GameError> {
        let mut tx = self.pool().begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext('show:create'))")
            .execute(&mut *tx)
            .await?;
        if let Some(open) = open_shows(&mut tx, now).await?.into_iter().next() {
            tx.commit().await?;
            return Err(GameError::ShowRunning {
                game_id: open.game_id,
                layout: open.layout,
            });
        }
        let created = if let [first, second] = bots {
            let game_id: Uuid = sqlx::query(
                "INSERT INTO games (origin, status, created_at, started_at, turn_started_at, black_seat, is_public, variant, time_control_ms, clock_ms_1, clock_ms_2) \
                 VALUES ('show', 'active', $1, $1, $1, $2, false, 'classic', $3, $3, $3) RETURNING id",
            )
            .bind(now)
            .bind(i16::from(black_seat.number()))
            .bind(INITIAL_CLOCK_MS as i32)
            .fetch_one(&mut *tx)
            .await?
            .try_get("id")?;
            insert_bot(&mut tx, game_id, 1, *first, now).await?;
            insert_bot(&mut tx, game_id, 2, *second, now).await?;
            ShowCreated {
                game_id,
                layout: ShowLayout::BotBot,
                code: None,
                expires_at: None,
            }
        } else {
            let [bot] = bots else {
                return Err(GameError::Corrupt);
            };
            let game_id: Uuid = sqlx::query(
                "INSERT INTO games (origin, status, created_at, is_public, variant, time_control_ms, clock_ms_1, clock_ms_2) \
                 VALUES ('show', 'waiting', $1, false, 'classic', $2, $2, $2) RETURNING id",
            )
            .bind(now)
            .bind(INITIAL_CLOCK_MS as i32)
            .fetch_one(&mut *tx)
            .await?
            .try_get("id")?;
            insert_bot(&mut tx, game_id, 1, *bot, now).await?;
            let mut chosen = None;
            for _ in 0..10 {
                let candidate = code();
                let taken = sqlx::query("SELECT 1 FROM invitations WHERE code = $1")
                    .bind(&candidate)
                    .fetch_optional(&mut *tx)
                    .await?;
                if taken.is_none() {
                    chosen = Some(candidate);
                    break;
                }
            }
            let code = chosen.ok_or(GameError::Corrupt)?;
            let expires_at = now + Duration::milliseconds(invitation_ttl_ms);
            sqlx::query("INSERT INTO invitations (game_id, code, created_at, expires_at) VALUES ($1, $2, $3, $4)")
                .bind(game_id)
                .bind(&code)
                .bind(now)
                .bind(expires_at)
                .execute(&mut *tx)
                .await?;
            ShowCreated {
                game_id,
                layout: ShowLayout::BotAgent,
                code: Some(code),
                expires_at: Some(expires_at),
            }
        };
        tx.commit().await?;
        Ok(created)
    }

    /// Reads a show: its lazy bookkeeping (expiry, the clock of a show nobody "opens", loss on time) and then the state.
    /// Anything that is not a show is `NotFound`.
    pub async fn show_read(
        &self,
        game_id: Uuid,
        after: usize,
        now: OffsetDateTime,
    ) -> Result<ShowRead, GameError> {
        let mut tx = self.pool().begin().await?;
        let exists = sqlx::query("SELECT 1 FROM games WHERE id = $1 AND origin = 'show'")
            .bind(game_id)
            .fetch_optional(&mut *tx)
            .await?;
        if exists.is_none() {
            return Err(GameError::NotFound);
        }
        expire_waiting(&mut tx, now, Scope::Game(game_id)).await?;
        // Nobody is the reader: the clock of a show starts as soon as every seat has seen it (a silent agent included).
        sqlx::query(
            "UPDATE games SET turn_started_at = $1 WHERE id = $2 AND origin = 'show' AND status = 'active' AND turn_started_at IS NULL \
             AND NOT EXISTS (SELECT 1 FROM game_players p WHERE p.game_id = games.id AND p.seen_at IS NULL)",
        )
        .bind(now)
        .bind(game_id)
        .execute(&mut *tx)
        .await?;
        settle_expired(&mut tx, game_id, now).await?;
        let game = read_game(&mut tx, game_id)
            .await?
            .ok_or(GameError::NotFound)?;
        if after > game.move_count {
            return Err(GameError::BadAfter);
        }
        let players: Vec<PlayerRow> = sqlx::query(sqlx::AssertSqlSafe(format!(
            "{PLAYER_SELECT} WHERE p.game_id = $1 ORDER BY p.seat"
        )))
        .bind(game_id)
        .fetch_all(&mut *tx)
        .await?
        .iter()
        .map(PlayerRow::from_row)
        .collect::<Result<_, _>>()?;
        let invitation = sqlx::query("SELECT code, expires_at, consumed_at IS NOT NULL AS consumed FROM invitations WHERE game_id = $1")
            .bind(game_id)
            .fetch_optional(&mut *tx)
            .await?
            .map(|r| {
                Ok::<_, GameError>(InvitationRow {
                    code: r.try_get("code")?,
                    expires_at: r.try_get("expires_at")?,
                    consumed: r.try_get("consumed")?,
                })
            })
            .transpose()?;
        tx.commit().await?;
        let closed_reason = (game.status == gomoku_core::types::GameStatus::Cancelled).then(|| {
            if game.started_at.is_some() || invitation.as_ref().is_some_and(|i| i.consumed) {
                "ended"
            } else {
                "expired"
            }
        });
        let bot_on_turn = match (game.status, game.black_seat, game.turn_started_at) {
            (gomoku_core::types::GameStatus::Active, Some(black), Some(started)) => {
                let seat = gomoku_core::rules::seat_on_turn(game.move_count, black);
                players
                    .iter()
                    .find(|p| p.seat == seat && p.kind == "bot")
                    .and_then(|p| p.bot_level)
                    .map(|level| (level, (super::to_ms(now) - super::to_ms(started)).max(0)))
            }
            _ => None,
        };
        let waiting = game.status == gomoku_core::types::GameStatus::Waiting;
        Ok(ShowRead {
            game,
            players,
            invitation: invitation.filter(|_| waiting),
            closed_reason,
            bot_on_turn,
        })
    }

    /// Ends a show: cancels it (no result, no counter), keeps the time the running clock had used, frees the seats and
    /// voids the code. A show that finished on its own is `ShowEnded`; repeating the end on a cancelled one is harmless.
    pub async fn show_end(&self, game_id: Uuid, now: OffsetDateTime) -> Result<(), GameError> {
        let mut tx = self.pool().begin().await?;
        let row =
            sqlx::query("SELECT status FROM games WHERE id = $1 AND origin = 'show' FOR UPDATE")
                .bind(game_id)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or(GameError::NotFound)?;
        let _: String = row.try_get("status")?;
        expire_waiting(&mut tx, now, Scope::Game(game_id)).await?;
        settle_expired(&mut tx, game_id, now).await?;
        let status: String = sqlx::query("SELECT status FROM games WHERE id = $1")
            .bind(game_id)
            .fetch_one(&mut *tx)
            .await?
            .try_get("status")?;
        if status == "finished" {
            tx.commit().await?;
            return Err(GameError::ShowEnded);
        }
        let used = |seat: i32| {
            format!(
                "CASE WHEN status = 'active' AND turn_started_at IS NOT NULL AND \
                 (CASE WHEN move_count % 2 = 0 THEN black_seat ELSE 3 - black_seat END) = {seat} \
                 THEN greatest(0, clock_ms_{seat} - floor(extract(epoch FROM ($1::timestamptz - turn_started_at)) * 1000)::int) \
                 ELSE clock_ms_{seat} END"
            )
        };
        let sql = format!(
            "UPDATE games SET status = 'cancelled', clock_ms_1 = {}, clock_ms_2 = {}, \
             finished_at = CASE WHEN started_at IS NOT NULL THEN $1::timestamptz END \
             WHERE id = $2 AND origin = 'show' AND status IN ('waiting', 'active') RETURNING id",
            used(1),
            used(2)
        );
        let updated = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(now)
            .bind(game_id)
            .fetch_optional(&mut *tx)
            .await?;
        if updated.is_some() {
            release_slots(&mut tx, game_id).await?;
            sqlx::query("UPDATE invitations SET consumed_at = $2 WHERE game_id = $1 AND consumed_at IS NULL")
                .bind(game_id)
                .bind(now)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }
}
