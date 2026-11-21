//! The `gomoku-db` command line: schema management and maintenance. The app never changes the schema by itself.

use time::OffsetDateTime;

use crate::cleanup::{CleanupOptions, cleanup_expired};
use crate::{Database, DbError};

/// What a command printed and how it ended; the binary only forwards it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliReport {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
}

impl CliReport {
    fn ok(stdout: impl Into<String>) -> Self {
        Self {
            stdout: stdout.into(),
            stderr: String::new(),
            code: 0,
        }
    }

    fn failed(stderr: impl Into<String>) -> Self {
        Self {
            stdout: String::new(),
            stderr: stderr.into(),
            code: 1,
        }
    }
}

const USAGE: &str = "usage: gomoku-db <migrate|cleanup>";

/// Only the class and the SQL state of an error: a driver message may carry the connection string or row values.
pub fn describe_error(error: &DbError) -> String {
    match error {
        DbError::Sqlx(sqlx::Error::Database(db)) => {
            format!("database error {}", db.code().as_deref().unwrap_or("?"))
        }
        DbError::Sqlx(sqlx::Error::PoolTimedOut) => {
            "database unavailable (pool timed out)".to_owned()
        }
        DbError::Sqlx(sqlx::Error::Io(_)) => "database unavailable (I/O error)".to_owned(),
        DbError::Sqlx(_) => "database error".to_owned(),
        DbError::Migrate(_) => "migration error".to_owned(),
    }
}

/// Runs one command. `cleanup` sweeps expired rows (cron, hourly) and keeps the account of `ADMIN_EMAIL`.
pub async fn run(
    args: &[String],
    db: &Database,
    env: &dyn Fn(&str) -> Option<String>,
    now: OffsetDateTime,
) -> CliReport {
    match args.first().map(String::as_str) {
        Some("migrate") if args.len() == 1 => match db.migrate().await {
            Ok(()) => CliReport::ok("migrations applied"),
            Err(error) => CliReport::failed(format!(
                "{{\"ok\":false,\"error\":\"{}\"}}",
                describe_error(&error)
            )),
        },
        Some("cleanup") if args.len() == 1 => {
            let mut options = CleanupOptions::new(now);
            options.protected_email = env("ADMIN_EMAIL")
                .map(|raw| gomoku_core::validators::normalize_email(&raw))
                .filter(|email| !email.is_empty());
            let result = cleanup_expired(db, &options).await;
            let line = serde_json::to_string(&result).unwrap_or_default();
            // The progress of the other rules is printed even when some failed; the failures go to stderr with exit code 1.
            let stderr = if result.errors.is_empty() {
                String::new()
            } else {
                serde_json::json!({ "ok": false, "errors": result.errors }).to_string()
            };
            CliReport {
                stdout: line,
                code: i32::from(!result.errors.is_empty()),
                stderr,
            }
        }
        _ => CliReport {
            stdout: String::new(),
            stderr: USAGE.to_owned(),
            code: 2,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DbConfig;

    fn dead_database() -> Database {
        Database::connect_lazy(&DbConfig {
            url: "postgres://u:p@127.0.0.1:1/none".into(),
            pool_max: 1,
        })
        .unwrap()
    }

    fn now() -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap()
    }

    async fn run_with(args: &[&str]) -> CliReport {
        let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        run(&args, &dead_database(), &|_| None, now()).await
    }

    #[tokio::test]
    async fn rejects_missing_unknown_and_extra_arguments() {
        for args in [
            vec![],
            vec!["nope"],
            vec!["migrate", "extra"],
            vec!["cleanup", "extra"],
        ] {
            let report = run_with(&args).await;
            assert_eq!(
                (report.code, report.stderr.as_str()),
                (2, USAGE),
                "{args:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_down_database_is_a_masked_failure() {
        let report = run_with(&["migrate"]).await;
        assert_eq!(report.code, 1);
        assert!(
            report.stderr.contains("\"ok\":false") && !report.stderr.contains("postgres://"),
            "{}",
            report.stderr
        );
    }

    #[tokio::test]
    async fn cleanup_reports_each_failed_rule_by_name_and_code_only() {
        let report = run_with(&["cleanup"]).await;
        assert_eq!(report.code, 1);
        assert!(report.stdout.contains("\"errors\"") && report.stdout.contains("\"sessions\":0"));
        assert!(
            report.stderr.contains("\"rule\":\"sessions\"")
                && !report.stderr.contains("postgres://")
        );
    }

    #[test]
    fn describes_errors_without_their_messages() {
        let timed_out = DbError::Sqlx(sqlx::Error::PoolTimedOut);
        assert_eq!(
            describe_error(&timed_out),
            "database unavailable (pool timed out)"
        );
        let io = DbError::Sqlx(sqlx::Error::Io(std::io::Error::other("secret")));
        assert_eq!(describe_error(&io), "database unavailable (I/O error)");
        assert_eq!(
            describe_error(&DbError::Sqlx(sqlx::Error::RowNotFound)),
            "database error"
        );
        let migrate = DbError::Migrate(sqlx::migrate::MigrateError::VersionMissing(1));
        assert_eq!(describe_error(&migrate), "migration error");
    }
}
