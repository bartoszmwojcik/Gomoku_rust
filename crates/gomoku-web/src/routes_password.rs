//! Password change and reset: `POST /api/auth/password`, `/password-reset/request`, `/password-reset/confirm`.

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum_extra::extract::CookieJar;
use gomoku_core::auth::{
    FieldIssue, validate_email, validate_link_token, validate_new_password,
    validate_password_change,
};
use gomoku_db::OffsetDateTime;
use serde_json::Value;
use time::Duration;

use crate::api::{ApiError, ok, read_json, string_field};
use crate::app::AppState;
use crate::email::{RESET_LINK_VALID_HOURS, ResetMessageInput, SEND_TIMEOUT, build_reset_message};
use crate::routes_auth::rate_limit;
use crate::security::{hash_opaque_token, new_opaque_token};
use crate::session::{CurrentUser, current_user};

/// 10 failed attempts per 15 minutes and account, so a stolen session cannot guess the password.
pub const PASSWORD_CHANGE_LIMIT: i32 = 10;
pub const PASSWORD_CHANGE_WINDOW_MS: i64 = 15 * 60 * 1000;
/// 10 reset requests per hour and client address (the per-e-mail limit lives in the service).
pub const RESET_REQUEST_IP_LIMIT: i32 = 10;
pub const RESET_REQUEST_IP_WINDOW_MS: i64 = 60 * 60 * 1000;
/// 10 confirmations per 15 minutes and client address (any outcome).
pub const RESET_CONFIRM_IP_LIMIT: i32 = 10;
pub const RESET_CONFIRM_IP_WINDOW_MS: i64 = 15 * 60 * 1000;
/// At most 3 reset messages per hour to one e-mail address (counted as `sha256(e-mail)`).
pub const RESET_EMAIL_LIMIT: i32 = 3;
pub const RESET_EMAIL_WINDOW_MS: i64 = 60 * 60 * 1000;
/// Reset has its own pool of `EMAIL_DAILY_LIMIT`, separate from e-mail verification.
pub const RESET_DAILY_KEY: &str = "email-daily:reset";
pub const DAILY_WINDOW_MS: i64 = 24 * 60 * 60 * 1000;
const DEV_ORIGIN: &str = "http://localhost:3000";

pub const INVALID_TOKEN_MESSAGE: &str = "Link wygasł lub został już użyty.";
pub const INVALID_CURRENT_PASSWORD_MESSAGE: &str = "Obecne hasło jest nieprawidłowe.";

fn db_error(error: impl std::fmt::Debug) -> ApiError {
    ApiError::internal_logged("database", &error)
}

/// Guard for route handlers: the current user, or a `401`.
pub async fn require_session(state: &AppState, jar: &CookieJar) -> Result<CurrentUser, ApiError> {
    current_user(state, jar)
        .await
        .ok_or_else(ApiError::unauthorized)
}

fn invalid_current_password() -> ApiError {
    ApiError::validation(INVALID_CURRENT_PASSWORD_MESSAGE)
        .with_code("INVALID_CURRENT_PASSWORD")
        .with_field(
            "currentPassword",
            "INVALID_CURRENT_PASSWORD",
            INVALID_CURRENT_PASSWORD_MESSAGE,
        )
}

