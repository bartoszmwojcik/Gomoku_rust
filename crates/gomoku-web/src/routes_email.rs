//! E-mail confirmation, e-mail change and display name: `POST /api/auth/email-verification/{confirm,resend}`,
//! `POST /api/auth/email-change[/confirm]`, `PATCH /api/auth/display-name`.

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum_extra::extract::CookieJar;
use gomoku_core::auth::{
    FieldIssue, safe_next_path, validate_current_password, validate_display_name, validate_email,
    validate_link_token,
};
use gomoku_core::validators::{is_installation_admin, normalize_display_name};
use gomoku_db::{OffsetDateTime, RenameOutcome, Uuid};
use serde_json::{Value, json};
use time::Duration;

use crate::api::{ApiError, ok, read_json, string_field};
use crate::app::AppState;
use crate::client_address::client_key;
use crate::email::{
    EMAIL_CHANGE_LINK_VALID_HOURS, SEND_TIMEOUT, VERIFICATION_LINK_VALID_HOURS,
    build_email_change_message, build_email_changed_notice, build_verification_message, mask_email,
};
use crate::routes_auth::rate_limit;
use crate::routes_password::{
    DAILY_WINDOW_MS, INVALID_CURRENT_PASSWORD_MESSAGE, INVALID_TOKEN_MESSAGE,
    PASSWORD_CHANGE_LIMIT, PASSWORD_CHANGE_WINDOW_MS, require_session,
};
use crate::security::{hash_opaque_token, new_opaque_token};

/// 3 resends per account and 10 per client address, each per hour.
pub const RESEND_USER_LIMIT: i32 = 3;
pub const RESEND_IP_LIMIT: i32 = 10;
pub const RESEND_WINDOW_MS: i64 = 60 * 60 * 1000;
/// E-mail verification may use at most this share of `EMAIL_DAILY_LIMIT`.
pub const VERIFICATION_DAILY_SHARE_PERCENT: u32 = 70;
pub const VERIFICATION_DAILY_KEY: &str = "email-daily:verification";
/// 3 link messages per hour per account (a 429 for the caller) and per target address (silent).
pub const EMAIL_CHANGE_ACCOUNT_LIMIT: i32 = 3;
pub const EMAIL_CHANGE_ADDRESS_LIMIT: i32 = 3;
pub const EMAIL_CHANGE_WINDOW_MS: i64 = 60 * 60 * 1000;
/// Its own pool of `EMAIL_DAILY_LIMIT`, like the reset's.
pub const EMAIL_CHANGE_DAILY_KEY: &str = "email-daily:email-change";
/// 20 rename attempts per hour and account; a request refused by validation spends none.
pub const DISPLAY_NAME_CHANGE_LIMIT: i32 = 20;
pub const DISPLAY_NAME_CHANGE_WINDOW_MS: i64 = 60 * 60 * 1000;
/// A player may rename themselves at most once per this window.
pub const DISPLAY_NAME_CHANGE_WINDOW: Duration = Duration::hours(24);
const DEV_ORIGIN: &str = "http://localhost:3000";

pub const ALREADY_VERIFIED_MESSAGE: &str = "Adres e-mail jest już potwierdzony.";
pub const EMAIL_SEND_FAILED_MESSAGE: &str = "Nie udało się wysłać wiadomości. Spróbuj później.";
pub const EMAIL_QUOTA_EXCEEDED_MESSAGE: &str = "Wysyłka wiadomości jest chwilowo niedostępna. Spróbuj jutro albo zagraj jako gość z zaproszenia.";
pub const SAME_EMAIL_MESSAGE: &str = "To już jest adres Twojego konta.";
pub const ADMIN_EMAIL_LOCKED_MESSAGE: &str = "Adresu e-mail tego konta nie można zmienić.";
pub const SAME_DISPLAY_NAME_MESSAGE: &str = "To już jest Twoja nazwa gracza.";
pub const DISPLAY_NAME_TAKEN_MESSAGE: &str = "Ta nazwa gracza jest już zajęta.";
pub const DISPLAY_NAME_COOLDOWN_MESSAGE: &str =
    "Nazwę gracza można zmienić raz na dobę. Spróbuj później.";

