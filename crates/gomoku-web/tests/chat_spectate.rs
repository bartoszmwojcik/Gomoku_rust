//! F4 end to end: the chat of a game, public games, spectators, the kill-switch, the QR code and what the cleanup removes.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::http::StatusCode;
use common::{Client, Reply, TestApp};
use gomoku_db::cleanup::{CleanupOptions, cleanup_expired};
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

async fn app_with(overrides: &[(&str, &str)]) -> TestApp {
    let mut all = vec![("TRUST_PROXY_HOPS", "1")];
    all.extend_from_slice(overrides);
    if !overrides
        .iter()
        .any(|(name, _)| *name == "CHAT_MIN_INTERVAL_MS")
    {
        all.push(("CHAT_MIN_INTERVAL_MS", "0"));
    }
    TestApp::start_with(&all).await
}

async fn app() -> TestApp {
    app_with(&[]).await
}

async fn signup(app: &TestApp, n: u8, name: &str) -> Client {
    let mut client = app
        .client()
        .with_header("x-forwarded-for", &format!("10.2.0.{n}"));
    let reply = client
        .post(
            "/api/auth/register",
            json!({ "email": format!("{}@example.com", name.to_lowercase()), "displayName": name, "password": PASSWORD }),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    client
}

fn visitor(app: &TestApp, n: u8) -> Client {
    app.client()
        .with_header("x-forwarded-for", &format!("10.2.1.{n}"))
}

async fn create(client: &mut Client, public: bool) -> (String, String) {
    let reply = client
        .post("/api/games", json!({ "isPublic": public }))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    let data = data_of(&reply);
    (
        data["gameId"].as_str().unwrap().to_owned(),
        data["invitation"]["code"].as_str().unwrap().to_owned(),
    )
}

/// A game in progress between two accounts; both opened it. Returns the id, the code and the two clients.
async fn started(app: &TestApp, public: bool) -> (String, String, Client, Client) {
    let mut ala = signup(app, 1, "Ala").await;
    let mut bob = signup(app, 2, "Bob").await;
    let (id, code) = create(&mut ala, public).await;
    let accepted = bob
        .post(
            &format!("/api/invitations/{code}/accept"),
            json!({ "as": "user" }),
        )
        .await;
    assert_eq!(accepted.status, StatusCode::OK, "{}", accepted.body);
    bob.get(&format!("/api/games/{id}")).await;
    ala.get(&format!("/api/games/{id}")).await;
    (id, code, ala, bob)
}

fn uuid() -> String {
    uuid_like::new()
}

mod uuid_like {
    pub fn new() -> String {
        gomoku_db::Uuid::new_v4().to_string()
    }
}

async fn say(client: &mut Client, id: &str, text: &str) -> Reply {
    client
        .post(
            &format!("/api/games/{id}/chat"),
            json!({ "text": text, "clientId": uuid() }),
        )
        .await
}

async fn chat(client: &mut Client, id: &str) -> Value {
    let reply = client.get(&format!("/api/games/{id}/chat")).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    data_of(&reply)
}

async fn count(app: &TestApp, sql: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_owned()))
        .fetch_one(app.db.pool())
        .await
        .unwrap()
}

async fn execute(app: &TestApp, sql: &str) {
    sqlx::query(sqlx::AssertSqlSafe(sql.to_owned()))
        .execute(app.db.pool())
        .await
        .unwrap();
}

async fn watch(client: &mut Client, code: &str, body: Value) -> Reply {
    client
        .post(&format!("/api/invitations/{code}/spectate"), body)
        .await
}

// ---- the chat of two players ---------------------------------------------------------------------------------------

#[tokio::test]
async fn players_exchange_messages_with_numbered_ids_and_idempotent_sends() {
    let app = app().await;
    let (id, _, mut ala, mut bob) = started(&app, false).await;
    let empty = chat(&mut ala, &id).await;
    assert_eq!(empty["state"], "open");
    assert_eq!(empty["messages"], json!([]));
    assert_eq!(empty["lastId"], 0);
    assert_eq!(
        empty["limits"],
        json!({ "maxLength": 200, "minIntervalMs": 0 })
    );

    let client_id = uuid();
    let body = json!({ "text": "  hej  ", "clientId": client_id });
    let first = ala
        .post(&format!("/api/games/{id}/chat"), body.clone())
        .await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.body);
    let message = data_of(&first)["message"].clone();
    assert_eq!(
        (message["id"].clone(), message["text"].clone()),
        (json!(1), json!("hej"))
    );
    assert!(message["seat"].is_number() && message.get("spectatorName").is_none());
    // The same attempt again: the stored message, no second row.
    let again = ala.post(&format!("/api/games/{id}/chat"), body).await;
    assert_eq!(again.status, StatusCode::OK);
    assert_eq!(data_of(&again)["message"]["id"], 1);
    assert_eq!(
        say(&mut bob, &id, "cześć").await.status,
        StatusCode::CREATED
    );
    assert_eq!(count(&app, "SELECT count(*) FROM chat_messages").await, 2);

    let read = chat(&mut bob, &id).await;
    let ids: Vec<i64> = read["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_i64().unwrap())
        .collect();
    assert_eq!((ids, read["lastId"].clone()), (vec![1, 2], json!(2)));
    // After a cursor and with a limit.
    let page = bob
        .get(&format!("/api/games/{id}/chat?after_id=1&limit=1"))
        .await;
    assert_eq!(data_of(&page)["messages"].as_array().unwrap().len(), 1);
    let beyond = bob.get(&format!("/api/games/{id}/chat?after_id=99")).await;
    let beyond = data_of(&beyond);
    assert_eq!(
        (beyond["messages"].clone(), beyond["lastId"].clone()),
        (json!([]), json!(99))
    );
    let latest = bob.get(&format!("/api/games/{id}/chat?limit=1")).await;
    assert_eq!(data_of(&latest)["messages"][0]["id"], 2);
    assert_eq!(
        reply_cache(&ala.get(&format!("/api/games/{id}/chat")).await),
        Some("private, no-store".to_owned())
    );
}

