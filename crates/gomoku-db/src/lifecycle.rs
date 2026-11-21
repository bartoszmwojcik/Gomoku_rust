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
    ) -> Result<DeleteOutcome, DbError> {
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
