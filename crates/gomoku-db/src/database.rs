use std::time::Duration;

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use thiserror::Error;

use crate::DbConfig;

#[derive(Debug, Error)]
pub enum DbError {
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("migration error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
}

/// Process-wide handle to PostgreSQL. The pool connects lazily, so building it never fails
/// because the database is down; availability is checked by running a query (`ping`).
#[derive(Debug, Clone)]
pub struct Database {
    pool: PgPool,
}

impl Database {
    pub fn connect_lazy(config: &DbConfig) -> Result<Self, DbError> {
        let pool = PgPoolOptions::new()
            .max_connections(config.pool_max)
            .acquire_timeout(Duration::from_secs(3))
            .connect_lazy(&config.url)?;
        Ok(Self { pool })
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Applies pending migrations from `crates/gomoku-db/migrations`.
    pub async fn migrate(&self) -> Result<(), DbError> {
        sqlx::migrate!("./migrations").run(&self.pool).await?;
        Ok(())
    }

    /// Runs `SELECT 1`; `orm.checkConnection`-style checks are not reliable with lazy pools.
    pub async fn ping(&self) -> Result<(), DbError> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dead_database() -> Database {
        let config = DbConfig {
            url: "postgres://u:p@127.0.0.1:1/none".into(),
            pool_max: 1,
        };
        Database::connect_lazy(&config).unwrap()
    }

    #[test]
    fn rejects_an_unparsable_url() {
        let config = DbConfig {
            url: "not a url".into(),
            pool_max: 1,
        };
        let err = Database::connect_lazy(&config).unwrap_err();
        assert!(matches!(err, DbError::Sqlx(_)));
        assert!(err.to_string().starts_with("database error:"));
    }

    #[tokio::test]
    async fn ping_and_migrate_report_errors_when_the_database_is_down() {
        let db = dead_database();
        assert!(matches!(db.ping().await.unwrap_err(), DbError::Sqlx(_)));
        let err = db.migrate().await.unwrap_err();
        assert!(matches!(err, DbError::Migrate(_)));
        assert!(err.to_string().starts_with("migration error:"));
    }
}