/// Changes the password of the signed-in user and ends their other sessions; the current one stays. Order:
/// session (401), input (400, spends no attempt), then the per-account counter grows before the slow password
/// check so parallel guesses cannot slip past it, and a successful change gives the attempt back.
pub async fn change(
    State(state): State<AppState>,
    headers: HeaderMap,
    jar: CookieJar,
    body: Body,
) -> Result<Response, ApiError> {
    let user = require_session(&state, &jar).await?;
    let body = read_json(body, &headers).await?;
    let current = string_field(&body, "currentPassword");
    let new = string_field(&body, "newPassword");
    let issues = validate_password_change(current.unwrap_or(""), new.unwrap_or("")).err();
    if current.is_none() || new.is_none() || issues.is_some() {
        let mut current_issues = issues
            .as_ref()
            .map(|i| i.current.clone())
            .unwrap_or_default();
        let mut new_issues = issues.map(|i| i.new).unwrap_or_default();
        if current.is_none() {
            current_issues = vec![FieldIssue::CurrentPasswordRequired];
        }
        if new.is_none() {
            new_issues = vec![FieldIssue::PasswordRequired];
        }
        return Err(ApiError::invalid_fields(
            [
                ("currentPassword", current_issues),
                ("newPassword", new_issues),
            ]
            .into_iter()
            .filter(|(_, issues)| !issues.is_empty())
            .collect(),
        ));
    }
    let (Some(current), Some(new)) = (current, new) else {
        return Err(ApiError::internal());
    };

    let key = format!("password-change:user:{}", user.id);
    rate_limit(
        &state,
        &key,
        PASSWORD_CHANGE_LIMIT,
        PASSWORD_CHANGE_WINDOW_MS,
    )
    .await?;

    let stored = state
        .db
        .find_user_by_id(user.id)
        .await
        .map_err(db_error)?
        .ok_or_else(ApiError::unauthorized)?;
    if !state
        .hasher
        .verify(current, &stored.password_hash)
        .await
        .map_err(db_error)?
    {
        return Err(invalid_current_password());
    }
    let new_hash = state.hasher.hash(new).await.map_err(db_error)?;
    let changed = state
        .db
        .change_password(
            user.id,
            &stored.password_hash,
            &new_hash,
            user.session_id,
            (state.clock)(),
        )
        .await
        .map_err(db_error)?;
    if !changed {
        // A parallel change got there first: the hash this request verified is gone.
        return Err(invalid_current_password());
    }
    state.db.rate_limit_reset(&key).await.map_err(db_error)?;
    Ok(ok(StatusCode::OK, Value::Null))
}

/// Asks for a password reset link. The answer is always the same `200` once the address limit and the body shape
/// pass, whatever the account is (unknown, unconfirmed, confirmed, limited or failing to send): all work that
/// depends on the account runs after the response, so neither the body nor the timing tells. The address limit
/// comes first, so malformed requests spend it too.
pub async fn reset_request(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    let key = crate::client_address::client_key(&headers, "password-reset-request", &state.config)
        .map_err(|_| {
            ApiError::internal_logged("client address", &"unavailable behind the proxy")
        })?;
    rate_limit(
        &state,
        &key,
        RESET_REQUEST_IP_LIMIT,
        RESET_REQUEST_IP_WINDOW_MS,
    )
    .await?;
    let body = read_json(body, &headers).await?;
    let email = match string_field(&body, "email") {
        Some(raw) => validate_email(raw),
        None => Err(vec![FieldIssue::EmailRequired]),
    }
    .map_err(|issues| ApiError::invalid_fields(vec![("email", issues)]))?;

    let background = state.clone();
    state.tasks.spawn(async move {
        if let Err(error) = send_reset_link(&background, &email).await {
            // Class only: never the address or the token.
            tracing::error!(error = ?error, "password reset request failed");
        }
    });
    Ok(ok(StatusCode::OK, Value::Null))
}

