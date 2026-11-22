//! Pages of the game: play (guest), join, invitation, game room, lobby, history, review.

use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::CookieJar;
use gomoku_core::auth::safe_next_path;
use gomoku_core::constants::INITIAL_CLOCK_MS;
use gomoku_core::game::BoardRow;
use gomoku_core::invitation_code::{format_invitation_code, normalize_invitation_code};
use gomoku_core::types::{GameVariant, Seat};
use gomoku_db::Uuid;
use gomoku_db::games::{GameError, PreviewState, Viewer};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::app::AppState;
use crate::game_view::{game_view, iso_ms, spectator_view};
use crate::guest::guest_token_hash;
use crate::i18n::Lang;
use crate::pages::{Ctx, no_referrer, render};
use crate::routes_chat::resolve_spectator;
use crate::routes_games::{origin_of, resolve_participant, with_lookup_attempt};
use crate::routes_lobby::{fetch_history, lobby_json};
use crate::session::{current_user, require_page_session};

/// JSON for a `<script type="application/json">` block: no `<` can end the element.
pub(crate) fn script_json(value: &Value) -> String {
    value.to_string().replace('<', "\\u003c")
}

#[derive(askama::Template)]
#[template(path = "play.html")]
struct PlayPage {
    ctx: Ctx,
    signed_in: bool,
    hint: String,
    bot_enabled: bool,
}

#[derive(askama::Template)]
#[template(path = "join.html")]
struct JoinPage {
    ctx: Ctx,
    hint: String,
}

#[derive(askama::Template)]
#[template(path = "invite.html")]
struct InvitePage {
    ctx: Ctx,
    /// `invalid`, `limit`, `joinable`, `has-active-game`, `has-own-waiting-game` or `watch`.
    state: &'static str,
    /// `visitor`, `account` or `guest`.
    viewer: &'static str,
    code: String,
    heading: String,
    code_line: String,
    game_id: String,
    next: String,
    hint: String,
    is_public: bool,
}

#[derive(askama::Template)]
#[template(path = "game.html")]
struct GamePage {
    ctx: Ctx,
    game_id: String,
    initial: String,
    secure: bool,
}

#[derive(askama::Template)]
#[template(path = "gate.html")]
struct GatePage {
    ctx: Ctx,
}

#[derive(askama::Template)]
#[template(path = "lobby.html")]
struct LobbyPage {
    ctx: Ctx,
    initial: String,
}

#[derive(askama::Template)]
#[template(path = "history.html")]
struct HistoryPage {
    ctx: Ctx,
    initial: String,
}

#[derive(askama::Template)]
#[template(path = "review.html")]
struct ReviewPage {
    ctx: Ctx,
    data: String,
    /// Opened with the invitation code: the back link goes home, not to the history.
    by_code: bool,
}

fn gate(
    state: &AppState,
    locale: gomoku_core::i18n::Locale,
    user: Option<crate::session::CurrentUser>,
) -> Response {
    let ctx = Ctx::new(state.catalogs.translator(locale), user, "/");
    let mut response = render(&GatePage { ctx });
    *response.status_mut() = axum::http::StatusCode::NOT_FOUND;
    response
}

/// The guest form: a person without an account creates a game for a friend.
pub async fn play(State(state): State<AppState>, Lang(locale): Lang, jar: CookieJar) -> Response {
    let user = current_user(&state, &jar).await;
    let signed_in = user.is_some();
    let tr = state.catalogs.translator(locale);
    let hint = tr.t_with("home.shared.nicknameHint", &[("seat", "1")]);
    render(&PlayPage {
        ctx: Ctx::new(tr, user, "/play").with_bag(&[
            "errors.api.",
            "home.settings.",
            "home.opponent.",
        ]),
        signed_in,
        hint,
        bot_enabled: state.bot.config.enabled,
    })
}

