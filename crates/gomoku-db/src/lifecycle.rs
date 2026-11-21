//! Account deletion and the installation administrator's account.

use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{Database, DbError};

/// Result of deleting an account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteOutcome {
    Deleted,
    /// The account no longer exists (a concurrent deletion won).
    Gone,
    /// The installation administrator's account is never deleted.
    Protected,
    /// The stored hash is not the one that was verified (the password changed meanwhile).
    HashChanged,
    /// A game is in progress; nothing was written.
    ActiveGame(Uuid),
}

/// What `provision_admin` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProvisionAction {
    Created,
    /// An existing account was taken over: new password, confirmed e-mail, every session and unused link revoked.
    TakenOver,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provisioned {
    pub action: ProvisionAction,
    pub user_id: Uuid,
    pub revoked_sessions: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum ProvisionError {
    #[error("a display name is required to create the account")]
    NameRequired,
    #[error("the display name is taken")]
    NameTaken,
    #[error(transparent)]
    Db(#[from] DbError),
}

impl Database {
    /// Deletes an account for good: its sessions and links go with it (`ON DELETE CASCADE`). One transaction locks
    /// the user row, refuses the administrator (`is_protected`, read under the lock so a race with the admin CLI
    /// cannot let the deletion through) and deletes only if the hash is still the verified one, so a password
    /// changed meanwhile wins.
    pub async fn delete_account(
        &self,
        user_id: Uuid,
        verified_hash: &str,
        is_protected: impl Fn(&str) -> bool,
        now: OffsetDateTime,
    ) -> Result<DeleteOutcome, crate::games::GameError> {
        // Games the clock or the ready deadline already ended are closed first, so only a game truly in progress blocks.
        self.settle_for(user_id, now).await?;
        {
            let mut conn = self.pool().acquire().await?;
            crate::games::close_stale_of_user(&mut conn, now, user_id).await?;
        }
        let mut tx = self.pool().begin().await?;
        let locked = sqlx::query("SELECT email FROM users WHERE id = $1 FOR NO KEY UPDATE")
            .bind(user_id)
            .fetch_optional(&mut *tx)
            .await?;
        let Some(row) = locked else {
            return Ok(DeleteOutcome::Gone);
        };
        let email: String = row.try_get("email")?;
        if is_protected(&email) {
            return Ok(DeleteOutcome::Protected);
        }
        // The account's games that still matter, locked in game-id order: a game in progress refuses the deletion before
        // any write; the games still waiting are removed with their invitations.
        let open = sqlx::query(
            "SELECT g.id, g.status FROM games g JOIN game_players p ON p.game_id = g.id \
             WHERE p.user_id = $1 AND g.status IN ('waiting', 'active') ORDER BY g.id FOR UPDATE OF g",
        )
        .bind(user_id)
        .fetch_all(&mut *tx)
        .await?;
        let mut waiting = Vec::new();
        for row in &open {
            let status: String = row.try_get("status")?;
            let id: Uuid = row.try_get("id")?;
            if status == "active" {
                return Ok(DeleteOutcome::ActiveGame(id));
            }
            waiting.push(id);
        }
        if !waiting.is_empty() {
            sqlx::query("DELETE FROM games WHERE id = ANY($1)")
                .bind(&waiting)
                .execute(&mut *tx)
                .await?;
        }
        // Pending rematch offers the account is part of are closed (locked in the same order).
        let offers = sqlx::query(
            "SELECT g.id FROM games g JOIN game_players p ON p.game_id = g.id \
             WHERE p.user_id = $1 AND g.rematch_state = 'pending' ORDER BY g.id FOR UPDATE OF g",
        )
        .bind(user_id)
        .fetch_all(&mut *tx)
        .await?;
        let offer_ids: Vec<Uuid> = offers
            .iter()
            .map(|r| r.try_get("id"))
            .collect::<Result<_, _>>()?;
        if !offer_ids.is_empty() {
            sqlx::query("UPDATE games SET rematch_state = 'declined' WHERE id = ANY($1)")
                .bind(&offer_ids)
                .execute(&mut *tx)
                .await?;
        }
        // The deleted account's name must not outlive it: the seat shows "deleted player".
        sqlx::query("UPDATE game_players SET display_name = NULL WHERE user_id = $1")
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
        let deleted = sqlx::query("DELETE FROM users WHERE id = $1 AND password_hash = $2")
            .bind(user_id)
            .bind(verified_hash)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        if deleted == 0 {
            return Ok(DeleteOutcome::HashChanged);
        }
        tx.commit().await?;
        Ok(DeleteOutcome::Deleted)
    }

    /// Creates the administrator's account (confirmed e-mail) or takes an existing one over. A taken-over account
    /// keeps its name; every session and unused link is revoked in the same transaction.
    pub async fn provision_admin(
        &self,
        email: &str,
        display_name: Option<(&str, &str)>,
        password_hash: &str,
        now: OffsetDateTime,
    ) -> Result<Provisioned, ProvisionError> {
        let mut tx = self.pool().begin().await.map_err(DbError::from)?;
        let existing = sqlx::query("SELECT id FROM users WHERE email = $1 FOR UPDATE")
            .bind(email)
            .fetch_optional(&mut *tx)
            .await
            .map_err(DbError::from)?;
        let provisioned = match existing {
            Some(row) => {
                let user_id: Uuid = row.try_get("id").map_err(DbError::from)?;
                sqlx::query("UPDATE users SET password_hash = $2, email_verified_at = COALESCE(email_verified_at, $3) WHERE id = $1")
                    .bind(user_id)
                    .bind(password_hash)
                    .bind(now)
                    .execute(&mut *tx)
                    .await
                    .map_err(DbError::from)?;
                let revoked = sqlx::query("DELETE FROM sessions WHERE user_id = $1")
                    .bind(user_id)
                    .execute(&mut *tx)
                    .await
                    .map_err(DbError::from)?
                    .rows_affected();
                for sql in [
                    "UPDATE password_reset_tokens SET used_at = $2 WHERE user_id = $1 AND used_at IS NULL",
                    "UPDATE email_verification_tokens SET used_at = $2 WHERE user_id = $1 AND used_at IS NULL",
                    "UPDATE email_change_tokens SET used_at = $2 WHERE user_id = $1 AND used_at IS NULL",
                ] {
                    sqlx::query(sql)
                        .bind(user_id)
                        .bind(now)
                        .execute(&mut *tx)
                        .await
                        .map_err(DbError::from)?;
                }
                Provisioned {
                    action: ProvisionAction::TakenOver,
                    user_id,
                    revoked_sessions: revoked,
                }
            }
            None => {
                let (name, key) = display_name.ok_or(ProvisionError::NameRequired)?;
                let inserted = sqlx::query(
                    "INSERT INTO users (email, display_name, display_name_key, password_hash, email_verified_at) \
                     VALUES ($1, $2, $3, $4, $5) RETURNING id",
                )
                .bind(email)
                .bind(name)
                .bind(key)
                .bind(password_hash)
                .bind(now)
                .fetch_one(&mut *tx)
                .await;
                match inserted {
                    Ok(row) => Provisioned {
                        action: ProvisionAction::Created,
                        user_id: row.try_get("id").map_err(DbError::from)?,
                        revoked_sessions: 0,
                    },
                    Err(error)
                        if error
                            .as_database_error()
                            .is_some_and(|db| db.is_unique_violation()) =>
                    {
                        return Err(ProvisionError::NameTaken);
                    }
                    Err(error) => return Err(DbError::from(error).into()),
                }
            }
        };
        tx.commit().await.map_err(DbError::from)?;
        Ok(provisioned)
    }
}
