//! Process entry point of the schema CLI: environment and exit code only; the commands live in `gomoku_db::cli`.

use gomoku_db::{Database, DbConfig, cli};

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let _ = dotenvy::dotenv();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let outcome = async {
        let config =
            DbConfig::from_lookup(|key| std::env::var(key).ok()).map_err(|e| e.to_string())?;
        let db = Database::connect_lazy(&config).map_err(|e| e.to_string())?;
        cli::run(&args, &db).await.map_err(|e| e.to_string())
    }
    .await;
    match outcome {
        Ok(message) => {
            println!("{message}");
            std::process::ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("{message}");
            std::process::ExitCode::FAILURE
        }
    }
}