fn reply_cache(reply: &Reply) -> Option<String> {
    reply.header("cache-control").map(str::to_owned)
}

#[tokio::test]
async fn bad_queries_and_bodies_are_refused_without_echoing_the_text() {
    let app = app().await;
    let (id, _, mut ala, _) = started(&app, false).await;
    for query in [
        "after_id=2147483648",
        "after_id=x",
        "after_id=",
        "limit=0",
        "limit=101",
        "limit=x",
        "limit=1000",
    ] {
        let reply = ala.get(&format!("/api/games/{id}/chat?{query}")).await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{query}");
    }
    // The query is judged before the participant.
    assert_eq!(
        visitor(&app, 5)
            .get(&format!("/api/games/{id}/chat?limit=0"))
            .await
            .status,
        StatusCode::BAD_REQUEST
    );

    let cases = [
        (
            json!({ "text": "http://example.com", "clientId": uuid() }),
            "url",
        ),
        (json!({ "text": "ok.com", "clientId": uuid() }), "url"),
        (
            json!({ "text": "a\nb", "clientId": uuid() }),
            "control_char",
        ),
        (json!({ "text": "   ", "clientId": uuid() }), "empty"),
        (
            json!({ "text": "a".repeat(201), "clientId": uuid() }),
            "too_long",
        ),
        (
            json!({ "text": "a\u{200B}b", "clientId": uuid() }),
            "invisible_char",
        ),
    ];
    for (body, reason) in cases {
        let secret = body["text"].as_str().unwrap().to_owned();
        let reply = ala.post(&format!("/api/games/{id}/chat"), body).await;
        assert_eq!(
            reply.status,
            StatusCode::BAD_REQUEST,
            "{reason}: {}",
            reply.body
        );
        let error = error_of(&reply);
        assert_eq!(error["code"], "VALIDATION");
        assert_eq!(error["data"]["reason"], reason);
        assert_eq!(
            error["fieldIssues"]["text"][0],
            json!({ "code": "CHAT_REJECTED", "params": { "reason": reason } })
        );
        if secret.trim().len() > 3 {
            assert!(
                !reply.body.contains(secret.trim()),
                "the text is never echoed"
            );
        }
    }
    for body in [
        json!({ "text": 5, "clientId": uuid() }),
        json!({ "text": "x".repeat(1001), "clientId": uuid() }),
        json!({ "text": "x", "clientId": "nope" }),
        json!({ "text": "x" }),
        json!({ "text": "x", "clientId": uuid(), "extra": 1 }),
        json!([1]),
    ] {
        let reply = ala.post(&format!("/api/games/{id}/chat"), body).await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    }
    assert_eq!(count(&app, "SELECT count(*) FROM chat_messages").await, 0);
}

#[tokio::test]
async fn a_sender_waits_between_messages_by_the_database_clock() {
    let app = app_with(&[("CHAT_MIN_INTERVAL_MS", "60000")]).await;
    let (id, _, mut ala, mut bob) = started(&app, false).await;
    let first = say(&mut ala, &id, "jeden").await;
    assert_eq!(data_of(&first)["retryAfterMs"], 60000);
    let second = say(&mut ala, &id, "dwa").await;
    assert_eq!(second.status, StatusCode::TOO_MANY_REQUESTS);
    let error = error_of(&second);
    assert_eq!(error["code"], "RATE_LIMITED");
    assert_eq!(error["data"]["reason"], "chat_interval");
    assert!(error["data"]["retryAfterMs"].as_u64().unwrap() > 50_000);
    assert!(second.header("retry-after").is_some());
    // The other player has their own interval; the read tells the waiting one how long is left.
    assert_eq!(say(&mut bob, &id, "ja").await.status, StatusCode::CREATED);
    assert!(chat(&mut ala, &id).await["retryAfterMs"].as_u64().unwrap() > 0);
    // A repeat of the first attempt still answers (it spends nothing).
    let client_id = uuid();
    execute(
        &app,
        "UPDATE chat_messages SET created_at = created_at - interval '2 minutes'",
    )
    .await;
    let third = ala
        .post(
            &format!("/api/games/{id}/chat"),
            json!({ "text": "trzy", "clientId": client_id }),
        )
        .await;
    assert_eq!(third.status, StatusCode::CREATED, "{}", third.body);
}

