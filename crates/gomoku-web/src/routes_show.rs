//! `/api/show*`: the administrator's show (two bots, or a bot and an agent). Everything here sits behind the admin gate.

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use gomoku_core::admin::ShowLayout;
use gomoku_core::bot::choose_move::BotLevel;
use gomoku_core::bot::profile::{bot_display_name, bot_table_mb};
use gomoku_core::constants::SHOW_WAITING_TTL_MS;
use gomoku_core::game::draw_black_seat;
use gomoku_core::invitation_code::{
    format_invitation_code, generate_invitation_code, invitation_url,
};
use gomoku_db::OffsetDateTime;
use gomoku_db::Uuid;
use gomoku_db::games::{GameError, ShowBot, ShowRead, SpectatorState};
use serde_json::{Value, json};
use std::collections::HashMap;

use crate::api::{ApiError, ok, read_json};
use crate::app::AppState;
use crate::bot::schedule_bot_turn;
use crate::game_view::{iso_ms, spectator_view};
use crate::routes_auth::rate_limit;
use crate::routes_games::{origin_of, random_int};

type ApiResult = Result<Response, ApiError>;

const SHOW_CREATE_LIMIT: i32 = 10;
const SHOW_CREATE_WINDOW_MS: i64 = 3_600_000;
/// Slack of the "stalled" threshold: the bot is allowed this much on top of its pause and its search ceiling.
const SHOW_STALL_GRACE_MS: i64 = 5000;

/// A parsed body of create and preflight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShowRequest {
    pub layout: ShowLayout,
    /// One level per bot (two for bot vs bot, one for bot vs agent).
    pub levels: Vec<i16>,
}

/// Strict: only the fields of the layout, levels literally 1 or 2.
pub fn parse_show_request(body: &Value) -> Result<ShowRequest, ApiError> {
    let invalid = |message: &str| ApiError::validation(message);
    let Some(object) = body.as_object() else {
        return Err(invalid("Nieprawidłowe dane pokazu."));
    };
    let layout = object
        .get("layout")
        .and_then(Value::as_str)
        .and_then(ShowLayout::parse)
        .ok_or_else(|| invalid("Nieprawidłowy układ pokazu."))?;
    let level_of = |value: &Value| match value.as_i64() {
        Some(1) => Some(1i16),
        Some(2) => Some(2i16),
        _ => None,
    };
    let (key, count) = match layout {
        ShowLayout::BotBot => ("levels", 2),
        ShowLayout::BotAgent => ("botLevel", 1),
    };
    if object.len() != 2 || !object.contains_key(key) {
        return Err(invalid("Nieprawidłowe dane pokazu."));
    }
    let value = &object[key];
    let levels: Vec<i16> = if count == 2 {
        let Some(items) = value.as_array().filter(|a| a.len() == 2) else {
            return Err(invalid("Podaj poziom każdego bota."));
        };
        items
            .iter()
            .map(level_of)
            .collect::<Option<_>>()
            .ok_or_else(|| invalid("Nieprawidłowy poziom bota."))?
    } else {
        vec![level_of(value).ok_or_else(|| invalid("Nieprawidłowy poziom bota."))?]
    };
    Ok(ShowRequest { layout, levels })
}

fn unavailable(reason: &str) -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "SHOW_UNAVAILABLE",
        "Pokaz jest teraz wyłączony.",
    )
    .with_data(json!({ "reason": reason }))
}

fn level_of(number: i16) -> BotLevel {
    BotLevel::from_number(i64::from(number)).unwrap_or(BotLevel::One)
}

fn open_json(open: &gomoku_db::games::ShowOpen) -> Value {
    json!({
        "gameId": open.game_id,
        "status": if open.waiting { "waiting" } else { "active" },
        "layout": open.layout.as_str(),
    })
}

