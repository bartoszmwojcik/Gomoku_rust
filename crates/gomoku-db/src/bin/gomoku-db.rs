//! Process entry point of the schema and maintenance CLI: environment and exit code only; the commands live in
//! `gomoku_db::cli`.

use gomoku_db::{Database, DbConfig, OffsetDateTime, cli};

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let _ = dotenvy::dotenv();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let env = |key: &str| std::env::var(key).ok();
    let report = match DbConfig::from_lookup(env).map(|config| Database::connect_lazy(&config)) {
        Ok(Ok(db)) => cli::run(&args, &db, &env, OffsetDateTime::now_utc()).await,
        Ok(Err(error)) => cli::CliReport {
            stdout: String::new(),
            stderr: cli::describe_error(&error),
            code: 1,
        },
        Err(error) => cli::CliReport {
            stdout: String::new(),
            stderr: error.to_string(),
            code: 1,
        },
    };
    if !report.stdout.is_empty() {
        println!("{}", report.stdout);
    }
    if !report.stderr.is_empty() {
        eprintln!("{}", report.stderr);
    }
    std::process::ExitCode::from(u8::try_from(report.code).unwrap_or(1))
}
