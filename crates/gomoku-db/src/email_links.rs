//! E-mail confirmation and change: one-time links and the profile writes that depend on them.

use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{Database, DbError};

const EMAIL_CONSTRAINT: &str = "users_email_key";
const DISPLAY_NAME_CONSTRAINT: &str = "users_display_name_key_unique";

/// What an applied e-mail change tells the caller: who, from where to where, and whether the old address had
/// been confirmed (only then it gets a notice).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmailChanged {
    pub user_id: Uuid,
    pub old_email: String,
    pub new_email: String,
    pub display_name: String,
    pub was_verified: bool,
}

/// Result of renaming a user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenameOutcome {
    Renamed,
    /// The 24 h window has not passed since `changed_at`.
    Cooldown {
        changed_at: OffsetDateTime,
    },
    Taken,
}

impl Database {
    pub async fn insert_verification_token(
        &self,
        user_id: Uuid,
        token_hash: &str,
        created_at: OffsetDateTime,
        expires_at: OffsetDateTime,
    ) -> Result<Uuid, DbError> {
        let row = sqlx::query(
            "INSERT INTO email_verification_tokens (user_id, token_hash, created_at, expires_at) \
             VALUES ($1, $2, $3, $4) RETURNING id",
        )
        .bind(user_id)
        .bind(token_hash)
        .bind(created_at)
        .bind(expires_at)
        .fetch_one(self.pool())
        .await?;
        Ok(row.try_get("id")?)
    }

    pub async fn delete_verification_token(&self, id: Uuid) -> Result<(), DbError> {
        sqlx::query("DELETE FROM email_verification_tokens WHERE id = $1")
            .bind(id)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// Invalidates the user's unused links older than `(created_at, id)`, so the newest link always stays valid.
    pub async fn invalidate_older_verification_tokens(
        &self,
        user_id: Uuid,
        created_at: OffsetDateTime,
        id: Uuid,
        now: OffsetDateTime,
    ) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE email_verification_tokens SET used_at = $4 \
             WHERE user_id = $1 AND used_at IS NULL AND (created_at < $2 OR (created_at = $2 AND id < $3))",
        )
        .bind(user_id)
        .bind(created_at)
        .bind(id)
        .bind(now)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Consumes the token and confirms the account's address in one transaction. `Ok(None)` for an unknown,
    /// expired, invalidated or used token. An account that is already confirmed keeps its original time; the
    /// token is still consumed.
    pub async fn confirm_email_verification(
        &self,
        token_hash: &str,
        now: OffsetDateTime,
    ) -> Result<Option<Uuid>, DbError> {
        let mut tx = self.pool().begin().await?;
        let consumed = sqlx::query(
            "UPDATE email_verification_tokens SET used_at = $2 \
             WHERE token_hash = $1 AND used_at IS NULL AND expires_at > $2 RETURNING user_id",
        )
        .bind(token_hash)
        .bind(now)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = consumed else { return Ok(None) };
        let user_id: Uuid = row.try_get("user_id")?;
        sqlx::query(
            "UPDATE users SET email_verified_at = $2 WHERE id = $1 AND email_verified_at IS NULL",
        )
        .bind(user_id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Some(user_id))
    }

    pub async fn email_is_taken(&self, email: &str) -> Result<bool, DbError> {
        let row = sqlx::query("SELECT EXISTS (SELECT 1 FROM users WHERE email = $1) AS taken")
            .bind(email)
            .fetch_one(self.pool())
            .await?;
        Ok(row.try_get("taken")?)
    }

    pub async fn insert_email_change_token(
        &self,
        user_id: Uuid,
        new_email: &str,
        token_hash: &str,
        created_at: OffsetDateTime,
        expires_at: OffsetDateTime,
    ) -> Result<Uuid, DbError> {
        let row = sqlx::query(
            "INSERT INTO email_change_tokens (user_id, new_email, token_hash, created_at, expires_at) \
             VALUES ($1, $2, $3, $4, $5) RETURNING id",
        )
        .bind(user_id)
        .bind(new_email)
        .bind(token_hash)
        .bind(created_at)
        .bind(expires_at)
        .fetch_one(self.pool())
        .await?;
        Ok(row.try_get("id")?)
    }