/// The checks of the preflight, in order; `engine` is a real trial only when `trial` is set.
async fn checks(
    state: &AppState,
    request: &ShowRequest,
    trial: bool,
) -> Result<Vec<Value>, ApiError> {
    let config = &state.config;
    let db_error = |e: GameError| ApiError::from(e);
    let mut out = vec![json!({ "id": "show_enabled", "ok": config.show.enabled })];
    if request.layout == ShowLayout::BotBot {
        out.push(json!({ "id": "bot_vs_bot_enabled", "ok": config.show.bot_vs_bot_enabled }));
        if request.levels.contains(&2) {
            out.push(json!({ "id": "level2_enabled", "ok": config.show.bot_level2_enabled }));
        }
    }
    out.push(json!({ "id": "bot_enabled", "ok": config.bot.enabled }));
    let available = state.bot.available_mb();
    out.push(json!({
        "id": "memory",
        "ok": available >= config.bot.min_available_mb as f64,
        "detail": { "availableMb": available.floor() as i64, "minAvailableMb": config.bot.min_available_mb },
    }));
    let active = state
        .db
        .count_active_bot_games((state.clock)())
        .await
        .map_err(db_error)?;
    out.push(json!({
        "id": "slot",
        "ok": active < i64::from(config.bot.max_active_games),
        "detail": { "activeGames": active, "maxActiveGames": config.bot.max_active_games },
    }));
    let current = state
        .db
        .show_current((state.clock)())
        .await
        .map_err(db_error)?;
    out.push(json!({ "id": "no_show_running", "ok": current.is_none() }));
    out.push(if trial && config.bot.enabled {
        let result = state.bot.trial().await;
        json!({ "id": "engine", "ok": result.ok, "detail": { "ms": result.ms, "limitMs": result.limit_ms, "waitedMs": result.waited_ms } })
    } else {
        json!({ "id": "engine", "ok": null })
    });
    Ok(out)
}

fn all_ok(checks: &[Value]) -> bool {
    checks.iter().all(|c| c["ok"] != json!(false))
}

/// The overview of the console: the switches, the default preflight (no engine trial) and the open show.
pub async fn overview_data(state: &AppState) -> Result<Value, ApiError> {
    let default = ShowRequest {
        layout: ShowLayout::BotBot,
        levels: vec![1, 1],
    };
    let checks = checks(state, &default, false).await?;
    let current = state.db.show_current((state.clock)()).await?;
    Ok(json!({
        "enabled": state.config.show.enabled,
        "botVsBotEnabled": state.config.show.bot_vs_bot_enabled,
        "level2Enabled": state.config.show.bot_level2_enabled,
        "preflight": { "ok": all_ok(&checks), "checks": checks },
        "current": current.as_ref().map(open_json),
    }))
}

/// `GET /api/show`.
pub async fn overview(State(state): State<AppState>) -> ApiResult {
    Ok(ok(StatusCode::OK, overview_data(&state).await?))
}

/// `POST /api/show/preflight`.
pub async fn preflight(State(state): State<AppState>, headers: HeaderMap, body: Body) -> ApiResult {
    let request = parse_show_request(&read_json(body, &headers).await?)?;
    let checks = checks(&state, &request, true).await?;
    Ok(ok(
        StatusCode::OK,
        json!({ "ok": all_ok(&checks), "checks": checks }),
    ))
}

/// `POST /api/show`. Order: flags, a running show (before the pool and the admission: a refusal must not spend them),
/// the pool, the admission, the transaction (which decides a race).
pub async fn create(State(state): State<AppState>, headers: HeaderMap, body: Body) -> ApiResult {
    let request = parse_show_request(&read_json(body, &headers).await?)?;
    let config = &state.config;
    if !config.show.enabled {
        return Err(unavailable("disabled"));
    }
    if request.layout == ShowLayout::BotBot {
        if !config.show.bot_vs_bot_enabled {
            return Err(unavailable("bot_vs_bot_disabled"));
        }
        if !config.show.bot_level2_enabled && request.levels.contains(&2) {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "BOT_UNAVAILABLE",
                gomoku_core::bot::profile::BOT_UNAVAILABLE_MESSAGE,
            )
            .with_retry_after(60)
            .with_data(json!({ "reason": "level2_not_allowed" })));
        }
    }
    let now = (state.clock)();
    if let Some(open) = state.db.show_current(now).await? {
        return Err(GameError::ShowRunning {
            game_id: open.game_id,
            layout: open.layout,
        }
        .into());
    }
    rate_limit(
        &state,
        "show-create:admin",
        SHOW_CREATE_LIMIT,
        SHOW_CREATE_WINDOW_MS,
    )
    .await?;
    let levels: Vec<BotLevel> = request.levels.iter().map(|&l| level_of(l)).collect();
    let strengths = state.bot.admit_show(&state, &levels).await?;
    let bots: Vec<ShowBot> = strengths
        .iter()
        .map(|s| ShowBot {
            level: s.level.number(),
            memory_mb: s.memory_mb.and_then(|m| i16::try_from(m).ok()),
            depth_cap: s.depth_cap.and_then(|c| i16::try_from(c).ok()),
        })
        .collect();
    let mut new_code = || generate_invitation_code(random_int);
    let created = state
        .db
        .show_create(
            &bots,
            draw_black_seat(random_int),
            &mut new_code,
            SHOW_WAITING_TTL_MS as i64,
            now,
        )
        .await?;
    tracing::info!(event = "security.show_created");
    let bots_json: Vec<Value> = strengths
        .iter()
        .enumerate()
        .map(|(i, s)| {
            json!({
                "seat": i + 1,
                "name": bot_display_name(s.level, s.memory_mb),
                "level": s.level.number(),
                "tableMb": bot_table_mb(s.level, s.memory_mb),
            })
        })
        .collect();
    let mut data =
        json!({ "gameId": created.game_id, "layout": created.layout.as_str(), "bots": bots_json });
    if let (Some(code), Some(expires)) = (&created.code, created.expires_at) {
        data["invitation"] = invitation_json(&state, code, expires);
    }
    if created.layout == ShowLayout::BotBot {
        schedule_bot_turn(&state, created.game_id);
    }
    Ok(ok(StatusCode::CREATED, data))
}

