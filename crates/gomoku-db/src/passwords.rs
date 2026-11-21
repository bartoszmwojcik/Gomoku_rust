//! Password change and reset: everything that writes a password hash also ends sessions and invalidates links,
//! all or nothing.

use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{Database, DbError};

/// A stored reset link: its id and the creation time that orders links of one user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResetToken {
    pub id: Uuid,
}

/// Marks the user's unused password reset and e-mail change links as used: a password change or reset kills a
/// stale link of either kind. Runs inside the caller's transaction.
async fn invalidate_link_tokens(
    tx: &mut sqlx::PgConnection,
    user_id: Uuid,
    now: OffsetDateTime,
) -> Result<(), sqlx::Error> {
    for sql in [
        "UPDATE password_reset_tokens SET used_at = $2 WHERE user_id = $1 AND used_at IS NULL",
        "UPDATE email_change_tokens SET used_at = $2 WHERE user_id = $1 AND used_at IS NULL",
    ] {
        sqlx::query(sql)
            .bind(user_id)
            .bind(now)
            .execute(&mut *tx)
            .await?;
    }
    Ok(())
}

impl Database {
    /// Replaces the hash only if it is still `previous_hash` (so of two parallel changes only the first wins),
    /// removes the user's other sessions (all but `current_session`) and invalidates their unused reset links, in
    /// one transaction. `Ok(false)` when the hash changed meanwhile (nothing was written).
    pub async fn change_password(
        &self,
        user_id: Uuid,
        previous_hash: &str,
        new_hash: &str,
        current_session: Uuid,
        now: OffsetDateTime,
    ) -> Result<bool, DbError> {
        let mut tx = self.pool().begin().await?;
        let updated =
            sqlx::query("UPDATE users SET password_hash = $3 WHERE id = $1 AND password_hash = $2")
                .bind(user_id)
                .bind(previous_hash)
                .bind(new_hash)
                .execute(&mut *tx)
                .await?
                .rows_affected();
        if updated == 0 {
            return Ok(false);
        }
        sqlx::query("DELETE FROM sessions WHERE user_id = $1 AND id <> $2")
            .bind(user_id)
            .bind(current_session)
            .execute(&mut *tx)
            .await?;
        invalidate_link_tokens(&mut tx, user_id, now).await?;
        tx.commit().await?;
        Ok(true)
    }

    pub async fn insert_reset_token(
        &self,
        user_id: Uuid,
        token_hash: &str,
        created_at: OffsetDateTime,
        expires_at: OffsetDateTime,
    ) -> Result<ResetToken, DbError> {
        let row = sqlx::query(
            "INSERT INTO password_reset_tokens (user_id, token_hash, created_at, expires_at) \
             VALUES ($1, $2, $3, $4) RETURNING id",
        )
        .bind(user_id)
        .bind(token_hash)
        .bind(created_at)
        .bind(expires_at)
        .fetch_one(self.pool())
        .await?;
        Ok(ResetToken {
            id: row.try_get("id")?,
        })
    }

    /// Removes a link that was never delivered.
    pub async fn delete_reset_token(&self, id: Uuid) -> Result<(), DbError> {
        sqlx::query("DELETE FROM password_reset_tokens WHERE id = $1")
            .bind(id)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// Invalidates the user's unused links older than `(created_at, id)`, so of parallel requests the newest link
    /// always stays valid.
    pub async fn invalidate_older_reset_tokens(
        &self,
        user_id: Uuid,
        created_at: OffsetDateTime,
        id: Uuid,
        now: OffsetDateTime,
    ) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE password_reset_tokens SET used_at = $4 \
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

    /// Sets a new password with a reset link. One transaction consumes the token (a conditional `UPDATE ...
    /// RETURNING`: of parallel calls exactly one wins; only for a confirmed address), replaces the hash, deletes
    /// all of the user's sessions and invalidates their other unused links. `Ok(None)` for an unknown, expired
    /// or used token.
    pub async fn confirm_password_reset(
        &self,
        token_hash: &str,
        new_hash: &str,
        now: OffsetDateTime,
    ) -> Result<Option<Uuid>, DbError> {
        let mut tx = self.pool().begin().await?;
        let consumed = sqlx::query(
            "UPDATE password_reset_tokens SET used_at = $2 \
             WHERE token_hash = $1 AND used_at IS NULL AND expires_at > $2 \
               AND EXISTS (SELECT 1 FROM users u WHERE u.id = password_reset_tokens.user_id \
                           AND u.email_verified_at IS NOT NULL) \
             RETURNING user_id",
        )
        .bind(token_hash)
        .bind(now)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = consumed else { return Ok(None) };
        let user_id: Uuid = row.try_get("user_id")?;
        sqlx::query("UPDATE users SET password_hash = $2 WHERE id = $1")
            .bind(user_id)
            .bind(new_hash)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM sessions WHERE user_id = $1")
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
        invalidate_link_tokens(&mut tx, user_id, now).await?;
        tx.commit().await?;
        Ok(Some(user_id))
    }
}
