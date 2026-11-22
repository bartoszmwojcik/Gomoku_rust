//! The game API: create, state (the poll), moves, resignation, cancel, rematch and accepting an invitation.

use std::collections::HashMap;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum_extra::extract::CookieJar;
use gomoku_core::auth::FieldIssue;
use gomoku_core::constants::{
    CREATE_GAME_LIMIT, CREATE_GAME_WINDOW_MS, INVITATION_LOOKUP_GLOBAL_LIMIT,
    INVITATION_LOOKUP_LIMIT, INVITATION_LOOKUP_WINDOW_MS, INVITATION_TTL_MS, REMATCH_OFFER_LIMIT,
    REMATCH_OFFER_WINDOW_MS,
};
use gomoku_core::game::{draw_black_seat, guest_cookie_max_age};
use gomoku_core::invitation_code::{
    format_invitation_code, generate_invitation_code, invitation_url, normalize_invitation_code,
};
use gomoku_core::nickname::{NicknameError, validate_nickname};
use gomoku_db::games::{
    AcceptInput, CreatedGame, GameError, GuestCreateInput, JoinAs, MoveInput, Snapshot, to_ms,
};
use gomoku_db::{OffsetDateTime, Uuid};
use serde_json::{Value, json};

use crate::api::{
    ApiError, bool_field, int_field, ok, read_json, read_optional_json, string_field,
};
use crate::app::AppState;
use crate::client_address::client_address;
use crate::game_view::{game_view, iso_ms};
use crate::guest::{guest_cookie, guest_cookie_default, guest_token, guest_token_hash, refreshed};
use crate::routes_auth::rate_limit;
use crate::security::{hash_opaque_token, new_opaque_token};
use crate::session::{CurrentUser, current_user};

const DEV_ORIGIN: &str = "http://localhost:3000";

pub fn origin_of(state: &AppState) -> &str {
    state.config.app_origin.as_deref().unwrap_or(DEV_ORIGIN)
}

/// A uniform random integer in `[0, max)` from the operating system's generator.
pub fn random_int(max: usize) -> usize {
    rand::random_range(0..max)
}

/// The caller as a participant of a game: the game, their seat's id, and how they proved it.
#[derive(Debug, Clone)]
pub struct Participant {
    pub game_id: Uuid,
    pub player_id: Uuid,
    pub user: Option<CurrentUser>,
    /// The raw guest token from the cookie, if the caller came in as a guest.
    pub guest_token: Option<String>,
}

/// Resolves who the caller is in the game: an account first, else a guest token still valid for the game's status;
/// otherwise nothing: the same `404` for "no such game" and "not your game". A malformed id is the same `404`.
pub async fn resolve_participant(
    state: &AppState,
    jar: &CookieJar,
    raw_game_id: &str,
) -> Result<Participant, ApiError> {
    let not_found = || ApiError::from(GameError::NotFound);
    let game_id = Uuid::parse_str(raw_game_id).map_err(|_| not_found())?;
    let secure = state.config.session_cookie_secure;
    let user = current_user(state, jar).await;
    let token = guest_token(jar, secure);
    let hash = token.as_deref().map(hash_opaque_token);
    let player = state
        .db
        .find_participant(
            game_id,
            user.as_ref().map(|u| u.id),
            hash.as_deref(),
            (state.clock)(),
        )
        .await
        .map_err(|e| ApiError::internal_logged("database", &e))?
        .ok_or_else(not_found)?;
    Ok(Participant {
        game_id,
        player_id: player,
        user,
        guest_token: token,
    })
}

fn invitation_json(created: &CreatedGame, origin: &str) -> Value {
    json!({
        "code": format_invitation_code(&created.code),
        "url": invitation_url(origin, &created.code),
        "expiresAt": iso_ms(created.expires_at),
    })
}

pub(crate) fn nickname_issue(error: NicknameError) -> FieldIssue {
    match error {
        NicknameError::TooLong { .. } => FieldIssue::NicknameTooLong,
        NicknameError::Invalid => FieldIssue::NicknameInvalid,
        NicknameError::Reserved => FieldIssue::NicknameReserved,
    }
}