/// Entering an invitation code.
pub async fn join(State(state): State<AppState>, Lang(locale): Lang, jar: CookieJar) -> Response {
    let user = current_user(&state, &jar).await;
    let tr = state.catalogs.translator(locale);
    let hint = tr.t_with("home.join.hint", &[("length", "8")]);
    render(&JoinPage {
        ctx: Ctx::new(tr, user, "/join").with_bag(&["home.join."]),
        hint,
    })
}

/// The page behind an invitation link. It consumes nothing (link previewers open it) and shares the pool of failed
/// lookups with accepting. Whoever already plays the game goes straight to it.
pub async fn invite(
    State(state): State<AppState>,
    Lang(locale): Lang,
    Path(raw_code): Path<String>,
    headers: axum::http::HeaderMap,
    jar: CookieJar,
) -> Response {
    let user = current_user(&state, &jar).await;
    let tr = state.catalogs.translator(locale);
    let secure = state.config.session_cookie_secure;
    let guest_hash = guest_token_hash(&jar, secure);
    let viewer = match (&user, guest_hash.as_deref()) {
        (Some(user), _) => Viewer::Account(user.id),
        (None, Some(hash)) => Viewer::Guest(hash),
        (None, None) => Viewer::Anonymous,
    };
    let viewer_kind = match viewer {
        Viewer::Account(_) => "account",
        Viewer::Guest(_) => "guest",
        Viewer::Anonymous => "visitor",
    };
    let normalized = normalize_invitation_code(&raw_code);
    let code_display = normalized
        .as_deref()
        .map_or_else(|| raw_code.clone(), format_invitation_code);
    let now = (state.clock)();
    let outcome = with_lookup_attempt(&state, &headers, async {
        let code = normalized.as_deref().ok_or(GameError::InvalidInvitation)?;
        state.db.preview_invitation(code, viewer, now).await
    })
    .await;
    let ctx = Ctx::new(tr.clone(), user, "/").with_bag(&["errors.api."]);
    let page = |state_name: &'static str, heading: String, game_id: String| InvitePage {
        ctx: ctx.clone(),
        state: state_name,
        viewer: viewer_kind,
        code: code_display.clone(),
        heading,
        code_line: String::new(),
        game_id,
        next: safe_next_path(Some(&format!("/invite/{code_display}"))),
        hint: tr.t_with("home.shared.nicknameHint", &[("seat", "2")]),
        is_public: false,
    };
    let response = match outcome {
        Err(error) if error.status() == axum::http::StatusCode::TOO_MANY_REQUESTS => render(&page(
            "limit",
            tr.t("home.invite.heading.limit"),
            String::new(),
        )),
        Err(error) if error.status() == axum::http::StatusCode::GONE => render(&page(
            "invalid",
            tr.t("home.invite.heading.invalid"),
            String::new(),
        )),
        Err(error) => error.into_response(),
        Ok(view) => {
            let heading = match (&view.creator_name, view.creator_is_guest) {
                (None, _) => tr.t("home.invite.heading.someone"),
                (Some(name), true) => tr.t_with(
                    "home.invite.heading.guestCreator",
                    &[("name", &guest_name(&tr, name, Seat::One))],
                ),
                (Some(name), false) => tr.t_with("home.invite.heading.creator", &[("name", name)]),
            };
            let rules = match view.settings.variant {
                GameVariant::Paper => {
                    let line = tr.t_with(
                        "home.settings.line",
                        &[
                            ("variant", &tr.t("home.settings.variants.paper")),
                            (
                                "minutes",
                                &(view.settings.time_control_ms / 60_000).to_string(),
                            ),
                        ],
                    );
                    tr.t_with("home.settings.paperRules", &[("settings", &line)])
                }
                GameVariant::Classic => tr.t_plural(
                    "home.settings.classicRules",
                    u64::try_from(INITIAL_CLOCK_MS / 60_000).unwrap_or(10),
                ),
            };
            let code_line = tr.t_with(
                "home.invite.codeLine",
                &[("code", &code_display), ("rules", &rules)],
            );
            let make = |name: &'static str, game_id: String| {
                let mut p = page(name, heading.clone(), game_id);
                p.code_line = code_line.clone();
                p.is_public = view.is_public;
                p
            };
            match view.state {
                PreviewState::OwnInvitation { game_id } | PreviewState::Participant { game_id } => {
                    return Redirect::to(&format!("/games/{game_id}")).into_response();
                }
                PreviewState::Joinable => render(&make("joinable", String::new())),
                PreviewState::HasActiveGame { game_id } => {
                    render(&make("has-active-game", game_id.to_string()))
                }
                PreviewState::HasOwnWaitingGame { game_id } => {
                    render(&make("has-own-waiting-game", game_id.to_string()))
                }
                PreviewState::Watchable { game_id } => {
                    let mut watch = make("watch", game_id.to_string());
                    watch.heading = tr.t("home.invite.heading.watch");
                    render(&watch)
                }
                PreviewState::Reviewable { game_id } => {
                    let code = normalized.as_deref().unwrap_or_default();
                    return Redirect::to(&format!("/games/{game_id}/review?code={code}"))
                        .into_response();
                }
            }
        }
    };
    no_referrer(response)
}