#[tokio::test]
async fn the_chat_of_a_game_is_capped() {
    let app = app_with(&[("CHAT_MAX_MESSAGES_PER_GAME", "2")]).await;
    let (id, _, mut ala, mut bob) = started(&app, false).await;
    let client_id = uuid();
    let body = json!({ "text": "a", "clientId": client_id });
    assert_eq!(
        ala.post(&format!("/api/games/{id}/chat"), body.clone())
            .await
            .status,
        StatusCode::CREATED
    );
    assert_eq!(say(&mut bob, &id, "b").await.status, StatusCode::CREATED);
    let full = say(&mut ala, &id, "c").await;
    assert_eq!(full.status, StatusCode::CONFLICT);
    assert_eq!(error_of(&full)["code"], "CHAT_FULL");
    // A repeat of a message that exists still answers when the chat is full.
    assert_eq!(
        ala.post(&format!("/api/games/{id}/chat"), body)
            .await
            .status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn the_switch_is_per_player_and_history_returns() {
    let app = app().await;
    let (id, _, mut ala, mut bob) = started(&app, false).await;
    say(&mut ala, &id, "pierwsza").await;
    let off = ala
        .put(
            &format!("/api/games/{id}/chat/settings"),
            json!({ "enabled": false }),
        )
        .await;
    assert_eq!(data_of(&off)["state"], "disabled_by_you");
    let mine = chat(&mut ala, &id).await;
    assert_eq!(
        (mine["state"].clone(), mine["messages"].clone()),
        (json!("disabled_by_you"), json!([]))
    );
    assert_eq!(
        error_of(&say(&mut ala, &id, "x").await)["code"],
        "CHAT_DISABLED"
    );
    // The other one still sees the history, cannot write.
    let theirs = chat(&mut bob, &id).await;
    assert_eq!(theirs["state"], "disabled_by_opponent");
    assert_eq!(theirs["messages"].as_array().unwrap().len(), 1);
    let refused = say(&mut bob, &id, "x").await;
    assert_eq!(
        (refused.status, error_of(&refused)["code"].clone()),
        (StatusCode::CONFLICT, json!("CHAT_DISABLED"))
    );
    // Repeating the same value changes nothing; turning it on returns the history.
    let again = ala
        .put(
            &format!("/api/games/{id}/chat/settings"),
            json!({ "enabled": false }),
        )
        .await;
    assert_eq!(data_of(&again)["state"], "disabled_by_you");
    let on = ala
        .put(
            &format!("/api/games/{id}/chat/settings"),
            json!({ "enabled": true }),
        )
        .await;
    assert_eq!(data_of(&on)["state"], "open");
    assert_eq!(
        chat(&mut ala, &id).await["messages"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    // Both off: each only knows their own switch.
    bob.put(
        &format!("/api/games/{id}/chat/settings"),
        json!({ "enabled": false }),
    )
    .await;
    ala.put(
        &format!("/api/games/{id}/chat/settings"),
        json!({ "enabled": false }),
    )
    .await;
    assert_eq!(chat(&mut bob, &id).await["state"], "disabled_by_you");
    for body in [
        json!({ "enabled": "yes" }),
        json!({}),
        json!({ "enabled": true, "x": 1 }),
        json!(null),
    ] {
        let reply = ala
            .put(&format!("/api/games/{id}/chat/settings"), body)
            .await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn the_chat_follows_the_state_of_the_game() {
    let app = app().await;
    let mut ala = signup(&app, 1, "Ala").await;
    let (waiting, code) = create(&mut ala, false).await;
    assert_eq!(chat(&mut ala, &waiting).await["state"], "unavailable");
    assert_eq!(
        error_of(&say(&mut ala, &waiting, "x").await)["code"],
        "CHAT_UNAVAILABLE"
    );
    let off = ala
        .put(
            &format!("/api/games/{waiting}/chat/settings"),
            json!({ "enabled": false }),
        )
        .await;
    assert_eq!(error_of(&off)["code"], "CHAT_UNAVAILABLE");
    let mut bob = signup(&app, 2, "Bob").await;
    bob.post(
        &format!("/api/invitations/{code}/accept"),
        json!({ "as": "user" }),
    )
    .await;
    ala.get(&format!("/api/games/{waiting}")).await;
    bob.get(&format!("/api/games/{waiting}")).await;
    say(&mut ala, &waiting, "hej").await;
    ala.post(&format!("/api/games/{waiting}/resign"), Value::Null)
        .await;
    // Open for an hour after the end, then closed and empty, whatever the cleanup did.
    assert_eq!(chat(&mut bob, &waiting).await["state"], "open");
    execute(
        &app,
        "UPDATE games SET finished_at = now() - interval '61 minutes'",
    )
    .await;
    let closed = chat(&mut bob, &waiting).await;
    assert_eq!(
        (closed["state"].clone(), closed["messages"].clone()),
        (json!("closed"), json!([]))
    );
    assert_eq!(
        error_of(&say(&mut bob, &waiting, "x").await)["code"],
        "CHAT_UNAVAILABLE"
    );
    let off = bob
        .put(
            &format!("/api/games/{waiting}/chat/settings"),
            json!({ "enabled": false }),
        )
        .await;
    assert_eq!(off.status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn strangers_and_unknown_games_get_the_same_not_found() {
    let app = app().await;
    let (id, _, _, _) = started(&app, false).await;
    let mut cy = signup(&app, 3, "Cyprian").await;
    for path in [
        format!("/api/games/{id}/chat"),
        format!("/api/games/{}/chat", uuid()),
        "/api/games/not-a-uuid/chat".to_owned(),
    ] {
        assert_eq!(cy.get(&path).await.status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(
            visitor(&app, 6).get(&path).await.status,
            StatusCode::NOT_FOUND
        );
    }
    assert_eq!(say(&mut cy, &id, "x").await.status, StatusCode::NOT_FOUND);
    let put = cy
        .put(
            &format!("/api/games/{id}/chat/settings"),
            json!({ "enabled": true }),
        )
        .await;
    assert_eq!(put.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn guests_chat_too_and_a_deleted_account_makes_the_chat_unavailable() {
    let app = app().await;
    let mut ala = signup(&app, 1, "Ala").await;
    let mut guest = visitor(&app, 1);
    let created = guest
        .post("/api/games", json!({ "acknowledgedGuestWarning": true }))
        .await;
    let data = data_of(&created);
    let (id, code) = (
        data["gameId"].as_str().unwrap().to_owned(),
        data["invitation"]["code"].as_str().unwrap().to_owned(),
    );
    ala.post(
        &format!("/api/invitations/{code}/accept"),
        json!({ "as": "user" }),
    )
    .await;
    guest.get(&format!("/api/games/{id}")).await;
    ala.get(&format!("/api/games/{id}")).await;
    assert_eq!(
        say(&mut guest, &id, "od gościa").await.status,
        StatusCode::CREATED
    );
    assert_eq!(
        say(&mut ala, &id, "od konta").await.status,
        StatusCode::CREATED
    );
    ala.post(&format!("/api/games/{id}/resign"), Value::Null)
        .await;
    let deleted = ala
        .post(
            "/api/account/delete",
            json!({ "password": PASSWORD, "confirm": true }),
        )
        .await;
    assert_eq!(deleted.status, StatusCode::OK, "{}", deleted.body);
    assert_eq!(
        count(&app, "SELECT count(*) FROM chat_messages").await,
        0,
        "the whole conversation went"
    );
    let read = chat(&mut guest, &id).await;
    assert_eq!(
        (read["state"].clone(), read["messages"].clone()),
        (json!("unavailable"), json!([]))
    );
    assert_eq!(
        error_of(&say(&mut guest, &id, "x").await)["code"],
        "CHAT_UNAVAILABLE"
    );
}

#[tokio::test]
async fn a_rematch_starts_with_an_empty_chat_and_both_switches_on() {
    let app = app().await;
    let (id, _, mut ala, mut bob) = started(&app, false).await;
    say(&mut ala, &id, "stara").await;
    bob.put(
        &format!("/api/games/{id}/chat/settings"),
        json!({ "enabled": false }),
    )
    .await;
    ala.post(&format!("/api/games/{id}/resign"), Value::Null)
        .await;
    ala.post(&format!("/api/games/{id}/rematch"), Value::Null)
        .await;
    let accepted = bob
        .post(&format!("/api/games/{id}/rematch/accept"), Value::Null)
        .await;
    let next = data_of(&accepted)["gameId"].as_str().unwrap().to_owned();
    ala.get(&format!("/api/games/{next}")).await;
    bob.get(&format!("/api/games/{next}")).await;
    let read = chat(&mut ala, &next).await;
    assert_eq!(
        (read["state"].clone(), read["messages"].clone()),
        (json!("open"), json!([]))
    );
    assert_eq!(
        say(&mut bob, &next, "nowa").await.status,
        StatusCode::CREATED
    );
}

// ---- public games and spectators ---------------------------------------------------------------------------------------

#[tokio::test]
async fn only_an_invitation_can_make_a_game_public_and_the_flag_must_be_a_boolean() {
    let app = app().await;
    let mut ala = signup(&app, 1, "Ala").await;
    let bad = ala.post("/api/games", json!({ "isPublic": "yes" })).await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        error_of(&bad)["fieldIssues"]["isPublic"][0]["code"],
        "invalid_type"
    );
    let (private, _) = create(&mut ala, false).await;
    assert!(!is_public(&app, &private).await);
    ala.post(&format!("/api/games/{private}/cancel"), Value::Null)
        .await;
    let (public, _) = create(&mut ala, true).await;
    assert!(is_public(&app, &public).await);
    // No body at all is private; a guest sends the flag along with the acknowledgement.
    ala.post(&format!("/api/games/{public}/cancel"), Value::Null)
        .await;
    let bare = ala.post("/api/games", json!({})).await;
    assert_eq!(bare.status, StatusCode::CREATED);
    assert!(!is_public(&app, data_of(&bare)["gameId"].as_str().unwrap()).await);
    let mut guest = visitor(&app, 1);
    let reply = guest
        .post(
            "/api/games",
            json!({ "acknowledgedGuestWarning": true, "isPublic": true }),
        )
        .await;
    assert!(is_public(&app, data_of(&reply)["gameId"].as_str().unwrap()).await);
    let bad = visitor(&app, 2)
        .post(
            "/api/games",
            json!({ "acknowledgedGuestWarning": true, "isPublic": 1 }),
        )
        .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
}

async fn is_public(app: &TestApp, id: &str) -> bool {
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT is_public FROM games WHERE id = '{id}'"
    )))
    .fetch_one(app.db.pool())
    .await
    .unwrap()
}

#[tokio::test]
async fn a_visitor_watches_a_public_game_and_reads_only_the_public_part() {
    let app = app().await;
    let (id, code, mut ala, _) = started(&app, true).await;
    let mut viewer = visitor(&app, 3);
    let joined = watch(&mut viewer, &code, json!({ "nickname": "Kuba" })).await;
    assert_eq!(joined.status, StatusCode::OK, "{}", joined.body);
    assert_eq!(data_of(&joined)["gameId"], id.as_str());
    let cookies = joined.set_cookies();
    let cookie = cookies
        .iter()
        .find(|c| c.starts_with("gomoku_spectator="))
        .expect("a spectator cookie");
    assert!(
        cookie.contains("Max-Age=3600") && cookie.contains("HttpOnly"),
        "{cookie}"
    );
    assert!(
        !joined
            .body
            .contains(cookie.split(';').next().unwrap().split('=').nth(1).unwrap()),
        "no token in the JSON"
    );

    let state = viewer.get(&format!("/api/games/{id}/spectate")).await;
    assert_eq!(state.status, StatusCode::OK, "{}", state.body);
    let view = data_of(&state);
    let mut keys: Vec<&str> = view
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "audience",
            "blackSeat",
            "canChat",
            "clock",
            "moveCount",
            "moves",
            "players",
            "result",
            "status",
            "timeControlMs",
            "turnSeat",
            "variant"
        ]
    );
    assert_eq!(view["audience"]["spectators"], 1);
    assert_eq!(view["canChat"], true);
    assert!(view["players"][0].get("seen").is_none());
    assert!(!state.body.contains(&code), "no code in the view");
    // The players see the audience and may close it.
    let seen = data_of(&ala.get(&format!("/api/games/{id}")).await);
    assert_eq!(seen["audience"]["spectators"], 1);
    assert_eq!(seen["canCloseAudience"], true);
    // A spectator is a stranger to every player route; a player is a stranger to the spectator routes.
    for reply in [
        viewer.get(&format!("/api/games/{id}")).await,
        viewer.get(&format!("/api/games/{id}/chat")).await,
        viewer
            .post(
                &format!("/api/games/{id}/moves"),
                json!({ "x": 1, "y": 1, "moveNumber": 1 }),
            )
            .await,
        viewer
            .post(&format!("/api/games/{id}/resign"), Value::Null)
            .await,
        ala.get(&format!("/api/games/{id}/spectate")).await,
        ala.get(&format!("/api/games/{id}/spectate/chat")).await,
    ] {
        assert_eq!(reply.status, StatusCode::NOT_FOUND);
    }
    assert_eq!(
        viewer
            .get(&format!("/api/games/{id}/spectate?after=5"))
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        viewer
            .get(&format!("/api/games/{id}/spectate?after=x"))
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn spectators_chat_under_a_name_and_players_see_them() {
    let app = app().await;
    let (id, code, mut ala, _) = started(&app, true).await;
    let mut kuba = visitor(&app, 3);
    watch(&mut kuba, &code, json!({ "nickname": "Kuba" })).await;
    let mut anon = visitor(&app, 4);
    watch(&mut anon, &code, json!({})).await;
    let mut member = signup(&app, 7, "Marta").await;
    assert_eq!(
        watch(&mut member, &code, json!({})).await.status,
        StatusCode::OK
    );
    assert!(
        member.cookie("gomoku_spectator").is_none(),
        "accounts need no cookie"
    );

    let sent = kuba
        .post(
            &format!("/api/games/{id}/spectate/chat"),
            json!({ "text": "kibicuję", "clientId": uuid() }),
        )
        .await;
    assert_eq!(sent.status, StatusCode::CREATED, "{}", sent.body);
    let message = data_of(&sent)["message"].clone();
    assert_eq!(
        (message["seat"].clone(), message["spectatorName"].clone()),
        (Value::Null, json!("Kuba"))
    );
    let marta = member
        .post(
            &format!("/api/games/{id}/spectate/chat"),
            json!({ "text": "ja też", "clientId": uuid() }),
        )
        .await;
    assert_eq!(data_of(&marta)["message"]["spectatorName"], "Marta");
    // Nobody without a name may write, even a repeat.
    let client_id = uuid();
    for _ in 0..2 {
        let refused = anon
            .post(
                &format!("/api/games/{id}/spectate/chat"),
                json!({ "text": "x", "clientId": client_id }),
            )
            .await;
        assert_eq!(refused.status, StatusCode::FORBIDDEN);
        assert_eq!(
            error_of(&refused)["data"]["reason"],
            "spectator_name_required"
        );
    }
    let reads = anon.get(&format!("/api/games/{id}/spectate/chat")).await;
    assert_eq!(data_of(&reads)["messages"].as_array().unwrap().len(), 2);
    let text = ala.get(&format!("/api/games/{id}/chat")).await;
    let from_players = data_of(&text);
    assert_eq!(
        from_players["messages"].as_array().unwrap().len(),
        2,
        "players see the spectators' lines"
    );
    // Players' switches never hide anything from a spectator.
    ala.put(
        &format!("/api/games/{id}/chat/settings"),
        json!({ "enabled": false }),
    )
    .await;
    assert_eq!(
        data_of(&kuba.get(&format!("/api/games/{id}/spectate/chat")).await)["state"],
        "open"
    );
    // In a public game the other player's switch does not block me.
    let mut bob2 = signup(&app, 8, "Zofia").await;
    let _ = &mut bob2;
    let read = chat(&mut ala, &id).await;
    assert_eq!(read["state"], "disabled_by_you");
    ala.put(
        &format!("/api/games/{id}/chat/settings"),
        json!({ "enabled": true }),
    )
    .await;
    // An account with a nickname is refused; a spectator's text obeys the same rules.
    let mut other = signup(&app, 9, "Ola").await;
    let bad = watch(&mut other, &code, json!({ "nickname": "Ola" })).await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        error_of(&bad)["fieldIssues"]["nickname"][0]["code"],
        "NICKNAME_GUEST_ONLY"
    );
    let url = kuba
        .post(
            &format!("/api/games/{id}/spectate/chat"),
            json!({ "text": "www.x.pl", "clientId": uuid() }),
        )
        .await;
    assert_eq!(url.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn in_a_public_game_the_opponents_switch_does_not_block_the_asker() {
    let app = app().await;
    let (id, _, mut ala, mut bob) = started(&app, true).await;
    bob.put(
        &format!("/api/games/{id}/chat/settings"),
        json!({ "enabled": false }),
    )
    .await;
    let read = chat(&mut ala, &id).await;
    assert_eq!(
        (read["state"].clone(), read["opponentDisabled"].clone()),
        (json!("open"), json!(true))
    );
    assert_eq!(
        say(&mut ala, &id, "do widzów").await.status,
        StatusCode::CREATED
    );
    assert_eq!(chat(&mut bob, &id).await["state"], "disabled_by_you");
    // After the kill-switch the private rule is back.
    ala.post(&format!("/api/games/{id}/audience/close"), Value::Null)
        .await;
    assert_eq!(chat(&mut ala, &id).await["state"], "disabled_by_opponent");
}

#[tokio::test]
async fn a_returning_spectator_is_not_counted_twice_and_the_twenty_first_is_refused() {
    let app = app().await;
    let (id, code, _, _) = started(&app, true).await;
    let mut crowd = Vec::new();
    for n in 0..20u8 {
        let mut v = visitor(&app, 20 + n);
        assert_eq!(
            watch(&mut v, &code, json!({})).await.status,
            StatusCode::OK,
            "spectator {n}"
        );
        crowd.push(v);
    }
    let mut late = visitor(&app, 99);
    let full = watch(&mut late, &code, json!({})).await;
    assert_eq!(full.status, StatusCode::CONFLICT);
    assert_eq!(error_of(&full)["code"], "SPECTATORS_FULL");
    // A returning one gets in (and slides), without a new cookie.
    let again = watch(&mut crowd[0], &code, json!({})).await;
    assert_eq!(again.status, StatusCode::OK);
    assert!(
        again
            .set_cookies()
            .iter()
            .all(|c| !c.starts_with("gomoku_spectator="))
    );
    // Silent for 15 s frees a place.
    app.advance_seconds(16);
    assert_eq!(
        watch(&mut late, &code, json!({})).await.status,
        StatusCode::OK
    );
    let view = data_of(&late.get(&format!("/api/games/{id}/spectate")).await);
    assert_eq!(
        view["audience"]["spectators"], 1,
        "only the reader is present now"
    );
    assert_eq!(
        count(&app, "SELECT count(*) FROM game_spectators").await,
        21
    );
}

#[tokio::test]
async fn watching_is_refused_for_private_waiting_and_finished_games_and_after_expiry() {
    let app = app().await;
    let (private, private_code, _, _) = started(&app, false).await;
    let _ = private;
    let mut v = visitor(&app, 3);
    let gone = watch(&mut v, &private_code, json!({})).await;
    assert_eq!(
        (gone.status, error_of(&gone)["code"].clone()),
        (StatusCode::GONE, json!("INVALID_INVITATION"))
    );
    let mut cy = signup(&app, 3, "Cyprian").await;
    let (_, waiting_code) = create(&mut cy, true).await;
    assert_eq!(
        watch(&mut v, &waiting_code, json!({})).await.status,
        StatusCode::GONE,
        "the first with the code plays"
    );
    assert_eq!(
        watch(&mut v, "garbage", json!({})).await.status,
        StatusCode::GONE
    );

    let (id, code, mut ala, _) = started_other(&app).await;
    assert_eq!(watch(&mut v, &code, json!({})).await.status, StatusCode::OK);
    // Access lapses an hour after the last read.
    app.advance_seconds(3601);
    assert_eq!(
        v.get(&format!("/api/games/{id}/spectate")).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        watch(&mut v, &code, json!({})).await.status,
        StatusCode::OK,
        "the code re-admits"
    );
    ala.post(&format!("/api/games/{id}/resign"), Value::Null)
        .await;
    let after = watch(&mut v, &code, json!({})).await;
    assert_eq!(
        after.status,
        StatusCode::GONE,
        "a finished game is reviewed, not watched"
    );
}

/// A second public game next to the first one of a test.
async fn started_other(app: &TestApp) -> (String, String, Client, Client) {
    let mut cy = signup(app, 31, "Dorota").await;
    let mut dan = signup(app, 32, "Daniel").await;
    let (id, code) = create(&mut cy, true).await;
    dan.post(
        &format!("/api/invitations/{code}/accept"),
        json!({ "as": "user" }),
    )
    .await;
    dan.get(&format!("/api/games/{id}")).await;
    cy.get(&format!("/api/games/{id}")).await;
    (id, code, cy, dan)
}

#[tokio::test]
async fn a_spectator_read_settles_a_loss_on_time_and_shows_the_finished_game() {
    let app = app().await;
    let (id, code, _, _) = started(&app, true).await;
    let mut v = visitor(&app, 3);
    watch(&mut v, &code, json!({})).await;
    app.advance_seconds(11 * 60);
    let view = data_of(&v.get(&format!("/api/games/{id}/spectate")).await);
    assert_eq!(view["status"], "finished");
    assert_eq!(view["result"]["reason"], "timeout");
    assert!(view["finishedAt"].is_string());
    assert!(
        view.get("audience").is_none(),
        "no audience flag once the game ended"
    );
}

#[tokio::test]
async fn only_the_creator_closes_the_audience_and_it_cannot_be_reopened() {
    let app = app().await;
    let (id, code, mut ala, mut bob) = started(&app, true).await;
    let mut v = visitor(&app, 3);
    watch(&mut v, &code, json!({ "nickname": "Kuba" })).await;
    v.post(
        &format!("/api/games/{id}/spectate/chat"),
        json!({ "text": "hej", "clientId": uuid() }),
    )
    .await;
    say(&mut ala, &id, "gracz").await;
    assert_eq!(
        bob.post(&format!("/api/games/{id}/audience/close"), Value::Null)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        v.post(&format!("/api/games/{id}/audience/close"), Value::Null)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert!(
        data_of(&bob.get(&format!("/api/games/{id}")).await)
            .get("canCloseAudience")
            .is_none()
    );

    let closed = ala
        .post(&format!("/api/games/{id}/audience/close"), Value::Null)
        .await;
    assert_eq!(closed.status, StatusCode::OK, "{}", closed.body);
    let view = data_of(&closed);
    assert_eq!(view["audienceClosed"], true);
    assert!(view.get("audience").is_none() && view.get("canCloseAudience").is_none());
    assert_eq!(
        count(
            &app,
            "SELECT count(*) FROM chat_messages WHERE spectator_id IS NOT NULL"
        )
        .await,
        0
    );
    assert_eq!(
        count(&app, "SELECT count(*) FROM chat_messages").await,
        1,
        "the players' line stays"
    );
    // Idempotent; the spectator is cut off; the code stops opening anything.
    assert_eq!(
        ala.post(&format!("/api/games/{id}/audience/close"), Value::Null)
            .await
            .status,
        StatusCode::OK
    );
    let cut = v.get(&format!("/api/games/{id}/spectate")).await;
    assert_eq!(
        (cut.status, error_of(&cut)["code"].clone()),
        (StatusCode::GONE, json!("AUDIENCE_CLOSED"))
    );
    assert_eq!(
        v.get(&format!("/api/games/{id}/spectate/chat"))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        watch(&mut visitor(&app, 9), &code, json!({})).await.status,
        StatusCode::GONE
    );
    assert_eq!(
        count(&app, "SELECT count(*) FROM game_spectators").await,
        1,
        "rows wait for the cleanup"
    );
}

#[tokio::test]
async fn the_kill_switch_refuses_private_and_unstarted_games() {
    let app = app().await;
    let mut ala = signup(&app, 1, "Ala").await;
    let (waiting, _) = create(&mut ala, true).await;
    let early = ala
        .post(&format!("/api/games/{waiting}/audience/close"), Value::Null)
        .await;
    assert_eq!(early.status, StatusCode::CONFLICT);
    assert_eq!(error_of(&early)["data"]["reason"], "audience_not_closable");
    ala.post(&format!("/api/games/{waiting}/cancel"), Value::Null)
        .await;
    let (id, _, mut cy, _) = started_private(&app).await;
    let reply = cy
        .post(&format!("/api/games/{id}/audience/close"), Value::Null)
        .await;
    assert_eq!(
        (reply.status, error_of(&reply)["code"].clone()),
        (StatusCode::CONFLICT, json!("GAME_NOT_PUBLIC"))
    );
    // A finished public game can still be closed (it blocks the review by code).
    let (pid, code, mut dora, _) = started_other(&app).await;
    dora.post(&format!("/api/games/{pid}/resign"), Value::Null)
        .await;
    assert!(is_public(&app, &pid).await);
    assert_eq!(
        dora.post(&format!("/api/games/{pid}/audience/close"), Value::Null)
            .await
            .status,
        StatusCode::OK
    );
    let page = visitor(&app, 5)
        .get(&format!("/games/{pid}/review?code={code}"))
        .await;
    assert_eq!(page.status, StatusCode::NOT_FOUND);
}

async fn started_private(app: &TestApp) -> (String, String, Client, Client) {
    let mut cy = signup(app, 41, "Pawel").await;
    let mut dan = signup(app, 42, "Piotr").await;
    let (id, code) = create(&mut cy, false).await;
    dan.post(
        &format!("/api/invitations/{code}/accept"),
        json!({ "as": "user" }),
    )
    .await;
    (id, code, cy, dan)
}

#[tokio::test]
async fn the_qr_code_exists_only_while_the_game_may_show_its_code() {
    let app = app().await;
    let mut ala = signup(&app, 1, "Ala").await;
    let (id, code) = create(&mut ala, false).await;
    let qr = ala.get(&format!("/api/games/{id}/qr")).await;
    assert_eq!(qr.status, StatusCode::OK);
    assert_eq!(qr.header("content-type"), Some("image/svg+xml"));
    assert_eq!(qr.header("cache-control"), Some("private, no-store"));
    assert_eq!(qr.header("x-content-type-options"), Some("nosniff"));
    assert_eq!(
        qr.header("content-security-policy"),
        Some("default-src 'none'; sandbox")
    );
    assert!(
        qr.body.starts_with("<?xml") || qr.body.contains("<svg"),
        "{}",
        &qr.body[..40.min(qr.body.len())]
    );
    assert!(
        qr.body.contains("viewBox") && qr.body.contains("#000000") && qr.body.contains("#ffffff")
    );
    // A stranger, an expired invitation: not found.
    assert_eq!(
        visitor(&app, 3)
            .get(&format!("/api/games/{id}/qr"))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    app.advance_seconds(25 * 3600);
    assert_eq!(
        ala.get(&format!("/api/games/{id}/qr")).await.status,
        StatusCode::NOT_FOUND
    );
    let _ = code;

    // A private game that started: no code any more. A public one: players and spectators may show it.
    let (private, _, mut cy, _) = started_private(&app).await;
    assert_eq!(
        cy.get(&format!("/api/games/{private}/qr")).await.status,
        StatusCode::NOT_FOUND
    );
    let (pid, pcode, mut dora, mut dan) = started_other(&app).await;
    assert_eq!(
        dora.get(&format!("/api/games/{pid}/qr")).await.status,
        StatusCode::OK
    );
    assert_eq!(
        dan.get(&format!("/api/games/{pid}/qr")).await.status,
        StatusCode::OK
    );
    let mut v = visitor(&app, 3);
    watch(&mut v, &pcode, json!({})).await;
    assert_eq!(
        v.get(&format!("/api/games/{pid}/qr")).await.status,
        StatusCode::OK
    );
    assert_eq!(
        visitor(&app, 4)
            .get(&format!("/api/games/{pid}/qr"))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    dora.post(&format!("/api/games/{pid}/audience/close"), Value::Null)
        .await;
    assert_eq!(
        v.get(&format!("/api/games/{pid}/qr")).await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn a_finished_public_game_is_reviewed_by_anybody_with_the_code() {
    let app = app().await;
    let (id, code, mut ala, _) = started(&app, true).await;
    ala.post(&format!("/api/games/{id}/resign"), Value::Null)
        .await;
    let mut reader = visitor(&app, 3);
    let page = reader.get(&format!("/games/{id}/review?code={code}")).await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    assert_eq!(page.header("referrer-policy"), Some("no-referrer"));
    assert!(page.body.contains(r#""viewerSeat":null"#) && page.body.contains(r#"href="/""#));
    // The invitation page sends the code's holder there.
    let invite = reader.get(&format!("/invite/{code}")).await;
    assert_eq!(invite.status, StatusCode::SEE_OTHER);
    assert!(
        invite
            .header("location")
            .unwrap()
            .starts_with(&format!("/games/{id}/review?code="))
    );
    // Everything else is one not-found page.
    let plain = code.replace('-', "");
    for query in [
        "code=XXXX-XXXX",
        "code=",
        &format!("code={plain}&code={plain}"),
        "other=1",
    ] {
        let reply = reader.get(&format!("/games/{id}/review?{query}")).await;
        assert!(
            reply.status == StatusCode::NOT_FOUND || reply.status == StatusCode::SEE_OTHER,
            "{query}: {}",
            reply.status
        );
    }
    assert_eq!(
        reader
            .get(&format!("/games/{id}/review?code={plain}&code={plain}"))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        reader
            .get(&format!("/games/{}/review?code={code}", uuid()))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        reader
            .get(&format!("/games/nope/review?code={code}"))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    // A private game cannot be opened that way; an unfinished one neither.
    let (pid, pcode, mut cy, mut dan) = started_private(&app).await;
    cy.post(&format!("/api/games/{pid}/resign"), Value::Null)
        .await;
    let _ = &mut dan;
    assert_eq!(
        reader
            .get(&format!("/games/{pid}/review?code={pcode}"))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    let (oid, ocode, _, _) = started_other(&app).await;
    assert_eq!(
        reader
            .get(&format!("/games/{oid}/review?code={ocode}"))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        count(&app, "SELECT count(*) FROM game_spectators").await,
        0,
        "a reader writes nothing"
    );
}

#[tokio::test]
async fn the_invitation_page_announces_public_games_and_offers_to_watch() {
    let app = app().await;
    let mut ala = signup(&app, 1, "Ala").await;
    let (_, code) = create(&mut ala, true).await;
    let joinable = visitor(&app, 3).get(&format!("/invite/{code}")).await;
    assert!(
        joinable.body.contains(r#"data-testid="invite-public""#)
            && joinable.body.contains(r#"data-state="joinable""#)
    );
    let mut bob = signup(&app, 2, "Bob").await;
    bob.post(
        &format!("/api/invitations/{code}/accept"),
        json!({ "as": "user" }),
    )
    .await;
    let mut v = visitor(&app, 3);
    let watching = v.get(&format!("/invite/{code}")).await;
    assert!(
        watching.body.contains(r#"data-state="watch""#)
            && watching.body.contains(r#"data-testid="watch-nickname""#)
    );
    assert!(watching.body.contains(r#"data-testid="invite-watch-note""#));
    let mut member = signup(&app, 4, "Cezary").await;
    let seen = member.get(&format!("/invite/{code}")).await;
    assert!(seen.body.contains(r#"data-state="watch""#) && !seen.body.contains("watch-nickname"));
    // The page of a spectator: the read-only room; the closed audience: its own screen; others: not found.
    let id = sqlx::query_scalar::<_, gomoku_db::Uuid>("SELECT game_id FROM invitations")
        .fetch_one(app.db.pool())
        .await
        .unwrap();
    assert_eq!(
        visitor(&app, 8).get(&format!("/games/{id}")).await.status,
        StatusCode::NOT_FOUND
    );
    watch(&mut v, &code, json!({ "nickname": "Kuba" })).await;
    let room = v.get(&format!("/games/{id}")).await;
    assert_eq!(room.status, StatusCode::OK, "{}", room.body);
    assert!(
        room.body.contains(r#"id="spectator-room""#) && room.body.contains("spectator-initial")
    );
    assert_eq!(room.header("referrer-policy"), Some("no-referrer"));
    ala.get(&format!("/api/games/{id}")).await;
    ala.post(&format!("/api/games/{id}/audience/close"), Value::Null)
        .await;
    let closed = v.get(&format!("/games/{id}")).await;
    assert_eq!(closed.status, StatusCode::OK);
    assert!(closed.body.contains(r#"data-testid="audience-closed""#));
}

// ---- cleanup -----------------------------------------------------------------------------------------------------------

#[tokio::test]
async fn cleanup_sweeps_old_chat_and_spectators_before_the_games() {
    let app = app().await;
    let (id, code, mut ala, mut bob) = started(&app, true).await;
    let mut v = visitor(&app, 3);
    watch(&mut v, &code, json!({ "nickname": "Kuba" })).await;
    say(&mut ala, &id, "stara").await;
    say(&mut bob, &id, "też").await;
    // Fresh rows stay.
    let result = cleanup_expired(&app.db, &CleanupOptions::new(common::now_of(&app))).await;
    assert_eq!(
        (result.chat_messages, result.game_spectators),
        (0, 0),
        "{result:?}"
    );
    // An abandoned active game loses its chat 1 h after its clock ran out; spectators 1 h after their access lapsed.
    app.advance_seconds(125 * 60);
    let result = cleanup_expired(&app.db, &CleanupOptions::new(common::now_of(&app))).await;
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_eq!(
        (result.chat_messages, result.game_spectators),
        (2, 1),
        "{result:?}"
    );
    assert_eq!(count(&app, "SELECT count(*) FROM chat_messages").await, 0);
    // The game of two accounts stays as history.
    assert_eq!(
        count(
            &app,
            &format!("SELECT count(*) FROM games WHERE id = '{id}'")
        )
        .await,
        1
    );
}

#[tokio::test]
async fn cleanup_removes_the_chat_of_finished_and_cancelled_games_after_the_window() {
    let app = app().await;
    let (id, _, mut ala, mut bob) = started(&app, false).await;
    say(&mut bob, &id, "na pożegnanie").await;
    ala.post(&format!("/api/games/{id}/resign"), Value::Null)
        .await;
    let result = cleanup_expired(&app.db, &CleanupOptions::new(common::now_of(&app))).await;
    assert_eq!(result.chat_messages, 0, "inside the hour");
    app.advance_seconds(61 * 60);
    let result = cleanup_expired(&app.db, &CleanupOptions::new(common::now_of(&app))).await;
    assert_eq!(result.chat_messages, 1, "{result:?}");
    assert_eq!(
        count(
            &app,
            &format!("SELECT count(*) FROM games WHERE id = '{id}'")
        )
        .await,
        1
    );
}

// ---- races ---------------------------------------------------------------------------------------------------------------

#[tokio::test]
async fn simultaneous_senders_get_consecutive_numbers_and_the_last_place_goes_to_one() {
    let app = app().await;
    let (id, code, mut ala, mut bob) = started(&app, true).await;
    let path = format!("/api/games/{id}/chat");
    let (a, b) = tokio::join!(
        ala.post(&path, json!({ "text": "a", "clientId": uuid() })),
        bob.post(&path, json!({ "text": "b", "clientId": uuid() })),
    );
    assert_eq!(
        (a.status, b.status),
        (StatusCode::CREATED, StatusCode::CREATED)
    );
    let mut ids = vec![
        data_of(&a)["message"]["id"].as_i64().unwrap(),
        data_of(&b)["message"]["id"].as_i64().unwrap(),
    ];
    ids.sort_unstable();
    assert_eq!(ids, vec![1, 2], "no gap, no duplicate");

    for n in 0..19u8 {
        assert_eq!(
            watch(&mut visitor(&app, 50 + n), &code, json!({}))
                .await
                .status,
            StatusCode::OK
        );
    }
    let (mut x, mut y) = (visitor(&app, 80), visitor(&app, 81));
    let (rx, ry) = tokio::join!(
        watch(&mut x, &code, json!({})),
        watch(&mut y, &code, json!({}))
    );
    let mut statuses = [rx.status.as_u16(), ry.status.as_u16()];
    statuses.sort_unstable();
    assert_eq!(statuses, [200, 409], "exactly one gets the last place");
}

#[tokio::test]
async fn a_chat_message_racing_with_the_switch_is_either_kept_or_refused() {
    let app = app().await;
    let (id, _, mut ala, mut bob) = started(&app, false).await;
    let switch_path = format!("/api/games/{id}/chat/settings");
    let (sent, off) = tokio::join!(
        say(&mut ala, &id, "wyścig"),
        bob.put(&switch_path, json!({ "enabled": false })),
    );
    assert_eq!(off.status, StatusCode::OK);
    assert!(
        sent.status == StatusCode::CREATED || sent.status == StatusCode::CONFLICT,
        "{}",
        sent.body
    );
    let kept = count(&app, "SELECT count(*) FROM chat_messages").await;
    assert_eq!(kept, i64::from(sent.status == StatusCode::CREATED));
}
