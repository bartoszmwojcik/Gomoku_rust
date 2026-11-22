//! The game API: create, state (the poll), moves, resignation, cancel, rematch and accepting an invitation.

use std::collections::HashMap;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum_extra::extract::CookieJar;
use gomoku_core::auth::FieldIssue;
use gomoku_core::bot::choose_move::BotLevel;
use gomoku_core::bot::opponent::{Opponent, OpponentChoice, OpponentIssue, resolve_opponent};
use gomoku_core::bot::profile::bot_display_name;
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
use gomoku_core::paper::{
    Field, GameSettings, PAPER_CELLS, SettingsIssue, is_paper_time_control, resolve_game_settings,
};
use gomoku_core::types::GameVariant;
use gomoku_db::games::{
    AcceptInput, BotSeat, CreatedGame, GameError, GuestCreateInput, JoinAs, MoveInput, Snapshot,
    to_ms,
};
use gomoku_db::{OffsetDateTime, Uuid};
use serde_json::{Value, json};

use crate::api::{
    ApiError, bool_field, int_field, ok, read_json, read_optional_json, string_field,
};
use crate::app::AppState;
use crate::bot::{bot_on_turn, schedule_bot_turn};
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
    let mut invitation = json!({
        "code": format_invitation_code(&created.code),
        "url": invitation_url(origin, &created.code),
        "expiresAt": iso_ms(created.expires_at),
    });
    if let Some(url) = gomoku_core::agent::agent_mcp_url(origin) {
        invitation["mcpUrl"] = json!(url);
    }
    invitation
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

/// `variant` and `timeControlMs` of a create (or lobby) body: unknown keys are ignored, wrong values are field errors, a
/// time without the paper variant is refused only when both fields are well-formed on their own.
pub(crate) fn game_settings(body: &Value) -> Result<GameSettings, ApiError> {
    let variant = match body.get("variant") {
        None => Field::Absent,
        Some(Value::String(text)) => GameVariant::parse(text).map_or(Field::Invalid, Field::Valid),
        Some(_) => Field::Invalid,
    };
    let time = match body.get("timeControlMs") {
        None => Field::Absent,
        Some(value) => value
            .as_i64()
            .filter(|ms| is_paper_time_control(*ms))
            .map_or(Field::Invalid, Field::Valid),
    };
    resolve_game_settings(variant, time).map_err(|issues| {
        let mut error = ApiError::validation("Nieprawidłowe dane.");
        for issue in issues {
            error = match issue {
                SettingsIssue::VariantInvalid => {
                    error.with_field("variant", "invalid_value", "Nieprawidłowy wariant partii.")
                }
                SettingsIssue::TimeControlInvalid => error.with_field(
                    "timeControlMs",
                    "invalid_union",
                    "Nieprawidłowy czas partii.",
                ),
                SettingsIssue::TimeControlClassic => error.with_field(
                    "timeControlMs",
                    "TIME_CONTROL_CLASSIC",
                    "Czas partii można wybrać tylko w wariancie „papier i ołówek”.",
                ),
            };
        }
        error
    })
}

/// `opponent` and `botLevel` of a create body (shape first, then the relations between the fields and the variant).
fn opponent_choice(
    body: &Value,
    settings: &GameSettings,
    is_public: bool,
) -> Result<OpponentChoice, ApiError> {
    let opponent = match body.get("opponent") {
        None => Field::Absent,
        Some(Value::String(text)) => Opponent::parse(text).map_or(Field::Invalid, Field::Valid),
        Some(_) => Field::Invalid,
    };
    let level = match body.get("botLevel") {
        None => Field::Absent,
        Some(value) => value
            .as_i64()
            .and_then(BotLevel::from_number)
            .map_or(Field::Invalid, Field::Valid),
    };
    resolve_opponent(opponent, level, settings.variant, is_public).map_err(|issues| {
        let mut error = ApiError::validation("Nieprawidłowe dane.");
        for issue in issues {
            error = match issue {
                OpponentIssue::OpponentInvalid => {
                    error.with_field("opponent", "invalid_value", "Nieprawidłowy przeciwnik.")
                }
                OpponentIssue::LevelInvalid => {
                    error.with_field("botLevel", "invalid_union", "Nieprawidłowy poziom bota.")
                }
                OpponentIssue::LevelWithoutBot => error.with_field(
                    "botLevel",
                    "BOT_LEVEL_WITHOUT_BOT",
                    "Poziom bota wybiera się tylko w partii z botem.",
                ),
                OpponentIssue::LevelRequired => {
                    error.with_field("botLevel", "BOT_LEVEL_REQUIRED", "Wybierz poziom bota.")
                }
                OpponentIssue::VariantUnsupported => error.with_field(
                    "opponent",
                    "BOT_VARIANT_UNSUPPORTED",
                    "Bot gra tylko w wariancie klasycznym.",
                ),
                OpponentIssue::PublicNotAllowed => error.with_field(
                    "isPublic",
                    "BOT_PUBLIC",
                    "Partia z botem nie może być publiczna.",
                ),
            };
        }
        error
    })
}

