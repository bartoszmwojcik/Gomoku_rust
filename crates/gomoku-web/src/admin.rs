//! The installation administrator: one account, decided on every request from the configuration (never stored), and
//! the two gates that hide the panel and the show from everybody else behind the answer of a page that does not exist.

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum_extra::extract::CookieJar;
use gomoku_core::validators::is_installation_admin;

use crate::api::ApiError;
use crate::app::AppState;
use crate::session::{CurrentUser, current_user};

/// The signed-in administrator: a confirmed address equal to `ADMIN_EMAIL`. Anybody else (no session, another player, an
/// unconfirmed address, `ADMIN_EMAIL` unset, a database that does not answer) is `None`, logged without any detail.
pub async fn admin_user(state: &AppState, jar: &CookieJar) -> Option<CurrentUser> {
    let user = current_user(state, jar).await;
    let admin = user.filter(|u| {
        u.email_verified_at.is_some()
            && is_installation_admin(&u.email, state.config.admin_email.as_deref())
    });
    if admin.is_none() {
        tracing::warn!(event = "security.admin_denied");
    }
    admin
}

/// What a page answers to anybody but the administrator: the plain 404 of a path that does not exist (no redirect, no hint).
pub fn denied_page() -> Response {
    StatusCode::NOT_FOUND.into_response()
}

/// The API gate: `404 NOT_FOUND` as for a game that does not exist. It runs BEFORE the CSRF check, so a stranger learns
/// nothing from the headers he sends.
pub async fn admin_api_gate(
    State(state): State<AppState>,
    jar: CookieJar,
    request: Request,
    next: Next,
) -> Response {
    if admin_user(&state, &jar).await.is_none() {
        return ApiError::from(gomoku_db::games::GameError::NotFound).into_response();
    }
    next.run(request).await
}
