//! The lobby and the history: `GET /api/lobby`, `PUT|DELETE /api/lobby/seeking`, `POST /api/lobby/seekers/{id}/pick`,
//! `GET /api/history`.

use std::collections::HashMap;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum_extra::extract::CookieJar;
use gomoku_core::auth::FieldIssue;
use gomoku_core::game::draw_black_seat;
use gomoku_core::history::{
    HistoryCursor, decode_history_cursor, encode_history_cursor, history_result_of,
    is_cursor_length_ok,
};
use gomoku_core::lobby::{
    LOBBY_LIST_LIMIT, LOBBY_MARK_LIMIT, LOBBY_MARK_WINDOW_MS, LOBBY_PICK_LIMIT,
    LOBBY_PICK_WINDOW_MS, SEEKING_TTL_MS,
};
use gomoku_core::nickname::default_nickname;
use gomoku_db::Uuid;
use gomoku_db::games::{GameError, HistoryEntryRow, SeekingStatus};
use serde_json::{Value, json};

use crate::api::{ApiError, ok, read_optional_json};
use crate::app::AppState;
use crate::game_view::iso_ms;
use crate::routes_auth::rate_limit;
use crate::routes_games::{game_settings, random_int};
use crate::routes_password::require_session;

fn me_json(status: &SeekingStatus) -> Value {
    json!({
        "id": status.id, "createdAt": iso_ms(status.created_at), "expiresInMs": status.expires_in_ms,
        "variant": status.settings.variant.as_str(), "timeControlMs": status.settings.time_control_ms,
    })
}

/// The lobby as the signed-in account sees it. Times are relative (the client counts down on its own monotonic clock).
pub async fn lobby(State(state): State<AppState>, jar: CookieJar) -> Result<Response, ApiError> {
    let user = require_session(&state, &jar).await?;
    let view = state
        .db
        .lobby_view(user.id, (state.clock)(), LOBBY_LIST_LIMIT as i64)
        .await?;
    Ok(ok(StatusCode::OK, lobby_json(&view)))
}

pub fn lobby_json(view: &gomoku_db::games::LobbyRows) -> Value {
    json!({
        "me": view.me.as_ref().map(me_json),
        "currentGame": view.current_game.map(|g| json!({ "gameId": g.game_id, "status": g.status.as_str() })),
        "seekers": view.seekers.iter().map(|s| json!({
            "id": s.id, "displayName": s.display_name, "sinceMs": s.since_ms, "expiresInMs": s.expires_in_ms,
            "variant": s.settings.variant.as_str(), "timeControlMs": s.settings.time_control_ms,
        })).collect::<Vec<_>>(),
    })
}

/// Marks the account as looking for a game (5 minutes, never renewed).
pub async fn mark_seeking(
    State(state): State<AppState>,
    headers: HeaderMap,
    jar: CookieJar,
    body: Body,
) -> Result<Response, ApiError> {
    let user = require_session(&state, &jar).await?;
    let settings = game_settings(&read_optional_json(body, &headers).await?)?;
    rate_limit(
        &state,
        &format!("lobby-mark:{}", user.id),
        LOBBY_MARK_LIMIT as i32,
        LOBBY_MARK_WINDOW_MS as i64,
    )
    .await?;
    let status = state
        .db
        .mark_seeking(user.id, (state.clock)(), SEEKING_TTL_MS as i64, settings)
        .await?;
    Ok(ok(StatusCode::OK, json!({ "me": me_json(&status) })))
}

