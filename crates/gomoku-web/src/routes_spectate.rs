//! Public games: becoming a spectator, reading as one, the creator's kill-switch and the QR code of the invitation.

use std::collections::HashMap;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum_extra::extract::CookieJar;
use gomoku_core::auth::FieldIssue;
use gomoku_core::invitation_code::{invitation_url, normalize_invitation_code};
use gomoku_core::nickname::validate_nickname;
use gomoku_db::games::{GameError, Spectator};
use qrcode::render::svg;
use qrcode::{EcLevel, QrCode};
use serde_json::json;

use crate::api::{ApiError, ok, read_optional_json, string_field};
use crate::app::AppState;
use crate::game_view::spectator_view;
use crate::guest::{spectator_cookie, spectator_refreshed, spectator_token};
use crate::routes_chat::resolve_spectator;
use crate::routes_games::{
    nickname_issue, origin_of, parse_after, refresh_guest, resolve_participant, view_response,
    with_lookup_attempt,
};
use crate::security::{hash_opaque_token, new_opaque_token};
use crate::session::current_user;

/// `POST /api/invitations/{code}/spectate`. Order: body (400), the account's nickname (400, before the pool), the pool
/// (429), then the join (409, 410). A new visitor gets the spectator cookie; the token is never in the JSON.
pub async fn spectate_join(
    State(state): State<AppState>,
    Path(code): Path<String>,
    headers: HeaderMap,
    jar: CookieJar,
    body: Body,
) -> Result<(CookieJar, axum::response::Response), ApiError> {
    let body = read_optional_json(body, &headers).await?;
    let nickname = match string_field(&body, "nickname").map(validate_nickname) {
        None | Some(Ok(None)) => None,
        Some(Ok(Some(nick))) => Some(nick),
        Some(Err(error)) => {
            return Err(ApiError::invalid_fields(vec![(
                "nickname",
                vec![nickname_issue(error)],
            )]));
        }
    };
    let user = current_user(&state, &jar).await;
    if user.is_some() && nickname.is_some() {
        return Err(ApiError::invalid_fields(vec![(
            "nickname",
            vec![FieldIssue::NicknameGuestOnly],
        )]));
    }
    let secure = state.config.session_cookie_secure;
    let new_token = new_opaque_token();
    let new_hash = hash_opaque_token(&new_token);
    let cookie_hash = spectator_token(&jar, secure).map(|token| hash_opaque_token(&token));
    let who = match &user {
        Some(user) => Spectator::Account(user.id),
        None => Spectator::Visitor {
            cookie_hash: cookie_hash.as_deref(),
            new_hash: &new_hash,
            nickname: nickname.as_deref(),
        },
    };
    let now = (state.clock)();
    let joined = with_lookup_attempt(&state, &headers, async {
        let code = normalize_invitation_code(&code).ok_or(GameError::InvalidInvitation)?;
        state.db.spectate_join(&code, who, now).await
    })
    .await?;
    let jar = if joined.new_cookie {
        jar.add(spectator_cookie(secure, new_token))
    } else {
        jar
    };
    Ok((jar, ok(StatusCode::OK, json!({ "gameId": joined.game_id }))))
}

/// `GET /api/games/{id}/spectate?after=N`: the state of a public game for a spectator; slides their access.
pub async fn spectate_state(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    jar: CookieJar,
) -> Result<(CookieJar, axum::response::Response), ApiError> {
    let after = parse_after(&query)?;
    let access = resolve_spectator(&state, &jar, &id).await?;
    let now = (state.clock)();
    let read = state
        .db
        .spectator_state(access.game_id, access.spectator_id, after, now)
        .await?;
    let view = spectator_view(&read, after, now)?;
    let jar = spectator_refreshed(jar, state.config.session_cookie_secure);
    Ok((jar, ok(StatusCode::OK, view)))
}

/// `POST /api/games/{id}/audience/close`: the creator ends the audience for good.
pub async fn audience_close(
    State(state): State<AppState>,
    Path(id): Path<String>,
    jar: CookieJar,
) -> Result<(CookieJar, axum::response::Response), ApiError> {
    let participant = resolve_participant(&state, &jar, &id).await?;
    let now = (state.clock)();
    let snapshot = state
        .db
        .close_audience(participant.game_id, participant.player_id, now)
        .await?;
    let response = view_response(&state, &snapshot, 0, now)?;
    Ok((refresh_guest(&state, jar, &participant), response))
}

/// The QR code of the invitation link as a stand-alone SVG: black on white in both themes, quiet zone included.
fn qr_svg(url: &str) -> Option<String> {
    let code = QrCode::with_error_correction_level(url.as_bytes(), EcLevel::M).ok()?;
    Some(
        code.render::<svg::Color>()
            .min_dimensions(200, 200)
            .quiet_zone(true)
            .dark_color(svg::Color("#000000"))
            .light_color(svg::Color("#ffffff"))
            .build(),
    )
}

/// `GET /api/games/{id}/qr`: for a player first, then a spectator; only while the game may show its code (a waiting game
/// with a live invitation, or a public game in progress). Everything else is the uniform `404`.
pub async fn game_qr(
    State(state): State<AppState>,
    Path(id): Path<String>,
    jar: CookieJar,
) -> Result<Response, ApiError> {
    let game_id = match resolve_participant(&state, &jar, &id).await {
        Ok(participant) => participant.game_id,
        Err(error) if error.status() == StatusCode::NOT_FOUND => {
            resolve_spectator(&state, &jar, &id).await?.game_id
        }
        Err(error) => return Err(error),
    };
    let code = state
        .db
        .qr_invitation_code(game_id, (state.clock)())
        .await
        .map_err(|e| ApiError::internal_logged("database", &e))?
        .ok_or(ApiError::from(GameError::NotFound))?;
    let svg = qr_svg(&invitation_url(origin_of(&state), &code))
        .ok_or_else(|| ApiError::internal_logged("qr", &"the link does not fit a QR code"))?;
    let mut response = (StatusCode::OK, svg).into_response();
    let headers = response.headers_mut();
    for (name, value) in [
        (header::CONTENT_TYPE, "image/svg+xml"),
        (header::CACHE_CONTROL, "private, no-store"),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (
            header::CONTENT_SECURITY_POLICY,
            "default-src 'none'; sandbox",
        ),
    ] {
        headers.insert(name, HeaderValue::from_static(value));
    }
    Ok(response)
}
