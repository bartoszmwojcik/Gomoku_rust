//! Creating and cancelling a waiting game.

use sqlx::{PgConnection, Row};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use super::GameError;
use super::{ACTIVE_SLOT_INDEX, Scope, close_stale, release_slots, unique_violation};
use crate::Database;

/// A freshly created waiting game and its invitation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedGame {
    pub game_id: Uuid,
    /// Storage form (no dash).
    pub code: String,
    pub expires_at: OffsetDateTime,
}

/// Outcome alias kept for readability at call sites.
pub type CreateOutcome = Result<CreatedGame, GameError>;

#[derive(Debug, Clone)]
pub struct GuestCreateInput<'a> {
    /// Hash of the cookie token the guest will use (their existing one, or a fresh one).
    pub token_hash: &'a str,
    pub nickname: Option<&'a str>,
    /// The guest agreed to give up their own waiting game for the new one.
    pub cancel_own_waiting_game: bool,
    /// Anybody with the code may watch the game once it is in progress.
    pub is_public: bool,
}

/// Cancels a waiting game only while nobody has joined; zero rows means somebody joined first. The invitation is voided
/// (`consumed_at` set, `consumed_by` left empty), which tells "cancelled by the creator" from "expired".
pub(crate) async fn cancel_waiting_game(
    conn: &mut PgConnection,
    game_id: Uuid,
    now: OffsetDateTime,
) -> Result<bool, sqlx::Error> {
    let row = sqlx::query(
        "WITH cancelled AS (UPDATE games SET status = 'cancelled' WHERE id = $1 AND status = 'waiting' RETURNING id), \
              voided AS (UPDATE invitations i SET consumed_at = COALESCE(i.consumed_at, $2) FROM cancelled c WHERE i.game_id = c.id) \
         SELECT id FROM cancelled",
    )
    .bind(game_id)
    .bind(now)
    .fetch_optional(conn)
    .await?;
    Ok(row.is_some())
}

async fn fresh_code(
    conn: &mut PgConnection,
    new_code: &mut (dyn FnMut() -> String + Send),
) -> Result<String, GameError> {
    for _ in 0..10 {
        let code = new_code();
        let taken = sqlx::query("SELECT 1 FROM invitations WHERE code = $1")
            .bind(&code)
            .fetch_optional(&mut *conn)
            .await?;
        if taken.is_none() {
            return Ok(code);
        }
    }
    Err(GameError::Corrupt)
}

/// Seat 1 of a new waiting game: either an account or a guest.
enum Creator<'a> {
    Account(Uuid, &'a str),
    Guest {
        token_hash: &'a str,
        nickname: Option<&'a str>,
    },
}

async fn insert_waiting_game(
    conn: &mut PgConnection,
    creator: Creator<'_>,
    code: String,
    expires_at: OffsetDateTime,
    now: OffsetDateTime,
    is_public: bool,
) -> Result<CreatedGame, sqlx::Error> {
    let game_id: Uuid = sqlx::query("INSERT INTO games (origin, status, created_at, is_public) VALUES ('invite', 'waiting', $1, $2) RETURNING id")
        .bind(now)
        .bind(is_public)
        .fetch_one(&mut *conn)
        .await?
        .try_get("id")?;
    match creator {
        Creator::Account(user_id, display_name) => {
            sqlx::query(
                "INSERT INTO game_players (game_id, seat, kind, user_id, is_active, joined_at, display_name) \
                 VALUES ($1, 1, 'account', $2, true, $3, $4)",
            )
            .bind(game_id)
            .bind(user_id)
            .bind(now)
            .bind(display_name)
            .execute(&mut *conn)
            .await?;
        }
        Creator::Guest {
            token_hash,
            nickname,
        } => {
            sqlx::query(
                "INSERT INTO game_players (game_id, seat, kind, guest_token_hash, nickname, is_active, joined_at) \
                 VALUES ($1, 1, 'guest', $2, $3, true, $4)",
            )
            .bind(game_id)
            .bind(token_hash)
            .bind(nickname)
            .bind(now)
            .execute(&mut *conn)
            .await?;
        }
    }
    sqlx::query(
        "INSERT INTO invitations (game_id, code, created_at, expires_at) VALUES ($1, $2, $3, $4)",
    )
    .bind(game_id)
    .bind(&code)
    .bind(now)
    .bind(expires_at)
    .execute(&mut *conn)
    .await?;
    Ok(CreatedGame {
        game_id,
        code,
        expires_at,
    })
}