/// The guest-only fields of a body: the acknowledgement of the guest limitations and the nickname.
fn guest_fields(body: &Value) -> Result<Option<String>, ApiError> {
    let mut fields: Vec<(&str, Vec<FieldIssue>)> = Vec::new();
    if bool_field(body, "acknowledgedGuestWarning") != Some(true) {
        fields.push((
            "acknowledgedGuestWarning",
            vec![FieldIssue::GuestWarningRequired],
        ));
    }
    let nickname = match string_field(body, "nickname").map(validate_nickname) {
        None => None,
        Some(Ok(nick)) => nick,
        Some(Err(error)) => {
            fields.push(("nickname", vec![nickname_issue(error)]));
            None
        }
    };
    if fields.is_empty() {
        Ok(nickname)
    } else {
        Err(ApiError::invalid_fields(fields))
    }
}

/// `isPublic` of a create body: absent is private; anything but a boolean is refused.
fn public_flag(body: &Value) -> Result<bool, ApiError> {
    match body.get("isPublic") {
        None => Ok(false),
        Some(Value::Bool(flag)) => Ok(*flag),
        Some(_) => Err(ApiError::validation("Nieprawidłowe dane.").with_field(
            "isPublic",
            "invalid_type",
            "Nieprawidłowa wartość.",
        )),
    }
}

fn new_code() -> String {
    generate_invitation_code(random_int)
}

/// Creates a game. An account sends no body; a guest (no session) sends `{ acknowledgedGuestWarning: true, nickname?,
/// cancelOwnWaitingGame? }`. Order for an account: session, body (400), the per-account counter (429), then the game
/// (the one-slot rule: 409). For a guest: body (400), client address (500), per-address counter (429), then the game.
pub async fn create_game(
    State(state): State<AppState>,
    headers: HeaderMap,
    jar: CookieJar,
    body: Body,
) -> Result<(CookieJar, Response), ApiError> {
    let now = (state.clock)();
    let secure = state.config.session_cookie_secure;
    if let Some(user) = current_user(&state, &jar).await {
        let is_public = public_flag(&read_optional_json(body, &headers).await?)?;
        rate_limit(
            &state,
            &format!("games-create:{}", user.id),
            CREATE_GAME_LIMIT as i32,
            CREATE_GAME_WINDOW_MS as i64,
        )
        .await?;
        let created = state
            .db
            .create_game_account(
                user.id,
                is_public,
                now,
                INVITATION_TTL_MS as i64,
                &mut new_code,
            )
            .await?;
        let data = json!({ "gameId": created.game_id, "invitation": invitation_json(&created, origin_of(&state)) });
        return Ok((jar, ok(StatusCode::CREATED, data)));
    }
    let body = read_json(body, &headers).await?;
    let nickname = guest_fields(&body)?;
    let is_public = public_flag(&body)?;
    let key = crate::client_address::client_key(&headers, "games-create-guest", &state.config)
        .map_err(|_| {
            ApiError::internal_logged("client address", &"unavailable behind the proxy")
        })?;
    rate_limit(
        &state,
        &key,
        CREATE_GAME_LIMIT as i32,
        CREATE_GAME_WINDOW_MS as i64,
    )
    .await?;
    // One browser, one slot: the token the browser already carries is reused, else a fresh one.
    let token = guest_token(&jar, secure).unwrap_or_else(new_opaque_token);
    let hash = hash_opaque_token(&token);
    let input = GuestCreateInput {
        token_hash: &hash,
        nickname: nickname.as_deref(),
        cancel_own_waiting_game: bool_field(&body, "cancelOwnWaitingGame") == Some(true),
        is_public,
    };
    let created = state
        .db
        .create_game_guest(&input, now, INVITATION_TTL_MS as i64, &mut new_code)
        .await?;
    let max_age = guest_cookie_max_age(Some(to_ms(created.expires_at)), to_ms(now));
    let jar = jar.add(guest_cookie(secure, token, max_age));
    let data = json!({ "gameId": created.game_id, "invitation": invitation_json(&created, origin_of(&state)) });
    Ok((jar, ok(StatusCode::CREATED, data)))
}

/// `?after=N`: how many moves the client already knows; digits only, at most five.
pub(crate) fn parse_after(query: &HashMap<String, String>) -> Result<usize, ApiError> {
    match query.get("after") {
        None => Ok(0),
        Some(raw) if (1..=5).contains(&raw.len()) && raw.bytes().all(|b| b.is_ascii_digit()) => raw
            .parse()
            .map_err(|_| ApiError::invalid_fields(vec![("after", vec![FieldIssue::AfterInvalid])])),
        Some(_) => Err(ApiError::invalid_fields(vec![(
            "after",
            vec![FieldIssue::AfterInvalid],
        )])),
    }
}