/// Withdraws the status; also fine without one.
pub async fn withdraw_seeking(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<Response, ApiError> {
    let user = require_session(&state, &jar).await?;
    state.db.withdraw_seeking(user.id).await?;
    Ok(ok(StatusCode::OK, json!({ "me": Value::Null })))
}

/// Starts a game with a seeker; the picker need not be looking themselves.
pub async fn pick(
    State(state): State<AppState>,
    Path(id): Path<String>,
    jar: CookieJar,
) -> Result<Response, ApiError> {
    let user = require_session(&state, &jar).await?;
    let seeker = Uuid::parse_str(&id).map_err(|_| {
        ApiError::new(
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            "Nie znaleziono takiego gracza w lobby.",
        )
    })?;
    rate_limit(
        &state,
        &format!("lobby-pick:{}", user.id),
        LOBBY_PICK_LIMIT as i32,
        LOBBY_PICK_WINDOW_MS as i64,
    )
    .await?;
    let game_id = state
        .db
        .pick_seeker(
            user.id,
            seeker,
            (state.clock)(),
            draw_black_seat(random_int),
        )
        .await?;
    Ok(ok(StatusCode::OK, json!({ "gameId": game_id })))
}

/// `2026-10-31T15:04:05.123456Z` -> `2026-10-31T15:04:05.123Z`.
fn iso_from_micros(text: &str) -> String {
    match (text.get(..23), text.ends_with('Z')) {
        (Some(head), true) => format!("{head}Z"),
        _ => text.to_owned(),
    }
}

pub fn history_entry_json(entry: &HistoryEntryRow) -> Value {
    let name = match entry.opponent_kind.as_str() {
        "guest" => Some(
            entry
                .opponent_nickname
                .clone()
                .unwrap_or_else(|| default_nickname(entry.opponent_seat)),
        ),
        _ => entry.opponent_name.clone(),
    };
    let mut entry_json = json!({
        "gameId": entry.game_id,
        "variant": entry.variant.as_str(),
        "finishedAt": iso_from_micros(&entry.finished_at),
        "result": match history_result_of(entry.winner, entry.my_seat) {
            gomoku_core::history::HistoryResult::Win => "win",
            gomoku_core::history::HistoryResult::Loss => "loss",
            gomoku_core::history::HistoryResult::Draw => "draw",
        },
        "reason": entry.reason.as_str(),
        "opponent": { "kind": entry.opponent_kind, "name": name },
    });
    if let (Some(scores), Some(object)) = (entry.scores, entry_json.as_object_mut()) {
        object.insert("scores".into(), json!(scores));
    }
    entry_json
}

/// The account's finished games, newest first. Order: query (400), session (401), then the read.
pub async fn history(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
    jar: CookieJar,
) -> Result<Response, ApiError> {
    let cursor = match query.get("cursor") {
        None => None,
        Some(raw) if is_cursor_length_ok(raw) => {
            Some(decode_history_cursor(raw).map_err(|_| cursor_error())?)
        }
        Some(_) => return Err(cursor_error()),
    };
    let user = require_session(&state, &jar).await?;
    let page = fetch_history(&state, user.id, cursor).await?;
    Ok(ok(StatusCode::OK, page))
}

fn cursor_error() -> ApiError {
    ApiError::invalid_fields(vec![("cursor", vec![FieldIssue::CursorInvalid])])
}

pub async fn fetch_history(
    state: &AppState,
    user_id: Uuid,
    cursor: Option<HistoryCursor>,
) -> Result<Value, GameError> {
    let page = state
        .db
        .history_page(
            user_id,
            cursor.as_ref().and_then(|c| {
                Uuid::parse_str(&c.id)
                    .ok()
                    .map(|id| (c.finished_at.as_str(), id))
            }),
            (state.clock)(),
        )
        .await?;
    Ok(json!({
        "entries": page.entries.iter().map(history_entry_json).collect::<Vec<_>>(),
        "nextCursor": page.next.map(|(finished_at, id)| encode_history_cursor(&HistoryCursor { finished_at, id: id.to_string() })),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_microseconds_to_milliseconds() {
        assert_eq!(
            iso_from_micros("2026-10-31T15:04:05.123456Z"),
            "2026-10-31T15:04:05.123Z"
        );
        assert_eq!(iso_from_micros("garbage"), "garbage");
    }
}
