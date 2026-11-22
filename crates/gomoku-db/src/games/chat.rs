//! The chat of a game: players write to each other (and, in a public game, to the spectators). Chat time (the interval
//! between messages, the window after the game ended, `created_at`) is the database clock, so ordering never depends on
//! the application servers. Sends, setting changes and `seq` allocation of one game are serialized by an advisory lock.

use gomoku_core::chat::{
    ChatState, derive_chat_state, derive_spectator_chat_state, retry_after_ms,
};
use gomoku_core::constants::CHAT_RETENTION_MS;
use gomoku_core::types::{GameStatus, Seat};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use time::OffsetDateTime;
use uuid::Uuid;

use super::{GameError, seat_of};
use crate::Database;

/// One message as it is shown: a player's has a seat, a spectator's has a name.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatMessageRow {
    pub seq: i32,
    pub seat: Option<Seat>,
    pub author_name: Option<String>,
    pub body: String,
    pub created_at: OffsetDateTime,
}

/// What a read of the chat answers.
#[derive(Debug, Clone)]
pub struct ChatRead {
    pub state: ChatState,
    pub messages: Vec<ChatMessageRow>,
    pub last_id: i64,
    pub retry_after_ms: u64,
    /// In a public game: the other player turned their chat off (it does not block the asker).
    pub opponent_disabled: bool,
}

/// What a send answers; `created` is false for a repeat of the same attempt.
#[derive(Debug, Clone)]
pub struct ChatSent {
    pub message: ChatMessageRow,
    pub retry_after_ms: u64,
    pub created: bool,
}

/// The asker, the game and the other seat, read in one statement.
struct Context {
    mine: bool,
    theirs: Option<bool>,
    status: GameStatus,
    is_public: bool,
    gone: bool,
    window_open: bool,
    since_last_ms: Option<f64>,
    total: i64,
}

fn from_row(row: &PgRow) -> Result<ChatMessageRow, GameError> {
    Ok(ChatMessageRow {
        seq: row.try_get("seq")?,
        seat: seat_of(row.try_get("seat")?),
        author_name: row.try_get("author_name")?,
        body: row.try_get("body")?,
        created_at: row.try_get("created_at")?,
    })
}

const MESSAGE_SELECT: &str = "SELECT m.seq, p.seat, m.author_name, m.body, m.created_at FROM chat_messages m \
     LEFT JOIN game_players p ON p.id = m.sender_id";

fn status_of(row: &PgRow) -> Result<GameStatus, GameError> {
    let status: String = row.try_get("status")?;
    GameStatus::parse(&status).ok_or(GameError::Corrupt)
}

/// A vanished game or participant (cleanup removed it under us) is "not found", never a server error.
fn vanished(error: sqlx::Error) -> GameError {
    if error
        .as_database_error()
        .is_some_and(|db| db.is_foreign_key_violation())
    {
        GameError::NotFound
    } else {
        error.into()
    }
}

async fn lock_game(conn: &mut PgConnection, game_id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text, 0))")
        .bind(game_id.to_string())
        .execute(conn)
        .await?;
    Ok(())
}

async fn player_context(
    conn: &mut PgConnection,
    game_id: Uuid,
    player_id: Uuid,
) -> Result<Context, GameError> {
    let row = sqlx::query(
        "SELECT p.chat_enabled AS mine, o.chat_enabled AS theirs, g.status, g.is_public, \
           EXISTS (SELECT 1 FROM game_players d WHERE d.game_id = g.id AND d.kind = 'account' AND d.user_id IS NULL) AS gone, \
           (g.status = 'finished' AND g.finished_at > now() - ($3::float8 * interval '1 millisecond')) AS window_open, \
           (SELECT extract(epoch FROM clock_timestamp() - max(m.created_at)) * 1000 FROM chat_messages m WHERE m.sender_id = p.id)::float8 AS since_last_ms, \
           (SELECT count(*) FROM chat_messages m WHERE m.game_id = g.id)::bigint AS total \
         FROM game_players p JOIN games g ON g.id = p.game_id \
         LEFT JOIN game_players o ON o.game_id = p.game_id AND o.id <> p.id \
         WHERE p.id = $1 AND g.id = $2",
    )
    .bind(player_id)
    .bind(game_id)
    .bind(CHAT_RETENTION_MS as f64)
    .fetch_optional(conn)
    .await?
    .ok_or(GameError::NotFound)?;
    Ok(Context {
        mine: row.try_get("mine")?,
        theirs: row.try_get("theirs")?,
        status: status_of(&row)?,
        is_public: row.try_get("is_public")?,
        gone: row.try_get("gone")?,
        window_open: row.try_get("window_open")?,
        since_last_ms: row.try_get("since_last_ms")?,
        total: row.try_get("total")?,
    })
}

