//! The `gomoku-db` command line: schema management. The app never changes the schema by itself.

use thiserror::Error;

use crate::{Database, DbError};

#[derive(Debug, Error)]
pub enum CliError {
    #[error("usage: gomoku-db <migrate>")]
    Usage,
    #[error(transparent)]
    Db(#[from] DbError),
}

/// Runs one command; returns the line to print on success.
pub async fn run(args: &[String], db: &Database) -> Result<String, CliError> {
    match args.first().map(String::as_str) {
        Some("migrate") if args.len() == 1 => {
            db.migrate().await?;
            Ok("migrations applied".to_owned())
        }
        _ => Err(CliError::Usage),
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

    #[tokio::test]
    async fn rejects_missing_unknown_and_extra_arguments() {
        let db = dead_database();
        for args in [
            vec![],
            vec!["nope".to_owned()],
            vec!["migrate".to_owned(), "extra".to_owned()],
        ] {
            assert!(
                matches!(run(&args, &db).await, Err(CliError::Usage)),
                "{args:?}"
            );
        }
    }

    #[tokio::test]
    async fn reports_a_database_failure() {
        let result = run(&["migrate".to_owned()], &dead_database()).await;
        assert!(matches!(result, Err(CliError::Db(_))));
    }
}