/// The cookie lifetime that follows a view: for a waiting game it lasts until the invitation expires plus the grace.
fn cookie_max_age(snapshot: &Snapshot, now: OffsetDateTime) -> i64 {
    let waiting_until = snapshot
        .invitation
        .as_ref()
        .filter(|_| snapshot.game.status == gomoku_core::types::GameStatus::Waiting)
        .map(|i| to_ms(i.expires_at));
    guest_cookie_max_age(waiting_until, to_ms(now))
}

pub(crate) fn view_response(
    state: &AppState,
    snapshot: &Snapshot,
    after: usize,
    now: OffsetDateTime,
) -> Result<Response, ApiError> {
    Ok(ok(
        StatusCode::OK,
        game_view(snapshot, after, now, origin_of(state))?,
    ))
}

/// The state of a game; also the poll. Everything lazy (cancellations, the clock start, a loss on time, `seen`) happens
/// inside; a guest's cookie slides forward and, once the opponent accepted a rematch, the proposing guest claims their
/// seat of the new game (a token derived from their cookie).
pub async fn get_game(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    jar: CookieJar,
) -> Result<(CookieJar, Response), ApiError> {
    let after = parse_after(&query)?;
    let participant = resolve_participant(&state, &jar, &id).await?;
    let now = (state.clock)();
    let secure = state.config.session_cookie_secure;
    let snapshot = state
        .db
        .game_state(participant.player_id, participant.game_id, after, now)
        .await?;
    let mut jar = jar;
    if snapshot
        .players
        .iter()
        .any(|p| p.seat == snapshot.me && p.is_guest())
    {
        jar = refreshed(jar, secure, cookie_max_age(&snapshot, now));
        if snapshot.game.rematch_state.as_deref() == Some("accepted")
            && let Some(token) = &participant.guest_token
            && let Some(derived) = state
                .db
                .claim_guest_seat(participant.player_id, participant.game_id, token)
                .await?
        {
            jar = jar.add(guest_cookie_default(secure, derived));
        }
    }
    let response = view_response(&state, &snapshot, after, now)?;
    Ok((jar, response))
}

/// A board coordinate or move number as the schema allows it first (0..100, 1..10201), then the classic board bounds.
fn move_input(body: &Value) -> Result<MoveInput, ApiError> {
    let x = int_field(body, "x");
    let y = int_field(body, "y");
    let number = int_field(body, "moveNumber");
    let in_range = |value: Option<i64>, max: i64| value.filter(|v| (0..=max).contains(v));
    let mut fields: Vec<(&str, Vec<FieldIssue>)> = Vec::new();
    let x = in_range(x, 100).filter(|v| *v <= 14);
    let y = in_range(y, 100).filter(|v| *v <= 14);
    let number = number.filter(|v| (1..=225).contains(v));
    if x.is_none() {
        fields.push(("x", vec![FieldIssue::ColumnInvalid]));
    }
    if y.is_none() {
        fields.push(("y", vec![FieldIssue::RowInvalid]));
    }
    if number.is_none() {
        fields.push(("moveNumber", vec![FieldIssue::MoveNumberInvalid]));
    }
    let (Some(x), Some(y), Some(number)) = (x, y, number) else {
        return Err(ApiError::invalid_fields(fields));
    };
    let cell = usize::try_from(y * 15 + x).map_err(|_| ApiError::internal())?;
    Ok(MoveInput {
        cell,
        move_number: usize::try_from(number).map_err(|_| ApiError::internal())?,
    })
}

pub(crate) fn refresh_guest(
    state: &AppState,
    jar: CookieJar,
    participant: &Participant,
) -> CookieJar {
    if participant.user.is_some() || participant.guest_token.is_none() {
        return jar;
    }
    refreshed(
        jar,
        state.config.session_cookie_secure,
        gomoku_core::game::GUEST_COOKIE_MAX_AGE_SECONDS,
    )
}

/// Plays a move. Order: body (400), participant (404), then the service (404, 409s).
pub async fn post_move(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    jar: CookieJar,
    body: Body,
) -> Result<(CookieJar, Response), ApiError> {
    let body = read_json(body, &headers).await?;
    let input = move_input(&body)?;
    let participant = resolve_participant(&state, &jar, &id).await?;
    let now = (state.clock)();
    let snapshot = state
        .db
        .play_move(participant.player_id, participant.game_id, input, now)
        .await?;
    let response = view_response(&state, &snapshot, input.move_number - 1, now)?;
    Ok((refresh_guest(&state, jar, &participant), response))
}

