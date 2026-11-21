//! Fixed-window attempt counters kept entirely in SQL, so concurrent requests cannot lose an increment:
//! one upsert either opens a new window (the old one elapsed) or bumps the counter. Database time (`now()`)
//! is used throughout, so app-server clock skew does not matter.

use sqlx::Row;

use crate::{Database, DbError};

/// State of a counter after an attempt was counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    pub count: i32,
    pub retry_after_seconds: i32,
}

impl Database {
    /// Counts one attempt for `key` in a window of `window_ms` and returns the counter state.
    pub async fn rate_limit_hit(&self, key: &str, window_ms: i64) -> Result<Hit, DbError> {
        let row = sqlx::query(
            r#"INSERT INTO rate_limit_buckets AS b (key, window_start, count)
               VALUES ($1, now(), 1)
               ON CONFLICT (key) DO UPDATE SET
                 window_start = CASE WHEN b.window_start + ($2::double precision * interval '1 millisecond') <= now()
                   THEN now() ELSE b.window_start END,
                 count = CASE WHEN b.window_start + ($2::double precision * interval '1 millisecond') <= now()
                   THEN 1 ELSE b.count + 1 END
               RETURNING count,
                 greatest(1, ceil(extract(epoch from
                   (b.window_start + ($2::double precision * interval '1 millisecond') - now()))))::int
                   AS retry_after_seconds"#,
        )
        .bind(key)
        .bind(window_ms as f64)
        .fetch_one(self.pool())
        .await?;
        Ok(Hit {
            count: row.try_get("count")?,
            retry_after_seconds: row.try_get("retry_after_seconds")?,
        })
    }

    /// Gives one attempt back, never below zero, only while the window it was counted in is live.
    pub async fn rate_limit_release(&self, key: &str, window_ms: i64) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE rate_limit_buckets SET count = greatest(count - 1, 0) \
             WHERE key = $1 AND window_start + ($2::double precision * interval '1 millisecond') > now()",
        )
        .bind(key)
        .bind(window_ms as f64)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Forgets the counter for `key`, e.g. after a successful login.
    pub async fn rate_limit_reset(&self, key: &str) -> Result<(), DbError> {
        sqlx::query("DELETE FROM rate_limit_buckets WHERE key = $1")
            .bind(key)
            .execute(self.pool())
            .await?;
        Ok(())
    }
}
