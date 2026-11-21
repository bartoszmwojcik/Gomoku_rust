//! Process entry point: wiring only (environment, logging, signals). Logic lives in the library.

use std::sync::Arc;

use gomoku_db::{Database, DbConfig};
use gomoku_web::app::{AppState, build_router};
use gomoku_web::clock::system_clock;
use gomoku_web::config::AppConfig;
use gomoku_web::email::create_email_sender;
use gomoku_web::i18n::Catalogs;
use gomoku_web::password::PasswordHasher;
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
    let config = AppConfig::from_lookup(lookup)?;
    let db = Database::connect_lazy(&DbConfig::from_lookup(lookup)?)?;
    // Migrations are applied by `gomoku-db migrate` / the deployment, never implicitly by the app.

    let clock = system_clock();
    let state = AppState {
        hasher: PasswordHasher::new(config.password)?,
        health: Arc::new(db.clone()),
        db,
        email: create_email_sender(&config.email, clock.clone()),
        tasks: tokio_util::task::TaskTracker::new(),
        clock,
        catalogs: Catalogs::embedded()?,
        config: Arc::new(config.clone()),
    };
    let tasks = state.tasks.clone();
    let router = build_router(state, &config.assets_dir);
    let listener = tokio::net::TcpListener::bind(config.bind_addr).await?;
    tracing::info!(addr = %config.bind_addr, "listening");
    serve(listener, router, async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await?;
    // Let the work that runs after a response (e.g. a reset e-mail being sent) finish.
    tasks.close();
    tasks.wait().await;
    Ok(())
}
