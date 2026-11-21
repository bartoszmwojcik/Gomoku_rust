//! `POST /api/auth/register`, `/login`, `/logout`.

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum_extra::extract::CookieJar;
use gomoku_core::auth::{
    FieldIssue, validate_display_name, validate_email, validate_login_password,
    validate_new_password,
};
use gomoku_core::validators::normalize_display_name;
use gomoku_db::{NewSession, NewUser, RegisterError};
use serde_json::{Value, json};

use crate::api::{ApiError, ok, read_json, string_field};
use crate::app::AppState;
use crate::client_address::client_key;
use crate::security::{hash_opaque_token, new_opaque_token};
use crate::session::{
    SESSION_TTL, current_user, expired_session_cookie, session_cookie, session_token,
};

/// 5 attempts (successful or not) per hour and client address.
pub const REGISTER_LIMIT: i32 = 5;
pub const REGISTER_WINDOW_MS: i64 = 60 * 60 * 1000;
/// 30 attempts per hour and client address (any outcome).
pub const LOGIN_IP_LIMIT: i32 = 30;
pub const LOGIN_IP_WINDOW_MS: i64 = 60 * 60 * 1000;
/// 10 failed attempts per 15 minutes and e-mail + address pair.
pub const LOGIN_EMAIL_IP_LIMIT: i32 = 10;
pub const LOGIN_EMAIL_IP_WINDOW_MS: i64 = 15 * 60 * 1000;
/// 100 failed attempts per hour and e-mail, against distributed attacks.
pub const LOGIN_EMAIL_LIMIT: i32 = 100;
pub const LOGIN_EMAIL_WINDOW_MS: i64 = 60 * 60 * 1000;

/// Uniform answer for a taken e-mail: it must not reveal whether the account exists.
pub const EMAIL_TAKEN_MESSAGE: &str =
    "Nie udało się założyć konta. Jeśli masz już konto, zaloguj się.";
pub const DISPLAY_NAME_TAKEN_MESSAGE: &str = "Ta nazwa gracza jest już zajęta.";
/// One answer for an unknown e-mail and a wrong password (never reveals which).
pub const INVALID_CREDENTIALS_MESSAGE: &str = "Nieprawidłowy e-mail lub hasło.";

fn db_error(error: impl std::fmt::Debug) -> ApiError {
    ApiError::internal_logged("database", &error)
}

/// Counts one attempt and refuses once `limit` attempts per window are exceeded. The counter is committed
/// immediately, so it survives a later failure of the caller's work.
pub async fn rate_limit(
    state: &AppState,
    key: &str,
    limit: i32,
    window_ms: i64,
) -> Result<(), ApiError> {
    let hit = state
        .db
        .rate_limit_hit(key, window_ms)
        .await
        .map_err(db_error)?;
    if hit.count > limit {
        // Only the first refusal of a window is logged, with the pool's name and never its key.
        if hit.count == limit + 1 {
            tracing::warn!(
                event = "rate_limited",
                scope = key.split(':').next().unwrap_or(""),
                "rate limited"
            );
        }
        return Err(ApiError::too_many_requests(
            u64::try_from(hit.retry_after_seconds).unwrap_or(1),
        ));
    }
    Ok(())
}

fn address_key(state: &AppState, headers: &HeaderMap, scope: &str) -> Result<String, ApiError> {
    client_key(headers, scope, &state.config)
        .map_err(|_| ApiError::internal_logged("client address", &"unavailable behind the proxy"))
}

fn already_authenticated() -> ApiError {
    ApiError::conflict("ALREADY_AUTHENTICATED", "Jesteś już zalogowany.")
}

fn issues_of<T>(result: Result<T, Vec<FieldIssue>>) -> (Option<T>, Vec<FieldIssue>) {
    match result {
        Ok(value) => (Some(value), Vec::new()),
        Err(issues) => (None, issues),
    }
}