fn db_error(error: impl std::fmt::Debug) -> ApiError {
    ApiError::internal_logged("database", &error)
}

/// Verification's slice of the daily limit: 70%, rounded down, but never below 1 (with `EMAIL_DAILY_LIMIT=1` a
/// plain `floor(1 * 0.7) = 0` would block every send).
pub fn verification_daily_budget(daily_limit: u32) -> u32 {
    let share = u64::from(daily_limit) * u64::from(VERIFICATION_DAILY_SHARE_PERCENT) / 100;
    u32::try_from(share).unwrap_or(u32::MAX).max(1)
}

/// Why a verification link could not be issued.
#[derive(Debug)]
pub enum IssueError {
    QuotaExceeded,
    SendFailed,
    Db(gomoku_db::DbError),
}

impl From<IssueError> for ApiError {
    fn from(error: IssueError) -> Self {
        match error {
            IssueError::QuotaExceeded => ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "EMAIL_QUOTA_EXCEEDED",
                EMAIL_QUOTA_EXCEEDED_MESSAGE,
            ),
            IssueError::SendFailed => ApiError::new(
                StatusCode::BAD_GATEWAY,
                "EMAIL_SEND_FAILED",
                EMAIL_SEND_FAILED_MESSAGE,
            ),
            IssueError::Db(error) => db_error(error),
        }
    }
}

/// Stores a fresh token (only its SHA-256) and sends the confirmation link. The user's older unused links are
/// invalidated only after the message was sent, so a failed send leaves the previous link valid. The daily send
/// limit (at most 70% of it for this channel) is checked first; without `EMAIL_DAILY_LIMIT` nothing is limited.
/// Parallel calls for one user are safe: a call invalidates only links older than its own by `(created_at, id)`.
pub async fn issue_verification(
    state: &AppState,
    user_id: Uuid,
    email: &str,
    display_name: &str,
    next: Option<&str>,
) -> Result<(), IssueError> {
    if let Some(limit) = state.config.email.daily_limit {
        let hit = state
            .db
            .rate_limit_hit(VERIFICATION_DAILY_KEY, DAILY_WINDOW_MS)
            .await
            .map_err(IssueError::Db)?;
        if i64::from(hit.count) > i64::from(verification_daily_budget(limit)) {
            return Err(IssueError::QuotaExceeded);
        }
    }
    let now = (state.clock)();
    let token = new_opaque_token();
    let id = state
        .db
        .insert_verification_token(
            user_id,
            &hash_opaque_token(&token),
            now,
            now + Duration::hours(VERIFICATION_LINK_VALID_HOURS),
        )
        .await
        .map_err(IssueError::Db)?;
    let origin = state.config.app_origin.as_deref().unwrap_or(DEV_ORIGIN);
    let message = build_verification_message(email, display_name, &token, origin, next);
    if let Err(error) = state.email.send(&message, SEND_TIMEOUT).await {
        // Only the short code: the message may carry the address or the token.
        tracing::error!(code = %error.code, "verification e-mail send failed");
        state
            .db
            .delete_verification_token(id)
            .await
            .map_err(IssueError::Db)?;
        return Err(IssueError::SendFailed);
    }
    state
        .db
        .invalidate_older_verification_tokens(user_id, now, id, now)
        .await
        .map_err(IssueError::Db)
}

