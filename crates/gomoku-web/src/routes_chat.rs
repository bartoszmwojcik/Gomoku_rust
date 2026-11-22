//! The chat API: players write to each other, spectators of a public game write to everybody. Every answer is about one
//! game and one asker, so none is cacheable. Text is never logged and never echoed in an error.

use std::collections::HashMap;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum_extra::extract::CookieJar;
use gomoku_core::chat::{ChatRejection, normalize_chat_message};
use gomoku_core::constants::{
    CHAT_MAX_PAGE_SIZE, CHAT_MAX_SEQ, CHAT_PAGE_SIZE, CHAT_RAW_MAX_UNITS,
};
use gomoku_db::Uuid;
use gomoku_db::games::{ChatMessageRow, ChatRead, GameError};
use serde_json::{Value, json};

use crate::api::{ApiError, bool_field, ok, read_json};
use crate::app::AppState;
use crate::game_view::iso_ms;
use crate::guest::spectator_token;
use crate::routes_games::{refresh_guest, resolve_participant};
use crate::security::hash_opaque_token;
use crate::session::{CurrentUser, current_user};

/// A spectator of one game, as proven by the account or the spectator cookie.
#[derive(Debug, Clone)]
pub struct SpectatorAccess {
    pub game_id: Uuid,
    pub spectator_id: Uuid,
    pub user: Option<CurrentUser>,
}

/// Resolves the caller as a live spectator of the game; the same `404` for "no such game", "not a spectator" and "access
/// expired".
pub async fn resolve_spectator(
    state: &AppState,
    jar: &CookieJar,
    raw_game_id: &str,
) -> Result<SpectatorAccess, ApiError> {
    let not_found = || ApiError::from(GameError::NotFound);
    let game_id = Uuid::parse_str(raw_game_id).map_err(|_| not_found())?;
    let user = current_user(state, jar).await;
    let hash = spectator_token(jar, state.config.session_cookie_secure)
        .map(|token| hash_opaque_token(&token));
    let spectator_id = state
        .db
        .find_spectator(
            game_id,
            user.as_ref().map(|u| u.id),
            hash.as_deref(),
            (state.clock)(),
        )
        .await
        .map_err(|e| ApiError::internal_logged("database", &e))?
        .ok_or_else(not_found)?;
    Ok(SpectatorAccess {
        game_id,
        spectator_id,
        user,
    })
}

fn digits(raw: &str, max_len: usize) -> Option<i64> {
    (!raw.is_empty() && raw.len() <= max_len && raw.bytes().all(|b| b.is_ascii_digit()))
        .then(|| raw.parse().ok())
        .flatten()
}

/// `?after_id=N&limit=M`: the cursor (optional) and the page size (1..100, default 50).
fn parse_page(query: &HashMap<String, String>) -> Result<(Option<i64>, i64), ApiError> {
    let after = match query.get("after_id") {
        None => None,
        Some(raw) => Some(
            digits(raw, 10)
                .filter(|n| *n <= CHAT_MAX_SEQ)
                .ok_or_else(|| ApiError::validation("Nieprawidłowy parametr after_id."))?,
        ),
    };
    let limit = match query.get("limit") {
        None => CHAT_PAGE_SIZE as i64,
        Some(raw) => digits(raw, 3)
            .filter(|n| (1..=CHAT_MAX_PAGE_SIZE as i64).contains(n))
            .ok_or_else(|| ApiError::validation("Nieprawidłowy parametr limit."))?,
    };
    Ok((after, limit))
}

fn message_json(message: &ChatMessageRow) -> Value {
    let mut json = json!({
        "id": message.seq,
        "seat": message.seat.map(|s| s.number()),
        "text": message.body,
        "createdAt": iso_ms(message.created_at),
    });
    if message.seat.is_none()
        && let (Some(name), Some(object)) = (&message.author_name, json.as_object_mut())
    {
        object.insert("spectatorName".into(), json!(name));
    }
    json
}

fn read_json_of(state: &AppState, read: &ChatRead) -> Value {
    let mut data = json!({
        "state": read.state.as_str(),
        "messages": read.messages.iter().map(message_json).collect::<Vec<_>>(),
        "lastId": read.last_id,
        "limits": {
            "maxLength": state.config.chat.max_length,
            "minIntervalMs": state.config.chat.min_interval_ms,
        },
    });
    if let Some(object) = data.as_object_mut() {
        if read.retry_after_ms > 0 {
            object.insert("retryAfterMs".into(), json!(read.retry_after_ms));
        }
        if read.opponent_disabled {
            object.insert("opponentDisabled".into(), json!(true));
        }
    }
    data
}