/// A guest's default name ("Gracz 1") is translated for the reader; a chosen nickname stays as it is.
fn guest_name(tr: &crate::i18n::Translator, name: &str, seat: Seat) -> String {
    if gomoku_core::nickname::default_nickname_seat(name) == Some(seat) {
        tr.t_with(
            "common.player.guestDefault",
            &[("seat", &seat.number().to_string())],
        )
    } else {
        name.to_owned()
    }
}

#[derive(Deserialize, Default)]
pub struct GameQuery {
    notice: Option<String>,
}

/// The game room: the first view is rendered here (and counts as a read: it marks the player as having seen the game),
/// the script then polls.
pub async fn game_room(
    State(state): State<AppState>,
    Lang(locale): Lang,
    Path(id): Path<String>,
    Query(query): Query<GameQuery>,
    jar: CookieJar,
) -> Response {
    let user = current_user(&state, &jar).await;
    let participant = match resolve_participant(&state, &jar, &id).await {
        Ok(p) => p,
        Err(error) if error.status() == axum::http::StatusCode::NOT_FOUND => {
            return spectator_room(&state, locale, user, &jar, &id).await;
        }
        Err(error) => return error.into_response(),
    };
    let now = (state.clock)();
    let snapshot = match state
        .db
        .game_state(participant.player_id, participant.game_id, 0, now)
        .await
    {
        Ok(snapshot) => snapshot,
        Err(GameError::NotFound) => return gate(&state, locale, user),
        Err(error) => return crate::api::ApiError::from(error).into_response(),
    };
    let view = match game_view(&snapshot, 0, now, origin_of(&state)) {
        Ok(view) => view,
        Err(error) => return crate::api::ApiError::from(error).into_response(),
    };
    let initial = json!({
        "gameId": participant.game_id,
        "view": view,
        "notice": query.notice.as_deref().filter(|n| *n == "own-waiting"),
        "received": gomoku_db::games::to_ms(now),
    });
    let ctx = Ctx::new(state.catalogs.translator(locale), user, "/").with_bag(&[
        "game.",
        "chat.",
        "common.",
        "home.shared.",
        "errors.api.",
        "errors.field.",
    ]);
    no_referrer(render(&GamePage {
        ctx,
        game_id: participant.game_id.to_string(),
        initial: script_json(&initial),
        secure: state.config.session_cookie_secure,
    }))
}

#[derive(askama::Template)]
#[template(path = "spectate.html")]
struct SpectatePage {
    ctx: Ctx,
    game_id: String,
    initial: String,
}

#[derive(askama::Template)]
#[template(path = "audience_closed.html")]
struct AudienceClosedPage {
    ctx: Ctx,
}

