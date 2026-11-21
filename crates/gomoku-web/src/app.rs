use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::routing::get;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;

use crate::health::{HealthProbe, health};
use crate::pages;

/// Seconds since the Unix epoch; injected so tests control time.
pub type NowFn = Arc<dyn Fn() -> u64 + Send + Sync>;

/// Shared application state: explicit composition instead of a DI container.
#[derive(Clone)]
pub struct AppState {
    pub health: Arc<dyn HealthProbe>,
    pub now: NowFn,
}

pub fn build_router(state: AppState, assets_dir: &Path) -> Router {
    Router::new()
        .route("/", get(pages::home))
        .route("/login", get(pages::login))
        .route("/spike/board", get(pages::board))
        .route("/spike/clock", get(pages::clock))
        .route("/api/health", get(health))
        .nest_service("/assets", ServeDir::new(assets_dir))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
