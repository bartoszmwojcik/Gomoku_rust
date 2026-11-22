//! The MCP endpoint end to end over a real PostgreSQL: the transport, the guard, the four tools and a whole game
//! between an agent and a browser.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{Client, Reply, TestApp};
use serde_json::{Value, json};

const PASSWORD: &str = "correct horse";

async fn app() -> TestApp {
    TestApp::start_with(&[("TRUST_PROXY_HOPS", "1"), ("MCP_POLL_INTERVAL_MS", "20")]).await
}

fn browser(app: &TestApp, ip: &str) -> Client {
    app.client().with_header("x-forwarded-for", ip)
}

fn data_of(reply: &Reply) -> Value {
    let body = reply.json();
    assert_eq!(body["ok"], true, "{}", reply.body);
    body["data"].clone()
}

async fn rpc_raw(client: &mut Client, body: &Value, extra: &[(&str, &str)]) -> Reply {
    let mut request = Request::post("/api/mcp")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::HOST, "localhost:3000")
        .header(header::ACCEPT, "application/json, text/event-stream");
    for (name, value) in extra {
        request = request.header(*name, *value);
    }
    client.send(request, Body::from(body.to_string())).await
}

async fn rpc(client: &mut Client, method: &str, params: Value) -> Value {
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    let reply = rpc_raw(client, &body, &[]).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    reply.json()
}

/// Calls a tool; returns `(data or error object, is_error)`.
async fn tool(client: &mut Client, name: &str, args: Value) -> (Value, bool) {
    let response = rpc(
        client,
        "tools/call",
        json!({ "name": name, "arguments": args }),
    )
    .await;
    let result = &response["result"];
    assert!(result.is_object(), "{response}");
    let structured = result["structuredContent"].clone();
    let is_error = result["isError"] == true;
    // The text mirror is the compact JSON of the same object.
    assert_eq!(
        serde_json::from_str::<Value>(result["content"][0]["text"].as_str().unwrap()).unwrap(),
        structured
    );
    if is_error {
        (structured["error"].clone(), true)
    } else {
        (structured, false)
    }
}

async fn ok(client: &mut Client, name: &str, args: Value) -> Value {
    let (data, is_error) = tool(client, name, args).await;
    assert!(!is_error, "{data}");
    data
}

async fn err(client: &mut Client, name: &str, args: Value) -> Value {
    let (data, is_error) = tool(client, name, args).await;
    assert!(is_error, "expected a failure, got {data}");
    data
}

async fn guest_game(app: &TestApp, ip: &str) -> (Client, String, String) {
    let mut client = browser(app, ip);
    let reply = client
        .post("/api/games", json!({ "acknowledgedGuestWarning": true }))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    let data = data_of(&reply);
    (
        client,
        data["gameId"].as_str().unwrap().to_owned(),
        data["invitation"]["code"].as_str().unwrap().to_owned(),
    )
}

