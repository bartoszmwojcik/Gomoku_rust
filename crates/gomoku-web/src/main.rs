//! Process entry point: wiring only (environment, logging, signals). Logic lives in the library.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use gomoku_db::{Database, DbConfig};
use gomoku_web::app::{AppState, build_router};
use gomoku_web::config::AppConfig;
use gomoku_web::server::serve;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let lookup = |key: &str| std::env::var(key).ok();
    let app_config = AppConfig::from_lookup(lookup)?;
    let db = Database::connect_lazy(&DbConfig::from_lookup(lookup)?)?;

    let state = AppState {
        health: Arc::new(db),
        now: Arc::new(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs())
        }),
    };
    let router = build_router(state, &app_config.assets_dir);
    let listener = tokio::net::TcpListener::bind(app_config.bind_addr).await?;
    tracing::info!(addr = %app_config.bind_addr, "listening");
    serve(listener, router, async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await?;
    Ok(())
}