/// The body of a send: `{ text, clientId }`, nothing else. Nothing of the text is repeated in an error.
fn send_body(body: &Value) -> Result<(String, Uuid), ApiError> {
    let invalid = || ApiError::validation("Nieprawidłowe dane.");
    let Some(object) = body.as_object() else {
        return Err(invalid().with_field("_form", "invalid_type", "Nieprawidłowe dane."));
    };
    let mut error = invalid();
    let mut failed = false;
    if object.keys().any(|k| k != "text" && k != "clientId") {
        error = error.with_field("_form", "unrecognized_keys", "Nieprawidłowe dane.");
        failed = true;
    }
    let text = match object.get("text") {
        Some(Value::String(text)) if text.encode_utf16().count() <= CHAT_RAW_MAX_UNITS => {
            Some(text.clone())
        }
        Some(Value::String(_)) => {
            error = error.with_field("text", "too_big", ChatRejection::TooLong.message());
            failed = true;
            None
        }
        _ => {
            error = error.with_field("text", "invalid_type", ChatRejection::NotText.message());
            failed = true;
            None
        }
    };
    let client_id = object
        .get("clientId")
        .and_then(Value::as_str)
        .and_then(|raw| Uuid::parse_str(raw).ok());
    if client_id.is_none() {
        error = error.with_field(
            "clientId",
            "invalid_string",
            "Nieprawidłowy identyfikator wiadomości.",
        );
        failed = true;
    }
    match (failed, text, client_id) {
        (false, Some(text), Some(client_id)) => Ok((text, client_id)),
        _ => Err(error),
    }
}

fn rejected(reason: ChatRejection) -> ApiError {
    let message = reason.message();
    ApiError::validation(message)
        .with_field_params(
            "text",
            json!({ "code": "CHAT_REJECTED", "params": { "reason": reason.as_str() } }),
            message,
        )
        .with_data(json!({ "reason": reason.as_str() }))
}

fn sent_response(sent: &gomoku_db::games::ChatSent) -> Response {
    let status = if sent.created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    ok(
        status,
        json!({ "message": message_json(&sent.message), "retryAfterMs": sent.retry_after_ms }),
    )
}

/// `GET /api/games/{id}/chat`: the query is checked before the participant.
pub async fn chat_get(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    jar: CookieJar,
) -> Result<Response, ApiError> {
    let (after, limit) = parse_page(&query)?;
    let participant = resolve_participant(&state, &jar, &id).await?;
    let read = state
        .db
        .chat_read(
            participant.game_id,
            participant.player_id,
            after,
            limit,
            state.config.chat.min_interval_ms,
        )
        .await?;
    Ok(ok(StatusCode::OK, read_json_of(&state, &read)))
}

/// `POST /api/games/{id}/chat`. Order: body (400), participant (404), the text (400), then the service.
pub async fn chat_post(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    jar: CookieJar,
    body: Body,
) -> Result<Response, ApiError> {
    let (text, client_id) = send_body(&read_json(body, &headers).await?)?;
    let participant = resolve_participant(&state, &jar, &id).await?;
    let text = normalize_chat_message(&text, state.config.chat.max_length).map_err(rejected)?;
    let sent = state
        .db
        .chat_send(
            participant.game_id,
            participant.player_id,
            client_id,
            &text,
            state.config.chat.min_interval_ms,
            state.config.chat.max_messages_per_game,
        )
        .await?;
    Ok(sent_response(&sent))
}

/// `PUT /api/games/{id}/chat/settings` with `{ "enabled": boolean }` and nothing else.
pub async fn chat_settings(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    jar: CookieJar,
    body: Body,
) -> Result<(CookieJar, Response), ApiError> {
    let body = read_json(body, &headers).await?;
    let enabled = match (
        bool_field(&body, "enabled"),
        body.as_object().map(serde_json::Map::len),
    ) {
        (Some(enabled), Some(1)) => enabled,
        _ => {
            return Err(
                ApiError::validation("Ustawienie czatu musi być wartością logiczną.").with_field(
                    "enabled",
                    "invalid_type",
                    "Ustawienie czatu musi być wartością logiczną.",
                ),
            );
        }
    };
    let participant = resolve_participant(&state, &jar, &id).await?;
    let state_after = state
        .db
        .chat_set_enabled(participant.game_id, participant.player_id, enabled)
        .await?;
    Ok((
        refresh_guest(&state, jar, &participant),
        ok(StatusCode::OK, json!({ "state": state_after.as_str() })),
    ))
}

/// `GET /api/games/{id}/spectate/chat`: the conversation of every sender, for a spectator.
pub async fn spectator_chat_get(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    jar: CookieJar,
) -> Result<Response, ApiError> {
    let (after, limit) = parse_page(&query)?;
    let access = resolve_spectator(&state, &jar, &id).await?;
    let read = state
        .db
        .spectator_chat_read(
            access.game_id,
            access.spectator_id,
            after,
            limit,
            state.config.chat.min_interval_ms,
        )
        .await?;
    Ok(ok(StatusCode::OK, read_json_of(&state, &read)))
}

/// `POST /api/games/{id}/spectate/chat`.
pub async fn spectator_chat_post(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    jar: CookieJar,
    body: Body,
) -> Result<Response, ApiError> {
    let (text, client_id) = send_body(&read_json(body, &headers).await?)?;
    let access = resolve_spectator(&state, &jar, &id).await?;
    let text = normalize_chat_message(&text, state.config.chat.max_length).map_err(rejected)?;
    let sent = state
        .db
        .spectator_chat_send(
            access.game_id,
            access.spectator_id,
            client_id,
            &text,
            state.config.chat.min_interval_ms,
            state.config.chat.max_messages_per_game,
        )
        .await?;
    Ok(sent_response(&sent))
}