async fn account_game(app: &TestApp, ip: &str, name: &str) -> (Client, String, String) {
    let mut client = browser(app, ip);
    let reply = client
        .post(
            "/api/auth/register",
            json!({ "email": format!("{}@example.com", name.to_lowercase()), "displayName": name, "password": PASSWORD }),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    let reply = client.post("/api/games", json!({})).await;
    let data = data_of(&reply);
    (
        client,
        data["gameId"].as_str().unwrap().to_owned(),
        data["invitation"]["code"].as_str().unwrap().to_owned(),
    )
}

/// An agent in a fresh game with a guest host who already opened it (so the clock runs).
async fn started(
    app: &TestApp,
    host_ip: &str,
    agent_ip: &str,
) -> (Client, Client, String, String, Value) {
    let (mut host, id, code) = guest_game(app, host_ip).await;
    let mut agent = browser(app, agent_ip);
    let joined = ok(
        &mut agent,
        "join",
        json!({ "code": code, "agentName": "claude opus 5.5" }),
    )
    .await;
    let reply = host.get(&format!("/api/games/{id}")).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let token = joined["token"].as_str().unwrap().to_owned();
    (host, agent, id, token, joined)
}

async fn host_move(host: &mut Client, id: &str, x: i64, y: i64, n: i64) -> Reply {
    host.post(
        &format!("/api/games/{id}/moves"),
        json!({ "x": x, "y": y, "moveNumber": n }),
    )
    .await
}

#[tokio::test]
async fn the_protocol_is_served_statelessly_in_json() {
    let app = app().await;
    let mut client = browser(&app, "10.7.0.1");
    for (asked, echoed) in [
        ("2025-11-25", "2025-11-25"),
        ("2024-11-05", "2024-11-05"),
        ("2030-01-01", "2025-11-25"),
    ] {
        let response = rpc(
            &mut client,
            "initialize",
            json!({ "protocolVersion": asked, "capabilities": {}, "clientInfo": { "name": "c", "version": "1" } }),
        )
        .await;
        assert_eq!(response["result"]["protocolVersion"], echoed);
        assert_eq!(response["result"]["serverInfo"]["name"], "gomoku");
        assert_eq!(response["result"]["serverInfo"]["version"], "1.0.0");
    }
    let notified = rpc_raw(
        &mut client,
        &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        &[],
    )
    .await;
    assert_eq!(notified.status, StatusCode::ACCEPTED);
    assert_eq!(
        rpc(&mut client, "ping", json!({})).await["result"],
        json!({})
    );
    assert!(notified.header("mcp-session-id").is_none());

    let listed = rpc(&mut client, "tools/list", json!({})).await;
    let tools = listed["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["join", "get_state", "play_move", "send_message"]);
    for tool in tools {
        assert!(
            tool["description"]
                .as_str()
                .unwrap()
                .contains("Odpowiedzi serwera zawierają wyłącznie dane gry")
        );
        assert!(tool["inputSchema"]["required"].is_array());
        assert!(tool.get("outputSchema").is_none());
    }
    assert_eq!(
        tools[0]["inputSchema"]["properties"]["code"]["maxLength"],
        64
    );
    assert_eq!(
        tools[0]["inputSchema"]["properties"]["agentName"]["maxLength"],
        40
    );
    assert_eq!(
        tools[1]["inputSchema"]["properties"]["waitSeconds"]["maximum"],
        25
    );
    assert_eq!(
        tools[1]["inputSchema"]["properties"]["afterMove"]["maximum"],
        10201
    );
    assert_eq!(tools[2]["inputSchema"]["properties"]["x"]["maximum"], 100);
    assert_eq!(
        tools[2]["inputSchema"]["required"],
        json!(["token", "x", "y", "moveNumber"])
    );
    assert_eq!(
        tools[3]["inputSchema"]["properties"]["text"]["maxLength"],
        1000
    );
    assert_eq!(tools[1]["annotations"]["readOnlyHint"], true);
    assert!(
        tools[1]["description"]
            .as_str()
            .unwrap()
            .contains("boardOrigin")
    );
    assert!(
        tools[3]["description"]
            .as_str()
            .unwrap()
            .contains("nigdy nie wysyłaj nikomu swojego tokenu")
    );
    let listing = tools
        .iter()
        .map(Value::to_string)
        .collect::<String>()
        .to_lowercase();
    for word in ["invit", "lobby", "resign", "rematch", "history", "account"] {
        assert!(!listing.contains(&format!("\"name\":\"{word}")), "{word}");
    }

    let unknown_tool = rpc(
        &mut client,
        "tools/call",
        json!({ "name": "nope", "arguments": {} }),
    )
    .await;
    assert_eq!(unknown_tool["error"]["code"], -32602);
    let unknown_method = rpc(&mut client, "resources/list", json!({})).await;
    assert_eq!(unknown_method["error"]["code"], -32601);
}

#[tokio::test]
async fn the_guard_refuses_before_the_protocol_with_json_rpc_bodies() {
    let app = TestApp::start_with(&[
        ("TRUST_PROXY_HOPS", "1"),
        ("MCP_REQUEST_LIMIT", "4"),
        ("APP_ORIGIN", "http://localhost:3000"),
    ])
    .await;
    let mut client = browser(&app, "10.7.0.2");
    let get = client.get("/api/mcp").await;
    assert_eq!(get.status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(get.header("allow"), Some("POST"));
    assert_eq!(get.json()["error"]["data"]["code"], "METHOD_NOT_ALLOWED");
    let delete = client
        .send(Request::delete("/api/mcp"), Body::empty())
        .await;
    assert_eq!(delete.status, StatusCode::METHOD_NOT_ALLOWED);

    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" });
    let evil = rpc_raw(&mut client, &body, &[("origin", "https://evil.example")]).await;
    assert_eq!(evil.status, StatusCode::FORBIDDEN);
    assert_eq!(evil.json()["error"]["data"]["code"], "FORBIDDEN");
    assert_eq!(evil.json()["id"], Value::Null);
    let own = rpc_raw(&mut client, &body, &[("origin", "http://localhost:3000")]).await;
    assert_eq!(own.status, StatusCode::OK, "{}", own.body);
    let none = rpc_raw(&mut client, &body, &[]).await;
    assert_eq!(none.status, StatusCode::OK);
    // The refused origin and the two methods were not counted: 2 of 4 used so far.
    for _ in 0..2 {
        assert_eq!(
            rpc_raw(&mut client, &body, &[]).await.status,
            StatusCode::OK
        );
    }
    let limited = rpc_raw(&mut client, &body, &[]).await;
    assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(
        limited
            .header("retry-after")
            .unwrap()
            .parse::<u64>()
            .unwrap()
            >= 1
    );
    assert_eq!(limited.json()["error"]["data"]["code"], "RATE_LIMITED");

    // Without APP_ORIGIN only a localhost origin is accepted.
    let dev = TestApp::start_with(&[("TRUST_PROXY_HOPS", "1")]).await;
    let mut client = browser(&dev, "10.7.0.3");
    let local = rpc_raw(&mut client, &body, &[("origin", "http://localhost:8080")]).await;
    assert_eq!(local.status, StatusCode::OK);
    let other = rpc_raw(
        &mut client,
        &body,
        &[("origin", "http://localhost.evil.example:80")],
    )
    .await;
    assert_eq!(other.status, StatusCode::FORBIDDEN);
    let weird = rpc_raw(&mut client, &body, &[("origin", "http://localhost:")]).await;
    assert_eq!(weird.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn an_unknown_address_is_refused_in_production() {
    let app = TestApp::start_with(&[
        ("APP_ENV", "production"),
        ("APP_ORIGIN", "https://gomoku.example"),
        ("TRUST_PROXY_HOPS", "1"),
    ])
    .await;
    let mut client = app.client();
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" });
    let refused = rpc_raw(&mut client, &body, &[]).await;
    assert_eq!(refused.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        refused.json()["error"]["data"]["code"],
        "ADDRESS_UNAVAILABLE"
    );
    let mut known = app.client().with_header("x-forwarded-for", "10.7.0.4");
    assert_eq!(rpc_raw(&mut known, &body, &[]).await.status, StatusCode::OK);
}

#[tokio::test]
async fn bad_arguments_always_end_as_one_tool_error() {
    let app = app().await;
    let mut client = browser(&app, "10.7.0.5");
    let token = "a".repeat(43);
    let e = err(&mut client, "get_state", json!({})).await;
    assert_eq!(e["code"], "INVALID_INPUT");
    assert_eq!(e["message"], "Nieprawidłowe pole: token.");
    assert_eq!(e["retryable"], false);
    assert_eq!(
        err(&mut client, "get_state", json!({ "token": "x" })).await["message"],
        "Nieprawidłowe pole: token."
    );
    assert_eq!(
        err(
            &mut client,
            "get_state",
            json!({ "token": token, "waitSeconds": 26 })
        )
        .await["message"],
        "Nieprawidłowe pole: waitSeconds."
    );
    assert_eq!(
        err(&mut client, "play_move", json!({ "token": token, "x": 1 })).await["message"],
        "Nieprawidłowe pole: y."
    );
    assert_eq!(
        err(
            &mut client,
            "play_move",
            json!({ "token": token, "x": 1.5, "y": 1, "moveNumber": 1 })
        )
        .await["message"],
        "Nieprawidłowe pole: x."
    );
    // A call without `arguments` reaches the tool too.
    let response = rpc(&mut client, "tools/call", json!({ "name": "get_state" })).await;
    assert_eq!(
        response["result"]["structuredContent"]["error"]["code"],
        "INVALID_INPUT"
    );
    // Well-formed but unknown tokens.
    for (name, args) in [
        ("get_state", json!({ "token": token })),
        (
            "play_move",
            json!({ "token": token, "x": 1, "y": 1, "moveNumber": 1 }),
        ),
        ("send_message", json!({ "token": token, "text": "hi" })),
    ] {
        let e = err(&mut client, name, args).await;
        assert_eq!(e["code"], "INVALID_TOKEN", "{name}");
        assert_eq!(e["message"], "Nieprawidłowy lub wygasły token.");
    }
    // Strangers create no counters.
    let counters: i64 = sqlx::query_scalar("SELECT count(*) FROM rate_limit_buckets WHERE key LIKE 'mcp-token:%' OR key LIKE 'mcp-chat:%'")
        .fetch_one(app.db.pool())
        .await
        .unwrap();
    assert_eq!(counters, 0);
}

#[tokio::test]
async fn joining_seats_the_agent_and_keeps_a_wrong_name_from_spending_the_code() {
    let app = app().await;
    let (mut host, id, code) = guest_game(&app, "10.7.1.1").await;
    let mut agent = browser(&app, "10.7.1.2");
    for args in [
        json!({}),
        json!({ "code": 5 }),
        json!({ "code": "   " }),
        json!({ "code": "x".repeat(65) }),
        json!({ "code": code, "agentName": "кириллица" }),
        json!({ "code": code, "agentName": 7 }),
        json!({ "code": code, "agentName": "evil.com" }),
    ] {
        let e = err(&mut agent, "join", args).await;
        assert_eq!(e["code"], "INVALID_INPUT");
        assert!(
            e["message"]
                .as_str()
                .unwrap()
                .starts_with("Nieprawidłowe dane. Podaj code")
        );
        assert!(e.get("reason").is_none());
    }
    // Nothing was counted for those.
    let counted: i64 = sqlx::query_scalar("SELECT count(*) FROM rate_limit_buckets WHERE key LIKE 'mcp-join:%' OR key LIKE 'agent-lookup:%'")
        .fetch_one(app.db.pool())
        .await
        .unwrap();
    assert_eq!(counted, 0);

    let link = format!("https://gomoku.example/invite/{code}");
    let joined = ok(
        &mut agent,
        "join",
        json!({ "code": link, "agentName": "  claude   opus 5.5 " }),
    )
    .await;
    assert_eq!(joined["token"].as_str().unwrap().len(), 43);
    assert_eq!(joined["gameId"], id);
    assert_eq!(joined["you"]["seat"], 2);
    assert_eq!(joined["status"], "active");
    assert_eq!(joined["opponent"]["kind"], "guest");
    assert!(["black", "white"].contains(&joined["you"]["color"].as_str().unwrap()));
    // Only the hash is stored.
    let token = joined["token"].as_str().unwrap();
    let stored: String =
        sqlx::query_scalar("SELECT agent_token_hash FROM game_players WHERE kind = 'agent'")
            .fetch_one(app.db.pool())
            .await
            .unwrap();
    assert_eq!(stored.len(), 64);
    assert_ne!(stored, token);
    let name: String =
        sqlx::query_scalar("SELECT agent_name FROM game_players WHERE kind = 'agent'")
            .fetch_one(app.db.pool())
            .await
            .unwrap();
    assert_eq!(name, "claude opus 5.5");

    // The code is used up: a second join (an agent or the browser) is the uniform invalid answer.
    let again = err(&mut agent, "join", json!({ "code": code })).await;
    assert_eq!(again["code"], "INVALID_INVITATION");
    assert_eq!(again["message"], "To zaproszenie jest już nieważne.");
    let mut browser2 = browser(&app, "10.7.1.3");
    let reply = browser2
        .post(
            &format!("/api/invitations/{code}/accept"),
            json!({ "as": "guest", "acknowledgedGuestWarning": true }),
        )
        .await;
    assert_eq!(reply.status, StatusCode::GONE, "{}", reply.body);

    // The host sees the agent, with the declared name and no token.
    let view = data_of(&host.get(&format!("/api/games/{id}")).await);
    let players = view["players"].as_array().unwrap();
    let agent_player = players.iter().find(|p| p["kind"] == "agent").unwrap();
    assert_eq!(agent_player["agentName"], "claude opus 5.5");
    assert_eq!(agent_player["displayName"], Value::Null);
    assert!(!view.to_string().contains(token));
}

#[tokio::test]
async fn an_unreadable_code_is_the_same_failure_and_spends_a_lookup_attempt() {
    let app = TestApp::start_with(&[
        ("TRUST_PROXY_HOPS", "1"),
        ("MCP_LOOKUP_LIMIT", "3"),
        ("MCP_POLL_INTERVAL_MS", "20"),
    ])
    .await;
    let (_host, _id, code) = guest_game(&app, "10.7.2.1").await;
    let mut agent = browser(&app, "10.7.2.2");
    for text in [
        "ZZZZ-ZZZZ",
        "no code here",
        "https://x/invite/",
        "ABCD-EFGH WXYZ-WXYZ",
    ] {
        let e = err(&mut agent, "join", json!({ "code": text })).await;
        // The fourth attempt meets the spent pool (3 allowed): rate limited even for a text, then for a good code.
        assert!(
            ["INVALID_INVITATION", "RATE_LIMITED"].contains(&e["code"].as_str().unwrap()),
            "{e}"
        );
    }
    let e = err(&mut agent, "join", json!({ "code": code })).await;
    assert_eq!(e["code"], "RATE_LIMITED");
    assert_eq!(e["message"], "Zbyt wiele prób. Spróbuj ponownie później.");
    assert_eq!(e["retryable"], true);
    assert!(e["retryAfterMs"].as_u64().unwrap() >= 1000);
    // Another address is unaffected.
    let mut other = browser(&app, "10.7.2.9");
    ok(&mut other, "join", json!({ "code": code })).await;
}

#[tokio::test]
async fn only_successful_joins_use_up_the_hourly_join_quota() {
    let app = TestApp::start_with(&[
        ("TRUST_PROXY_HOPS", "1"),
        ("MCP_JOIN_LIMIT", "2"),
        ("MCP_POLL_INTERVAL_MS", "20"),
    ])
    .await;
    let mut agent = browser(&app, "10.7.3.2");
    let failed = err(&mut agent, "join", json!({ "code": "ZZZZ-ZZZZ" })).await;
    assert_eq!(failed["code"], "INVALID_INVITATION");
    for n in 0..2u8 {
        let (_host, _id, code) = guest_game(&app, &format!("10.7.3.{}", 10 + n)).await;
        ok(&mut agent, "join", json!({ "code": code })).await;
    }
    let (_host, _id, code) = guest_game(&app, "10.7.3.20").await;
    let e = err(&mut agent, "join", json!({ "code": code })).await;
    assert_eq!(e["code"], "RATE_LIMITED");
}

#[tokio::test]
async fn a_whole_game_between_an_agent_and_a_browser() {
    let app = app().await;
    let (mut host, mut agent, id, token, joined) = started(&app, "10.7.4.1", "10.7.4.2").await;
    let me_black = joined["you"]["color"] == "black";
    let state = ok(&mut agent, "get_state", json!({ "token": token })).await;
    assert_eq!(state["status"], "active");
    assert_eq!(state["opponent"], json!({ "kind": "guest" }));
    assert_eq!(state["nextMoveNumber"], 1);
    assert_eq!(state["board"].as_array().unwrap().len(), 15);
    assert_eq!(state["board"][0], "...............");
    assert_eq!(state["turn"], if me_black { "you" } else { "opponent" });
    assert_eq!(state["clock"]["runningSide"], state["turn"]);
    assert_eq!(state["result"], Value::Null);
    assert_eq!(state["chatStatus"], "open");
    assert_eq!(state["chat"], json!([]));
    assert!(state["chatNotice"].as_str().unwrap().contains("untrusted"));
    assert!(state.get("variant").is_none());
    let text = state.to_string();
    assert!(
        !text.contains("public") && !text.contains("spectator") && !text.contains("displayName")
    );

    // Black builds a row on y = 3, white answers on y = 5; black's fifth stone wins.
    let row = |y: i64| -> Vec<(i64, i64)> { (0..5).map(|i| (3 + i, y)).collect() };
    let (black_cells, white_cells) = (row(3), row(5));
    let mut number = 1;
    for i in 0..5 {
        for (is_black, cells) in [(true, &black_cells), (false, &white_cells)] {
            if !is_black && i == 4 {
                break;
            }
            let (x, y) = cells[i];
            if is_black == me_black {
                let moved = ok(
                    &mut agent,
                    "play_move",
                    json!({ "token": token, "x": x, "y": y, "moveNumber": number }),
                )
                .await;
                assert_eq!(moved["nextMoveNumber"], number + 1);
                assert!(moved.get("chat").is_none());
                assert_eq!(moved["moves"].as_array().unwrap().len() as i64, number);
            } else {
                let reply = host_move(&mut host, &id, x, y, number).await;
                assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
            }
            number += 1;
        }
    }
    let end = ok(
        &mut agent,
        "get_state",
        json!({ "token": token, "afterMove": 8 }),
    )
    .await;
    assert_eq!(end["status"], "finished");
    assert_eq!(
        end["result"]["outcome"],
        if me_black { "win" } else { "loss" }
    );
    assert_eq!(end["result"]["reason"], "five_in_row");
    assert_eq!(end["result"]["winLine"].as_array().unwrap().len(), 5);
    assert_eq!(end["turn"], Value::Null);
    assert_eq!(end["retryAfterMs"], Value::Null);
    assert_eq!(end["clock"]["runningSide"], Value::Null);
    assert_eq!(end["moves"].as_array().unwrap().len(), 1);
    let late = err(
        &mut agent,
        "play_move",
        json!({ "token": token, "x": 0, "y": 0, "moveNumber": 11 }),
    )
    .await;
    assert_eq!(late["code"], "GAME_FINISHED");
    assert_eq!(late["retryable"], false);
    // A rematch is never offered with an agent.
    let rematch = host
        .post(&format!("/api/games/{id}/rematch"), json!({}))
        .await;
    assert_eq!(rematch.status, StatusCode::CONFLICT, "{}", rematch.body);
    assert_eq!(rematch.json()["error"]["code"], "REMATCH_UNAVAILABLE");
    assert_eq!(rematch.json()["error"]["data"]["reason"], "agent");
}

#[tokio::test]
async fn rejected_moves_change_nothing() {
    let app = app().await;
    let (mut host, mut agent, id, token, joined) = started(&app, "10.7.5.1", "10.7.5.2").await;
    let me_black = joined["you"]["color"] == "black";
    if !me_black {
        let reply = host_move(&mut host, &id, 7, 7, 1).await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    }
    let n = if me_black { 1 } else { 2 };
    let snapshot = || async {
        let row: (i32, i32, i32, Option<time::OffsetDateTime>) = sqlx::query_as(
            "SELECT move_count::int4, clock_ms_1, clock_ms_2, turn_started_at FROM games",
        )
        .fetch_one(app.db.pool())
        .await
        .unwrap();
        row
    };
    let before = snapshot().await;
    let cases = [
        (
            json!({ "x": 15, "y": 0, "moveNumber": n }),
            "ILLEGAL_MOVE",
            Some("out_of_board"),
            "Pole jest poza planszą (współrzędne 0–14).",
        ),
        (
            json!({ "x": -1, "y": 0, "moveNumber": n }),
            "ILLEGAL_MOVE",
            Some("out_of_board"),
            "Pole jest poza planszą (współrzędne 0–14).",
        ),
        (
            json!({ "x": 0, "y": 0, "moveNumber": n + 3 }),
            "STALE_MOVE",
            None,
            "Plansza zmieniła się — odśwież widok.",
        ),
    ];
    for (args, code, reason, message) in cases {
        let mut full = args.clone();
        full["token"] = json!(token);
        let e = err(&mut agent, "play_move", full).await;
        assert_eq!(e["code"], code);
        assert_eq!(e["message"], message);
        assert_eq!(e.get("reason").and_then(Value::as_str), reason);
    }
    if !me_black {
        let e = err(
            &mut agent,
            "play_move",
            json!({ "token": token, "x": 7, "y": 7, "moveNumber": 2 }),
        )
        .await;
        assert_eq!(e["code"], "ILLEGAL_MOVE");
        assert_eq!(e["reason"], "cell_occupied");
        assert_eq!(e["message"], "To pole jest już zajęte.");
    }
    // Not the agent's turn: play once, then again out of turn.
    ok(
        &mut agent,
        "play_move",
        json!({ "token": token, "x": 1, "y": 1, "moveNumber": n }),
    )
    .await;
    let out_of_turn = err(
        &mut agent,
        "play_move",
        json!({ "token": token, "x": 2, "y": 2, "moveNumber": n + 1 }),
    )
    .await;
    assert_eq!(out_of_turn["code"], "NOT_YOUR_TURN");
    assert_eq!(out_of_turn["retryable"], true);
    assert_eq!(out_of_turn["message"], "Teraz ruch przeciwnika.");
    let after = snapshot().await;
    assert_eq!(after.0, before.0 + 1, "only the legal move was stored");
    let unchanged = (before.1, before.2) == (after.1, after.2) || after.0 == before.0 + 1;
    assert!(unchanged);
}

#[tokio::test]
async fn a_waiting_get_state_wakes_on_the_turn_and_on_a_message() {
    let app = app().await;
    let (mut host, mut agent, id, token, joined) = started(&app, "10.7.6.1", "10.7.6.2").await;
    let me_black = joined["you"]["color"] == "black";
    if me_black {
        ok(
            &mut agent,
            "play_move",
            json!({ "token": token, "x": 7, "y": 7, "moveNumber": 1 }),
        )
        .await;
    }
    // The agent waits for its turn; the host moves a little later.
    let mut waiting_client = browser(&app, "10.7.6.3");
    let waiting = {
        let token = token.clone();
        tokio::spawn(async move {
            let started = std::time::Instant::now();
            let state = ok(
                &mut waiting_client,
                "get_state",
                json!({ "token": token, "waitSeconds": 20 }),
            )
            .await;
            (state, started.elapsed())
        })
    };
    tokio::time::sleep(Duration::from_millis(200)).await;
    let n = if me_black { 2 } else { 1 };
    let reply = host_move(&mut host, &id, 3, 3, n).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let (state, elapsed) = waiting.await.unwrap();
    assert_eq!(state["turn"], "you");
    assert!(elapsed < Duration::from_secs(10), "{elapsed:?}");

    // Without a cursor a message does not wake the wait; with one it does.
    ok(
        &mut agent,
        "play_move",
        json!({ "token": token, "x": 9, "y": 9, "moveNumber": n + 1 }),
    )
    .await;
    let cursor = ok(&mut agent, "get_state", json!({ "token": token })).await["chatLastId"].clone();
    assert_eq!(cursor, 0);
    let mut waiting_client = browser(&app, "10.7.6.4");
    let waiting = {
        let token = token.clone();
        tokio::spawn(async move {
            ok(
                &mut waiting_client,
                "get_state",
                json!({ "token": token, "waitSeconds": 20, "afterMessage": 0 }),
            )
            .await
        })
    };
    tokio::time::sleep(Duration::from_millis(200)).await;
    let reply = host
        .post(
            &format!("/api/games/{id}/chat"),
            json!({ "text": "hej agencie", "clientId": "6f1c1a52-0f43-4a7e-9c2c-1d0c2a1f9a10" }),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    let state = waiting.await.unwrap();
    assert_eq!(state["chat"][0]["from"], "opponent");
    assert_eq!(state["chat"][0]["text"], "hej agencie");
    assert_eq!(state["chat"][0]["untrusted"], true);
    assert_eq!(state["chatLastId"], 1);
    assert_eq!(state["turn"], "opponent");

    // afterMove past the number of moves is refused after the read.
    let e = err(
        &mut agent,
        "get_state",
        json!({ "token": token, "afterMove": 99 }),
    )
    .await;
    assert_eq!(e["message"], "Nieprawidłowe pole: afterMove.");
    // The wait ends at once when the game is over (here: resigned by the host).
    let reply = host
        .post(&format!("/api/games/{id}/resign"), json!({}))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let over = ok(
        &mut agent,
        "get_state",
        json!({ "token": token, "waitSeconds": 20 }),
    )
    .await;
    assert_eq!(over["result"]["reason"], "resignation");
    assert_eq!(over["result"]["outcome"], "win");
}

#[tokio::test]
async fn the_agent_chats_within_the_rules_of_the_chat() {
    let app = TestApp::start_with(&[
        ("TRUST_PROXY_HOPS", "1"),
        ("MCP_POLL_INTERVAL_MS", "20"),
        ("CHAT_AGENT_MIN_INTERVAL_MS", "0"),
        ("CHAT_AGENT_MAX_LENGTH", "40"),
        ("CHAT_MAX_MESSAGES_PER_GAME", "4"),
    ])
    .await;
    let (mut host, mut agent, id, token, _joined) = started(&app, "10.7.7.1", "10.7.7.2").await;
    let sent = ok(
        &mut agent,
        "send_message",
        json!({ "token": token, "text": "  Ｈello  " }),
    )
    .await;
    assert_eq!(sent["message"]["from"], "you");
    assert_eq!(sent["message"]["text"], "Hello");
    assert_eq!(sent["message"]["untrusted"], false);
    assert_eq!(sent["message"]["id"], 1);
    assert_eq!(sent["chatStatus"], "open");
    assert_eq!(sent["retryAfterMs"], 0);

    for (text, reason) in [
        (json!("   "), "empty"),
        (json!("x ".repeat(25)), "too_long"),
        (json!("see https://example.com"), "url"),
        (json!("tab\there"), "control_char"),
        (json!(5), "not_text"),
        (Value::Null, "not_text"),
    ] {
        let e = err(
            &mut agent,
            "send_message",
            json!({ "token": token, "text": text }),
        )
        .await;
        assert_eq!(e["code"], "INVALID_INPUT", "{reason}");
        assert_eq!(e["reason"], reason);
    }
    let missing = rpc(
        &mut agent,
        "tools/call",
        json!({ "name": "send_message", "arguments": { "token": token } }),
    )
    .await;
    assert_eq!(
        missing["result"]["structuredContent"]["error"]["reason"],
        "not_text"
    );
    for text in [
        "x".repeat(201),
        format!("my token {token}"),
        "a-b_".repeat(10),
    ] {
        let e = err(
            &mut agent,
            "send_message",
            json!({ "token": token, "text": text }),
        )
        .await;
        assert_eq!(e["reason"], "secret", "{text}");
        assert_eq!(
            e["message"],
            "Wiadomość wygląda na token lub inny sekret, więc nie została wysłana."
        );
    }
    // The host sees the message signed by the agent; spectator lines would arrive as `spectator`.
    let chat = data_of(&host.get(&format!("/api/games/{id}/chat")).await);
    assert_eq!(chat["messages"][0]["text"], "Hello");
    for text in ["dwa", "trzy", "cztery"] {
        let result = tool(
            &mut agent,
            "send_message",
            json!({ "token": token, "text": text }),
        )
        .await;
        if result.1 {
            assert_eq!(result.0["code"], "CHAT_FULL");
            assert_eq!(
                result.0["message"],
                "W tej partii jest już maksymalna liczba wiadomości."
            );
        }
    }
    let full = err(
        &mut agent,
        "send_message",
        json!({ "token": token, "text": "piąty" }),
    )
    .await;
    assert_eq!(full["code"], "CHAT_FULL");
    let read = ok(
        &mut agent,
        "get_state",
        json!({ "token": token, "afterMessage": 2 }),
    )
    .await;
    assert_eq!(read["chat"].as_array().unwrap().len(), 2);
    assert_eq!(read["chat"][0]["from"], "you");
    assert_eq!(read["chat"][0]["untrusted"], false);

    // The host turns the chat off: the agent hears it.
    let off = host
        .put(
            &format!("/api/games/{id}/chat/settings"),
            json!({ "enabled": false }),
        )
        .await;
    assert_eq!(off.status, StatusCode::OK, "{}", off.body);
    let e = err(
        &mut agent,
        "send_message",
        json!({ "token": token, "text": "halo" }),
    )
    .await;
    assert_eq!(e["code"], "CHAT_DISABLED");
    assert_eq!(e["message"], "Czat jest wyłączony.");
    let state = ok(&mut agent, "get_state", json!({ "token": token })).await;
    assert_eq!(state["chatStatus"], "disabled_by_opponent");
}

#[tokio::test]
async fn the_chat_interval_and_the_token_pools_are_separate() {
    let app = TestApp::start_with(&[
        ("TRUST_PROXY_HOPS", "1"),
        ("MCP_POLL_INTERVAL_MS", "20"),
        ("MCP_TOKEN_LIMIT", "3"),
        ("MCP_CHAT_LIMIT", "2"),
    ])
    .await;
    let (_host, mut agent, _id, token, _joined) = started(&app, "10.7.8.1", "10.7.8.2").await;
    ok(
        &mut agent,
        "send_message",
        json!({ "token": token, "text": "pierwsza" }),
    )
    .await;
    let e = err(
        &mut agent,
        "send_message",
        json!({ "token": token, "text": "druga" }),
    )
    .await;
    assert_eq!(e["code"], "RATE_LIMITED");
    assert_eq!(e["message"], "Poczekaj chwilę przed następną wiadomością.");
    assert!(e["retryAfterMs"].as_u64().unwrap() > 0);
    // The chat pool (2 per minute) is spent by now, apart from the game pool (3).
    let limited = err(
        &mut agent,
        "send_message",
        json!({ "token": token, "text": "trzecia" }),
    )
    .await;
    assert_eq!(limited["code"], "RATE_LIMITED");
    for _ in 0..3 {
        ok(&mut agent, "get_state", json!({ "token": token })).await;
    }
    let over = err(&mut agent, "get_state", json!({ "token": token })).await;
    assert_eq!(over["code"], "RATE_LIMITED");
    assert_eq!(
        over["message"],
        "Zbyt wiele prób. Spróbuj ponownie później."
    );
}

#[tokio::test]
async fn the_token_lives_an_hour_after_the_game_and_the_cleanup_clears_it() {
    let app = app().await;
    let (mut host, mut agent, id, token, _joined) = started(&app, "10.7.9.1", "10.7.9.2").await;
    let reply = host
        .post(&format!("/api/games/{id}/resign"), json!({}))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let state = ok(&mut agent, "get_state", json!({ "token": token })).await;
    assert_eq!(state["status"], "finished");
    app.advance_seconds(59 * 60);
    ok(&mut agent, "get_state", json!({ "token": token })).await;
    app.advance_seconds(2 * 60);
    let e = err(&mut agent, "get_state", json!({ "token": token })).await;
    assert_eq!(e["code"], "INVALID_TOKEN");
    let options = gomoku_db::cleanup::CleanupOptions::new(common::now_of(&app));
    let result = gomoku_db::cleanup::cleanup_expired(&app.db, &options).await;
    // A guest-only game is removed whole, token and all.
    assert_eq!(result.guest_only_finished_games, 1, "{result:?}");
}

#[tokio::test]
async fn an_account_host_is_told_apart_and_history_names_the_agent() {
    let app = app().await;
    let (mut host, id, code) = account_game(&app, "10.7.10.1", "Ala").await;
    let mut agent = browser(&app, "10.7.10.2");
    let joined = ok(
        &mut agent,
        "join",
        json!({ "code": code, "agentName": "gpt-4.5" }),
    )
    .await;
    assert_eq!(joined["opponent"]["kind"], "account");
    let token = joined["token"].as_str().unwrap().to_owned();
    let state = data_of(&host.get(&format!("/api/games/{id}")).await);
    assert_eq!(state["status"], "active");
    let reply = host
        .post(&format!("/api/games/{id}/resign"), json!({}))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let after = ok(&mut agent, "get_state", json!({ "token": token })).await;
    assert_eq!(after["result"]["outcome"], "win");
    let history = data_of(&host.get("/api/history").await);
    let entry = &history["entries"][0];
    assert_eq!(entry["opponent"]["kind"], "agent");
    assert_eq!(entry["opponent"]["name"], "gpt-4.5");
    // The pair counter counts it as account-agent.
    let kinds: (String, String) = sqlx::query_as("SELECT kind_a, kind_b FROM game_pair_counters")
        .fetch_one(app.db.pool())
        .await
        .unwrap();
    assert_eq!(kinds, ("account".to_owned(), "agent".to_owned()));
}

#[tokio::test]
async fn an_agent_plays_a_paper_game() {
    let app = app().await;
    let mut host = browser(&app, "10.7.11.1");
    let reply = host
        .post(
            "/api/games",
            json!({ "variant": "paper", "acknowledgedGuestWarning": true }),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    let data = data_of(&reply);
    let id = data["gameId"].as_str().unwrap().to_owned();
    let code = data["invitation"]["code"].as_str().unwrap().to_owned();
    let mut agent = browser(&app, "10.7.11.2");
    let joined = ok(&mut agent, "join", json!({ "code": code })).await;
    let token = joined["token"].as_str().unwrap().to_owned();
    host.get(&format!("/api/games/{id}")).await;
    let state = ok(&mut agent, "get_state", json!({ "token": token })).await;
    assert_eq!(state["variant"], "paper");
    assert_eq!(state["boardOrigin"], json!({ "x": 47, "y": 47 }));
    assert_eq!(state["board"].as_array().unwrap().len(), 7);
    assert_eq!(state["scores"], json!({ "you": 0, "opponent": 0 }));
    assert_eq!(state["autoSeat"], Value::Null);
    let agent_first = state["turn"] == "you";
    if agent_first {
        let far = err(
            &mut agent,
            "play_move",
            json!({ "token": token, "x": 10, "y": 10, "moveNumber": 1 }),
        )
        .await;
        assert_eq!(far["reason"], "first_move_center");
        let moved = ok(
            &mut agent,
            "play_move",
            json!({ "token": token, "x": 50, "y": 50, "moveNumber": 1 }),
        )
        .await;
        assert_eq!(moved["board"][3], "...B...");
        assert_eq!(moved["moves"][0]["auto"], false);
        assert_eq!(moved["moves"][0]["color"], "black");
        let outside = err(
            &mut agent,
            "play_move",
            json!({ "token": token, "x": 101, "y": 50, "moveNumber": 2 }),
        )
        .await;
        assert_eq!(
            outside["message"],
            "Pole jest poza planszą (współrzędne 0–100)."
        );
    } else {
        let reply = host_move(&mut host, &id, 50, 50, 1).await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
        let far = err(
            &mut agent,
            "play_move",
            json!({ "token": token, "x": 60, "y": 60, "moveNumber": 2 }),
        )
        .await;
        assert_eq!(far["reason"], "too_far");
        assert_eq!(
            far["message"],
            "Kamień może stać najwyżej 3 pola od innego."
        );
        let moved = ok(
            &mut agent,
            "play_move",
            json!({ "token": token, "x": 51, "y": 50, "moveNumber": 2 }),
        )
        .await;
        assert_eq!(moved["board"][3], "...BW...");
        assert_eq!(moved["retryAfterMs"], 2000);
    }
}

#[tokio::test]
async fn the_creator_is_told_the_mcp_address() {
    let app = TestApp::start_with(&[
        ("TRUST_PROXY_HOPS", "1"),
        ("APP_ORIGIN", "http://localhost:3000"),
    ])
    .await;
    let mut host = browser(&app, "10.7.12.1");
    let reply = host
        .post("/api/games", json!({ "acknowledgedGuestWarning": true }))
        .await;
    let data = data_of(&reply);
    assert_eq!(
        data["invitation"]["mcpUrl"],
        "http://localhost:3000/api/mcp"
    );
    let id = data["gameId"].as_str().unwrap();
    let view = data_of(&host.get(&format!("/api/games/{id}")).await);
    assert_eq!(
        view["invitation"]["mcpUrl"],
        "http://localhost:3000/api/mcp"
    );
    // The game page itself carries the card's script and the texts.
    let page = host.get(&format!("/games/{id}")).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("/assets/agent-connect.js"));
    assert!(page.body.contains("game.agentClients.claudeCheck"));
}