/// The spectator's side: no switches, a name, and only public games (a closed audience is "not found").
struct SpectatorContext {
    name: Option<String>,
    status: GameStatus,
    window_open: bool,
    since_last_ms: Option<f64>,
    total: i64,
}

async fn spectator_context(
    conn: &mut PgConnection,
    game_id: Uuid,
    spectator_id: Uuid,
) -> Result<SpectatorContext, GameError> {
    let row = sqlx::query(
        "SELECT coalesce(s.nickname, u.display_name) AS name, g.status, \
           (g.status = 'finished' AND g.finished_at > now() - ($3::float8 * interval '1 millisecond')) AS window_open, \
           (SELECT extract(epoch FROM clock_timestamp() - max(m.created_at)) * 1000 FROM chat_messages m WHERE m.spectator_id = s.id)::float8 AS since_last_ms, \
           (SELECT count(*) FROM chat_messages m WHERE m.game_id = g.id)::bigint AS total \
         FROM game_spectators s JOIN games g ON g.id = s.game_id LEFT JOIN users u ON u.id = s.user_id \
         WHERE s.id = $1 AND g.id = $2 AND g.is_public",
    )
    .bind(spectator_id)
    .bind(game_id)
    .bind(CHAT_RETENTION_MS as f64)
    .fetch_optional(conn)
    .await?
    .ok_or(GameError::NotFound)?;
    Ok(SpectatorContext {
        name: row.try_get("name")?,
        status: status_of(&row)?,
        window_open: row.try_get("window_open")?,
        since_last_ms: row.try_get("since_last_ms")?,
        total: row.try_get("total")?,
    })
}

fn player_state(ctx: &Context) -> ChatState {
    if ctx.gone {
        ChatState::Unavailable
    } else {
        derive_chat_state(
            ctx.status,
            ctx.window_open,
            ctx.mine,
            ctx.theirs,
            ctx.is_public,
        )
    }
}

/// The page of messages: after a cursor, else the latest `limit`.
async fn page(
    conn: &mut PgConnection,
    game_id: Uuid,
    after: Option<i64>,
    limit: i64,
) -> Result<Vec<ChatMessageRow>, GameError> {
    let rows = match after {
        Some(after) => {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "{MESSAGE_SELECT} WHERE m.game_id = $1 AND m.seq > $2 ORDER BY m.seq LIMIT $3"
            )))
            .bind(game_id)
            .bind(after)
            .bind(limit)
            .fetch_all(conn)
            .await?
        }
        None => {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "SELECT * FROM ({MESSAGE_SELECT} WHERE m.game_id = $1 ORDER BY m.seq DESC LIMIT $2) latest ORDER BY seq"
            )))
            .bind(game_id)
            .bind(limit)
            .fetch_all(conn)
            .await?
        }
    };
    rows.iter().map(from_row).collect()
}

fn answer(
    state: ChatState,
    messages: Vec<ChatMessageRow>,
    after: Option<i64>,
    wait: u64,
    opponent_disabled: bool,
) -> ChatRead {
    let last_id = messages
        .last()
        .map_or(after.unwrap_or(0), |m| i64::from(m.seq));
    ChatRead {
        state,
        messages,
        last_id,
        retry_after_ms: if state == ChatState::Open { wait } else { 0 },
        opponent_disabled,
    }
}

