use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::middleware::from_fn_with_state;
use axum::routing::{get, patch, post};
use gomoku_db::Database;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;

use crate::api::csrf;
use crate::clock::Clock;
use crate::config::AppConfig;
use crate::email::EmailSender;
use crate::health::{HealthProbe, health};
use crate::i18n::{Catalogs, lang_redirect};
use crate::password::PasswordHasher;
use crate::{pages, routes_account, routes_auth, routes_email, routes_password};

/// Shared application state: explicit composition instead of a DI container.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<AppConfig>,
    pub db: Database,
    pub health: Arc<dyn HealthProbe>,
    pub clock: Clock,
    pub hasher: PasswordHasher,
    pub catalogs: Arc<Catalogs>,
    pub email: Arc<dyn EmailSender>,
    /// Work that runs after the response (e.g. sending a reset link); awaited on shutdown and in tests.
    pub tasks: tokio_util::task::TaskTracker,
}

pub fn build_router(state: AppState, assets_dir: &Path) -> Router {
    let api = Router::new()
        .route("/auth/register", post(routes_auth::register))
        .route("/auth/login", post(routes_auth::login))
        .route("/auth/logout", post(routes_auth::logout))
        .route("/auth/password", post(routes_password::change))
        .route(
            "/auth/password-reset/request",
            post(routes_password::reset_request),
        )
        .route(
            "/auth/password-reset/confirm",
            post(routes_password::reset_confirm),
        )
        .route(
            "/auth/email-verification/resend",
            post(routes_email::resend),
        )
        .route(
            "/auth/email-verification/confirm",
            post(routes_email::confirm_verification),
        )
        .route(
            "/auth/email-change",
            post(routes_email::change_email_request),
        )
        .route(
            "/auth/email-change/confirm",
            post(routes_email::confirm_email_change),
        )
        .route(
            "/auth/display-name",
            patch(routes_email::change_display_name),
        )
        .route("/account/delete", post(routes_account::delete))
        .layer(from_fn_with_state(state.clone(), csrf));
    Router::new()
        .route("/", get(pages::home))
        .route("/login", get(pages::login))
        .route("/register", get(pages::register))
        .route("/account", get(pages::account))
        .route("/verify-email", get(pages::verify_email))
        .route("/confirm-email-change", get(pages::confirm_email_change))
        .route("/forgot-password", get(pages::forgot_password))
        .route("/reset-password", get(pages::reset_password))
        .route("/spike/board", get(pages::board))
        .route("/spike/clock", get(pages::clock))
        .route("/api/health", get(health))
        .nest("/api", api)
        .nest_service("/assets", ServeDir::new(assets_dir))
        .layer(from_fn_with_state(state.clone(), lang_redirect))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
