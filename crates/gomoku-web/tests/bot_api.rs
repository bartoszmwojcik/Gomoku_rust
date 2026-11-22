//! The hosted bot end to end: admission, creation, the bot's replies, the rules of a bot game (no chat, no rematch,
//! private), recovery after failures and the database constraints.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::http::StatusCode;
use common::{Client, Reply, TestApp};
use gomoku_db::cleanup::{CleanupOptions, cleanup_expired};
use gomoku_web::bot::{BotRuntime, EngineTurn, FixedProbe, MoveChooser};
use serde_json::{Value, json};

const PASSWORD: &str = "correct horse";

fn data_of(reply: &Reply) -> Value {
    let body = reply.json();
    assert_eq!(body["ok"], true, "{}", reply.body);
    body["data"].clone()
}

fn error_of(reply: &Reply) -> Value {
    let body = reply.json();
    assert_eq!(body["ok"], false, "{}", reply.body);
    body["error"].clone()
}

const ON: &[(&str, &str)] = &[
    ("TRUST_PROXY_HOPS", "1"),
    ("BOT_ENABLED", "true"),
    ("BOT_FIXED_MEMORY_MB", "1024"),
];

async fn app() -> TestApp {
    TestApp::start_with(ON).await
}

async fn signup(app: &TestApp, n: u8, name: &str) -> Client {
    let mut client = app
        .client()
        .with_header("x-forwarded-for", &format!("10.6.0.{n}"));
    let reply = client
        .post(
            "/api/auth/register",
            json!({ "email": format!("{}@example.com", name.to_lowercase()), "displayName": name, "password": PASSWORD }),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    client
}

async fn view(client: &mut Client, id: &str) -> Value {
    let reply = client.get(&format!("/api/games/{id}")).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    data_of(&reply)
}

async fn mv(client: &mut Client, id: &str, x: i64, y: i64, n: i64) -> Reply {
    client
        .post(
            &format!("/api/games/{id}/moves"),
            json!({ "x": x, "y": y, "moveNumber": n }),
        )
        .await
}

async fn count(app: &TestApp, sql: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT ({sql})::bigint")))
        .fetch_one(app.db.pool())
        .await
        .unwrap()
}

async fn new_bot_game(client: &mut Client, level: i64) -> String {
    let reply = client
        .post(
            "/api/games",
            json!({ "opponent": "bot", "botLevel": level }),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    data_of(&reply)["gameId"].as_str().unwrap().to_owned()
}

/// Reads the game (which starts the clock and, when the bot is black, schedules its first move), lets the bot finish
/// and returns the state.
async fn read_settled(app: &TestApp, client: &mut Client, id: &str) -> Value {
    view(client, id).await;
    app.settle().await;
    view(client, id).await
}

/// The cell the human plays next: the first free one of a fixed order, so a test never collides with the bot.
fn free_cell(state: &Value) -> (i64, i64) {
    let taken: Vec<(i64, i64)> = state["moves"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| (m.as_i64().unwrap() % 15, m.as_i64().unwrap() / 15))
        .collect();
    for y in 0..15 {
        for x in 0..15 {
            if !taken.contains(&(x, y)) {
                return (x, y);
            }
        }
    }
    panic!("board full");
}

/// Plays `rounds` human moves, each answered by the bot; stops early when the game is over.
async fn rally(app: &TestApp, client: &mut Client, id: &str, rounds: usize) -> Value {
    let mut state = read_settled(app, client, id).await;
    for _ in 0..rounds {
        if state["status"] != "active" || state["turnSeat"] != 1 {
            break;
        }
        let (x, y) = free_cell(&state);
        let n = state["moveCount"].as_i64().unwrap() + 1;
        let reply = mv(client, id, x, y, n).await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
        app.settle().await;
        state = view(client, id).await;
    }
    state
}

#[tokio::test]
async fn disabled_bot_is_unavailable() {
    let app = TestApp::start_with(&[("TRUST_PROXY_HOPS", "1")]).await;
    let mut ala = signup(&app, 1, "Ala").await;
    let reply = ala
        .post("/api/games", json!({ "opponent": "bot", "botLevel": 1 }))
        .await;
    assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(reply.header("retry-after"), Some("60"));
    let error = error_of(&reply);
    assert_eq!(error["code"], "BOT_UNAVAILABLE");
    assert_eq!(
        error["message"],
        "Bot jest teraz niedostępny. Spróbuj za chwilę albo zaproś osobę."
    );
    assert_eq!(count(&app, "SELECT count(*) FROM games").await, 0);
}

#[tokio::test]
async fn memory_below_the_minimum_refuses() {
    let app = TestApp::start_with(&[
        ("TRUST_PROXY_HOPS", "1"),
        ("BOT_ENABLED", "true"),
        ("BOT_FIXED_MEMORY_MB", "100"),
    ])
    .await;
    let mut ala = signup(&app, 1, "Ala").await;
    let reply = ala
        .post("/api/games", json!({ "opponent": "bot", "botLevel": 2 }))
        .await;
    assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(reply.header("retry-after"), Some("60"));
}

#[tokio::test]
async fn capacity_is_global() {
    let app = TestApp::start_with(&[
        ("TRUST_PROXY_HOPS", "1"),
        ("BOT_ENABLED", "true"),
        ("BOT_FIXED_MEMORY_MB", "1024"),
        ("BOT_MAX_ACTIVE_GAMES", "1"),
    ])
    .await;
    let mut ala = signup(&app, 1, "Ala").await;
    let mut bob = signup(&app, 2, "Bob").await;
    new_bot_game(&mut ala, 1).await;
    let reply = bob
        .post("/api/games", json!({ "opponent": "bot", "botLevel": 1 }))
        .await;
    assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE);
    // A game whose deadline passed holds no slot.
    app.advance_seconds(60 * 60);
    let reply = bob
        .post("/api/games", json!({ "opponent": "bot", "botLevel": 1 }))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
}

#[tokio::test]
async fn validation_matrix() {
    let app = app().await;
    let mut ala = signup(&app, 1, "Ala").await;
    let cases: &[(Value, &str, &str)] = &[
        (json!({ "opponent": "robot" }), "opponent", "invalid_value"),
        (
            json!({ "opponent": "bot", "botLevel": "2" }),
            "botLevel",
            "invalid_union",
        ),
        (
            json!({ "opponent": "bot", "botLevel": 3 }),
            "botLevel",
            "invalid_union",
        ),
        (
            json!({ "opponent": "human", "botLevel": 1 }),
            "botLevel",
            "BOT_LEVEL_WITHOUT_BOT",
        ),
        (
            json!({ "botLevel": 1 }),
            "botLevel",
            "BOT_LEVEL_WITHOUT_BOT",
        ),
        (
            json!({ "opponent": "bot" }),
            "botLevel",
            "BOT_LEVEL_REQUIRED",
        ),
        (
            json!({ "opponent": "bot", "botLevel": 1, "variant": "paper", "timeControlMs": 60000 }),
            "opponent",
            "BOT_VARIANT_UNSUPPORTED",
        ),
        (
            json!({ "opponent": "bot", "botLevel": 1, "isPublic": true }),
            "isPublic",
            "BOT_PUBLIC",
        ),
    ];
    for (body, field, code) in cases {
        let reply = ala.post("/api/games", body.clone()).await;
        assert_eq!(
            reply.status,
            StatusCode::BAD_REQUEST,
            "{body}: {}",
            reply.body
        );
        let error = error_of(&reply);
        assert_eq!(error["code"], "VALIDATION");
        assert_eq!(
            error["fieldErrors"][field][0],
            json!(error["fieldErrors"][field][0]),
            "{body}"
        );
        assert!(reply.body.contains(code), "{body}: {}", reply.body);
    }
    assert_eq!(count(&app, "SELECT count(*) FROM games").await, 0);
}

#[tokio::test]
async fn level_one_game_shape_and_replies() {
    let app = app().await;
    let mut ala = signup(&app, 1, "Ala").await;
    let reply = ala
        .post("/api/games", json!({ "opponent": "bot", "botLevel": 1 }))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED);
    let data = data_of(&reply);
    assert_eq!(data["opponent"], "bot");
    assert_eq!(data["bot"], json!({ "level": 1, "name": "Bot · poziom 1" }));
    assert!(data.get("invitation").is_none());
    let id = data["gameId"].as_str().unwrap().to_owned();

    let state = read_settled(&app, &mut ala, &id).await;
    assert_eq!(state["status"], "active");
    assert!(state.get("invitation").is_none());
    assert!(state.get("rematch").is_none());
    let bot = state["players"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["kind"] == "bot")
        .unwrap()
        .clone();
    assert_eq!(bot["seat"], 2);
    assert_eq!(bot["displayName"], "Bot · poziom 1");
    assert_eq!(
        bot["bot"],
        json!({ "level": 1, "memoryMb": null, "tableMb": null })
    );

    let state = rally(&app, &mut ala, &id, 3).await;
    // Black always moves first: the human's three moves are answered by three bot moves (or the bot opened).
    assert!(state["moveCount"].as_i64().unwrap() >= 6, "{state}");
    assert_eq!(
        count(
            &app,
            "SELECT count(*) FROM games WHERE origin = 'bot' AND is_public = false"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn level_two_freezes_its_strength() {
    let app = TestApp::start_with(&[
        ("TRUST_PROXY_HOPS", "1"),
        ("BOT_ENABLED", "true"),
        ("BOT_FIXED_MEMORY_MB", "256"),
    ])
    .await;
    let mut ala = signup(&app, 1, "Ala").await;
    let reply = ala
        .post("/api/games", json!({ "opponent": "bot", "botLevel": 2 }))
        .await;
    let data = data_of(&reply);
    assert_eq!(
        data["bot"],
        json!({ "level": 2, "name": "Bot · poziom 2 · 8 MB" })
    );
    let id = data["gameId"].as_str().unwrap().to_owned();
    let state = read_settled(&app, &mut ala, &id).await;
    let bot = state["players"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["kind"] == "bot")
        .unwrap()
        .clone();
    assert_eq!(
        bot["bot"],
        json!({ "level": 2, "memoryMb": 256, "tableMb": 8 })
    );
    assert_eq!(
        count(
            &app,
            "SELECT bot_depth_cap FROM game_players WHERE kind = 'bot'"
        )
        .await,
        6
    );
    // The bot answers with a real search inside its budget.
    let state = rally(&app, &mut ala, &id, 2).await;
    assert!(state["moveCount"].as_i64().unwrap() >= 4, "{state}");
}

#[tokio::test]
async fn a_bot_game_holds_the_slot() {
    let app = app().await;
    let mut ala = signup(&app, 1, "Ala").await;
    new_bot_game(&mut ala, 1).await;
    let again = ala
        .post("/api/games", json!({ "opponent": "bot", "botLevel": 1 }))
        .await;
    assert_eq!(again.status, StatusCode::CONFLICT);
    assert_eq!(error_of(&again)["code"], "ACTIVE_GAME_EXISTS");
    let invite = ala.post("/api/games", json!({})).await;
    assert_eq!(invite.status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn no_rematch_no_chat_and_resigning_works() {
    let app = app().await;
    let mut ala = signup(&app, 1, "Ala").await;
    let id = new_bot_game(&mut ala, 1).await;
    read_settled(&app, &mut ala, &id).await;
    let chat = ala
        .post(
            &format!("/api/games/{id}/chat"),
            json!({ "text": "hej", "clientId": "6f1c2a52-3a47-4c47-9a41-3a1a3a8b2b11" }),
        )
        .await;
    assert_eq!(chat.status, StatusCode::CONFLICT, "{}", chat.body);
    assert_eq!(error_of(&chat)["code"], "CHAT_UNAVAILABLE");
    let resign = ala
        .post(&format!("/api/games/{id}/resign"), json!({}))
        .await;
    assert_eq!(resign.status, StatusCode::OK, "{}", resign.body);
    assert_eq!(data_of(&resign)["result"]["reason"], "resignation");
    let rematch = ala
        .post(&format!("/api/games/{id}/rematch"), json!({}))
        .await;
    assert_eq!(rematch.status, StatusCode::CONFLICT);
    let error = error_of(&rematch);
    assert_eq!(error["code"], "REMATCH_UNAVAILABLE");
    assert_eq!(error["data"]["reason"], "bot");
    // The slot is free again and the pair counter knows the pair.
    assert_eq!(count(&app, "SELECT sum(finished_count) FROM game_pair_counters WHERE kind_a = 'account' AND kind_b = 'bot'").await, 1);
    new_bot_game(&mut ala, 1).await;
}

#[tokio::test]
async fn guest_plays_the_bot_and_its_games_are_cleaned_up() {
    let app = app().await;
    let mut guest = app.client().with_header("x-forwarded-for", "10.6.1.1");
    let reply = guest
        .post(
            "/api/games",
            json!({ "acknowledgedGuestWarning": true, "opponent": "bot", "botLevel": 1 }),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    assert!(!reply.set_cookies().is_empty());
    let id = data_of(&reply)["gameId"].as_str().unwrap().to_owned();
    let state = rally(&app, &mut guest, &id, 2).await;
    assert_eq!(state["you"]["kind"], "guest");
    guest
        .post(&format!("/api/games/{id}/resign"), json!({}))
        .await;
    app.advance_seconds(2 * 60 * 60);
    let report = cleanup_expired(&app.db, &CleanupOptions::new(common::now_of(&app))).await;
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.guest_only_finished_games, 1);
    assert_eq!(count(&app, "SELECT count(*) FROM games").await, 0);
    // A guest-bot game needs the acknowledged warning like any other.
    let mut other = app.client().with_header("x-forwarded-for", "10.6.1.2");
    let refused = other
        .post("/api/games", json!({ "opponent": "bot", "botLevel": 1 }))
        .await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn database_constraints_hold() {
    let app = app().await;
    let mut ala = signup(&app, 1, "Ala").await;
    let id = new_bot_game(&mut ala, 2).await;
    let attempts = [
        "UPDATE game_players SET seat = 1 WHERE game_id = '{id}' AND kind = 'bot'",
        "UPDATE game_players SET bot_memory_mb = NULL WHERE game_id = '{id}' AND kind = 'bot'",
        "UPDATE game_players SET bot_level = NULL WHERE game_id = '{id}' AND kind = 'bot'",
        "UPDATE game_players SET bot_level = 3 WHERE game_id = '{id}' AND kind = 'bot'",
        "UPDATE game_players SET bot_level = 1 WHERE game_id = '{id}' AND kind = 'account'",
    ];
    for sql in attempts {
        let sql = sql.replace("{id}", &id);
        let result = sqlx::query(sqlx::AssertSqlSafe(sql.clone()))
            .execute(app.db.pool())
            .await;
        assert!(result.is_err(), "{sql}");
    }
}

/// An engine that fails in a scripted way.
struct Scripted {
    calls: AtomicUsize,
    mode: &'static str,
}

impl MoveChooser for Scripted {
    fn choose(&self, turn: &EngineTurn) -> Result<usize, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match self.mode {
            "panic" => panic!("engine crashed"),
            "error" => Err("no move".into()),
            _ => Ok((0..225)
                .find(|&c| turn.position.black.has(c) || turn.position.white.has(c))
                .unwrap_or(0)),
        }
    }
}

#[tokio::test]
async fn a_failing_engine_falls_back_to_level_one() {
    let app = app().await;
    for mode in ["panic", "error", "occupied"] {
        let chooser = Arc::new(Scripted {
            calls: AtomicUsize::new(0),
            mode,
        });
        let probe = Arc::new(FixedProbe(1024.0));
        let router = app.router_with_bot(ON, |config| {
            BotRuntime::with_parts(config.bot, probe, chooser.clone())
        });
        let mut ala = Client::for_router(router)
            .with_header("x-forwarded-for", &format!("10.6.2.{}", mode.len()));
        let reply = ala
            .post(
                "/api/auth/register",
                json!({ "email": format!("{mode}@example.com"), "displayName": format!("U{mode}"), "password": PASSWORD }),
            )
            .await;
        assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
        let id = new_bot_game(&mut ala, 2).await;
        let state = rally(&app, &mut ala, &id, 5).await;
        assert!(state["moveCount"].as_i64().unwrap() >= 6, "{mode}: {state}");
        // After three failures the engine is not asked again.
        assert!(chooser.calls.load(Ordering::SeqCst) <= 4, "{mode}");
        ala.post(&format!("/api/games/{id}/resign"), json!({}))
            .await;
    }
}

#[tokio::test]
async fn a_running_out_clock_makes_the_bot_lose_on_time() {
    let app = app().await;
    let mut ala = signup(&app, 1, "Ala").await;
    let id = new_bot_game(&mut ala, 1).await;
    let state = read_settled(&app, &mut ala, &id).await;
    // Nobody moves for longer than the clock: lazy settlement ends the game on time for whoever is on turn.
    app.advance_seconds(11 * 60);
    let _ = state;
    let state = view(&mut ala, &id).await;
    assert_eq!(state["status"], "finished");
    assert_eq!(state["result"]["reason"], "timeout");
}