impl Database {
    /// The chat as a player sees it. Messages come only in a state in which the player may see the conversation.
    pub async fn chat_read(
        &self,
        game_id: Uuid,
        player_id: Uuid,
        after: Option<i64>,
        limit: i64,
        min_interval_ms: u64,
    ) -> Result<ChatRead, GameError> {
        let mut conn = self.pool().acquire().await?;
        let ctx = player_context(&mut conn, game_id, player_id).await?;
        let state = player_state(&ctx);
        let messages = if state.readable() {
            page(&mut conn, game_id, after, limit).await?
        } else {
            Vec::new()
        };
        let wait = retry_after_ms(ctx.since_last_ms, min_interval_ms);
        let opponent_disabled =
            state == ChatState::Open && ctx.is_public && ctx.theirs == Some(false);
        Ok(answer(state, messages, after, wait, opponent_disabled))
    }

    /// Writes a message of a player (the text is already normalized). A repeat of the same attempt returns the stored
    /// message without any check.
    pub async fn chat_send(
        &self,
        game_id: Uuid,
        player_id: Uuid,
        client_id: Uuid,
        body: &str,
        min_interval_ms: u64,
        max_messages: usize,
    ) -> Result<ChatSent, GameError> {
        let mut tx = self.pool().begin().await?;
        lock_game(&mut tx, game_id).await?;
        let ctx = player_context(&mut tx, game_id, player_id).await?;
        let repeat = sqlx::query(sqlx::AssertSqlSafe(format!(
            "{MESSAGE_SELECT} WHERE m.sender_id = $1 AND m.client_id = $2"
        )))
        .bind(player_id)
        .bind(client_id)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(row) = repeat {
            return Ok(ChatSent {
                message: from_row(&row)?,
                retry_after_ms: retry_after_ms(ctx.since_last_ms, min_interval_ms),
                created: false,
            });
        }
        match player_state(&ctx) {
            ChatState::Unavailable | ChatState::Closed => return Err(GameError::ChatUnavailable),
            ChatState::DisabledByYou | ChatState::DisabledByOpponent => {
                return Err(GameError::ChatDisabled);
            }
            ChatState::Open => {}
        }
        if usize::try_from(ctx.total).unwrap_or(usize::MAX) >= max_messages {
            return Err(GameError::ChatFull);
        }
        let wait = retry_after_ms(ctx.since_last_ms, min_interval_ms);
        if wait > 0 {
            return Err(GameError::ChatInterval {
                retry_after_ms: wait,
            });
        }
        sqlx::query(
            "INSERT INTO chat_messages (game_id, seq, sender_id, client_id, body, created_at) \
             VALUES ($1, (SELECT coalesce(max(seq), 0) + 1 FROM chat_messages WHERE game_id = $1), $2, $3, $4, clock_timestamp())",
        )
        .bind(game_id)
        .bind(player_id)
        .bind(client_id)
        .bind(body)
        .execute(&mut *tx)
        .await
        .map_err(vanished)?;
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "{MESSAGE_SELECT} WHERE m.sender_id = $1 AND m.client_id = $2"
        )))
        .bind(player_id)
        .bind(client_id)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await.map_err(vanished)?;
        Ok(ChatSent {
            message: from_row(&row)?,
            retry_after_ms: min_interval_ms,
            created: true,
        })
    }

    /// Switches the player's own chat; returns the state after the change as the player sees it.
    pub async fn chat_set_enabled(
        &self,
        game_id: Uuid,
        player_id: Uuid,
        enabled: bool,
    ) -> Result<ChatState, GameError> {
        let mut tx = self.pool().begin().await?;
        lock_game(&mut tx, game_id).await?;
        let mut ctx = player_context(&mut tx, game_id, player_id).await?;
        if matches!(
            player_state(&ctx),
            ChatState::Unavailable | ChatState::Closed
        ) {
            return Err(GameError::ChatUnavailable);
        }
        if ctx.mine != enabled {
            sqlx::query("UPDATE game_players SET chat_enabled = $2 WHERE id = $1")
                .bind(player_id)
                .bind(enabled)
                .execute(&mut *tx)
                .await
                .map_err(vanished)?;
            ctx.mine = enabled;
        }
        tx.commit().await.map_err(vanished)?;
        Ok(player_state(&ctx))
    }

    /// The chat as a spectator sees it: the players' switches never hide anything.
    pub async fn spectator_chat_read(
        &self,
        game_id: Uuid,
        spectator_id: Uuid,
        after: Option<i64>,
        limit: i64,
        min_interval_ms: u64,
    ) -> Result<ChatRead, GameError> {
        let mut conn = self.pool().acquire().await?;
        let ctx = spectator_context(&mut conn, game_id, spectator_id).await?;
        let state = derive_spectator_chat_state(ctx.status, ctx.window_open);
        let messages = if state == ChatState::Open {
            page(&mut conn, game_id, after, limit).await?
        } else {
            Vec::new()
        };
        let wait = retry_after_ms(ctx.since_last_ms, min_interval_ms);
        Ok(answer(state, messages, after, wait, false))
    }

    /// A spectator's message. Needs a name (a nickname or an account); the check comes before the repeat check.
    pub async fn spectator_chat_send(
        &self,
        game_id: Uuid,
        spectator_id: Uuid,
        client_id: Uuid,
        body: &str,
        min_interval_ms: u64,
        max_messages: usize,
    ) -> Result<ChatSent, GameError> {
        let mut tx = self.pool().begin().await?;
        lock_game(&mut tx, game_id).await?;
        let ctx = spectator_context(&mut tx, game_id, spectator_id).await?;
        let name = ctx.name.clone().ok_or(GameError::SpectatorNameRequired)?;
        let select = format!("{MESSAGE_SELECT} WHERE m.spectator_id = $1 AND m.client_id = $2");
        let repeat = sqlx::query(sqlx::AssertSqlSafe(select.clone()))
            .bind(spectator_id)
            .bind(client_id)
            .fetch_optional(&mut *tx)
            .await?;
        if let Some(row) = repeat {
            return Ok(ChatSent {
                message: from_row(&row)?,
                retry_after_ms: retry_after_ms(ctx.since_last_ms, min_interval_ms),
                created: false,
            });
        }
        if derive_spectator_chat_state(ctx.status, ctx.window_open) != ChatState::Open {
            return Err(GameError::ChatUnavailable);
        }
        if usize::try_from(ctx.total).unwrap_or(usize::MAX) >= max_messages {
            return Err(GameError::ChatFull);
        }
        let wait = retry_after_ms(ctx.since_last_ms, min_interval_ms);
        if wait > 0 {
            return Err(GameError::ChatInterval {
                retry_after_ms: wait,
            });
        }
        sqlx::query(
            "INSERT INTO chat_messages (game_id, seq, spectator_id, author_name, client_id, body, created_at) \
             VALUES ($1, (SELECT coalesce(max(seq), 0) + 1 FROM chat_messages WHERE game_id = $1), $2, $3, $4, $5, clock_timestamp())",
        )
        .bind(game_id)
        .bind(spectator_id)
        .bind(&name)
        .bind(client_id)
        .bind(body)
        .execute(&mut *tx)
        .await
        .map_err(vanished)?;
        let row = sqlx::query(sqlx::AssertSqlSafe(select))
            .bind(spectator_id)
            .bind(client_id)
            .fetch_one(&mut *tx)
            .await?;
        tx.commit().await.map_err(vanished)?;
        Ok(ChatSent {
            message: from_row(&row)?,
            retry_after_ms: min_interval_ms,
            created: true,
        })
    }
}