/// The game page for somebody who does not play it: a spectator of a public game gets the read-only room (this read
/// slides their access), the closed audience gets its own screen, everybody else the not-found page.
async fn spectator_room(
    state: &AppState,
    locale: gomoku_core::i18n::Locale,
    user: Option<crate::session::CurrentUser>,
    jar: &CookieJar,
    id: &str,
) -> Response {
    let access = match resolve_spectator(state, jar, id).await {
        Ok(access) => access,
        Err(error) if error.status() == axum::http::StatusCode::NOT_FOUND => {
            return gate(state, locale, user);
        }
        Err(error) => return error.into_response(),
    };
    let now = (state.clock)();
    let ctx = Ctx::new(state.catalogs.translator(locale), user, "/");
    let read = match state
        .db
        .spectator_state(access.game_id, access.spectator_id, 0, now)
        .await
    {
        Ok(read) => read,
        Err(GameError::AudienceClosed) => {
            return no_referrer(render(&AudienceClosedPage {
                ctx: ctx.with_bag(&["game.audience."]),
            }));
        }
        Err(GameError::NotFound) => return gate(state, locale, ctx.user.clone()),
        Err(error) => return crate::api::ApiError::from(error).into_response(),
    };
    let view = match spectator_view(&read, 0, now) {
        Ok(view) => view,
        Err(error) => return crate::api::ApiError::from(error).into_response(),
    };
    let initial = json!({ "gameId": access.game_id, "view": view });
    let ctx = ctx.with_bag(&[
        "game.",
        "chat.",
        "common.",
        "home.shared.",
        "errors.api.",
        "errors.field.",
    ]);
    no_referrer(render(&SpectatePage {
        ctx,
        game_id: access.game_id.to_string(),
        initial: script_json(&initial),
    }))
}

/// The lobby; the first view is rendered here.
pub async fn lobby(State(state): State<AppState>, Lang(locale): Lang, jar: CookieJar) -> Response {
    let user = match require_page_session(&state, &jar, "/lobby").await {
        Ok(user) => user,
        Err(redirect) => return redirect.into_response(),
    };
    let view = state
        .db
        .lobby_view(
            user.id,
            (state.clock)(),
            gomoku_core::lobby::LOBBY_LIST_LIMIT as i64,
        )
        .await;
    let initial = match view {
        Ok(view) => lobby_json(&view),
        Err(error) => return crate::api::ApiError::from(error).into_response(),
    };
    let ctx = Ctx::new(state.catalogs.translator(locale), Some(user), "/lobby").with_bag(&[
        "home.shared.",
        "common.",
        "errors.api.",
    ]);
    let ctx = Ctx {
        bag: merge_bag(&ctx, &["lobby."]),
        ..ctx
    };
    render(&LobbyPage {
        ctx,
        initial: script_json(&initial),
    })
}

fn merge_bag(ctx: &Ctx, extra: &[&str]) -> String {
    let mut prefixes = vec!["home.shared.", "home.settings.", "common.", "errors.api."];
    prefixes.extend_from_slice(extra);
    ctx.tr.bag_json(&prefixes)
}

/// The history: the first page is rendered here, older ones are fetched by the script.
pub async fn history(
    State(state): State<AppState>,
    Lang(locale): Lang,
    jar: CookieJar,
) -> Response {
    let user = match require_page_session(&state, &jar, "/history").await {
        Ok(user) => user,
        Err(redirect) => return redirect.into_response(),
    };
    let page = match fetch_history(&state, user.id, None).await {
        Ok(page) => page,
        Err(error) => return crate::api::ApiError::from(error).into_response(),
    };
    let ctx = Ctx::new(state.catalogs.translator(locale), Some(user), "/history");
    let ctx = Ctx {
        bag: merge_bag(
            &ctx,
            &["common.", "history.", "game.outcome.", "game.shared."],
        ),
        ..ctx
    };
    render(&HistoryPage {
        ctx,
        initial: script_json(&page),
    })
}