/// Sends a new confirmation link to a signed-in player. Order: session (401), already confirmed (409), both
/// counters (3 per account and 10 per client address per hour, counted before the costly work and kept even when
/// the send fails), then the issue.
pub async fn resend(
    State(state): State<AppState>,
    headers: HeaderMap,
    jar: CookieJar,
) -> Result<Response, ApiError> {
    let user = require_session(&state, &jar).await?;
    if user.email_verified_at.is_some() {
        return Err(ApiError::conflict(
            "ALREADY_VERIFIED",
            ALREADY_VERIFIED_MESSAGE,
        ));
    }
    rate_limit(
        &state,
        &format!(
            "verification-resend:user:{}",
            hash_opaque_token(&user.id.to_string())
        ),
        RESEND_USER_LIMIT,
        RESEND_WINDOW_MS,
    )
    .await?;
    let ip_key = client_key(&headers, "verification-resend", &state.config).map_err(|_| {
        ApiError::internal_logged("client address", &"unavailable behind the proxy")
    })?;
    rate_limit(&state, &ip_key, RESEND_IP_LIMIT, RESEND_WINDOW_MS).await?;
    issue_verification(&state, user.id, &user.email, &user.display_name, None).await?;
    Ok(ok(StatusCode::OK, Value::Null))
}

fn token_issues(token: Option<&str>) -> Vec<FieldIssue> {
    match token {
        Some(raw) => validate_link_token(raw).err().unwrap_or_default(),
        None => vec![FieldIssue::TokenRequired],
    }
}

fn invalid_token() -> ApiError {
    ApiError::validation(INVALID_TOKEN_MESSAGE).with_code("INVALID_TOKEN")
}

/// Confirms the e-mail of the account the token belongs to. No session is needed (the token proves access to the
/// mailbox) and nobody is logged in. The token is single-use, so there is no attempt counter; `next` is only
/// echoed back after `safe_next_path`.
pub async fn confirm_verification(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    let body = read_json(body, &headers).await?;
    let token = string_field(&body, "token");
    let issues = token_issues(token);
    let (Some(token), true) = (token, issues.is_empty()) else {
        return Err(ApiError::invalid_fields(vec![("token", issues)]));
    };
    let confirmed = state
        .db
        .confirm_email_verification(&hash_opaque_token(token), (state.clock)())
        .await
        .map_err(db_error)?;
    match confirmed {
        Some(_) => Ok(ok(
            StatusCode::OK,
            json!({ "next": safe_next_path(string_field(&body, "next")) }),
        )),
        None => Err(invalid_token()),
    }
}

/// Asks to change the account's e-mail address. Order: session (401), input (400, spends no attempt), the shared
/// password counter grows before the slow password check (the same pool as the password change: two forms must
/// not double the guesses), then the password (400), the administrator lock (403), the address differs (400) and
/// the account limit (429). The answer is then always `202`: whether the address is free, limited or failing to
/// send is decided after the response, so neither the body nor the timing tells.
pub async fn change_email_request(
    State(state): State<AppState>,
    headers: HeaderMap,
    jar: CookieJar,
    body: Body,
) -> Result<Response, ApiError> {
    let user = require_session(&state, &jar).await?;
    let body = read_json(body, &headers).await?;
    let new_email = string_field(&body, "newEmail");
    let current = string_field(&body, "currentPassword");
    let email_result = match new_email {
        Some(raw) => validate_email(raw),
        None => Err(vec![FieldIssue::EmailRequired]),
    };
    let current_issues = match current {
        Some(raw) => validate_current_password(raw).err().unwrap_or_default(),
        None => vec![FieldIssue::CurrentPasswordRequired],
    };
    let (Ok(new_email), Some(current), true) = (&email_result, current, current_issues.is_empty())
    else {
        return Err(ApiError::invalid_fields(
            [
                ("newEmail", email_result.err().unwrap_or_default()),
                ("currentPassword", current_issues),
            ]
            .into_iter()
            .filter(|(_, issues)| !issues.is_empty())
            .collect(),
        ));
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
        return Err(ApiError::validation(INVALID_CURRENT_PASSWORD_MESSAGE)
            .with_code("INVALID_CURRENT_PASSWORD")
            .with_field(
                "currentPassword",
                "INVALID_CURRENT_PASSWORD",
                INVALID_CURRENT_PASSWORD_MESSAGE,
            ));
    }
    // After the password check (so it is no oracle without it) and before any write.
    if is_installation_admin(&stored.email, state.config.admin_email.as_deref()) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "ADMIN_EMAIL_LOCKED",
            ADMIN_EMAIL_LOCKED_MESSAGE,
        ));
    }
    if &stored.email == new_email {
        return Err(ApiError::validation(SAME_EMAIL_MESSAGE).with_field(
            "newEmail",
            "EMAIL_UNCHANGED",
            SAME_EMAIL_MESSAGE,
        ));
    }
    rate_limit(
        &state,
        &format!(
            "email-change:user:{}",
            hash_opaque_token(&user.id.to_string())
        ),
        EMAIL_CHANGE_ACCOUNT_LIMIT,
        EMAIL_CHANGE_WINDOW_MS,
    )
    .await?;
    state.db.rate_limit_reset(&key).await.map_err(db_error)?;

    let (background, address, name) = (
        state.clone(),
        new_email.clone(),
        stored.display_name.clone(),
    );
    state.tasks.spawn(async move {
        if let Err(error) = deliver_email_change(&background, user.id, &name, &address).await {
            tracing::error!(error = ?error, "e-mail change request failed");
        }
    });
    Ok(ok(StatusCode::ACCEPTED, Value::Null))
}