/// Validates the registration body; every field's problems are reported together.
fn parse_registration(body: &Value) -> Result<(String, String, String), ApiError> {
    let (email, email_issues) = match string_field(body, "email") {
        Some(raw) => issues_of(validate_email(raw)),
        None => (None, vec![FieldIssue::EmailRequired]),
    };
    let (name, name_issues) = match string_field(body, "displayName") {
        Some(raw) => issues_of(validate_display_name(raw)),
        None => (None, vec![FieldIssue::DisplayNameRequired]),
    };
    let (password, password_issues) = match string_field(body, "password") {
        Some(raw) => (
            Some(raw),
            validate_new_password(raw).err().unwrap_or_default(),
        ),
        None => (None, vec![FieldIssue::PasswordRequired]),
    };
    match (email, name, password) {
        (Some(email), Some(name), Some(password)) if password_issues.is_empty() => {
            Ok((email, name, password.to_owned()))
        }
        _ => Err(ApiError::invalid_fields(
            [
                ("email", email_issues),
                ("displayName", name_issues),
                ("password", password_issues),
            ]
            .into_iter()
            .filter(|(_, issues)| !issues.is_empty())
            .collect(),
        )),
    }
}

pub async fn register(
    State(state): State<AppState>,
    headers: HeaderMap,
    jar: CookieJar,
    body: Body,
) -> Result<(CookieJar, Response), ApiError> {
    let body = read_json(body, &headers).await?;
    // A signed-in client (e.g. logged in from another tab while filling in the form) gets a dedicated code and
    // does not spend the attempt budget.
    if current_user(&state, &jar).await.is_some() {
        return Err(already_authenticated());
    }
    // Counted before validation and hashing, so a `400` or `409` outcome never gives the attempt back.
    let key = address_key(&state, &headers, "register")?;
    rate_limit(&state, &key, REGISTER_LIMIT, REGISTER_WINDOW_MS).await?;
    let (email, display_name, password) = parse_registration(&body)?;

    // Hashed before the insert on every path, so the response time does not depend on whether the e-mail exists.
    let password_hash = state.hasher.hash(&password).await.map_err(db_error)?;
    let token = new_opaque_token();
    let token_hash = hash_opaque_token(&token);
    let key_name = normalize_display_name(&display_name);
    let created = state
        .db
        .register_user(
            &NewUser {
                email: &email,
                display_name: &display_name,
                display_name_key: &key_name,
                password_hash: &password_hash,
            },
            &NewSession {
                token_hash: &token_hash,
                expires_at: (state.clock)() + SESSION_TTL,
            },
        )
        .await;
    let id = match created {
        Ok(id) => id,
        Err(RegisterError::EmailTaken) => {
            return Err(ApiError::conflict("CONFLICT", EMAIL_TAKEN_MESSAGE)
                .with_data(json!({ "reason": "email_taken" })));
        }
        Err(RegisterError::DisplayNameTaken) => {
            return Err(ApiError::conflict("CONFLICT", DISPLAY_NAME_TAKEN_MESSAGE)
                .with_field(
                    "displayName",
                    "DISPLAY_NAME_TAKEN",
                    DISPLAY_NAME_TAKEN_MESSAGE,
                )
                .with_data(json!({ "reason": "display_name_taken" })));
        }
        Err(RegisterError::Db(error)) => return Err(db_error(error)),
    };
    let jar = jar.add(session_cookie(state.config.session_cookie_secure, token));
    let user = json!({ "id": id, "displayName": display_name });
    Ok((jar, ok(StatusCode::CREATED, json!({ "user": user }))))
}