/// A finished game, step by step: for one of its account players, or for anybody with the code of a public game
/// (`?code=`, given exactly once). Everything else is the same not-found page.
pub async fn review(
    State(state): State<AppState>,
    Lang(locale): Lang,
    Path(id): Path<String>,
    Query(query): Query<Vec<(String, String)>>,
    jar: CookieJar,
) -> Response {
    let codes: Vec<&String> = query
        .iter()
        .filter(|(key, _)| key == "code")
        .map(|(_, value)| value)
        .collect();
    let by_code = !codes.is_empty();
    let user = if by_code {
        // A reader with a code needs no session and reads none: they are nobody in the game.
        None
    } else {
        match require_page_session(&state, &jar, &format!("/games/{id}/review")).await {
            Ok(user) => Some(user),
            Err(redirect) => return redirect.into_response(),
        }
    };
    let not_found = || {
        let ctx = Ctx::new(state.catalogs.translator(locale), None, "/");
        let mut response = render(&ReviewNotFound { ctx });
        *response.status_mut() = axum::http::StatusCode::NOT_FOUND;
        no_referrer(response)
    };
    let Ok(game_id) = Uuid::parse_str(&id) else {
        return not_found();
    };
    let opened = match (&user, codes.as_slice()) {
        (Some(user), _) => state.db.open_review(user.id, game_id).await,
        (None, [code]) => match normalize_invitation_code(code) {
            Some(code) => state.db.open_review_by_code(game_id, &code).await,
            None => return not_found(),
        },
        (None, _) => return not_found(),
    };
    let rows = match opened {
        Ok(Some(rows)) => rows,
        Ok(None) => return not_found(),
        Err(error) => return crate::api::ApiError::from(error).into_response(),
    };
    let paper = rows.game.variant == gomoku_core::types::GameVariant::Paper;
    let (moves, result) = if paper {
        // A paper game has no move log to step through; the result is what the review shows.
        let result = rows.game.end_reason.map(|reason| {
            json!({ "winnerSeat": rows.game.winner.map(Seat::number), "reason": reason.as_str(), "winLine": null })
        });
        (json!([]), result)
    } else {
        let board = gomoku_core::game::board_view(
            &BoardRow {
                status: rows.game.status,
                black_seat: rows.game.black_seat,
                move_count: rows.game.move_count,
                moves: &rows.game.moves,
                winner: rows.game.winner,
                end_reason: rows.game.end_reason,
            },
            0,
        );
        let Ok(board) = board else {
            return crate::api::ApiError::from(GameError::Corrupt).into_response();
        };
        let result = board.result.as_ref().map(|r| {
            json!({ "winnerSeat": r.winner.map(Seat::number), "reason": r.reason.as_str(), "winLine": r.win_line })
        });
        (json!(board.moves), result)
    };
    let players: Vec<Value> = rows
        .players
        .iter()
        .map(|p| {
            let mut player = json!({
                "seat": p.seat.number(),
                "kind": p.kind,
                "displayName": gomoku_core::game::display_name_of(p.is_guest(), p.seat, p.nickname.as_deref(), p.display_name.as_deref(), p.current_name.as_deref()),
            });
            if let Some(bot) = crate::game_view::bot_json(p) {
                player["displayName"] = bot["name"].clone();
                player["bot"] = bot["bot"].clone();
            }
            if p.kind == "agent" {
                player["agentName"] = json!(p.agent_name);
            }
            player
        })
        .collect();
    let data = json!({
        "gameId": rows.game.id,
        "finishedAt": rows.game.finished_at.map(iso_ms),
        "blackSeat": rows.game.black_seat.map(Seat::number),
        "variant": rows.game.variant.as_str(),
        "moves": moves,
        "result": result,
        "players": players,
        "viewerSeat": rows.viewer_seat.map(Seat::number),
    });
    let back = if by_code { "/" } else { "/history" };
    let ctx = Ctx::new(state.catalogs.translator(locale), user, back);
    let ctx = Ctx {
        bag: merge_bag(
            &ctx,
            &[
                "common.",
                "review.",
                "history.reason.",
                "game.outcome.",
                "game.sides.",
                "game.board.",
                "game.shared.",
            ],
        ),
        ..ctx
    };
    no_referrer(render(&ReviewPage {
        ctx,
        data: script_json(&data),
        by_code,
    }))
}

#[derive(askama::Template)]
#[template(path = "review_not_found.html")]
struct ReviewNotFound {
    ctx: Ctx,
}