/// Per-address hourly limit first (a throttled address must not use up the daily pool), then the daily pool.
async fn address_allowed(state: &AppState, address: &str) -> Result<bool, gomoku_db::DbError> {
    let hourly = state
        .db
        .rate_limit_hit(
            &format!("email-change:address:{}", hash_opaque_token(address)),
            EMAIL_CHANGE_WINDOW_MS,
        )
        .await?;
    if hourly.count > EMAIL_CHANGE_ADDRESS_LIMIT {
        return Ok(false);
    }
    if let Some(limit) = state.config.email.daily_limit {
        let daily = state
            .db
            .rate_limit_hit(EMAIL_CHANGE_DAILY_KEY, DAILY_WINDOW_MS)
            .await?;
        if i64::from(daily.count) > i64::from(limit) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Sends the link to the new address, after the response: a taken address, an exhausted limit (the earlier link
/// stays valid), the administrator's address or a failed send all resolve without a trace for the caller. The
/// user's older unused change links die only after the message was sent, so a failed send keeps the old link valid.
pub async fn deliver_email_change(
    state: &AppState,
    user_id: Uuid,
    display_name: &str,
    new_email: &str,
) -> Result<(), gomoku_db::DbError> {
    // Nobody may take the administrator's address; answered like a taken one, with no trace.
    if is_installation_admin(new_email, state.config.admin_email.as_deref())
        || !address_allowed(state, new_email).await?
        || state.db.email_is_taken(new_email).await?
    {
        return Ok(());
    }
    let now = (state.clock)();
    let token = new_opaque_token();
    let id = state
        .db
        .insert_email_change_token(
            user_id,
            new_email,
            &hash_opaque_token(&token),
            now,
            now + Duration::hours(EMAIL_CHANGE_LINK_VALID_HOURS),
        )
        .await?;
    let origin = state.config.app_origin.as_deref().unwrap_or(DEV_ORIGIN);
    let message = build_email_change_message(new_email, display_name, &token, origin);
    if let Err(error) = state.email.send(&message, SEND_TIMEOUT).await {
        tracing::error!(code = %error.code, "e-mail change message send failed");
        return state.db.delete_email_change_token(id).await;
    }
    state
        .db
        .invalidate_older_email_change_tokens(user_id, now, id, now)
        .await
}

/// Applies an e-mail change with the link token. No session is needed (the token proves access to the new
/// mailbox) and nobody is logged in; every session of the account ends. The token is single-use, so there is no
/// attempt counter. Afterwards the previous address gets a notice, but only when it had been confirmed; a failure
/// of that message is logged and never fails the change.
pub async fn confirm_email_change(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    let body = read_json(body, &headers).await?;
    let token = string_field(&body, "token");
    let issues = token_issues(token);
    let (Some(token), true) = (token, issues.is_empty()) else {
        return Err(ApiError::invalid_fields(vec![("token", issues)]));
    };
    let admin = state.config.admin_email.clone();
    let changed = state
        .db
        .apply_email_change(
            &hash_opaque_token(token),
            (state.clock)(),
            move |old, new| {
                is_installation_admin(old, admin.as_deref())
                    || is_installation_admin(new, admin.as_deref())
            },
        )
        .await
        .map_err(db_error)?
        .ok_or_else(invalid_token)?;
    if changed.was_verified {
        let notice = build_email_changed_notice(
            &changed.old_email,
            &changed.display_name,
            &mask_email(&changed.new_email),
        );
        if let Err(error) = state.email.send(&notice, SEND_TIMEOUT).await {
            tracing::error!(code = %error.code, "e-mail change notice send failed");
        }
    }
    Ok(ok(StatusCode::OK, Value::Null))
}

/// When the user may rename themselves again, or `None` when they may now (never renamed, or the window passed).
pub fn display_name_available_at(
    changed_at: Option<OffsetDateTime>,
    now: OffsetDateTime,
) -> Option<OffsetDateTime> {
    changed_at
        .map(|at| at + DISPLAY_NAME_CHANGE_WINDOW)
        .filter(|available| *available > now)
}

/// Renames the signed-in user, at most once per 24 h. Order: session (401), input (400, no attempt spent), the
/// per-account counter (429), then the answers: 400 for the current name, 409 for a taken one and 429
/// `DISPLAY_NAME_COOLDOWN` inside the window. Seats of games already created keep their snapshot of the old name.
pub async fn change_display_name(
    State(state): State<AppState>,
    headers: HeaderMap,
    jar: CookieJar,
    body: Body,
) -> Result<Response, ApiError> {
    let user = require_session(&state, &jar).await?;
    let body = read_json(body, &headers).await?;
    let display_name = match string_field(&body, "displayName") {
        Some(raw) => validate_display_name(raw),
        None => Err(vec![FieldIssue::DisplayNameRequired]),
    }
    .map_err(|issues| ApiError::invalid_fields(vec![("displayName", issues)]))?;

    rate_limit(
        &state,
        &format!("display-name-change:user:{}", user.id),
        DISPLAY_NAME_CHANGE_LIMIT,
        DISPLAY_NAME_CHANGE_WINDOW_MS,
    )
    .await?;
    let stored = state
        .db
        .find_user_by_id(user.id)
        .await
        .map_err(db_error)?
        .ok_or_else(ApiError::unauthorized)?;
    if stored.display_name == display_name {
        return Err(ApiError::validation(SAME_DISPLAY_NAME_MESSAGE).with_field(
            "displayName",
            "DISPLAY_NAME_UNCHANGED",
            SAME_DISPLAY_NAME_MESSAGE,
        ));
    }
    let now = (state.clock)();
    let outcome = state
        .db
        .rename_user(
            user.id,
            &display_name,
            &normalize_display_name(&display_name),
            now,
            now - DISPLAY_NAME_CHANGE_WINDOW,
        )
        .await
        .map_err(db_error)?;
    match outcome {
        RenameOutcome::Renamed => Ok(ok(StatusCode::OK, json!({ "displayName": display_name }))),
        RenameOutcome::Taken => Err(ApiError::conflict("CONFLICT", DISPLAY_NAME_TAKEN_MESSAGE)
            .with_field(
                "displayName",
                "DISPLAY_NAME_TAKEN",
                DISPLAY_NAME_TAKEN_MESSAGE,
            )
            .with_data(json!({ "reason": "display_name_taken" }))),
        RenameOutcome::Cooldown { changed_at } => {
            let remaining = (changed_at + DISPLAY_NAME_CHANGE_WINDOW - now).whole_seconds();
            let seconds = u64::try_from(remaining).unwrap_or(0).max(1);
            Err(ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "DISPLAY_NAME_COOLDOWN",
                DISPLAY_NAME_COOLDOWN_MESSAGE,
            )
            .with_retry_after(seconds))
        }
    }
}
