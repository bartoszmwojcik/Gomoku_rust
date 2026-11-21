//! The game API end to end: accounts and guests play on the real router over a real PostgreSQL.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::http::StatusCode;
use common::{Client, Reply, TestApp};
use serde_json::{Value, json};
use sqlx::Row;

const PASSWORD: &str = "correct horse";

fn error_of(reply: &Reply) -> Value {
    let body = reply.json();
    assert_eq!(body["ok"], false, "{body}");
    body["error"].clone()
}

fn data_of(reply: &Reply) -> Value {
    let body = reply.json();
    assert_eq!(body["ok"], true, "{}", reply.body);
    body["data"].clone()
}

async fn signup(app: &TestApp, ip: &str, email: &str, name: &str) -> Client {
    let mut client = app.client().with_header("x-forwarded-for", ip);
    let reply = client
        .post(
            "/api/auth/register",
            json!({ "email": email, "displayName": name, "password": PASSWORD }),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    client
}

async fn app() -> TestApp {
    TestApp::start_with(&[("TRUST_PROXY_HOPS", "1")]).await
}

fn guest(app: &TestApp, ip: &str) -> Client {
    app.client().with_header("x-forwarded-for", ip)
}

async fn create(client: &mut Client) -> (String, String) {
    let reply = client.post("/api/games", json!({})).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    let data = data_of(&reply);
    (
        data["gameId"].as_str().unwrap().to_owned(),
        data["invitation"]["code"].as_str().unwrap().to_owned(),
    )
}

async fn view(client: &mut Client, id: &str) -> Value {
    let reply = client.get(&format!("/api/games/{id}")).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    data_of(&reply)
}

async fn accept_user(client: &mut Client, code: &str) -> Reply {
    client
        .post(
            &format!("/api/invitations/{code}/accept"),
            json!({ "as": "user" }),
        )
        .await
}

async fn mv(client: &mut Client, id: &str, x: i64, y: i64, number: i64) -> Reply {
    client
        .post(
            &format!("/api/games/{id}/moves"),
            json!({ "x": x, "y": y, "moveNumber": number }),
        )
        .await
}

/// Two accounts with a started game, both having opened it (the clock runs). Returns the game id and the two clients
/// ordered by seat (seat 1 first), with who plays black.
async fn started_game(app: &TestApp) -> (String, Client, Client) {
    started_game_as(app, "").await
}

async fn started_game_as(app: &TestApp, tag: &str) -> (String, Client, Client) {
    let ip = |n: u8| format!("10.1.{}.{n}", tag.len());
    let mut creator = signup(
        app,
        &ip(1),
        &format!("creator{tag}@example.com"),
        &format!("Creator{tag}"),
    )
    .await;
    let mut joiner = signup(
        app,
        &ip(2),
        &format!("joiner{tag}@example.com"),
        &format!("Joiner{tag}"),
    )
    .await;
    let (id, code) = create(&mut creator).await;
    assert_eq!(accept_user(&mut joiner, &code).await.status, StatusCode::OK);
    view(&mut joiner, &id).await;
    view(&mut creator, &id).await;
    view(&mut joiner, &id).await;
    (id, creator, joiner)
}

/// The client whose seat plays black, and the other one.
async fn by_colour(id: &str, creator: Client, joiner: Client) -> (Client, Client) {
    let mut creator = creator;
    let black_seat = view(&mut creator, id).await["blackSeat"].as_i64().unwrap();
    if black_seat == 1 {
        (creator, joiner)
    } else {
        (joiner, creator)
    }
}

async fn count(app: &TestApp, sql: &str) -> i64 {
    sqlx::query(sqlx::AssertSqlSafe(sql.to_owned()))
        .fetch_one(app.db.pool())
        .await
        .unwrap()
        .get::<i64, _>(0)
}

// ---- create, invitation, join -----------------------------------------------------------------------------

#[tokio::test]
async fn an_account_creates_a_waiting_game_and_only_its_players_can_see_it() {
    let app = app().await;
    let mut ala = signup(&app, "10.0.0.1", "ala@example.com", "Ala").await;
    let reply = ala.post("/api/games", json!({})).await;
    assert_eq!(reply.status, StatusCode::CREATED);
    let data = data_of(&reply);
    let id = data["gameId"].as_str().unwrap();
    let code = data["invitation"]["code"].as_str().unwrap();
    assert!(code.len() == 9 && code.as_bytes()[4] == b'-', "{code}");
    assert_eq!(
        data["invitation"]["url"],
        format!("http://localhost:3000/invite/{code}")
    );
    assert!(
        data["invitation"]["expiresAt"]
            .as_str()
            .unwrap()
            .ends_with('Z')
    );

    let state = view(&mut ala, id).await;
    assert_eq!(state["status"], "waiting");
    assert_eq!(state["blackSeat"], Value::Null);
    assert_eq!(state["turnSeat"], Value::Null);
    assert_eq!(state["invitation"]["code"], code);
    assert_eq!(state["players"].as_array().unwrap().len(), 1);
    assert_eq!(state["players"][0]["displayName"], "Ala");
    assert_eq!(state["you"], json!({ "seat": 1, "kind": "account" }));
    assert_eq!(state["clock"]["running"], Value::Null);
    assert_eq!(state["variant"], "classic");

    // A stranger, a guest and a malformed id all get the same 404.
    let mut bob = signup(&app, "10.0.0.2", "bob@example.com", "Bob").await;
    for reply in [
        bob.get(&format!("/api/games/{id}")).await,
        guest(&app, "10.0.0.3")
            .get(&format!("/api/games/{id}"))
            .await,
        bob.get("/api/games/not-a-uuid").await,
        bob.get(&format!("/api/games/{}", uuid_like())).await,
    ] {
        assert_eq!(reply.status, StatusCode::NOT_FOUND);
        assert_eq!(error_of(&reply)["code"], "NOT_FOUND");
    }
}

fn uuid_like() -> String {
    "00000000-0000-4000-8000-000000000000".to_owned()
}

#[tokio::test]
async fn one_account_holds_one_game_slot() {
    let app = app().await;
    let mut ala = signup(&app, "10.0.0.1", "ala@example.com", "Ala").await;
    create(&mut ala).await;
    let again = ala.post("/api/games", json!({})).await;
    assert_eq!(again.status, StatusCode::CONFLICT);
    assert_eq!(error_of(&again)["code"], "ACTIVE_GAME_EXISTS");
    // The body of an account's create is optional but must be JSON when present.
    let junk = ala
        .send(
            axum::http::Request::post("/api/games")
                .header("origin", common::ORIGIN)
                .header("x-gomoku-request", "1"),
            axum::body::Body::from("not json"),
        )
        .await;
    assert_eq!(junk.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn creating_games_is_limited_to_ten_attempts_per_hour() {
    let app = app().await;
    let mut ala = signup(&app, "10.0.0.1", "ala@example.com", "Ala").await;
    let (id, _) = create(&mut ala).await;
    assert_eq!(
        ala.post(&format!("/api/games/{id}/cancel"), Value::Null)
            .await
            .status,
        StatusCode::OK
    );
    for _ in 0..9 {
        let (id, _) = create(&mut ala).await;
        ala.post(&format!("/api/games/{id}/cancel"), Value::Null)
            .await;
    }
    let blocked = ala.post("/api/games", json!({})).await;
    assert_eq!(blocked.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(blocked.header("retry-after").is_some());
}

#[tokio::test]
async fn accepting_starts_the_game_and_the_clock_waits_for_both_players() {
    let app = app().await;
    let mut creator = signup(&app, "10.0.0.1", "creator@example.com", "Creator").await;
    let mut joiner = signup(&app, "10.0.0.2", "joiner@example.com", "Joiner").await;
    let (id, code) = create(&mut creator).await;

    let joined = accept_user(&mut joiner, &code).await;
    assert_eq!(joined.status, StatusCode::OK);
    assert_eq!(data_of(&joined)["gameId"], id);
    assert!(
        joined
            .set_cookies()
            .iter()
            .all(|c| !c.contains("gomoku_guest")),
        "an account gets no guest cookie"
    );

    // The joiner is on the page already; the creator has not opened the game: the clock does not run.
    let theirs = view(&mut joiner, &id).await;
    assert_eq!(theirs["status"], "active");
    assert_eq!(theirs["clock"]["running"], Value::Null);
    assert_eq!(theirs["clock"]["remainingMs"], json!([600_000, 600_000]));
    assert!(theirs["blackSeat"].as_i64().is_some());
    assert_eq!(theirs["players"].as_array().unwrap().len(), 2);
    let mine = view(&mut creator, &id).await;
    assert_eq!(mine["invitation"], Value::Null);
    assert_eq!(mine["players"][1]["displayName"], "Joiner");
    // The creator's first read was the last one needed: the clock starts on the next read of either.
    let started = view(&mut joiner, &id).await;
    assert_eq!(
        started["clock"]["running"], started["turnSeat"],
        "{started}"
    );
    assert!(started["clock"]["now"].as_i64().unwrap() > 1_700_000_000_000);
    assert_eq!(
        count(
            &app,
            "SELECT count(*) FROM games WHERE turn_started_at IS NOT NULL"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn an_invitation_is_uniform_for_every_kind_of_bad_code() {
    let app = app().await;
    let mut creator = signup(&app, "10.0.0.1", "creator@example.com", "Creator").await;
    let mut joiner = signup(&app, "10.0.0.2", "joiner@example.com", "Joiner").await;
    let mut third = signup(&app, "10.0.0.3", "third@example.com", "Third").await;
    let (_, code) = create(&mut creator).await;
    assert_eq!(accept_user(&mut joiner, &code).await.status, StatusCode::OK);

    let bodies: Vec<String> = {
        let mut out = Vec::new();
        for bad in ["XXXX-XXXX", "abc", &code, "ABCD-EFG0"] {
            let reply = accept_user(&mut third, bad).await;
            assert_eq!(reply.status, StatusCode::GONE, "{bad}: {}", reply.body);
            assert_eq!(error_of(&reply)["code"], "INVALID_INVITATION");
            out.push(reply.body);
        }
        out
    };
    assert!(bodies.iter().all(|b| b == &bodies[0]), "{bodies:?}");
}

#[tokio::test]
async fn joining_ones_own_invitation_or_while_holding_a_game_is_refused() {
    let app = app().await;
    let mut creator = signup(&app, "10.0.0.1", "creator@example.com", "Creator").await;
    let mut other = signup(&app, "10.0.0.2", "other@example.com", "Other").await;
    let mut third = signup(&app, "10.0.0.3", "third@example.com", "Third").await;
    let (id, code) = create(&mut creator).await;
    let own = accept_user(&mut creator, &code).await;
    assert_eq!(own.status, StatusCode::CONFLICT);
    let error = error_of(&own);
    assert_eq!(
        (error["code"].clone(), error["data"]["gameId"].clone()),
        (json!("OWN_INVITATION"), json!(id))
    );

    // A player with a waiting game of their own must swap it away explicitly.
    let (other_id, _) = create(&mut other).await;
    let refused = accept_user(&mut other, &code).await;
    let error = error_of(&refused);
    assert_eq!(error["code"], "OWN_WAITING_GAME");
    assert_eq!(
        (
            error["data"]["reason"].clone(),
            error["data"]["gameId"].clone()
        ),
        (json!("join"), json!(other_id))
    );
    let swapped = other
        .post(
            &format!("/api/invitations/{code}/accept"),
            json!({ "as": "user", "cancelOwnWaitingGame": true }),
        )
        .await;
    assert_eq!(swapped.status, StatusCode::OK, "{}", swapped.body);
    assert_eq!(view(&mut other, &other_id).await["status"], "cancelled");

    // A player already in a running game cannot join another.
    let (_, third_code) = create(&mut third).await;
    let mut fourth = signup(&app, "10.0.0.4", "fourth@example.com", "Fourth").await;
    let busy = accept_user(&mut other, &third_code).await;
    assert_eq!(error_of(&busy)["code"], "ACTIVE_GAME_EXISTS");
    assert!(error_of(&busy)["data"]["gameId"].is_string());
    let _ = fourth.get("/").await;
}

#[tokio::test]
async fn the_accept_request_is_validated_before_anything_else() {
    let app = app().await;
    let mut creator = signup(&app, "10.0.0.1", "creator@example.com", "Creator").await;
    let (_, code) = create(&mut creator).await;
    let mut visitor = guest(&app, "10.0.0.9");
    let path = format!("/api/invitations/{code}/accept");
    let none = error_of(&visitor.post(&path, json!({})).await);
    assert_eq!(none["fieldIssues"]["as"][0]["code"], "AS_REQUIRED");
    let no_ack = error_of(&visitor.post(&path, json!({ "as": "guest" })).await);
    assert_eq!(
        no_ack["fieldIssues"]["acknowledgedGuestWarning"][0]["code"],
        "GUEST_WARNING_REQUIRED"
    );
    let bad_nick = error_of(
        &visitor
            .post(
                &path,
                json!({ "as": "guest", "acknowledgedGuestWarning": true, "nickname": "a@b" }),
            )
            .await,
    );
    assert_eq!(
        bad_nick["fieldIssues"]["nickname"][0]["code"],
        "NICKNAME_INVALID"
    );
    let reserved = error_of(
        &visitor
            .post(
                &path,
                json!({ "as": "guest", "acknowledgedGuestWarning": true, "nickname": "Admin" }),
            )
            .await,
    );
    assert_eq!(
        reserved["fieldIssues"]["nickname"][0]["code"],
        "NICKNAME_RESERVED"
    );
    let long = error_of(&visitor.post(&path, json!({ "as": "guest", "acknowledgedGuestWarning": true, "nickname": "x".repeat(25) })).await);
    assert_eq!(
        long["fieldIssues"]["nickname"][0]["code"],
        "NICKNAME_TOO_LONG"
    );
    let user_nick = error_of(
        &visitor
            .post(&path, json!({ "as": "user", "nickname": "Ola" }))
            .await,
    );
    assert_eq!(
        user_nick["fieldIssues"]["nickname"][0]["code"],
        "NICKNAME_GUEST_ONLY"
    );
    // Joining as an account needs a session; a signed-in visitor cannot join as a guest.
    let anonymous = visitor.post(&path, json!({ "as": "user" })).await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
    assert_eq!(error_of(&anonymous)["data"]["reason"], "join_sign_in");
    let mut member = signup(&app, "10.0.0.5", "member@example.com", "Member").await;
    let as_guest = member
        .post(
            &path,
            json!({ "as": "guest", "acknowledgedGuestWarning": true }),
        )
        .await;
    assert_eq!(as_guest.status, StatusCode::CONFLICT);
    assert_eq!(error_of(&as_guest)["code"], "GUEST_NOT_ALLOWED");
}

#[tokio::test]
async fn the_lookup_pool_counts_failures_only_and_runs_dry_for_correct_codes_too() {
    let app = app().await;
    let mut creator = signup(&app, "10.0.0.1", "creator@example.com", "Creator").await;
    let (_, code) = create(&mut creator).await;
    let mut visitor = guest(&app, "10.0.0.9");
    // A successful preview-like lookup gives the attempt back; failures stay counted.
    for index in 0..20 {
        let reply = visitor
            .post(
                &format!("/api/invitations/BADC-{index:04}/accept"),
                json!({ "as": "user" }),
            )
            .await;
        // `as: user` without a session is refused BEFORE the pool, so use a guest body to reach the lookup.
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    }
    for _ in 0..20 {
        let reply = visitor
            .post(
                "/api/invitations/BADC-ZZZZ/accept",
                json!({ "as": "guest", "acknowledgedGuestWarning": true }),
            )
            .await;
        assert_eq!(reply.status, StatusCode::GONE);
    }
    let dry = visitor
        .post(
            &format!("/api/invitations/{code}/accept"),
            json!({ "as": "guest", "acknowledgedGuestWarning": true }),
        )
        .await;
    assert_eq!(
        dry.status,
        StatusCode::TOO_MANY_REQUESTS,
        "a correct code gets no answer from a spent pool: {}",
        dry.body
    );
    assert!(dry.header("retry-after").is_some());
    // Another address is unaffected, and a success does not spend the pool.
    let mut elsewhere = guest(&app, "10.0.0.77");
    for _ in 0..3 {
        let reply = elsewhere
            .post(
                "/api/invitations/BADC-ZZZZ/accept",
                json!({ "as": "guest", "acknowledgedGuestWarning": true }),
            )
            .await;
        assert_eq!(reply.status, StatusCode::GONE);
    }
    let used: i32 = sqlx::query_scalar("SELECT count FROM rate_limit_buckets WHERE key LIKE 'invitation-lookup:ip:%' ORDER BY count LIMIT 1")
        .fetch_one(app.db.pool())
        .await
        .unwrap();
    assert_eq!(used, 3);
}

// ---- moves ------------------------------------------------------------------------------------------------

#[tokio::test]
async fn moves_alternate_and_each_refusal_has_its_own_code() {
    let app = app().await;
    let (id, creator, joiner) = started_game(&app).await;
    let (mut black, mut white) = by_colour(&id, creator, joiner).await;

    let early = mv(&mut white, &id, 3, 3, 1).await;
    assert_eq!(error_of(&early)["code"], "NOT_YOUR_TURN");
    assert_eq!(error_of(&early)["data"]["reason"], "opponent_turn");
    let first = mv(&mut black, &id, 7, 7, 1).await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.body);
    let state = data_of(&first);
    assert_eq!(
        (
            state["moveCount"].clone(),
            state["moves"].clone(),
            state["turnSeat"].clone()
        ),
        (
            json!(1),
            json!([112]),
            state["you"]["seat"]
                .clone()
                .as_i64()
                .map(|s| json!(3 - s))
                .unwrap()
        )
    );
    assert!(state["clock"]["now"].is_number());

    // The same move again is a retry: 200, nothing written.
    let retry = mv(&mut black, &id, 7, 7, 1).await;
    assert_eq!(retry.status, StatusCode::OK);
    assert_eq!(data_of(&retry)["moveCount"], 1);
    // Out of turn (the turn check comes before the stale check), wrong number, occupied cell.
    assert_eq!(
        error_of(&mv(&mut black, &id, 8, 8, 2).await)["code"],
        "NOT_YOUR_TURN"
    );
    assert_eq!(
        error_of(&mv(&mut white, &id, 8, 8, 5).await)["code"],
        "STALE_MOVE"
    );
    assert_eq!(
        error_of(&mv(&mut white, &id, 7, 7, 2).await)["code"],
        "CELL_OCCUPIED"
    );
    let ok = mv(&mut white, &id, 8, 8, 2).await;
    assert_eq!(ok.status, StatusCode::OK);

    // The move log is what the poll serves, from `after` on.
    let all = view(&mut black, &id).await;
    assert_eq!(all["moves"], json!([112, 128]));
    let tail = black.get(&format!("/api/games/{id}?after=1")).await;
    assert_eq!(data_of(&tail)["moves"], json!([128]));
    let beyond = black.get(&format!("/api/games/{id}?after=3")).await;
    assert_eq!(beyond.status, StatusCode::BAD_REQUEST);
    for bad in ["x", "-1", "123456", ""] {
        let reply = black.get(&format!("/api/games/{id}?after={bad}")).await;
        assert_eq!(
            error_of(&reply)["fieldIssues"]["after"][0]["code"],
            "AFTER_INVALID",
            "{bad}"
        );
    }
}

#[tokio::test]
async fn move_input_is_validated_on_the_board_bounds() {
    let app = app().await;
    let (id, creator, joiner) = started_game(&app).await;
    let (mut black, _white) = by_colour(&id, creator, joiner).await;
    for (x, y, n, field) in [
        (15, 0, 1, "x"),
        (-1, 0, 1, "x"),
        (0, 15, 1, "y"),
        (101, 0, 1, "x"),
        (0, 0, 0, "moveNumber"),
        (0, 0, 226, "moveNumber"),
    ] {
        let reply = mv(&mut black, &id, x, y, n).await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{x},{y},{n}");
        assert!(
            error_of(&reply)["fieldIssues"][field].is_array(),
            "{field}: {}",
            reply.body
        );
    }
    let strings = black
        .post(
            &format!("/api/games/{id}/moves"),
            json!({ "x": "7", "y": 7, "moveNumber": 1 }),
        )
        .await;
    assert_eq!(
        error_of(&strings)["fieldIssues"]["x"][0]["code"],
        "COLUMN_INVALID"
    );
    // A stranger sending a good body gets 404.
    let mut stranger = signup(&app, "10.0.0.8", "stranger@example.com", "Stranger").await;
    assert_eq!(
        mv(&mut stranger, &id, 7, 7, 1).await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn a_move_before_the_opponent_opened_the_game_waits() {
    let app = app().await;
    let mut creator = signup(&app, "10.0.0.1", "creator@example.com", "Creator").await;
    let mut joiner = signup(&app, "10.0.0.2", "joiner@example.com", "Joiner").await;
    let (id, code) = create(&mut creator).await;
    accept_user(&mut joiner, &code).await;
    let state = view(&mut joiner, &id).await;
    let black_is_joiner = state["blackSeat"] == state["you"]["seat"];
    let (starter, other) = if black_is_joiner {
        (&mut joiner, &mut creator)
    } else {
        (&mut creator, &mut joiner)
    };
    let reply = mv(starter, &id, 7, 7, 1).await;
    // The creator has not opened the game yet: nobody is on the clock, whoever is to move.
    if black_is_joiner {
        assert_eq!(error_of(&reply)["code"], "GAME_NOT_STARTED");
    }
    let _ = other;
}

#[tokio::test]
async fn five_in_a_row_wins_and_frees_both_slots() {
    let app = app().await;
    let (id, creator, joiner) = started_game(&app).await;
    let (mut black, mut white) = by_colour(&id, creator, joiner).await;
    let mut number = 1;
    for i in 0..5 {
        assert_eq!(
            mv(&mut black, &id, 3 + i, 7, number).await.status,
            StatusCode::OK
        );
        number += 1;
        if i < 4 {
            assert_eq!(
                mv(&mut white, &id, i, 0, number).await.status,
                StatusCode::OK
            );
            number += 1;
        }
    }
    let state = view(&mut black, &id).await;
    assert_eq!(state["status"], "finished");
    assert_eq!(state["turnSeat"], Value::Null);
    assert_eq!(state["result"]["reason"], "five_in_row");
    assert_eq!(state["result"]["winnerSeat"], state["you"]["seat"]);
    assert_eq!(state["result"]["winLine"], json!([108, 109, 110, 111, 112]));
    assert!(state["finishedAt"].is_string());
    assert_eq!(state["clock"]["running"], Value::Null);
    assert_eq!(
        count(&app, "SELECT count(*) FROM game_players WHERE is_active").await,
        0
    );
    assert_eq!(count(&app, "SELECT sum(finished_count) FROM game_pair_counters WHERE kind_a = 'account' AND kind_b = 'account'").await, 1);
    // Nothing more can be played, and the finished game is not "not active".
    assert_eq!(
        error_of(&mv(&mut white, &id, 9, 9, 10).await)["code"],
        "GAME_FINISHED"
    );
    // Both can start a new game at once.
    create(&mut black).await;
    create(&mut white).await;
}

#[tokio::test]
async fn the_clock_runs_out_lazily_and_the_loss_is_dated_at_the_deadline() {
    let app = app().await;
    let (id, creator, joiner) = started_game(&app).await;
    let (mut black, mut white) = by_colour(&id, creator, joiner).await;
    // Black has the move; ten minutes pass (the deadline is exactly 600 s after the clock started).
    app.advance_seconds(599);
    assert_eq!(view(&mut white, &id).await["status"], "active");
    app.advance_seconds(1);
    let lost = view(&mut white, &id).await;
    assert_eq!(lost["status"], "finished");
    assert_eq!(lost["result"]["reason"], "timeout");
    assert_eq!(lost["result"]["winnerSeat"], lost["you"]["seat"]);
    assert_eq!(
        lost["clock"]["remainingMs"][lost["blackSeat"].as_i64().unwrap() as usize - 1],
        0
    );
    // `finished_at` is the moment the time ran out, not the moment somebody noticed.
    app.advance_seconds(3_000);
    let later = view(&mut black, &id).await;
    let finished_at: String = later["finishedAt"].as_str().unwrap().to_owned();
    assert!(
        finished_at.starts_with("2027-01-15T08:10:0"),
        "{finished_at}"
    );
    assert_eq!(
        count(&app, "SELECT count(*) FROM game_players WHERE is_active").await,
        0
    );
}

#[tokio::test]
async fn a_move_at_the_deadline_is_too_late_and_the_settlement_is_kept() {
    let app = app().await;
    let (id, creator, joiner) = started_game(&app).await;
    let (mut black, _white) = by_colour(&id, creator, joiner).await;
    app.advance_seconds(600);
    let late = mv(&mut black, &id, 7, 7, 1).await;
    assert_eq!(late.status, StatusCode::CONFLICT);
    assert_eq!(error_of(&late)["code"], "GAME_FINISHED");
    // The refusal did not roll the loss back.
    assert_eq!(view(&mut black, &id).await["result"]["reason"], "timeout");
}

#[tokio::test]
async fn a_move_charges_the_movers_time_and_starts_the_opponents_clock() {
    let app = app().await;
    let (id, creator, joiner) = started_game(&app).await;
    let (mut black, mut white) = by_colour(&id, creator, joiner).await;
    app.advance_seconds(30);
    let state = data_of(&mv(&mut black, &id, 7, 7, 1).await);
    let black_seat = state["blackSeat"].as_i64().unwrap() as usize;
    assert_eq!(state["clock"]["remainingMs"][black_seat - 1], 570_000);
    assert_eq!(state["clock"]["remainingMs"][2 - black_seat], 600_000);
    assert_eq!(state["clock"]["running"], json!(3 - black_seat));
    app.advance_seconds(5);
    let state = view(&mut white, &id).await;
    assert_eq!(state["clock"]["remainingMs"][2 - black_seat], 595_000);
}

#[tokio::test]
async fn resigning_ends_the_game_any_time_and_repeats_are_harmless() {
    let app = app().await;
    let (id, creator, joiner) = started_game(&app).await;
    let (mut black, mut white) = by_colour(&id, creator, joiner).await;
    // Out of turn, before any move.
    let resigned = white
        .post(&format!("/api/games/{id}/resign"), Value::Null)
        .await;
    assert_eq!(resigned.status, StatusCode::OK);
    let state = data_of(&resigned);
    assert_eq!(state["result"]["reason"], "resignation");
    assert_eq!(
        state["result"]["winnerSeat"],
        black.get(&format!("/api/games/{id}")).await.json()["data"]["you"]["seat"]
    );
    assert_eq!(
        white
            .post(&format!("/api/games/{id}/resign"), Value::Null)
            .await
            .status,
        StatusCode::OK,
        "a repeat is fine"
    );
    assert_eq!(
        error_of(
            &black
                .post(&format!("/api/games/{id}/resign"), Value::Null)
                .await
        )["code"],
        "GAME_FINISHED"
    );
    assert_eq!(
        count(&app, "SELECT count(*) FROM game_players WHERE is_active").await,
        0
    );
}

#[tokio::test]
async fn resigning_a_game_that_is_not_active_is_refused_and_the_time_loss_wins() {
    let app = app().await;
    let mut creator = signup(&app, "10.0.0.1", "creator@example.com", "Creator").await;
    let (id, _) = create(&mut creator).await;
    let waiting = creator
        .post(&format!("/api/games/{id}/resign"), Value::Null)
        .await;
    assert_eq!(error_of(&waiting)["code"], "GAME_NOT_ACTIVE");
    assert_eq!(error_of(&waiting)["data"]["reason"], "game_not_active");

    let (game, c, j) = started_game_as(&app, "x").await;
    let (mut black, _w) = by_colour(&game, c, j).await;
    app.advance_seconds(600);
    let late = black
        .post(&format!("/api/games/{game}/resign"), Value::Null)
        .await;
    assert_eq!(error_of(&late)["code"], "GAME_FINISHED");
    assert_eq!(view(&mut black, &game).await["result"]["reason"], "timeout");
}

// ---- cancel and lazy closing --------------------------------------------------------------------------------

#[tokio::test]
async fn the_creator_cancels_a_waiting_game_and_the_invitation_dies() {
    let app = app().await;
    let mut creator = signup(&app, "10.0.0.1", "creator@example.com", "Creator").await;
    let mut joiner = signup(&app, "10.0.0.2", "joiner@example.com", "Joiner").await;
    let (id, code) = create(&mut creator).await;
    assert_eq!(
        error_of(
            &joiner
                .post(&format!("/api/games/{id}/cancel"), Value::Null)
                .await
        )["code"],
        "NOT_FOUND"
    );
    let cancelled = creator
        .post(&format!("/api/games/{id}/cancel"), Value::Null)
        .await;
    assert_eq!(cancelled.json(), json!({ "ok": true, "data": null }));
    let state = view(&mut creator, &id).await;
    assert_eq!(
        (state["status"].clone(), state["closedReason"].clone()),
        (json!("cancelled"), json!("cancelled"))
    );
    assert_eq!(
        accept_user(&mut joiner, &code).await.status,
        StatusCode::GONE
    );
    assert_eq!(
        error_of(
            &creator
                .post(&format!("/api/games/{id}/cancel"), Value::Null)
                .await
        )["code"],
        "GAME_NOT_CANCELLABLE"
    );
    create(&mut creator).await;
}

#[tokio::test]
async fn a_cancel_after_the_opponent_joined_is_refused() {
    let app = app().await;
    let (id, mut creator, _joiner) = started_game(&app).await;
    let reply = creator
        .post(&format!("/api/games/{id}/cancel"), Value::Null)
        .await;
    assert_eq!(reply.status, StatusCode::CONFLICT);
    assert_eq!(error_of(&reply)["code"], "GAME_NOT_CANCELLABLE");
}

#[tokio::test]
async fn an_unjoined_invitation_expires_lazily_after_24_hours() {
    let app = app().await;
    let mut creator = signup(&app, "10.0.0.1", "creator@example.com", "Creator").await;
    let mut joiner = signup(&app, "10.0.0.2", "joiner@example.com", "Joiner").await;
    let (id, code) = create(&mut creator).await;
    app.advance_seconds(24 * 3600);
    assert_eq!(
        accept_user(&mut joiner, &code).await.status,
        StatusCode::GONE
    );
    let state = view(&mut creator, &id).await;
    assert_eq!(
        (state["status"].clone(), state["closedReason"].clone()),
        (json!("cancelled"), json!("expired"))
    );
    assert_eq!(
        count(&app, "SELECT count(*) FROM game_players WHERE is_active").await,
        0
    );
    // The slot is free again.
    create(&mut creator).await;
}

#[tokio::test]
async fn a_game_somebody_never_opened_is_cancelled_after_fifteen_minutes() {
    let app = app().await;
    let mut creator = signup(&app, "10.0.0.1", "creator@example.com", "Creator").await;
    let mut joiner = signup(&app, "10.0.0.2", "joiner@example.com", "Joiner").await;
    let (id, code) = create(&mut creator).await;
    accept_user(&mut joiner, &code).await;
    app.advance_seconds(15 * 60 - 1);
    assert_eq!(view(&mut joiner, &id).await["status"], "active");
    app.advance_seconds(1);
    // A late first visit of the creator cannot revive the game.
    let state = view(&mut creator, &id).await;
    assert_eq!(
        (state["status"].clone(), state["closedReason"].clone()),
        (json!("cancelled"), json!("not_opened"))
    );
    assert_eq!(
        count(&app, "SELECT count(*) FROM game_players WHERE is_active").await,
        0
    );
    assert_eq!(
        count(
            &app,
            "SELECT count(*) FROM games WHERE turn_started_at IS NOT NULL"
        )
        .await,
        0
    );
}

// ---- guests -------------------------------------------------------------------------------------------------

fn guest_cookie_of(reply: &Reply) -> Option<String> {
    reply
        .set_cookies()
        .into_iter()
        .find(|c| c.starts_with("gomoku_guest="))
}

#[tokio::test]
async fn a_guest_creates_a_game_with_a_long_lived_cookie_and_the_token_is_stored_hashed() {
    let app = app().await;
    let mut visitor = guest(&app, "10.0.0.9");
    let no_ack = visitor.post("/api/games", json!({})).await;
    assert_eq!(
        error_of(&no_ack)["fieldIssues"]["acknowledgedGuestWarning"][0]["code"],
        "GUEST_WARNING_REQUIRED"
    );

    let reply = visitor
        .post(
            "/api/games",
            json!({ "acknowledgedGuestWarning": true, "nickname": "  Zosia  " }),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    let cookie = guest_cookie_of(&reply).expect("a guest cookie");
    assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Lax"));
    let max_age: i64 = cookie
        .split("Max-Age=")
        .nth(1)
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        (90_000 - 5..=90_000).contains(&max_age),
        "invitation + 1 h grace = 25 h: {max_age}"
    );
    let token = visitor.cookie("gomoku_guest").unwrap().to_owned();
    let stored: String = sqlx::query_scalar("SELECT guest_token_hash FROM game_players")
        .fetch_one(app.db.pool())
        .await
        .unwrap();
    assert_eq!(stored.len(), 64);
    assert_ne!(stored, token);

    let id = data_of(&reply)["gameId"].as_str().unwrap().to_owned();
    let state = view(&mut visitor, &id).await;
    assert_eq!(state["you"], json!({ "seat": 1, "kind": "guest" }));
    assert_eq!(state["players"][0]["displayName"], "Zosia");
    assert!(state["invitation"]["code"].is_string());
    // Reads slide the cookie forward; a waiting game keeps its long lifetime.
    let read = visitor.get(&format!("/api/games/{id}")).await;
    assert!(
        guest_cookie_of(&read).unwrap().contains("Max-Age=90000"),
        "{:?}",
        read.set_cookies()
    );
}

#[tokio::test]
async fn a_guest_with_a_waiting_game_must_cancel_it_to_create_another() {
    let app = app().await;
    let mut visitor = guest(&app, "10.0.0.9");
    let first = data_of(
        &visitor
            .post("/api/games", json!({ "acknowledgedGuestWarning": true }))
            .await,
    );
    let refused = visitor
        .post("/api/games", json!({ "acknowledgedGuestWarning": true }))
        .await;
    assert_eq!(refused.status, StatusCode::CONFLICT);
    let error = error_of(&refused);
    assert_eq!(
        (error["code"].clone(), error["data"]["reason"].clone()),
        (json!("OWN_WAITING_GAME"), json!("create"))
    );
    assert_eq!(error["data"]["gameId"], first["gameId"]);

    let swapped = visitor
        .post(
            "/api/games",
            json!({ "acknowledgedGuestWarning": true, "cancelOwnWaitingGame": true }),
        )
        .await;
    assert_eq!(swapped.status, StatusCode::CREATED, "{}", swapped.body);
    // The token moved to the new game: the old one is unreachable for this browser.
    let old = visitor
        .get(&format!("/api/games/{}", first["gameId"].as_str().unwrap()))
        .await;
    assert_eq!(old.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_guest_joins_plays_and_the_token_never_travels_in_json() {
    let app = app().await;
    let mut creator = signup(&app, "10.0.0.1", "creator@example.com", "Creator").await;
    let (id, code) = create(&mut creator).await;
    let mut visitor = guest(&app, "10.0.0.9");
    let reply = visitor
        .post(
            &format!("/api/invitations/{code}/accept"),
            json!({ "as": "guest", "acknowledgedGuestWarning": true, "nickname": "Gość" }),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let cookie = guest_cookie_of(&reply).expect("a guest cookie");
    assert!(cookie.contains("Max-Age=7200"), "{cookie}");
    assert!(!reply.body.contains(visitor.cookie("gomoku_guest").unwrap()));

    let state = view(&mut visitor, &id).await;
    assert_eq!(state["you"], json!({ "seat": 2, "kind": "guest" }));
    assert_eq!(state["players"][1]["displayName"], "Gość");
    assert_eq!(state["players"][0]["displayName"], "Creator");
    view(&mut creator, &id).await;

    // A repeat of the join is 200 and gives no new token.
    let again = visitor
        .post(
            &format!("/api/invitations/{code}/accept"),
            json!({ "as": "guest", "acknowledgedGuestWarning": true }),
        )
        .await;
    assert_eq!(again.status, StatusCode::OK);
    assert!(guest_cookie_of(&again).is_none());
    // The guest plays like anybody else.
    let blank = view(&mut creator, &id).await;
    let guest_is_black = blank["blackSeat"] == json!(2);
    let (first, _) = if guest_is_black {
        (&mut visitor, &mut creator)
    } else {
        (&mut creator, &mut visitor)
    };
    let moved = mv(first, &id, 7, 7, 1).await;
    assert_eq!(moved.status, StatusCode::OK, "{}", moved.body);
    if guest_is_black {
        assert!(guest_cookie_of(&moved).unwrap().contains("Max-Age=7200"));
    }
}

#[tokio::test]
async fn a_nameless_guest_is_called_by_the_default_and_guest_tokens_expire_an_hour_after_the_end() {
    let app = app().await;
    let mut creator = signup(&app, "10.0.0.1", "creator@example.com", "Creator").await;
    let (id, code) = create(&mut creator).await;
    let mut visitor = guest(&app, "10.0.0.9");
    visitor
        .post(
            &format!("/api/invitations/{code}/accept"),
            json!({ "as": "guest", "acknowledgedGuestWarning": true }),
        )
        .await;
    let state = view(&mut visitor, &id).await;
    assert_eq!(state["players"][1]["displayName"], "Gracz 2");
    view(&mut creator, &id).await;
    visitor
        .post(&format!("/api/games/{id}/resign"), Value::Null)
        .await;
    app.advance_seconds(3599);
    assert_eq!(
        visitor.get(&format!("/api/games/{id}")).await.status,
        StatusCode::OK
    );
    app.advance_seconds(2);
    assert_eq!(
        visitor.get(&format!("/api/games/{id}")).await.status,
        StatusCode::NOT_FOUND,
        "the grace is over"
    );
    assert_eq!(
        view(&mut creator, &id).await["status"],
        "finished",
        "the account has no limit"
    );
}

#[tokio::test]
async fn a_guest_cannot_cancel_someone_elses_game_or_play_in_another_browser() {
    let app = app().await;
    let mut visitor = guest(&app, "10.0.0.9");
    let id = data_of(
        &visitor
            .post("/api/games", json!({ "acknowledgedGuestWarning": true }))
            .await,
    )["gameId"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut other_browser = guest(&app, "10.0.0.8");
    assert_eq!(
        other_browser
            .post(&format!("/api/games/{id}/cancel"), Value::Null)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        visitor
            .post(&format!("/api/games/{id}/cancel"), Value::Null)
            .await
            .status,
        StatusCode::OK
    );
    // A game cancelled by its creator voids the guest token at once.
    assert_eq!(
        visitor.get(&format!("/api/games/{id}")).await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn a_guest_creating_games_is_limited_per_client_address() {
    let app = app().await;
    for index in 0..10 {
        let mut visitor = guest(&app, "10.0.0.9");
        let reply = visitor
            .post("/api/games", json!({ "acknowledgedGuestWarning": true }))
            .await;
        assert_eq!(reply.status, StatusCode::CREATED, "game {index}");
    }
    let mut visitor = guest(&app, "10.0.0.9");
    assert_eq!(
        visitor
            .post("/api/games", json!({ "acknowledgedGuestWarning": true }))
            .await
            .status,
        StatusCode::TOO_MANY_REQUESTS
    );
}

// ---- rematch ------------------------------------------------------------------------------------------------

async fn finished_game(app: &TestApp) -> (String, Client, Client) {
    let (id, creator, joiner) = started_game(app).await;
    let (mut black, white) = by_colour(&id, creator, joiner).await;
    black
        .post(&format!("/api/games/{id}/resign"), Value::Null)
        .await;
    (id, black, white)
}

#[tokio::test]
async fn a_rematch_swaps_the_colours_and_creates_a_fresh_game() {
    let app = app().await;
    let (id, mut loser, mut winner) = finished_game(&app).await;
    let before = view(&mut winner, &id).await;
    let offered = loser
        .post(&format!("/api/games/{id}/rematch"), Value::Null)
        .await;
    assert_eq!(offered.status, StatusCode::OK, "{}", offered.body);
    let offer = data_of(&offered);
    assert_eq!(
        (offer["state"].clone(), offer["gameId"].clone()),
        (json!("pending"), Value::Null)
    );
    assert_eq!(view(&mut winner, &id).await["rematch"]["state"], "pending");
    assert_eq!(
        error_of(
            &loser
                .post(&format!("/api/games/{id}/rematch"), Value::Null)
                .await
        )["code"],
        "REMATCH_ALREADY_OFFERED"
    );
    assert_eq!(
        error_of(
            &loser
                .post(&format!("/api/games/{id}/rematch/accept"), Value::Null)
                .await
        )["code"],
        "REMATCH_NOT_OFFERED"
    );

    let accepted = winner
        .post(&format!("/api/games/{id}/rematch/accept"), Value::Null)
        .await;
    assert_eq!(accepted.status, StatusCode::OK, "{}", accepted.body);
    let new_id = data_of(&accepted)["gameId"].as_str().unwrap().to_owned();
    // Accepting again returns the same game.
    let again = winner
        .post(&format!("/api/games/{id}/rematch/accept"), Value::Null)
        .await;
    assert_eq!(data_of(&again)["gameId"], new_id.as_str());

    let fresh = view(&mut winner, &new_id).await;
    assert_eq!(
        (
            fresh["status"].clone(),
            fresh["fromRematch"].clone(),
            fresh["moveCount"].clone()
        ),
        (json!("active"), json!(true), json!(0))
    );
    assert_ne!(
        fresh["blackSeat"], before["blackSeat"],
        "the colours are swapped"
    );
    assert_eq!(
        view(&mut loser, &id).await["rematch"]["gameId"],
        new_id.as_str()
    );
    // The new game starts like any other: the clock waits until both opened it.
    assert_eq!(fresh["clock"]["running"], Value::Null);
}

#[tokio::test]
async fn a_rematch_can_be_declined_expires_and_needs_a_finished_game() {
    let app = app().await;
    let (live, mut c, mut j) = started_game_as(&app, "live").await;
    assert_eq!(
        error_of(
            &c.post(&format!("/api/games/{live}/rematch"), Value::Null)
                .await
        )["code"],
        "GAME_NOT_FINISHED"
    );
    assert_eq!(
        error_of(
            &j.post(&format!("/api/games/{live}/rematch/accept"), Value::Null)
                .await
        )["code"],
        "GAME_NOT_FINISHED"
    );
    drop((c, j));

    let (id, mut loser, mut winner) = finished_game(&app).await;
    loser
        .post(&format!("/api/games/{id}/rematch"), Value::Null)
        .await;
    assert_eq!(
        winner
            .post(&format!("/api/games/{id}/rematch/decline"), Value::Null)
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        winner
            .post(&format!("/api/games/{id}/rematch/decline"), Value::Null)
            .await
            .status,
        StatusCode::OK,
        "idempotent"
    );
    assert_eq!(view(&mut loser, &id).await["rematch"]["state"], "declined");
    // After a refusal either may offer again; an unanswered offer expires after five minutes (lazily).
    assert_eq!(
        winner
            .post(&format!("/api/games/{id}/rematch"), Value::Null)
            .await
            .status,
        StatusCode::OK
    );
    app.advance_seconds(300);
    assert_eq!(view(&mut loser, &id).await["rematch"]["state"], "expired");
    assert_eq!(
        error_of(
            &loser
                .post(&format!("/api/games/{id}/rematch/accept"), Value::Null)
                .await
        )["code"],
        "REMATCH_EXPIRED"
    );
    assert_eq!(
        error_of(
            &loser
                .post(&format!("/api/games/{id}/rematch/decline"), Value::Null)
                .await
        )["code"],
        "REMATCH_EXPIRED"
    );
    assert_eq!(
        loser
            .post(&format!("/api/games/{id}/rematch"), Value::Null)
            .await
            .status,
        StatusCode::OK,
        "an expired offer can be replaced"
    );
}

#[tokio::test]
async fn guests_play_a_rematch_with_a_derived_token_claimed_through_the_old_game() {
    let app = app().await;
    let mut host = signup(&app, "10.0.0.1", "host@example.com", "Host").await;
    let (id, code) = create(&mut host).await;
    let mut visitor = guest(&app, "10.0.0.9");
    visitor
        .post(
            &format!("/api/invitations/{code}/accept"),
            json!({ "as": "guest", "acknowledgedGuestWarning": true }),
        )
        .await;
    view(&mut visitor, &id).await;
    view(&mut host, &id).await;
    visitor
        .post(&format!("/api/games/{id}/resign"), Value::Null)
        .await;

    // The guest proposes; the account accepts. The guest never called accept, yet gets a token for the new game by polling.
    assert_eq!(
        visitor
            .post(&format!("/api/games/{id}/rematch"), Value::Null)
            .await
            .status,
        StatusCode::OK
    );
    let accepted = host
        .post(&format!("/api/games/{id}/rematch/accept"), Value::Null)
        .await;
    let new_id = data_of(&accepted)["gameId"].as_str().unwrap().to_owned();
    assert_eq!(
        visitor.get(&format!("/api/games/{new_id}")).await.status,
        StatusCode::NOT_FOUND,
        "no token for it yet"
    );
    let poll = visitor.get(&format!("/api/games/{id}")).await;
    assert!(
        guest_cookie_of(&poll).is_some(),
        "the poll claimed the seat: {:?}",
        poll.set_cookies()
    );
    let fresh = view(&mut visitor, &new_id).await;
    assert_eq!(fresh["you"]["kind"], "guest");
    assert_eq!(fresh["fromRematch"], true);
    // The cookie now carries the derived token, so the old game is no longer readable from this browser.
    assert_eq!(
        visitor.get(&format!("/api/games/{id}")).await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn a_rematch_is_refused_while_a_player_holds_another_game_and_against_a_deleted_account() {
    let app = app().await;
    let (id, mut loser, mut winner) = finished_game(&app).await;
    loser
        .post(&format!("/api/games/{id}/rematch"), Value::Null)
        .await;
    // The winner starts another game meanwhile.
    create(&mut winner).await;
    let refused = winner
        .post(&format!("/api/games/{id}/rematch/accept"), Value::Null)
        .await;
    let error = error_of(&refused);
    assert_eq!(error["code"], "ACTIVE_GAME_EXISTS");
    assert!(
        error["data"]["gameId"].is_string(),
        "the caller is told about their own game"
    );
    assert_eq!(
        view(&mut winner, &id).await["rematch"]["state"],
        "pending",
        "the offer stays pending"
    );

    // An account that deleted itself makes a rematch impossible.
    let (id2, mut a, mut b) = {
        let mut x = signup(&app, "10.0.0.21", "x@example.com", "Xavier").await;
        let mut y = signup(&app, "10.0.0.22", "y@example.com", "Yolanda").await;
        let (gid, code) = create(&mut x).await;
        accept_user(&mut y, &code).await;
        view(&mut y, &gid).await;
        view(&mut x, &gid).await;
        x.post(&format!("/api/games/{gid}/resign"), Value::Null)
            .await;
        (gid, x, y)
    };
    let gone = a
        .post(
            "/api/account/delete",
            json!({ "password": PASSWORD, "confirm": true }),
        )
        .await;
    assert_eq!(gone.status, StatusCode::OK, "{}", gone.body);
    let reply = b
        .post(&format!("/api/games/{id2}/rematch"), Value::Null)
        .await;
    assert_eq!(error_of(&reply)["code"], "REMATCH_UNAVAILABLE");
    assert_eq!(error_of(&reply)["data"]["reason"], "opponent_gone");
    // The deleted player reads as null in the opponent's view.
    let state = view(&mut b, &id2).await;
    assert!(
        state["players"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["displayName"].is_null()),
        "{state}"
    );
}

#[tokio::test]
async fn rematch_offers_are_limited_to_ten_an_hour() {
    let app = app().await;
    let (id, mut loser, mut winner) = finished_game(&app).await;
    for _ in 0..5 {
        assert_eq!(
            loser
                .post(&format!("/api/games/{id}/rematch"), Value::Null)
                .await
                .status,
            StatusCode::OK
        );
        assert_eq!(
            winner
                .post(&format!("/api/games/{id}/rematch/decline"), Value::Null)
                .await
                .status,
            StatusCode::OK
        );
    }
    for _ in 0..4 {
        assert_eq!(
            loser
                .post(&format!("/api/games/{id}/rematch"), Value::Null)
                .await
                .status,
            StatusCode::OK
        );
        loser
            .post(&format!("/api/games/{id}/rematch/decline"), Value::Null)
            .await;
    }
    assert_eq!(
        loser
            .post(&format!("/api/games/{id}/rematch"), Value::Null)
            .await
            .status,
        StatusCode::OK
    );
    loser
        .post(&format!("/api/games/{id}/rematch/decline"), Value::Null)
        .await;
    assert_eq!(
        loser
            .post(&format!("/api/games/{id}/rematch"), Value::Null)
            .await
            .status,
        StatusCode::TOO_MANY_REQUESTS
    );
}
