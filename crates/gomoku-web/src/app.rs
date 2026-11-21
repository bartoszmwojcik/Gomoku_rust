use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::middleware::from_fn_with_state;
use axum::routing::{get, post};
use gomoku_db::Database;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;

use crate::api::csrf;
use crate::clock::Clock;
use crate::config::AppConfig;
use crate::health::{HealthProbe, health};
use crate::i18n::{Catalogs, lang_redirect};
use crate::password::PasswordHasher;
use crate::{pages, routes_auth};

/// Shared application state: explicit composition instead of a DI container.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<AppConfig>,
    pub db: Database,
    pub health: Arc<dyn HealthProbe>,
    pub clock: Clock,
    pub hasher: PasswordHasher,
    pub catalogs: Arc<Catalogs>,
}

pub fn build_router(state: AppState, assets_dir: &Path) -> Router {
    let api = Router::new()
        .route("/auth/register", post(routes_auth::register))
        .route("/auth/login", post(routes_auth::login))
        .route("/auth/logout", post(routes_auth::logout))
        .layer(from_fn_with_state(state.clone(), csrf));
    Router::new()
        .route("/", get(pages::home))
        .route("/login", get(pages::login))
        .route("/register", get(pages::register))
        .route("/account", get(pages::account))
        .route("/spike/board", get(pages::board))
        .route("/spike/clock", get(pages::clock))
        .route("/api/health", get(health))
        .nest("/api", api)
        .nest_service("/assets", ServeDir::new(assets_dir))
        .layer(from_fn_with_state(state.clone(), lang_redirect))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
