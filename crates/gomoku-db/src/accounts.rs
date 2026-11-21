//! Users and sessions.

use sqlx::Row;
use sqlx::postgres::PgRow;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{Database, DbError};

const EMAIL_CONSTRAINT: &str = "users_email_key";
const DISPLAY_NAME_CONSTRAINT: &str = "users_display_name_key_unique";

/// Data of an account to create; `email` and `display_name_key` are already normalized.
#[derive(Debug, Clone)]
pub struct NewUser<'a> {
    pub email: &'a str,
    pub display_name: &'a str,
    pub display_name_key: &'a str,
    pub password_hash: &'a str,
}

/// A new session: only the hash of the cookie token is stored.
#[derive(Debug, Clone)]
pub struct NewSession<'a> {
    pub token_hash: &'a str,
    pub expires_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub id: Uuid,
    pub email: String,
    pub display_name: String,
    pub password_hash: String,
    pub email_verified_at: Option<OffsetDateTime>,
    pub display_name_changed_at: Option<OffsetDateTime>,
}

fn user_of(row: &PgRow) -> Result<User, sqlx::Error> {
    Ok(User {
        id: row.try_get("id")?,
        email: row.try_get("email")?,
        display_name: row.try_get("display_name")?,
        password_hash: row.try_get("password_hash")?,
        email_verified_at: row.try_get("email_verified_at")?,
        display_name_changed_at: row.try_get("display_name_changed_at")?,
    })
}

/// Why an account could not be created.
#[derive(Debug, thiserror::Error)]
pub enum RegisterError {
    #[error("e-mail taken")]
    EmailTaken,
    #[error("display name taken")]
    DisplayNameTaken,
    #[error(transparent)]
    Db(#[from] DbError),
}

/// The user behind a valid session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionUser {
    pub session_id: Uuid,
    pub user_id: Uuid,
    pub email: String,
    pub display_name: String,
    pub email_verified_at: Option<OffsetDateTime>,
    pub expires_at: OffsetDateTime,
}

impl Database {
    /// Creates an account and its first session in one transaction. Uniqueness is decided by the database
    /// indexes, not by a prior lookup, so concurrent registrations cannot both succeed. PostgreSQL reports only
    /// the first violated index, so when the name index fired the e-mail may be taken as well: it is looked up,
    /// so the uniform e-mail answer wins and a name conflict never confirms an e-mail.
    pub async fn register_user(
        &self,
        user: &NewUser<'_>,
        session: &NewSession<'_>,
    ) -> Result<Uuid, RegisterError> {
        let mut tx = self.pool().begin().await.map_err(DbError::from)?;
        let inserted = sqlx::query(
            "INSERT INTO users (email, display_name, display_name_key, password_hash) \
             VALUES ($1, $2, $3, $4) RETURNING id",
        )
        .bind(user.email)
        .bind(user.display_name)
        .bind(user.display_name_key)
        .bind(user.password_hash)
        .fetch_one(&mut *tx)
        .await;
        let id: Uuid = match inserted {
            Ok(row) => row.try_get("id").map_err(DbError::from)?,
            Err(error) => {
                let constraint = error
                    .as_database_error()
                    .filter(|db| db.is_unique_violation())
                    .and_then(|db| db.constraint().map(str::to_owned));
                drop(tx);
                return Err(match constraint.as_deref() {
                    Some(EMAIL_CONSTRAINT) => RegisterError::EmailTaken,
                    Some(DISPLAY_NAME_CONSTRAINT) => {
                        if self.find_user_by_email(user.email).await?.is_some() {
                            RegisterError::EmailTaken
                        } else {
                            RegisterError::DisplayNameTaken
                        }
                    }
                    _ => RegisterError::Db(error.into()),
                });
            }
        };
        sqlx::query("INSERT INTO sessions (user_id, token_hash, expires_at) VALUES ($1, $2, $3)")
            .bind(id)
            .bind(session.token_hash)
            .bind(session.expires_at)
            .execute(&mut *tx)
            .await
            .map_err(DbError::from)?;
        tx.commit().await.map_err(DbError::from)?;
        Ok(id)
    }

    pub async fn find_user_by_email(&self, email: &str) -> Result<Option<User>, DbError> {
        let row = sqlx::query(
            "SELECT id, email, display_name, password_hash, email_verified_at, display_name_changed_at \
             FROM users WHERE email = $1",
        )
        .bind(email)
        .fetch_optional(self.pool())
        .await?;
        Ok(row.as_ref().map(user_of).transpose()?)
    }

    pub async fn find_user_by_id(&self, id: Uuid) -> Result<Option<User>, DbError> {
        let row = sqlx::query(
            "SELECT id, email, display_name, password_hash, email_verified_at, display_name_changed_at \
             FROM users WHERE id = $1",
        )
            .bind(id)
            .fetch_optional(self.pool())
            .await?;
        Ok(row.as_ref().map(user_of).transpose()?)
    }

    pub async fn create_session(
        &self,
        user_id: Uuid,
        session: &NewSession<'_>,
    ) -> Result<(), DbError> {
        sqlx::query("INSERT INTO sessions (user_id, token_hash, expires_at) VALUES ($1, $2, $3)")
            .bind(user_id)
            .bind(session.token_hash)
            .bind(session.expires_at)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// The user behind a token hash, or `None` when the session is unknown or expired at `now`.
    pub async fn find_valid_session(
        &self,
        token_hash: &str,
        now: OffsetDateTime,
    ) -> Result<Option<SessionUser>, DbError> {
        let row = sqlx::query(
            "SELECT s.id AS session_id, s.expires_at, u.id AS user_id, u.email, u.display_name, \
                    u.email_verified_at \
             FROM sessions s JOIN users u ON u.id = s.user_id \
             WHERE s.token_hash = $1 AND (s.expires_at > $2 OR EXISTS ( \
                SELECT 1 FROM game_players p JOIN games g ON g.id = p.game_id \
                WHERE p.user_id = s.user_id AND p.is_active AND g.status = 'active' AND g.started_at <= s.expires_at))",
        )
        .bind(token_hash)
        .bind(now)
        .fetch_optional(self.pool())
        .await?;
        row.map(|row| {
            Ok(SessionUser {
                session_id: row.try_get("session_id")?,
                user_id: row.try_get("user_id")?,
                email: row.try_get("email")?,
                display_name: row.try_get("display_name")?,
                email_verified_at: row.try_get("email_verified_at")?,
                expires_at: row.try_get("expires_at")?,
            })
        })
        .transpose()
        .map_err(DbError::Sqlx)
    }

    /// Revokes only the session behind `token_hash`; idempotent.
    /// Whoever logs out also stops looking for a game (best effort: a failure here must not keep the session alive; the
    /// status expires on its own).
    pub async fn revoke_session(&self, token_hash: &str) -> Result<(), DbError> {
        let _ = sqlx::query("DELETE FROM lobby_seekers WHERE user_id IN (SELECT user_id FROM sessions WHERE token_hash = $1)")
            .bind(token_hash)
            .execute(self.pool())
            .await;
        sqlx::query("DELETE FROM sessions WHERE token_hash = $1")
            .bind(token_hash)
            .execute(self.pool())
            .await?;
        Ok(())
    }
}