/// Resigns the game; the answer is the whole state.
pub async fn post_resign(
    State(state): State<AppState>,
    Path(id): Path<String>,
    jar: CookieJar,
) -> Result<(CookieJar, Response), ApiError> {
    let participant = resolve_participant(&state, &jar, &id).await?;
    let now = (state.clock)();
    let snapshot = state
        .db
        .resign(participant.player_id, participant.game_id, now)
        .await?;
    let response = view_response(&state, &snapshot, 0, now)?;
    Ok((refresh_guest(&state, jar, &participant), response))
}

/// Cancels the caller's waiting game (only the creator, only while nobody has joined).
pub async fn post_cancel(
    State(state): State<AppState>,
    Path(id): Path<String>,
    jar: CookieJar,
) -> Result<Response, ApiError> {
    let participant = resolve_participant(&state, &jar, &id).await?;
    state
        .db
        .cancel_game(participant.player_id, participant.game_id, (state.clock)())
        .await?;
    Ok(ok(StatusCode::OK, Value::Null))
}

/// Proposes a rematch. The checks that cost nothing come first (finished, opponent still there), then the attempt is
/// counted before the write; a lost race gives the attempt back.
pub async fn rematch_offer(
    State(state): State<AppState>,
    Path(id): Path<String>,
    jar: CookieJar,
) -> Result<(CookieJar, Response), ApiError> {
    let participant = resolve_participant(&state, &jar, &id).await?;
    let now = (state.clock)();
    let subject = state
        .db
        .rematch_subject(participant.player_id, participant.game_id)
        .await?;
    let key = format!("rematch-offer:{}", hash_opaque_token(&subject.to_string()));
    rate_limit(
        &state,
        &key,
        REMATCH_OFFER_LIMIT as i32,
        REMATCH_OFFER_WINDOW_MS as i64,
    )
    .await?;
    let Some(offer) = state
        .db
        .offer_rematch(participant.player_id, participant.game_id, now)
        .await?
    else {
        let _ = state
            .db
            .rate_limit_release(&key, REMATCH_OFFER_WINDOW_MS as i64)
            .await;
        return Err(GameError::RematchAlreadyOffered.into());
    };
    let data = json!({
        "state": "pending",
        "offeredBySeat": offer.offered_by_seat.number(),
        "expiresAt": iso_ms(offer.expires_at),
        "gameId": Value::Null,
    });
    Ok((
        refresh_guest(&state, jar, &participant),
        ok(StatusCode::OK, data),
    ))
}

/// Accepts the opponent's rematch offer; a guest's token for the new game goes only into the cookie.
pub async fn rematch_accept(
    State(state): State<AppState>,
    Path(id): Path<String>,
    jar: CookieJar,
) -> Result<(CookieJar, Response), ApiError> {
    let participant = resolve_participant(&state, &jar, &id).await?;
    let accepted = state
        .db
        .accept_rematch(
            participant.player_id,
            participant.game_id,
            participant.guest_token.as_deref(),
            (state.clock)(),
        )
        .await?;
    let jar = match accepted.guest_token {
        Some(token) => jar.add(guest_cookie_default(
            state.config.session_cookie_secure,
            token,
        )),
        None => jar,
    };
    Ok((
        jar,
        ok(StatusCode::OK, json!({ "gameId": accepted.game_id })),
    ))
}

/// Declines (or withdraws) the offer.
pub async fn rematch_decline(
    State(state): State<AppState>,
    Path(id): Path<String>,
    jar: CookieJar,
) -> Result<(CookieJar, Response), ApiError> {
    let participant = resolve_participant(&state, &jar, &id).await?;
    state
        .db
        .decline_rematch(participant.player_id, participant.game_id, (state.clock)())
        .await?;
    Ok((
        refresh_guest(&state, jar, &participant),
        ok(StatusCode::OK, Value::Null),
    ))
}

