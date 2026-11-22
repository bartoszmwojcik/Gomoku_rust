use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::middleware::from_fn_with_state;
use axum::routing::{get, patch, post, put};
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
use crate::{
    pages, pages_games, routes_account, routes_auth, routes_chat, routes_email, routes_games,
    routes_lobby, routes_password, routes_spectate,
};

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
    /// The hosted bot: admission, compute gate and turn service.
    pub bot: Arc<crate::bot::BotRuntime>,
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
        .route("/games", post(routes_games::create_game))
        .route("/games/{id}", get(routes_games::get_game))
        .route("/games/{id}/moves", post(routes_games::post_move))
        .route("/games/{id}/resign", post(routes_games::post_resign))
        .route("/games/{id}/cancel", post(routes_games::post_cancel))
        .route("/games/{id}/rematch", post(routes_games::rematch_offer))
        .route(
            "/games/{id}/rematch/accept",
            post(routes_games::rematch_accept),
        )
        .route(
            "/games/{id}/rematch/decline",
            post(routes_games::rematch_decline),
        )
        .route(
            "/invitations/{code}/accept",
            post(routes_games::accept_invitation),
        )
        .route(
            "/games/{id}/chat",
            get(routes_chat::chat_get).post(routes_chat::chat_post),
        )
        .route("/games/{id}/chat/settings", put(routes_chat::chat_settings))
        .route(
            "/games/{id}/audience/close",
            post(routes_spectate::audience_close),
        )
        .route("/games/{id}/spectate", get(routes_spectate::spectate_state))
        .route(
            "/games/{id}/spectate/chat",
            get(routes_chat::spectator_chat_get).post(routes_chat::spectator_chat_post),
        )
        .route("/games/{id}/qr", get(routes_spectate::game_qr))
        .route(
            "/invitations/{code}/spectate",
            post(routes_spectate::spectate_join),
        )
        .route("/lobby", get(routes_lobby::lobby))
        .route(
            "/lobby/seeking",
            put(routes_lobby::mark_seeking).delete(routes_lobby::withdraw_seeking),
        )
        .route("/lobby/seekers/{id}/pick", post(routes_lobby::pick))
        .route("/history", get(routes_lobby::history))
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
        .route("/play", get(pages_games::play))
        .route("/join", get(pages_games::join))
        .route("/invite/{code}", get(pages_games::invite))
        .route("/games/{id}", get(pages_games::game_room))
        .route("/games/{id}/review", get(pages_games::review))
        .route("/lobby", get(pages_games::lobby))
        .route("/history", get(pages_games::history))
        .route("/api/health", get(health))
        .nest("/api", api)
        .merge(crate::mcp::mcp_router(&state))
        .nest_service("/assets", ServeDir::new(assets_dir))
        .layer(from_fn_with_state(state.clone(), lang_redirect))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
