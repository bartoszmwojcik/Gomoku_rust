//! Spectators of a public game in progress, the creator's kill-switch and the invitation behind the QR code.
//!
//! A spectator is neither a participant nor a guest: no seat, no history, no game slot. They read one public game and
//! may write in its chat. Spectator times use the application clock (like the rest of the game); only the chat uses the
//! database clock.

use gomoku_core::constants::{SPECTATOR_ACCESS_MS, SPECTATOR_TOUCH_INTERVAL_MS, SPECTATORS_MAX};
use gomoku_core::types::{GameStatus, GameVariant};
use sqlx::Row;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use super::paper::{self, PaperSnapshot};
use super::{
    GameError, GameRow, PLAYER_SELECT, PlayerRow, Snapshot, audience_of, count_present_spectators,
    read_game, settle_expired, snapshot,
};
use crate::{Database, DbError};

/// Who wants to watch.
#[derive(Debug, Clone, Copy)]
pub enum Spectator<'a> {
    Account(Uuid),
    /// A visitor: `cookie_hash` of the token the browser already carries, `new_hash` of a fresh one for a new row.
    Visitor {
        cookie_hash: Option<&'a str>,
        new_hash: &'a str,
        nickname: Option<&'a str>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpectateJoin {
    pub game_id: Uuid,
    /// A new visitor row was made: the fresh token must be set as the cookie.
    pub new_cookie: bool,
}

/// What a spectator reads about a game.
#[derive(Debug, Clone)]
pub struct SpectatorState {
    pub game: GameRow,
    pub players: Vec<PlayerRow>,
    pub audience: Option<i64>,
    pub paper: Option<PaperSnapshot>,
    /// The spectator has a name (a nickname or an account), so may write.
    pub can_chat: bool,
}

fn ms(value: u64) -> Duration {
    Duration::milliseconds(value as i64)
}

impl Database {
    /// The spectator row of the caller for this game, still alive: the account's first, then the token's.
    pub async fn find_spectator(
        &self,
        game_id: Uuid,
        user_id: Option<Uuid>,
        token_hash: Option<&str>,
        now: OffsetDateTime,
    ) -> Result<Option<Uuid>, DbError> {
        if let Some(user) = user_id {
            let row = sqlx::query(
                "SELECT id FROM game_spectators WHERE game_id = $1 AND user_id = $2 AND expires_at > $3",
            )
            .bind(game_id)
            .bind(user)
            .bind(now)
            .fetch_optional(self.pool())
            .await?;
            if let Some(row) = row {
                return Ok(Some(row.try_get("id")?));
            }
        }
        let Some(hash) = token_hash.filter(|h| !h.is_empty()) else {
            return Ok(None);
        };
        let row = sqlx::query(
            "SELECT id FROM game_spectators WHERE game_id = $1 AND token_hash = $2 AND expires_at > $3",
        )
        .bind(game_id)
        .bind(hash)
        .bind(now)
        .fetch_optional(self.pool())
        .await?;
        Ok(row.map(|r| r.try_get("id")).transpose()?)
    }