/// The bot's seat for a game: strength frozen from the admission, colours drawn.
fn bot_seat(strength: gomoku_core::bot::budget::BotStrength) -> BotSeat {
    BotSeat {
        level: strength.level.number(),
        memory_mb: strength.memory_mb.and_then(|m| i16::try_from(m).ok()),
        depth_cap: strength.depth_cap.and_then(|c| i16::try_from(c).ok()),
        black_seat: draw_black_seat(random_int),
    }
}

fn bot_game_json(game_id: Uuid, seat: &BotSeat) -> Value {
    let level = BotLevel::from_number(i64::from(seat.level)).unwrap_or(BotLevel::One);
    json!({
        "gameId": game_id,
        "opponent": "bot",
        "bot": { "level": seat.level, "name": bot_display_name(level, seat.memory_mb.map(i64::from)) },
    })
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
        let body = read_optional_json(body, &headers).await?;
        let is_public = public_flag(&body)?;
        let settings = game_settings(&body)?;
        let choice = opponent_choice(&body, &settings, is_public)?;
        let strength = match choice {
            OpponentChoice::Bot(level) => Some(state.bot.admit(&state, level).await?),
            OpponentChoice::Human => None,
        };
        rate_limit(
            &state,
            &format!("games-create:{}", user.id),
            CREATE_GAME_LIMIT as i32,
            CREATE_GAME_WINDOW_MS as i64,
        )
        .await?;
        if let Some(strength) = strength {
            let seat = bot_seat(strength);
            let game_id = state.db.create_bot_game_account(user.id, seat, now).await?;
            return Ok((jar, ok(StatusCode::CREATED, bot_game_json(game_id, &seat))));
        }
        let created = state
            .db
            .create_game_account(
                user.id,
                is_public,
                settings,
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
    let settings = game_settings(&body)?;
    let key = crate::client_address::client_key(&headers, "games-create-guest", &state.config)
        .map_err(|_| {
            ApiError::internal_logged("client address", &"unavailable behind the proxy")
        })?;
    let choice = opponent_choice(&body, &settings, is_public)?;
    let strength = match choice {
        OpponentChoice::Bot(level) => Some(state.bot.admit(&state, level).await?),
        OpponentChoice::Human => None,
    };
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
    if let Some(strength) = strength {
        let seat = bot_seat(strength);
        let cancel_own = bool_field(&body, "cancelOwnWaitingGame") == Some(true);
        let game_id = state
            .db
            .create_bot_game_guest(&hash, nickname.as_deref(), cancel_own, seat, now)
            .await?;
        let max_age = guest_cookie_max_age(None, to_ms(now));
        let jar = jar.add(guest_cookie(secure, token, max_age));
        return Ok((jar, ok(StatusCode::CREATED, bot_game_json(game_id, &seat))));
    }
    let input = GuestCreateInput {
        token_hash: &hash,
        nickname: nickname.as_deref(),
        cancel_own_waiting_game: bool_field(&body, "cancelOwnWaitingGame") == Some(true),
        is_public,
        settings,
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
            .ok()
            .filter(|after| *after <= PAPER_CELLS)
            .ok_or_else(|| {
                ApiError::invalid_fields(vec![("after", vec![FieldIssue::AfterInvalid])])
            }),
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
    if bot_on_turn(&snapshot) {
        schedule_bot_turn(&state, participant.game_id);
    }
    let response = view_response(&state, &snapshot, after, now)?;
    Ok((jar, response))
}

/// A move as the wire allows it for either variant (x, y in 0..100, the number in 1..10201); the classic board's own
/// bounds are checked once the game is known.
fn move_input(body: &Value) -> Result<MoveInput, ApiError> {
    let in_range =
        |value: Option<i64>, min: i64, max: i64| value.filter(|v| (min..=max).contains(v));
    let x = in_range(int_field(body, "x"), 0, 100);
    let y = in_range(int_field(body, "y"), 0, 100);
    let number = in_range(int_field(body, "moveNumber"), 1, PAPER_CELLS as i64);
    let mut fields: Vec<(&str, Vec<FieldIssue>)> = Vec::new();
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
    Ok(MoveInput {
        x: i32::try_from(x).map_err(|_| ApiError::internal())?,
        y: i32::try_from(y).map_err(|_| ApiError::internal())?,
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
    if bot_on_turn(&snapshot) {
        schedule_bot_turn(&state, participant.game_id);
    }
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