impl Database {
    /// Creates a waiting game for an account. The caller counted the attempt (rate limit) beforehand. The user row is
    /// locked (which also serializes account deletion), the account's own lobby status is withdrawn and stale slots are
    /// closed; the partial unique index then decides the one-slot rule: `ActiveGameExists`.
    pub async fn create_game_account(
        &self,
        user_id: Uuid,
        is_public: bool,
        now: OffsetDateTime,
        invitation_ttl_ms: i64,
        new_code: &mut (dyn FnMut() -> String + Send),
    ) -> CreateOutcome {
        self.settle_for(user_id, now).await?;
        let mut tx = self.pool().begin().await?;
        let user = sqlx::query("SELECT display_name FROM users WHERE id = $1 FOR NO KEY UPDATE")
            .bind(user_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(GameError::UserGone)?;
        let display_name: String = user.try_get("display_name")?;
        sqlx::query("DELETE FROM lobby_seekers WHERE user_id = $1")
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
        close_stale(&mut tx, now, Scope::User(user_id)).await?;
        let code = fresh_code(&mut tx, new_code).await?;
        let expires_at = now + Duration::milliseconds(invitation_ttl_ms);
        let created = insert_waiting_game(
            &mut tx,
            Creator::Account(user_id, &display_name),
            code,
            expires_at,
            now,
            is_public,
        )
        .await;
        match created {
            Ok(game) => {
                tx.commit().await?;
                Ok(game)
            }
            Err(error) if unique_violation(&error).as_deref() == Some(ACTIVE_SLOT_INDEX) => {
                // The lazy closings above are kept: commit what is committable by rolling back only the insert.
                Err(GameError::ActiveGameExists { game_id: None })
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Creates a waiting game for a guest. The guest's one-slot rule is kept by the service under an advisory lock on the
    /// token hash. A refusal is decided inside the transaction and returned after it committed, so lazy closings are kept.
    pub async fn create_game_guest(
        &self,
        input: &GuestCreateInput<'_>,
        now: OffsetDateTime,
        invitation_ttl_ms: i64,
        new_code: &mut (dyn FnMut() -> String + Send),
    ) -> CreateOutcome {
        self.settle_for_guest(input.token_hash, now).await?;
        let mut tx = self.pool().begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
            .bind(input.token_hash)
            .execute(&mut *tx)
            .await?;
        close_stale(&mut tx, now, Scope::Guest(input.token_hash)).await?;
        let slot = sqlx::query(
            "SELECT p.game_id, g.status, p.seat FROM game_players p JOIN games g ON g.id = p.game_id \
             WHERE p.guest_token_hash = $1 AND p.is_active",
        )
        .bind(input.token_hash)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(slot) = slot {
            let game_id: Uuid = slot.try_get("game_id")?;
            let status: String = slot.try_get("status")?;
            let seat: i16 = slot.try_get("seat")?;
            let own_waiting = status == "waiting" && seat == 1;
            let refusal = if !own_waiting {
                Some(GameError::ActiveGameExists {
                    game_id: Some(game_id),
                })
            } else if !input.cancel_own_waiting_game {
                Some(GameError::OwnWaitingGame {
                    game_id,
                    on_join: false,
                })
            } else if !cancel_waiting_game(&mut tx, game_id, now).await? {
                // Somebody joined between the read and the cancel: the game is theirs now.
                Some(GameError::ActiveGameExists {
                    game_id: Some(game_id),
                })
            } else {
                release_slots(&mut tx, game_id).await?;
                None
            };
            if let Some(refusal) = refusal {
                tx.commit().await?;
                return Err(refusal);
            }
        }
        // The token moves to the new game: older rows must not keep it (it is unique).
        sqlx::query("UPDATE game_players SET guest_token_hash = NULL WHERE guest_token_hash = $1")
            .bind(input.token_hash)
            .execute(&mut *tx)
            .await?;
        let code = fresh_code(&mut tx, new_code).await?;
        let expires_at = now + Duration::milliseconds(invitation_ttl_ms);
        let created = insert_waiting_game(
            &mut tx,
            Creator::Guest {
                token_hash: input.token_hash,
                nickname: input.nickname,
            },
            code,
            expires_at,
            now,
            input.is_public,
        )
        .await?;
        tx.commit().await?;
        Ok(created)
    }

    /// Cancels the caller's waiting game: only the creator (seat 1) may, and only while nobody has joined.
    pub async fn cancel_game(
        &self,
        player_id: Uuid,
        game_id: Uuid,
        now: OffsetDateTime,
    ) -> Result<(), GameError> {
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
        if !cancel_waiting_game(&mut tx, game_id, now).await? {
            return Err(GameError::GameNotCancellable);
        }
        release_slots(&mut tx, game_id).await?;
        tx.commit().await?;
        Ok(())
    }
}