/// The pool of failed invitation-code lookups, shared by the invitation page and accepting: 20 per client address in 15
/// minutes, or one global pool of 300 when the address cannot be trusted. The attempt is counted BEFORE the lookup (so a
/// spent pool answers `429` even for a correct code), released when the lookup succeeds (the pool counts failures only)
/// and kept for an invalid invitation. A failing release is swallowed: the counter just stays high.
pub async fn with_lookup_attempt<T, F>(
    state: &AppState,
    headers: &HeaderMap,
    lookup: F,
) -> Result<T, ApiError>
where
    F: std::future::Future<Output = Result<T, GameError>>,
{
    let (key, limit) = match client_address(headers, &state.config) {
        Some(address) => (
            format!("invitation-lookup:ip:{}", hash_opaque_token(&address)),
            INVITATION_LOOKUP_LIMIT,
        ),
        None => (
            "invitation-lookup:global".to_owned(),
            INVITATION_LOOKUP_GLOBAL_LIMIT,
        ),
    };
    rate_limit(
        state,
        &key,
        limit as i32,
        INVITATION_LOOKUP_WINDOW_MS as i64,
    )
    .await?;
    let result = lookup.await;
    match &result {
        Err(GameError::InvalidInvitation) => {
            tracing::warn!(
                event = "invitation_lookup_failed",
                "invitation lookup failed"
            );
        }
        _ => {
            let _ = state
                .db
                .rate_limit_release(&key, INVITATION_LOOKUP_WINDOW_MS as i64)
                .await;
        }
    }
    Ok(result?)
}

/// Accepts an invitation as an account (`as: "user"`) or as a guest (`as: "guest"`). Order: body (400), session (401 for
/// `user`, 409 for a signed-in `guest`), the lookup pool (429), then the join (409, 410).
pub async fn accept_invitation(
    State(state): State<AppState>,
    Path(code): Path<String>,
    headers: HeaderMap,
    jar: CookieJar,
    body: Body,
) -> Result<(CookieJar, Response), ApiError> {
    let body = read_json(body, &headers).await?;
    let join = string_field(&body, "as");
    let mut fields: Vec<(&str, Vec<FieldIssue>)> = Vec::new();
    let nickname = match string_field(&body, "nickname").map(validate_nickname) {
        None => None,
        Some(Ok(nick)) => nick,
        Some(Err(error)) => {
            fields.push(("nickname", vec![nickname_issue(error)]));
            None
        }
    };
    match join {
        Some("user") if nickname.is_some() => {
            fields.push(("nickname", vec![FieldIssue::NicknameGuestOnly]))
        }
        Some("guest") if bool_field(&body, "acknowledgedGuestWarning") != Some(true) => {
            fields.push((
                "acknowledgedGuestWarning",
                vec![FieldIssue::GuestWarningRequired],
            ));
        }
        Some("user" | "guest") => {}
        _ => fields.insert(0, ("as", vec![FieldIssue::JoinAsRequired])),
    }
    if !fields.is_empty() {
        return Err(ApiError::invalid_fields(fields));
    }
    let secure = state.config.session_cookie_secure;
    let user = current_user(&state, &jar).await;
    let cancel_own = bool_field(&body, "cancelOwnWaitingGame") == Some(true);
    let now = (state.clock)();
    let black_seat = draw_black_seat(random_int);
    let new_token = new_opaque_token();
    let new_hash = hash_opaque_token(&new_token);
    let cookie_hash = guest_token_hash(&jar, secure);

    let join_as = match (join, &user) {
        (Some("user"), None) => {
            return Err(ApiError::new(
                StatusCode::UNAUTHORIZED,
                "UNAUTHORIZED",
                "Zaloguj się, aby dołączyć do partii.",
            )
            .with_data(json!({ "reason": "join_sign_in" })));
        }
        (Some("user"), Some(user)) => JoinAs::Account(user.id),
        (_, Some(_)) => {
            return Err(ApiError::conflict(
                "GUEST_NOT_ALLOWED",
                "Jesteś zalogowany — dołącz jako on sam. Gra bez konta jest dla niezalogowanych.",
            ));
        }
        _ => JoinAs::Guest {
            cookie_hash: cookie_hash.as_deref(),
            new_hash: &new_hash,
            nickname: nickname.as_deref(),
        },
    };
    let input = AcceptInput {
        join_as,
        cancel_own_waiting_game: cancel_own,
        black_seat,
    };
    // A malformed code is a failed lookup like any other (counted, uniform answer).
    let outcome = with_lookup_attempt(&state, &headers, async {
        let normalized = normalize_invitation_code(&code).ok_or(GameError::InvalidInvitation)?;
        state.db.accept_invitation(&normalized, &input, now).await
    })
    .await?;
    let jar = if outcome.new_guest_seat {
        jar.add(guest_cookie_default(secure, new_token))
    } else {
        jar
    };
    Ok((
        jar,
        ok(StatusCode::OK, json!({ "gameId": outcome.game_id })),
    ))
}