    /// Becomes a spectator of the public game in progress behind the (normalized) code. Joins of one game are
    /// serialized, so the last place goes to exactly one of two racers. A returning spectator never counts twice.
    pub async fn spectate_join(
        &self,
        code: &str,
        who: Spectator<'_>,
        now: OffsetDateTime,
    ) -> Result<SpectateJoin, GameError> {
        let mut tx = self.pool().begin().await?;
        let game_id: Uuid = sqlx::query(
            "SELECT g.id FROM invitations i JOIN games g ON g.id = i.game_id WHERE i.code = $1",
        )
        .bind(code)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(GameError::InvalidInvitation)?
        .try_get("id")?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('spectate:' || $1::text, 0))")
            .bind(game_id.to_string())
            .execute(&mut *tx)
            .await?;
        let game = read_game(&mut tx, game_id)
            .await?
            .ok_or(GameError::InvalidInvitation)?;
        if game.status != GameStatus::Active || !game.is_public {
            return Err(GameError::InvalidInvitation);
        }
        let expires_at = now + ms(SPECTATOR_ACCESS_MS);
        let slide = "UPDATE game_spectators SET last_seen_at = $2, expires_at = $3 WHERE id = $1";
        match who {
            Spectator::Account(user) => {
                let plays =
                    sqlx::query("SELECT 1 FROM game_players WHERE game_id = $1 AND user_id = $2")
                        .bind(game_id)
                        .bind(user)
                        .fetch_optional(&mut *tx)
                        .await?;
                if plays.is_some() {
                    return Ok(SpectateJoin {
                        game_id,
                        new_cookie: false,
                    });
                }
                let existing = sqlx::query(
                    "SELECT id FROM game_spectators WHERE game_id = $1 AND user_id = $2",
                )
                .bind(game_id)
                .bind(user)
                .fetch_optional(&mut *tx)
                .await?;
                if let Some(row) = existing {
                    sqlx::query(slide)
                        .bind(row.try_get::<Uuid, _>("id")?)
                        .bind(now)
                        .bind(expires_at)
                        .execute(&mut *tx)
                        .await?;
                } else {
                    ensure_room(&mut tx, game_id, now).await?;
                    sqlx::query(
                        "INSERT INTO game_spectators (game_id, user_id, created_at, last_seen_at, expires_at) VALUES ($1, $2, $3, $3, $4)",
                    )
                    .bind(game_id)
                    .bind(user)
                    .bind(now)
                    .bind(expires_at)
                    .execute(&mut *tx)
                    .await?;
                }
                tx.commit().await?;
                Ok(SpectateJoin {
                    game_id,
                    new_cookie: false,
                })
            }
            Spectator::Visitor {
                cookie_hash,
                new_hash,
                nickname,
            } => {
                let existing =
                    match cookie_hash {
                        Some(hash) => sqlx::query(
                            "SELECT id FROM game_spectators WHERE game_id = $1 AND token_hash = $2",
                        )
                        .bind(game_id)
                        .bind(hash)
                        .fetch_optional(&mut *tx)
                        .await?,
                        None => None,
                    };
                if let Some(row) = existing {
                    sqlx::query(slide)
                        .bind(row.try_get::<Uuid, _>("id")?)
                        .bind(now)
                        .bind(expires_at)
                        .execute(&mut *tx)
                        .await?;
                    tx.commit().await?;
                    return Ok(SpectateJoin {
                        game_id,
                        new_cookie: false,
                    });
                }
                ensure_room(&mut tx, game_id, now).await?;
                sqlx::query(
                    "INSERT INTO game_spectators (game_id, nickname, token_hash, created_at, last_seen_at, expires_at) VALUES ($1, $2, $3, $4, $4, $5)",
                )
                .bind(game_id)
                .bind(nickname)
                .bind(new_hash)
                .bind(now)
                .bind(expires_at)
                .execute(&mut *tx)
                .await?;
                tx.commit().await?;
                Ok(SpectateJoin {
                    game_id,
                    new_cookie: true,
                })
            }
        }
    }

    /// What a spectator reads. Their presence and access slide at most every few seconds. A closed audience is
    /// `AudienceClosed`, raised inside the transaction so the slide is rolled back with it.
    pub async fn spectator_state(
        &self,
        game_id: Uuid,
        spectator_id: Uuid,
        after: usize,
        now: OffsetDateTime,
    ) -> Result<SpectatorState, GameError> {
        let mut tx = self.pool().begin().await?;
        let touched_before = now - ms(SPECTATOR_TOUCH_INTERVAL_MS);
        let row = sqlx::query(
            "SELECT last_seen_at <= $3 AS stale, (nickname IS NOT NULL OR user_id IS NOT NULL) AS can_chat \
             FROM game_spectators WHERE id = $1 AND game_id = $2 AND expires_at > $4",
        )
        .bind(spectator_id)
        .bind(game_id)
        .bind(touched_before)
        .bind(now)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(GameError::NotFound)?;
        if row.try_get::<bool, _>("stale")? {
            sqlx::query(
                "UPDATE game_spectators SET last_seen_at = $2, expires_at = $3 WHERE id = $1",
            )
            .bind(spectator_id)
            .bind(now)
            .bind(now + ms(SPECTATOR_ACCESS_MS))
            .execute(&mut *tx)
            .await?;
        }
        // The only change a spectator's read may make to a game: a loss on time that nobody noticed yet.
        settle_expired(&mut tx, game_id, now).await?;
        let game = read_game(&mut tx, game_id)
            .await?
            .ok_or(GameError::NotFound)?;
        if !game.is_public {
            return Err(GameError::AudienceClosed);
        }
        let paper = if game.variant == GameVariant::Paper {
            paper::load_paper(&mut tx, game_id).await?
        } else {
            None
        };
        let count = paper.as_ref().map_or(game.move_count, |p| p.move_count);
        if after > count {
            return Err(GameError::BadAfter);
        }
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "{PLAYER_SELECT} WHERE p.game_id = $1 ORDER BY p.seat"
        )))
        .bind(game_id)
        .fetch_all(&mut *tx)
        .await?;
        let players = rows
            .iter()
            .map(PlayerRow::from_row)
            .collect::<Result<Vec<_>, _>>()?;
        let audience = audience_of(&mut tx, &game, now).await?;
        let can_chat = row.try_get("can_chat")?;
        tx.commit().await?;
        Ok(SpectatorState {
            game,
            players,
            audience,
            paper,
            can_chat,
        })
    }

    /// The creator pulls the kill-switch: the game stops being public (for good) and what spectators wrote leaves the
    /// players' chat. Idempotent. Returns the creator's view of the game.
    pub async fn close_audience(
        &self,
        game_id: Uuid,
        player_id: Uuid,
        now: OffsetDateTime,
    ) -> Result<Snapshot, GameError> {
        let mut tx = self.pool().begin().await?;
        let creator =
            sqlx::query("SELECT 1 FROM game_players WHERE id = $1 AND game_id = $2 AND seat = 1")
                .bind(player_id)
                .bind(game_id)
                .fetch_optional(&mut *tx)
                .await?;
        if creator.is_none() {
            return Err(GameError::NotFound);
        }
        let row = sqlx::query(
            "SELECT status, is_public, audience_closed_at IS NOT NULL AS closed FROM games WHERE id = $1 FOR UPDATE",
        )
        .bind(game_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(GameError::NotFound)?;
        if !row.try_get::<bool, _>("closed")? {
            let status: String = row.try_get("status")?;
            if !matches!(status.as_str(), "active" | "finished") {
                return Err(GameError::AudienceNotClosable);
            }
            if !row.try_get::<bool, _>("is_public")? {
                return Err(GameError::GameNotPublic);
            }
            sqlx::query(
                "UPDATE games SET is_public = false, audience_closed_at = $2 WHERE id = $1",
            )
            .bind(game_id)
            .bind(now)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "DELETE FROM chat_messages WHERE game_id = $1 AND spectator_id IS NOT NULL",
            )
            .bind(game_id)
            .execute(&mut *tx)
            .await?;
        }
        let state = snapshot(&mut tx, game_id, player_id, now).await?;
        tx.commit().await?;
        Ok(state)
    }

    /// The code behind the QR image, if the game may still show it: a waiting game with a live invitation, or a public
    /// game in progress. Everything else is `None`, so the code never outlives its page.
    pub async fn qr_invitation_code(
        &self,
        game_id: Uuid,
        now: OffsetDateTime,
    ) -> Result<Option<String>, DbError> {
        let row = sqlx::query(
            "SELECT g.status, g.is_public, i.code, i.expires_at > $2 AS alive \
             FROM invitations i JOIN games g ON g.id = i.game_id WHERE i.game_id = $1",
        )
        .bind(game_id)
        .bind(now)
        .fetch_optional(self.pool())
        .await?;
        let Some(row) = row else { return Ok(None) };
        let status: String = row.try_get("status")?;
        let allowed = (status == "waiting" && row.try_get::<bool, _>("alive")?)
            || (status == "active" && row.try_get::<bool, _>("is_public")?);
        Ok(if allowed {
            Some(row.try_get("code")?)
        } else {
            None
        })
    }
}

/// Fails with `SpectatorsFull` when the game already has its limit of present spectators.
async fn ensure_room(
    conn: &mut sqlx::PgConnection,
    game_id: Uuid,
    now: OffsetDateTime,
) -> Result<(), GameError> {
    if count_present_spectators(conn, game_id, now).await? >= SPECTATORS_MAX as i64 {
        return Err(GameError::SpectatorsFull);
    }
    Ok(())
}