/// Per-address hourly limit first (a throttled address must not use up the daily pool), then the daily pool.
async fn allowed(state: &AppState, address: &str) -> Result<bool, gomoku_db::DbError> {
    let hourly = state
        .db
        .rate_limit_hit(
            &format!("password-reset:email:{}", hash_opaque_token(address)),
            RESET_EMAIL_WINDOW_MS,
        )
        .await?;
    if hourly.count > RESET_EMAIL_LIMIT {
        return Ok(false);
    }
    if let Some(daily_limit) = state.config.email.daily_limit {
        let daily = state
            .db
            .rate_limit_hit(RESET_DAILY_KEY, DAILY_WINDOW_MS)
            .await?;
        if i64::from(daily.count) > i64::from(daily_limit) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Sends a reset link to the owner of a confirmed address. It never tells the caller what happened: an unknown or
/// unconfirmed address, an exhausted limit (the earlier link stays valid) or a failed send all resolve quietly.
/// The user's older unused links are invalidated after the message was sent, only those older by `(created_at,
/// id)`, so of parallel calls the newest link always stays valid. A failed send removes the new token and keeps
/// the previous link valid.
pub async fn send_reset_link(state: &AppState, address: &str) -> Result<(), gomoku_db::DbError> {
    let Some(user) = state.db.find_user_by_email(address).await? else {
        return Ok(());
    };
    if user.email_verified_at.is_none() || !allowed(state, address).await? {
        return Ok(());
    }
    let now: OffsetDateTime = (state.clock)();
    let token = new_opaque_token();
    let record = state
        .db
        .insert_reset_token(
            user.id,
            &hash_opaque_token(&token),
            now,
            now + Duration::hours(RESET_LINK_VALID_HOURS),
        )
        .await?;
    let origin = state.config.app_origin.as_deref().unwrap_or(DEV_ORIGIN);
    let message = build_reset_message(&ResetMessageInput {
        to: &user.email,
        name: &user.display_name,
        token: &token,
        origin,
    });
    if let Err(error) = state.email.send(&message, SEND_TIMEOUT).await {
        tracing::error!(code = %error.code, "password reset e-mail send failed");
        state.db.delete_reset_token(record.id).await?;
        return Ok(());
    }
    state
        .db
        .invalidate_older_reset_tokens(user.id, now, record.id, now)
        .await
}

/// Sets a new password with the token from the e-mail. No session is needed (the link is the proof) and nobody is
/// logged in afterwards; all sessions of the account are deleted. The address limit comes first, so every
/// request spends it. The password is hashed before the transaction, also for an invalid token, so timing says
/// nothing.
pub async fn reset_confirm(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    let key = crate::client_address::client_key(&headers, "password-reset-confirm", &state.config)
        .map_err(|_| {
            ApiError::internal_logged("client address", &"unavailable behind the proxy")
        })?;
    rate_limit(
        &state,
        &key,
        RESET_CONFIRM_IP_LIMIT,
        RESET_CONFIRM_IP_WINDOW_MS,
    )
    .await?;
    let body = read_json(body, &headers).await?;
    let token = string_field(&body, "token");
    let new = string_field(&body, "newPassword");
    let token_issues = match token {
        Some(raw) => validate_link_token(raw).err().unwrap_or_default(),
        None => vec![FieldIssue::TokenRequired],
    };
    let password_issues = match new {
        Some(raw) => validate_new_password(raw).err().unwrap_or_default(),
        None => vec![FieldIssue::PasswordRequired],
    };
    let (Some(token), Some(new)) = (token, new) else {
        return Err(invalid_confirmation(token_issues, password_issues));
    };
    if !token_issues.is_empty() || !password_issues.is_empty() {
        return Err(invalid_confirmation(token_issues, password_issues));
    }
    let new_hash = state.hasher.hash(new).await.map_err(db_error)?;
    let confirmed = state
        .db
        .confirm_password_reset(&hash_opaque_token(token), &new_hash, (state.clock)())
        .await
        .map_err(db_error)?;
    match confirmed {
        Some(_user_id) => Ok(ok(StatusCode::OK, Value::Null)),
        None => Err(ApiError::validation(INVALID_TOKEN_MESSAGE).with_code("INVALID_TOKEN")),
    }
}

fn invalid_confirmation(token: Vec<FieldIssue>, new_password: Vec<FieldIssue>) -> ApiError {
    ApiError::invalid_fields(
        [("token", token), ("newPassword", new_password)]
            .into_iter()
            .filter(|(_, issues)| !issues.is_empty())
            .collect(),
    )
}