pub async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    jar: CookieJar,
    body: Body,
) -> Result<(CookieJar, Response), ApiError> {
    // The address limit comes first, before the session check and before parsing, so every request (also a
    // signed-in one or a malformed one) spends the address budget and cannot be used to probe for free. The 409
    // and 400 answers spend no e-mail counters.
    let ip_key = address_key(&state, &headers, "login")?;
    rate_limit(&state, &ip_key, LOGIN_IP_LIMIT, LOGIN_IP_WINDOW_MS).await?;
    if current_user(&state, &jar).await.is_some() {
        return Err(already_authenticated());
    }
    let body = read_json(body, &headers).await?;
    let (email, email_issues) = match string_field(&body, "email") {
        Some(raw) => issues_of(validate_email(raw)),
        None => (None, vec![FieldIssue::EmailRequired]),
    };
    let (password, password_issues) = match string_field(&body, "password") {
        Some(raw) => match validate_login_password(raw) {
            Ok(()) => (Some(raw), Vec::new()),
            Err(issues) => (None, issues),
        },
        None => (None, vec![FieldIssue::PasswordRequired]),
    };
    let (Some(email), Some(password)) = (email, password) else {
        return Err(ApiError::invalid_fields(
            [("email", email_issues), ("password", password_issues)]
                .into_iter()
                .filter(|(_, issues)| !issues.is_empty())
                .collect(),
        ));
    };

    // Keys are hashes: no e-mail or address in the table. The e-mail counters grow before the (slow) password
    // check, so parallel guesses cannot slip past the limit; a successful login gives the attempt back.
    let email_ip_key = format!(
        "login:email-ip:{}",
        hash_opaque_token(&format!("{email}\n{ip_key}"))
    );
    let email_key = format!("login:email:{}", hash_opaque_token(&email));
    rate_limit(
        &state,
        &email_ip_key,
        LOGIN_EMAIL_IP_LIMIT,
        LOGIN_EMAIL_IP_WINDOW_MS,
    )
    .await?;
    rate_limit(&state, &email_key, LOGIN_EMAIL_LIMIT, LOGIN_EMAIL_WINDOW_MS).await?;

    // `verify` always runs, for an unknown e-mail against the dummy hash, so the response time does not reveal
    // whether the account exists. An unconfirmed e-mail does not matter.
    let user = state
        .db
        .find_user_by_email(&email)
        .await
        .map_err(db_error)?;
    let hash = user
        .as_ref()
        .map_or_else(|| state.hasher.dummy_hash(), |u| u.password_hash.as_str());
    let matches = state
        .hasher
        .verify(password, hash)
        .await
        .map_err(db_error)?;
    let user = match user {
        Some(user) if matches => user,
        _ => {
            tracing::warn!(event = "login_failed", "login failed");
            return Err(ApiError::new(
                StatusCode::UNAUTHORIZED,
                "INVALID_CREDENTIALS",
                INVALID_CREDENTIALS_MESSAGE,
            ));
        }
    };
    state
        .db
        .rate_limit_reset(&email_ip_key)
        .await
        .map_err(db_error)?;
    state
        .db
        .rate_limit_reset(&email_key)
        .await
        .map_err(db_error)?;

    let token = new_opaque_token();
    state
        .db
        .create_session(
            user.id,
            &NewSession {
                token_hash: &hash_opaque_token(&token),
                expires_at: (state.clock)() + SESSION_TTL,
            },
        )
        .await
        .map_err(db_error)?;
    let jar = jar.add(session_cookie(state.config.session_cookie_secure, token));
    let user = json!({ "id": user.id, "displayName": user.display_name });
    Ok((jar, ok(StatusCode::OK, json!({ "user": user }))))
}

/// Logs out the current session only. Idempotent: without a cookie, or with an unknown or expired session, the
/// answer is still `200`. The cookie is removed even when the database fails (the revocation is best effort, the
/// session row expires on its own).
pub async fn logout(State(state): State<AppState>, jar: CookieJar) -> (CookieJar, Response) {
    let secure = state.config.session_cookie_secure;
    if let Some(token) = session_token(&jar, secure)
        && let Err(error) = state.db.revoke_session(&hash_opaque_token(&token)).await
    {
        tracing::error!(error = ?error, "session revocation failed");
    }
    (
        jar.add(expired_session_cookie(secure)),
        ok(StatusCode::OK, Value::Null),
    )
}
