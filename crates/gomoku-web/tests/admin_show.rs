//! The administrator's gate, the read-only panel and the show (bot vs bot, bot vs agent) over a real PostgreSQL.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{Client, Reply, TestApp};
use serde_json::{Value, json};

const PASSWORD: &str = "correct horse";
const ADMIN: &str = "boss@example.com";
const SHOW: &[(&str, &str)] = &[
    ("TRUST_PROXY_HOPS", "1"),
    ("ADMIN_EMAIL", ADMIN),
    ("SHOW_ENABLED", "true"),
    ("SHOW_BOT_VS_BOT_ENABLED", "true"),
    ("BOT_ENABLED", "true"),
    ("BOT_FIXED_MEMORY_MB", "1024"),
    ("BOT_SHOW_PAUSE_MIN_MS", "0"),
    ("BOT_SHOW_PAUSE_MAX_MS", "0"),
    ("MCP_POLL_INTERVAL_MS", "20"),
];

async fn plain_app() -> TestApp {
    TestApp::start_with(&[("TRUST_PROXY_HOPS", "1"), ("MCP_POLL_INTERVAL_MS", "20")]).await
}

async fn register(app: &TestApp, ip: &str, email: &str, name: &str, verified: bool) -> Client {
    let mut client = app.client().with_header("x-forwarded-for", ip);
    let reply = client
        .post(
            "/api/auth/register",
            json!({ "email": email, "displayName": name, "password": PASSWORD }),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    if verified {
        sqlx::query("UPDATE users SET email_verified_at = now() WHERE email = $1")
            .bind(email)
            .execute(app.db.pool())
            .await
            .unwrap();
    }
    client
}

/// A client of the app with the admin settings that carries the session of `from`.
fn as_user(app: &TestApp, overrides: &[(&str, &str)], from: &Client, ip: &str) -> Client {
    let mut client =
        Client::for_router(app.router_with(overrides)).with_header("x-forwarded-for", ip);
    client.set_cookie("gomoku_session", from.cookie("gomoku_session").unwrap());
    client
}

async fn admin(app: &TestApp) -> Client {
    let boss = register(app, "10.8.0.1", ADMIN, "Szef", true).await;
    as_user(app, SHOW, &boss, "10.8.0.1")
}

fn data_of(reply: &Reply) -> Value {
    let body = reply.json();
    assert_eq!(body["ok"], true, "{}", reply.body);
    body["data"].clone()
}

#[tokio::test]
async fn everybody_but_the_administrator_gets_the_404_of_a_missing_page() {
    let app = plain_app().await;
    let boss = register(&app, "10.8.1.1", ADMIN, "Szef", true).await;
    let unconfirmed = register(&app, "10.8.1.2", "other@example.com", "Inny", false).await;
    let player = register(&app, "10.8.1.3", "player@example.com", "Gracz", true).await;
    let id = "6f1c1a52-0f43-4a7e-9c2c-1d0c2a1f9a10";

    let mut crowd = vec![
        (
            "nobody",
            app.client().with_header("x-forwarded-for", "10.8.1.9"),
        ),
        ("a player", as_user(&app, SHOW, &player, "10.8.1.3")),
        (
            "an unconfirmed admin address",
            as_user(&app, SHOW, &unconfirmed, "10.8.1.2"),
        ),
    ];
    // The administrator's address, but ADMIN_EMAIL is not configured: nobody is the administrator.
    crowd.push((
        "admin unset",
        as_user(&app, &[("TRUST_PROXY_HOPS", "1")], &boss, "10.8.1.1"),
    ));
    for (who, client) in &mut crowd {
        for page in ["/backend", "/show", &format!("/show/{id}")] {
            let reply = client.get(page).await;
            assert_eq!(reply.status, StatusCode::NOT_FOUND, "{who} {page}");
            assert!(reply.header("location").is_none() && reply.set_cookies().is_empty());
        }
        let known = app.client().get("/nope").await;
        let hidden = client.get("/backend").await;
        assert_eq!(
            hidden.body, known.body,
            "{who}: the same body as any unknown path"
        );
        for (method, uri) in [
            ("GET", "/api/show"),
            ("POST", "/api/show/preflight"),
            ("POST", "/api/show"),
            ("GET", &format!("/api/show/{id}")),
            ("POST", &format!("/api/show/{id}/end")),
        ] {
            // No Origin and no CSRF header: a stranger must still get the 404, not the 403.
            let request = Request::builder()
                .method(method)
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/json");
            let reply = client.send(request, Body::from("{}")).await;
            assert_eq!(reply.status, StatusCode::NOT_FOUND, "{who} {method} {uri}");
            assert_eq!(reply.json()["error"]["code"], "NOT_FOUND");
        }
    }
    // The administrator without the CSRF headers meets the CSRF check; with them, the data.
    let mut boss = as_user(&app, SHOW, &boss, "10.8.1.1");
    let request =
        Request::post("/api/show/preflight").header(header::CONTENT_TYPE, "application/json");
    let bare = boss
        .send(
            request,
            Body::from(r#"{"layout":"bot-agent","botLevel":1}"#),
        )
        .await;
    assert_eq!(bare.status, StatusCode::FORBIDDEN);
    assert_eq!(boss.get("/backend").await.status, StatusCode::OK);
    assert_eq!(boss.get("/show").await.status, StatusCode::OK);
    assert_eq!(
        boss.get("/show/not-an-id").await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        boss.get(&format!("/show/{id}")).await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn the_panel_shows_counters_and_a_searchable_sortable_paged_account_list() {
    let app = plain_app().await;
    let boss = register(&app, "10.8.2.1", ADMIN, "Szef", true).await;
    register(&app, "10.8.2.2", "ala@example.com", "Ala_Ma", false).await;
    register(&app, "10.8.2.3", "ola@example.com", "Ola", true).await;
    for n in 0..24 {
        sqlx::query("INSERT INTO users (email, display_name, display_name_key, password_hash) VALUES ($1, $2, $3, 'x')")
            .bind(format!("bulk{n}@example.com"))
            .bind(format!("Bulk {n:02}"))
            .bind(format!("bulk{n:02}"))
            .execute(app.db.pool())
            .await
            .unwrap();
    }
    sqlx::query("INSERT INTO game_pair_counters (day, kind_a, kind_b, finished_count) VALUES (($1::timestamptz AT TIME ZONE 'Europe/Warsaw')::date, 'account', 'guest', 3), ('2020-01-01', 'bot', 'guest', 2)")
        .bind(app.now())
        .execute(app.db.pool())
        .await
        .unwrap();
    let mut admin = as_user(&app, SHOW, &boss, "10.8.2.1");
    let page = admin.get("/backend").await;
    assert!(
        page.body.contains("<dt>Dziś</dt><dd>3</dd>")
            && page.body.contains("<dt>Razem</dt><dd>5</dd>"),
        "{}",
        page.body
    );
    assert!(
        page.body.contains("<dt>konto – gość</dt><dd>3</dd>")
            && page.body.contains("<dt>bot – gość</dt><dd>2</dd>")
    );
    assert_eq!(page.status, StatusCode::OK);
    assert_eq!(page.header("cache-control"), Some("private, no-store"));
    assert!(
        page.body
            .contains(r#"name="robots" content="noindex,nofollow""#)
    );
    assert!(page.body.contains("Panel administracyjny"));
    assert!(
        page.body
            .contains("Pokaz partii botów (strona administratora)")
    );
    assert!(
        page.body.contains("Strona 1 z 2 (27 kont)"),
        "{}",
        page.body
    );
    assert!(
        page.body.contains("Gracz") && !page.body.contains("Administrator"),
        "the oldest account is on page 2"
    );
    assert!(page.body.contains("agent – gość") && page.body.contains("konto – bot"));
    assert!(page.body.contains("Połączono"));
    assert!(page.body.contains("15×15"));
    // The second page holds the rest; a page past the last is cut to it.
    let second = admin.get("/backend?page=2").await;
    assert!(second.body.contains("Strona 2 z 2 (27 kont)"));
    assert!(second.body.contains("Administrator"));
    assert!(
        admin
            .get("/backend?page=99")
            .await
            .body
            .contains("Strona 2 z 2")
    );
    // Search by a name prefix, never by e-mail; `_` and `%` are not wildcards.
    let found = admin.get("/backend?q=ala").await;
    assert!(found.body.contains("ala@example.com") && found.body.contains("Strona 1 z 1 (1 kont)"));
    assert!(
        admin
            .get("/backend?q=ala%40example")
            .await
            .body
            .contains("Brak kont pasujących do wyszukiwania.")
    );
    assert!(
        admin
            .get("/backend?q=%25")
            .await
            .body
            .contains("Brak kont pasujących do wyszukiwania.")
    );
    // Sorting falls back to the defaults for nonsense.
    let by_name = admin.get("/backend?sort=name").await;
    assert!(by_name.body.contains(r#"aria-sort="ascending""#));
    assert_eq!(
        admin.get("/backend?sort=bogus&page=abc&dir=x").await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn a_bot_show_is_created_driven_to_the_end_by_reads_and_never_counted() {
    let app = plain_app().await;
    let mut admin = admin(&app).await;
    let overview = data_of(&admin.get("/api/show").await);
    assert_eq!(overview["enabled"], true);
    assert_eq!(overview["botVsBotEnabled"], true);
    assert_eq!(overview["level2Enabled"], false);
    assert_eq!(overview["current"], Value::Null);
    assert_eq!(overview["preflight"]["ok"], true);
    assert_eq!(
        overview["preflight"]["checks"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()["ok"],
        Value::Null
    );

    let checked = data_of(
        &admin
            .post(
                "/api/show/preflight",
                json!({ "layout": "bot-bot", "levels": [1, 1] }),
            )
            .await,
    );
    assert_eq!(checked["ok"], true, "{checked}");
    let engine = checked["checks"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(engine["id"], "engine");
    assert_eq!(engine["ok"], true);
    assert!(engine["detail"]["limitMs"].as_i64().unwrap() > 0);

    let created = admin
        .post(
            "/api/show",
            json!({ "layout": "bot-bot", "levels": [1, 1] }),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let data = data_of(&created);
    let id = data["gameId"].as_str().unwrap().to_owned();
    assert_eq!(data["layout"], "bot-bot");
    assert_eq!(data["bots"].as_array().unwrap().len(), 2);
    assert!(data.get("invitation").is_none());

    // A second one is refused before anything is spent.
    let second = admin
        .post(
            "/api/show",
            json!({ "layout": "bot-bot", "levels": [1, 1] }),
        )
        .await;
    assert_eq!(second.status, StatusCode::CONFLICT);
    assert_eq!(second.json()["error"]["code"], "SHOW_RUNNING");
    assert_eq!(second.json()["error"]["data"]["gameId"], id);

    // Reading drives the bots: poll until the game ends (level 1 against level 1 ends by five or by a full board).
    let mut state = Value::Null;
    for _ in 0..400 {
        app.settle().await;
        let reply = admin.get(&format!("/api/show/{id}?after=0")).await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
        state = data_of(&reply);
        if state["status"] == "finished" {
            break;
        }
        assert_eq!(state["layout"], "bot-bot");
        assert!(
            state.get("you").is_none()
                && state.get("rematch").is_none()
                && state.get("canChat").is_none()
        );
        assert_eq!(state["players"][0]["kind"], "bot");
        assert!(state["players"][0].get("seen").is_none());
    }
    assert_eq!(state["status"], "finished", "{state}");
    assert!(state["moveCount"].as_i64().unwrap() >= 9);
    assert!(state["finishedAt"].is_string());
    let counted: i64 =
        sqlx::query_scalar("SELECT coalesce(sum(finished_count), 0)::int8 FROM game_pair_counters")
            .fetch_one(app.db.pool())
            .await
            .unwrap();
    assert_eq!(counted, 0, "a show is never counted");
    // Not in the panel counters, and a finished show is over: ending it is refused.
    let page = admin.get("/backend").await;
    assert!(page.body.contains("Partie w bazie teraz"));
    let again = admin.post(&format!("/api/show/{id}/end"), json!({})).await;
    assert_eq!(again.status, StatusCode::CONFLICT);
    assert_eq!(again.json()["error"]["data"]["reason"], "show_ended");
    // A new one may start.
    let next = admin
        .post("/api/show", json!({ "layout": "bot-agent", "botLevel": 1 }))
        .await;
    assert_eq!(next.status, StatusCode::CREATED, "{}", next.body);
}

#[tokio::test]
async fn creation_is_refused_for_every_unavailable_reason_and_a_bad_body() {
    let app = plain_app().await;
    let mut admin = admin(&app).await;
    for (body, message) in [
        (json!({ "layout": "x" }), "Nieprawidłowy układ pokazu."),
        (json!({}), "Nieprawidłowy układ pokazu."),
        (json!([]), "Nieprawidłowe dane pokazu."),
        (
            json!({ "layout": "bot-bot", "levels": [1] }),
            "Podaj poziom każdego bota.",
        ),
        (
            json!({ "layout": "bot-bot", "levels": [1, 3] }),
            "Nieprawidłowy poziom bota.",
        ),
        (
            json!({ "layout": "bot-bot", "levels": ["1", 1] }),
            "Nieprawidłowy poziom bota.",
        ),
        (
            json!({ "layout": "bot-agent", "botLevel": 3 }),
            "Nieprawidłowy poziom bota.",
        ),
        (
            json!({ "layout": "bot-agent", "botLevel": 1, "extra": 1 }),
            "Nieprawidłowe dane pokazu.",
        ),
        (
            json!({ "layout": "bot-agent", "levels": [1, 1] }),
            "Nieprawidłowe dane pokazu.",
        ),
    ] {
        let reply = admin.post("/api/show", body.clone()).await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(reply.json()["error"]["message"], message, "{body}");
    }
    let level2 = admin
        .post(
            "/api/show",
            json!({ "layout": "bot-bot", "levels": [1, 2] }),
        )
        .await;
    assert_eq!(level2.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(level2.json()["error"]["code"], "BOT_UNAVAILABLE");
    assert_eq!(
        level2.json()["error"]["data"]["reason"],
        "level2_not_allowed"
    );
    assert_eq!(level2.header("retry-after"), Some("60"));
    let checks = data_of(
        &admin
            .post(
                "/api/show/preflight",
                json!({ "layout": "bot-bot", "levels": [1, 2] }),
            )
            .await,
    );
    assert_eq!(checks["ok"], false);
    assert!(
        checks["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["id"] == "level2_enabled" && c["ok"] == false)
    );

    // Switches off, one by one.
    let boss = register(&app, "10.8.4.1", "boss2@example.com", "Szef2", true).await;
    for (extra, body, code, reason) in [
        (
            vec![("SHOW_ENABLED", "false")],
            json!({ "layout": "bot-agent", "botLevel": 1 }),
            "SHOW_UNAVAILABLE",
            "disabled",
        ),
        (
            vec![("SHOW_BOT_VS_BOT_ENABLED", "false")],
            json!({ "layout": "bot-bot", "levels": [1, 1] }),
            "SHOW_UNAVAILABLE",
            "bot_vs_bot_disabled",
        ),
        (
            vec![("BOT_ENABLED", "false")],
            json!({ "layout": "bot-agent", "botLevel": 1 }),
            "BOT_UNAVAILABLE",
            "disabled",
        ),
        (
            vec![("BOT_FIXED_MEMORY_MB", "10")],
            json!({ "layout": "bot-agent", "botLevel": 1 }),
            "BOT_UNAVAILABLE",
            "memory",
        ),
        (
            vec![("BOT_MAX_ACTIVE_GAMES", "1")],
            json!({ "layout": "bot-agent", "botLevel": 1 }),
            "",
            "",
        ),
    ] {
        let mut overrides: Vec<(&str, &str)> = SHOW
            .iter()
            .copied()
            .filter(|(k, _)| !extra.iter().any(|(e, _)| e == k))
            .collect();
        overrides.extend(extra.iter().copied());
        overrides.retain(|(k, _)| *k != "ADMIN_EMAIL");
        overrides.push(("ADMIN_EMAIL", "boss2@example.com"));
        let mut client = as_user(&app, &overrides, &boss, "10.8.4.1");
        let reply = client.post("/api/show", body).await;
        if code.is_empty() {
            assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
            let id = data_of(&reply)["gameId"].as_str().unwrap().to_owned();
            let ended = client.post(&format!("/api/show/{id}/end"), json!({})).await;
            assert_eq!(ended.status, StatusCode::OK, "{}", ended.body);
            continue;
        }
        assert_eq!(
            reply.status,
            StatusCode::SERVICE_UNAVAILABLE,
            "{reason}: {}",
            reply.body
        );
        assert_eq!(reply.json()["error"]["code"], code);
        assert_eq!(reply.json()["error"]["data"]["reason"], reason);
    }
}

#[tokio::test]
async fn a_show_waits_for_an_agent_that_joins_by_code_and_plays_the_bot() {
    let app = plain_app().await;
    let mut admin = admin(&app).await;
    let created = admin
        .post("/api/show", json!({ "layout": "bot-agent", "botLevel": 1 }))
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let data = data_of(&created);
    let id = data["gameId"].as_str().unwrap().to_owned();
    let code = data["invitation"]["code"].as_str().unwrap().to_owned();
    assert!(
        data["invitation"]["mcpUrl"]
            .as_str()
            .unwrap()
            .ends_with("/api/mcp")
    );

    let waiting = data_of(&admin.get(&format!("/api/show/{id}")).await);
    assert_eq!(waiting["status"], "waiting");
    assert_eq!(waiting["layout"], "bot-agent");
    assert_eq!(waiting["invitation"]["code"], code);

    // The code is nothing to a browser (a guest or an account): the same refusal as a wrong code.
    let mut guest = app.client().with_header("x-forwarded-for", "10.8.5.2");
    let reply = guest
        .post(
            &format!("/api/invitations/{}/accept", code.replace('-', "")),
            json!({ "as": "guest", "acknowledgedGuestWarning": true }),
        )
        .await;
    assert_eq!(reply.status, StatusCode::GONE, "{}", reply.body);
    let page = guest.get(&format!("/invite/{code}")).await;
    let wrong = guest.get("/invite/ZZZZ-ZZZZ").await;
    assert_eq!(
        page.status, wrong.status,
        "the invitation page does not know the code"
    );

    // The agent joins through MCP and learns that the opponent is a bot.
    let mut agent = app.client().with_header("x-forwarded-for", "10.8.5.3");
    let join = mcp(
        &mut agent,
        "join",
        json!({ "code": code, "agentName": "gpt-4.5" }),
        None,
    )
    .await;
    assert_eq!(join["opponent"]["kind"], "bot");
    let token = join["token"].as_str().unwrap().to_owned();
    let joined = data_of(&admin.get(&format!("/api/show/{id}")).await);
    assert_eq!(joined["status"], "active");
    assert!(
        joined.get("invitation").is_none(),
        "the code is gone once the agent has joined"
    );
    assert_eq!(joined["players"][1]["agentName"], "gpt-4.5");

    // The agent's reads drive the bot: when the bot has black, it moves at the agent's first read.
    let mut state = mcp(&mut agent, "get_state", json!({ "token": token }), None).await;
    app.settle().await;
    state = if state["turn"] == "you" {
        state
    } else {
        let after = mcp(
            &mut agent,
            "get_state",
            json!({ "token": token, "waitSeconds": 5 }),
            None,
        )
        .await;
        assert_eq!(after["turn"], "you");
        after
    };
    assert_eq!(state["opponent"]["kind"], "bot");
    assert_eq!(state["chatStatus"], "unavailable");
    let n = state["nextMoveNumber"].as_i64().unwrap();
    let cell = (0..15)
        .flat_map(|y| (0..15).map(move |x| (x, y)))
        .find(|(x, y)| state["board"][*y].as_str().unwrap().as_bytes()[*x] == b'.')
        .unwrap();
    let moved = mcp(
        &mut agent,
        "play_move",
        json!({ "token": token, "x": cell.0, "y": cell.1, "moveNumber": n }),
        None,
    )
    .await;
    assert_eq!(moved["nextMoveNumber"], n + 1);

    // The administrator ends it: no result, no counter, the agent reads "cancelled".
    let ended = admin.post(&format!("/api/show/{id}/end"), json!({})).await;
    assert_eq!(ended.status, StatusCode::OK, "{}", ended.body);
    let ended = data_of(&ended);
    assert_eq!(ended["status"], "cancelled");
    assert_eq!(ended["closedReason"], "ended");
    let again = admin.post(&format!("/api/show/{id}/end"), json!({})).await;
    assert_eq!(again.status, StatusCode::OK);
    let seen = mcp(&mut agent, "get_state", json!({ "token": token }), None).await;
    assert_eq!(seen["status"], "cancelled");
    let counted: i64 = sqlx::query_scalar("SELECT count(*) FROM game_pair_counters")
        .fetch_one(app.db.pool())
        .await
        .unwrap();
    assert_eq!(counted, 0);
    // The administrator's end of an unknown or non-show game is the same 404.
    assert_eq!(
        admin
            .post(
                "/api/show/6f1c1a52-0f43-4a7e-9c2c-1d0c2a1f9a10/end",
                json!({})
            )
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        admin.get(&format!("/api/show/{id}?after=x")).await.status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        admin.get(&format!("/api/show/{id}?after=999")).await.status,
        StatusCode::BAD_REQUEST
    );
}

async fn mcp(client: &mut Client, name: &str, args: Value, _unused: Option<()>) -> Value {
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": name, "arguments": args } });
    let request = Request::post("/api/mcp")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::HOST, "localhost:3000")
        .header(header::ACCEPT, "application/json, text/event-stream");
    let reply = client.send(request, Body::from(body.to_string())).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let result = reply.json()["result"].clone();
    assert!(result["isError"] != true, "{result}");
    result["structuredContent"].clone()
}

#[tokio::test]
async fn an_unanswered_invitation_expires_and_frees_the_show_slot() {
    let app = plain_app().await;
    let mut admin = admin(&app).await;
    let created = data_of(
        &admin
            .post("/api/show", json!({ "layout": "bot-agent", "botLevel": 1 }))
            .await,
    );
    let id = created["gameId"].as_str().unwrap().to_owned();
    app.advance_seconds(31 * 60);
    let state = data_of(&admin.get(&format!("/api/show/{id}")).await);
    assert_eq!(state["status"], "cancelled");
    assert_eq!(state["closedReason"], "expired");
    assert!(state.get("invitation").is_none());
    assert_eq!(
        data_of(&admin.get("/api/show").await)["current"],
        Value::Null
    );
}

#[tokio::test]
async fn the_show_pages_render_for_the_administrator() {
    let app = plain_app().await;
    let mut admin = admin(&app).await;
    let console = admin.get("/show").await;
    assert_eq!(console.status, StatusCode::OK);
    assert!(
        console.body.contains("show-console.js") && console.body.contains("show.console.start")
    );
    assert!(
        console
            .body
            .contains(r#"name="robots" content="noindex,nofollow""#)
    );
    let created = data_of(
        &admin
            .post(
                "/api/show",
                json!({ "layout": "bot-bot", "levels": [1, 1] }),
            )
            .await,
    );
    let id = created["gameId"].as_str().unwrap();
    let room = admin.get(&format!("/show/{id}")).await;
    assert_eq!(room.status, StatusCode::OK);
    assert!(room.body.contains("show-room.js") && room.body.contains(id));
    assert!(
        !room.body.contains("<title>Pokaz"),
        "no title names the show"
    );
}

#[tokio::test]
async fn a_show_bot_waits_its_pause_and_a_silent_drive_is_reported_as_stalled() {
    let app = plain_app().await;
    let boss = register(&app, "10.8.9.1", ADMIN, "Szef", true).await;
    let overrides: Vec<(&str, &str)> = SHOW
        .iter()
        .copied()
        .filter(|(k, _)| !k.starts_with("BOT_SHOW_PAUSE"))
        .chain([
            ("BOT_SHOW_PAUSE_MIN_MS", "1500"),
            ("BOT_SHOW_PAUSE_MAX_MS", "1500"),
        ])
        .collect();
    let mut admin = as_user(&app, &overrides, &boss, "10.8.9.1");
    let created = data_of(
        &admin
            .post(
                "/api/show",
                json!({ "layout": "bot-bot", "levels": [1, 1] }),
            )
            .await,
    );
    let id = created["gameId"].as_str().unwrap().to_owned();
    // No move before the pause is over (the clock is frozen, so no time has passed).
    for _ in 0..3 {
        let state = data_of(&admin.get(&format!("/api/show/{id}")).await);
        app.settle().await;
        assert_eq!(state["moveCount"], 0);
        assert_eq!(
            state["drive"],
            json!({ "botOnTurn": true, "stalled": false })
        );
    }
    // After the pause the next read moves the bot; the pause was charged to its clock.
    app.advance_seconds(2);
    data_of(&admin.get(&format!("/api/show/{id}")).await);
    app.settle().await;
    let state = data_of(&admin.get(&format!("/api/show/{id}")).await);
    assert_eq!(state["moveCount"], 1);
    assert_eq!(state["moves"].as_array().unwrap().len(), 1);
    let later = data_of(&admin.get(&format!("/api/show/{id}?after=1")).await);
    assert_eq!(
        later["moves"],
        json!([]),
        "only the moves after `after` are sent"
    );
    // Nobody reads for a long time: the next read says the drive stalled (the bot has been on turn too long).
    app.advance_seconds(60);
    let state = data_of(&admin.get(&format!("/api/show/{id}")).await);
    assert_eq!(state["drive"]["botOnTurn"], true);
    assert_eq!(state["drive"]["stalled"], true);
}