    pub async fn delete_email_change_token(&self, id: Uuid) -> Result<(), DbError> {
        sqlx::query("DELETE FROM email_change_tokens WHERE id = $1")
            .bind(id)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    pub async fn invalidate_older_email_change_tokens(
        &self,
        user_id: Uuid,
        created_at: OffsetDateTime,
        id: Uuid,
        now: OffsetDateTime,
    ) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE email_change_tokens SET used_at = $4 \
             WHERE user_id = $1 AND used_at IS NULL AND (created_at < $2 OR (created_at = $2 AND id < $3))",
        )
        .bind(user_id)
        .bind(created_at)
        .bind(id)
        .bind(now)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Applies an e-mail change with the link token. One transaction consumes the token (of parallel calls exactly
    /// one wins), sets the new address as confirmed, deletes every session and kills the unused reset,
    /// verification and change links. `Ok(None)` for an unknown, expired or used token, for an address taken
    /// meanwhile (the unique index) and when `is_blocked(old, new)` says one of the addresses is the
    /// administrator's (the role may have been assigned after the request); in those cases nothing is written.
    pub async fn apply_email_change(
        &self,
        token_hash: &str,
        now: OffsetDateTime,
        is_blocked: impl Fn(&str, &str) -> bool,
    ) -> Result<Option<EmailChanged>, DbError> {
        let mut tx = self.pool().begin().await?;
        let consumed = sqlx::query(
            "UPDATE email_change_tokens SET used_at = $2 \
             WHERE token_hash = $1 AND used_at IS NULL AND expires_at > $2 RETURNING user_id, new_email",
        )
        .bind(token_hash)
        .bind(now)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = consumed else { return Ok(None) };
        let user_id: Uuid = row.try_get("user_id")?;
        let new_email: String = row.try_get("new_email")?;
        let user = sqlx::query(
            "SELECT email, display_name, email_verified_at FROM users WHERE id = $1 FOR UPDATE",
        )
        .bind(user_id)
        .fetch_one(&mut *tx)
        .await?;
        let old_email: String = user.try_get("email")?;
        if is_blocked(&old_email, &new_email) {
            return Ok(None);
        }
        let was_verified = user
            .try_get::<Option<OffsetDateTime>, _>("email_verified_at")?
            .is_some();
        let updated =
            sqlx::query("UPDATE users SET email = $2, email_verified_at = $3 WHERE id = $1")
                .bind(user_id)
                .bind(&new_email)
                .bind(now)
                .execute(&mut *tx)
                .await;
        if let Err(error) = updated {
            let taken = error.as_database_error().is_some_and(|db| {
                db.is_unique_violation() && db.constraint() == Some(EMAIL_CONSTRAINT)
            });
            return if taken { Ok(None) } else { Err(error.into()) };
        }
        sqlx::query("DELETE FROM sessions WHERE user_id = $1")
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
        for sql in [
            "UPDATE password_reset_tokens SET used_at = $2 WHERE user_id = $1 AND used_at IS NULL",
            "UPDATE email_verification_tokens SET used_at = $2 WHERE user_id = $1 AND used_at IS NULL",
            "UPDATE email_change_tokens SET used_at = $2 WHERE user_id = $1 AND used_at IS NULL",
        ] {
            sqlx::query(sql)
                .bind(user_id)
                .bind(now)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(Some(EmailChanged {
            user_id,
            old_email,
            new_email,
            display_name: user.try_get("display_name")?,
            was_verified,
        }))
    }

    /// Renames a user once per window: the window is checked in the same conditional `UPDATE` that writes the
    /// name (`changed_at` null or at most `cutoff`), so two parallel requests cannot both pass; uniqueness is
    /// decided by the `display_name_key` index.
    pub async fn rename_user(
        &self,
        user_id: Uuid,
        display_name: &str,
        display_name_key: &str,
        now: OffsetDateTime,
        cutoff: OffsetDateTime,
    ) -> Result<RenameOutcome, DbError> {
        let updated = sqlx::query(
            "UPDATE users SET display_name = $2, display_name_key = $3, display_name_changed_at = $4 \
             WHERE id = $1 AND (display_name_changed_at IS NULL OR display_name_changed_at <= $5)",
        )
        .bind(user_id)
        .bind(display_name)
        .bind(display_name_key)
        .bind(now)
        .bind(cutoff)
        .execute(self.pool())
        .await;
        match updated {
            Ok(done) if done.rows_affected() > 0 => Ok(RenameOutcome::Renamed),
            Ok(_) => {
                let row = sqlx::query("SELECT display_name_changed_at FROM users WHERE id = $1")
                    .bind(user_id)
                    .fetch_optional(self.pool())
                    .await?;
                let changed_at = row
                    .map(|r| r.try_get::<Option<OffsetDateTime>, _>("display_name_changed_at"))
                    .transpose()?
                    .flatten()
                    .unwrap_or(now);
                Ok(RenameOutcome::Cooldown { changed_at })
            }
            Err(error)
                if error.as_database_error().is_some_and(|db| {
                    db.is_unique_violation() && db.constraint() == Some(DISPLAY_NAME_CONSTRAINT)
                }) =>
            {
                Ok(RenameOutcome::Taken)
            }
            Err(error) => Err(error.into()),
        }
    }
}
