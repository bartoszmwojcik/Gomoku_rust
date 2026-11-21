//! Account deletion: `POST /api/account/delete`.

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum_extra::extract::CookieJar;
use gomoku_core::auth::{FieldIssue, validate_login_password};
use gomoku_core::validators::is_installation_admin;
use gomoku_db::DeleteOutcome;
use serde_json::{Value, json};

use crate::api::{ApiError, ok, read_json, string_field};
use crate::app::AppState;
use crate::routes_auth::rate_limit;
use crate::session::expired_session_cookie;

/// 10 wrong passwords per 15 minutes and account, so a stolen session cannot guess the password to delete the
/// account. Only wrong passwords stay counted.
pub const ACCOUNT_DELETE_LIMIT: i32 = 10;
pub const ACCOUNT_DELETE_WINDOW_MS: i64 = 15 * 60 * 1000;

pub const INVALID_PASSWORD_MESSAGE: &str = "Nieprawidłowe hasło.";
pub const CONFIRM_MESSAGE: &str = "Zaznacz potwierdzenie, aby usunąć konto.";
pub const ADMIN_ACCOUNT_PROTECTED_MESSAGE: &str = "Tego konta nie można usunąć.";

fn invalid_password() -> ApiError {
    ApiError::validation(INVALID_PASSWORD_MESSAGE)
        .with_code("INVALID_PASSWORD")
        .with_field("password", "INVALID_PASSWORD", INVALID_PASSWORD_MESSAGE)
}

fn admin_protected() -> ApiError {
    ApiError::new(
        StatusCode::FORBIDDEN,
        "ADMIN_ACCOUNT_PROTECTED",
        ADMIN_ACCOUNT_PROTECTED_MESSAGE,
    )
}

/// Deletes the signed-in account. Order: CSRF (403), session (401), input (400, spends no attempt), the per-account
/// counter grows before the slow password check (429), then the answers. A wrong password stays counted; every other
/// outcome gives the attempt back and a success forgets the counter. The session cookie is removed after a success and
/// when the account turned out to be gone already (a concurrent deletion); on any other failure it stays, so the user
/// can retry. A POST, not a DELETE: proxies drop the body of a DELETE.
pub async fn delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    jar: CookieJar,
    body: Body,
) -> Response {
    let secure = state.config.session_cookie_secure;
    match try_delete(&state, &headers, &jar, body).await {
        Ok(()) => (
            jar.add(expired_session_cookie(secure)),
            ok(StatusCode::OK, Value::Null),
        )
            .into_response(),
        Err((error, clear_cookie)) if clear_cookie => {
            (jar.add(expired_session_cookie(secure)), error).into_response()
        }
        Err((error, _)) => error.into_response(),
    }
}

/// `Err((error, clear_cookie))`.
async fn try_delete(
    state: &AppState,
    headers: &HeaderMap,
    jar: &CookieJar,
    body: Body,
) -> Result<(), (ApiError, bool)> {
    let user = crate::session::current_user(state, jar)
        .await
        .ok_or_else(|| (ApiError::unauthorized(), false))?;
    let body = read_json(body, headers).await.map_err(|e| (e, false))?;
    let password = string_field(&body, "password");
    let password_issues = match password {
        Some(raw) => validate_login_password(raw).err().unwrap_or_default(),
        None => vec![FieldIssue::PasswordRequired],
    };
    let confirmed = body.get("confirm") == Some(&json!(true));
    let (Some(password), true, true) = (password, password_issues.is_empty(), confirmed) else {
        let mut error = ApiError::invalid_fields(
            [("password", password_issues)]
                .into_iter()
                .filter(|(_, issues)| !issues.is_empty())
                .collect(),
        );
        if !confirmed {
            error = error.with_field("confirm", "CONFIRM_REQUIRED", CONFIRM_MESSAGE);
        }
        return Err((error, false));
    };
    let key = format!("account-delete:user:{}", user.id);
    rate_limit(state, &key, ACCOUNT_DELETE_LIMIT, ACCOUNT_DELETE_WINDOW_MS)
        .await
        .map_err(|e| (e, false))?;
    let release = || async {
        let _ = state
            .db
            .rate_limit_release(&key, ACCOUNT_DELETE_WINDOW_MS)
            .await;
    };
    let db_failure = |error: gomoku_db::DbError| ApiError::internal_logged("database", &error);

    let stored = match state.db.find_user_by_id(user.id).await {
        Ok(stored) => stored,
        Err(error) => {
            release().await;
            return Err((db_failure(error), false));
        }
    };
    let Some(stored) = stored else {
        release().await;
        return Err((ApiError::unauthorized(), true));
    };
    // Before the password check and any write: the administrator's account is never deleted.
    let admin = state.config.admin_email.clone();
    if is_installation_admin(&stored.email, admin.as_deref()) {
        release().await;
        return Err((admin_protected(), false));
    }
    match state.hasher.verify(password, &stored.password_hash).await {
        Ok(true) => {}
        Ok(false) => return Err((invalid_password(), false)),
        Err(error) => {
            release().await;
            return Err((ApiError::internal_logged("password", &error), false));
        }
    }
    let outcome = state
        .db
        .delete_account(user.id, &stored.password_hash, |email| {
            is_installation_admin(email, admin.as_deref())
        })
        .await;
    match outcome {
        Ok(DeleteOutcome::Deleted) => {
            let _ = state.db.rate_limit_reset(&key).await;
            Ok(())
        }
        Ok(DeleteOutcome::HashChanged) => Err((invalid_password(), false)),
        Ok(DeleteOutcome::Protected) => {
            release().await;
            Err((admin_protected(), false))
        }
        Ok(DeleteOutcome::Gone) => {
            release().await;
            Err((ApiError::unauthorized(), true))
        }
        Err(error) => {
            release().await;
            Err((db_failure(error), false))
        }
    }
}