fn invitation_json(state: &AppState, code: &str, expires: OffsetDateTime) -> Value {
    let origin = origin_of(state);
    json!({
        "code": format_invitation_code(code),
        "url": invitation_url(origin, code),
        "mcpUrl": gomoku_core::agent::agent_mcp_url(origin),
        "expiresAt": iso_ms(expires),
    })
}

/// The JSON of a show: the spectator's view without chat and audience, plus the layout, the drive and the invitation.
pub fn show_state(
    state: &AppState,
    read: &ShowRead,
    after: usize,
    now: OffsetDateTime,
) -> Result<Value, GameError> {
    let spectator = SpectatorState {
        game: read.game.clone(),
        players: read.players.clone(),
        audience: None,
        paper: None,
        can_chat: false,
    };
    let mut view = spectator_view(&spectator, after, now)?;
    let bots = read.players.iter().filter(|p| p.kind == "bot").count();
    let limits = state.config.bot.limits;
    let stalled = read.bot_on_turn.is_some_and(|(level, elapsed)| {
        let own = if level == 2 {
            limits.level2.ceiling_ms
        } else {
            limits.level1.ceiling_ms
        };
        let queue = i64::try_from(state.bot.pending())
            .unwrap_or(i64::MAX)
            .saturating_mul(limits.level2.ceiling_ms + 500);
        elapsed > state.config.show.pause_max_ms + SHOW_STALL_GRACE_MS + queue + own
    });
    if let Some(object) = view.as_object_mut() {
        object.remove("canChat");
        object.insert(
            "layout".into(),
            json!(ShowLayout::of_bots(bots as i64).as_str()),
        );
        object.insert(
            "drive".into(),
            json!({ "botOnTurn": read.bot_on_turn.is_some(), "stalled": stalled }),
        );
        if let Some(invitation) = &read.invitation {
            object.insert(
                "invitation".into(),
                invitation_json(state, &invitation.code, invitation.expires_at),
            );
        }
        if let Some(reason) = read.closed_reason {
            object.insert("closedReason".into(), json!(reason));
        }
    }
    Ok(view)
}

/// A show id from the path; anything else is the same 404 as an unknown show.
pub fn show_id(raw: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(raw).map_err(|_| GameError::NotFound.into())
}

/// `GET /api/show/{id}?after=N`: also the drive of the show (a bot on turn plays after the response).
pub async fn read(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult {
    let id = show_id(&id)?;
    let after = match query.get("after") {
        None => 0,
        Some(raw)
            if raw.len() <= 5 && raw.bytes().all(|b| b.is_ascii_digit()) && !raw.is_empty() =>
        {
            raw.parse::<usize>()
                .map_err(|_| ApiError::validation("Nieprawidłowy parametr after."))?
        }
        Some(_) => return Err(ApiError::validation("Nieprawidłowy parametr after.")),
    };
    let now = (state.clock)();
    let read = state.db.show_read(id, after, now).await?;
    let view = show_state(&state, &read, after, now)?;
    if read.bot_on_turn.is_some() {
        schedule_bot_turn(&state, id);
    }
    Ok(ok(StatusCode::OK, view))
}

/// `POST /api/show/{id}/end`.
pub async fn end(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult {
    let id = show_id(&id)?;
    let now = (state.clock)();
    state.db.show_end(id, now).await?;
    tracing::info!(event = "security.show_ended");
    let read = state.db.show_read(id, 0, now).await?;
    Ok(ok(StatusCode::OK, show_state(&state, &read, 0, now)?))
}
