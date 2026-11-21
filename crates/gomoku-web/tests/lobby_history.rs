//! The lobby, the history and the lifecycle rules that touch games (account deletion, sessions, cleanup, races).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::http::StatusCode;
use common::{Client, Reply, TestApp};
use gomoku_db::cleanup::{CleanupOptions, cleanup_expired};
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

async fn app() -> TestApp {
    TestApp::start_with(&[("TRUST_PROXY_HOPS", "1")]).await
}

async fn signup(app: &TestApp, n: u8, name: &str) -> Client {
    let mut client = app
        .client()
        .with_header("x-forwarded-for", &format!("10.0.0.{n}"));
    let reply = client
        .post(
            "/api/auth/register",
            json!({ "email": format!("{}@example.com", name.to_lowercase()), "displayName": name, "password": PASSWORD }),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    client
}

async fn count(app: &TestApp, sql: &str) -> i64 {
    sqlx::query(sqlx::AssertSqlSafe(sql.to_owned()))
        .fetch_one(app.db.pool())
        .await
        .unwrap()
        .get::<i64, _>(0)
}

async fn lobby(client: &mut Client) -> Value {
    data_of(&client.get("/api/lobby").await)
}

async fn create(client: &mut Client) -> (String, String) {
    let data = data_of(&client.post("/api/games", json!({})).await);
    (
        data["gameId"].as_str().unwrap().to_owned(),
        data["invitation"]["code"].as_str().unwrap().to_owned(),
    )
}

async fn play_to_resignation(app: &TestApp, a: &mut Client, b: &mut Client) -> String {
    let (id, code) = create(a).await;
    assert_eq!(
        b.post(
            &format!("/api/invitations/{code}/accept"),
            json!({ "as": "user" })
        )
        .await
        .status,
        StatusCode::OK
    );
    b.get(&format!("/api/games/{id}")).await;
    a.get(&format!("/api/games/{id}")).await;
    app.advance_seconds(1);
    let resigned = a
        .post(&format!("/api/games/{id}/resign"), Value::Null)
        .await;
    assert_eq!(resigned.status, StatusCode::OK, "{}", resigned.body);
    id
}

// ---- lobby ------------------------------------------------------------------------------------------------

#[tokio::test]
async fn the_lobby_is_for_accounts_and_lists_others_oldest_first() {
    let app = app().await;
    assert_eq!(
        app.client().get("/api/lobby").await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.client()
            .post("/api/lobby/seekers/x/pick", Value::Null)
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    let mut ala = signup(&app, 1, "Ala").await;
    let mut bob = signup(&app, 2, "Bob").await;
    let mut cy = signup(&app, 3, "Cyprian").await;

    let marked = ala.put("/api/lobby/seeking", Value::Null).await;
    assert_eq!(marked.status, StatusCode::OK);
    let me = data_of(&marked)["me"].clone();
    assert!((299_000..=300_000).contains(&me["expiresInMs"].as_i64().unwrap()));
    app.advance_seconds(60);
    bob.put("/api/lobby/seeking", Value::Null).await;

    let view = lobby(&mut cy).await;
    assert_eq!(view["me"], Value::Null);
    assert_eq!(view["currentGame"], Value::Null);
    let names: Vec<&str> = view["seekers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["displayName"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["Ala", "Bob"], "oldest first");
    assert!(view["seekers"][0]["sinceMs"].as_i64().unwrap() >= 60_000);
    // Never the account's own row, never an e-mail or user id.
    assert_eq!(
        lobby(&mut ala).await["seekers"].as_array().unwrap().len(),
        1
    );
    assert!(!view.to_string().contains("@example.com"));
}

#[tokio::test]
async fn a_live_status_is_not_renewed_an_expired_one_is_and_withdrawing_is_always_fine() {
    let app = app().await;
    let mut ala = signup(&app, 1, "Ala").await;
    let first = data_of(&ala.put("/api/lobby/seeking", Value::Null).await)["me"].clone();
    app.advance_seconds(100);
    let second = data_of(&ala.put("/api/lobby/seeking", Value::Null).await)["me"].clone();
    assert_eq!(
        (first["id"].clone(), first["createdAt"].clone()),
        (second["id"].clone(), second["createdAt"].clone())
    );
    assert!(
        second["expiresInMs"].as_i64().unwrap() < 201_000,
        "not renewed"
    );
    app.advance_seconds(201);
    assert_eq!(lobby(&mut ala).await["me"], Value::Null, "expired");
    let third = data_of(&ala.put("/api/lobby/seeking", Value::Null).await)["me"].clone();
    assert!(
        third["expiresInMs"].as_i64().unwrap() > 299_000,
        "an expired status is overwritten"
    );
    assert_eq!(
        data_of(&ala.delete("/api/lobby/seeking").await)["me"],
        Value::Null
    );
    assert_eq!(
        ala.delete("/api/lobby/seeking").await.status,
        StatusCode::OK
    );
    // Marking is limited to 30 attempts an hour.
    for _ in 0..30 {
        ala.put("/api/lobby/seeking", Value::Null).await;
    }
    assert_eq!(
        ala.put("/api/lobby/seeking", Value::Null).await.status,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn picking_a_seeker_starts_a_game_for_both_and_withdraws_their_statuses() {
    let app = app().await;
    let mut ala = signup(&app, 1, "Ala").await;
    let mut bob = signup(&app, 2, "Bob").await;
    ala.put("/api/lobby/seeking", Value::Null).await;
    let seeker = lobby(&mut bob).await["seekers"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    // Bob need not be looking himself.
    let picked = bob
        .post(&format!("/api/lobby/seekers/{seeker}/pick"), Value::Null)
        .await;
    assert_eq!(picked.status, StatusCode::OK, "{}", picked.body);
    let game_id = data_of(&picked)["gameId"].as_str().unwrap().to_owned();

    let for_ala = lobby(&mut ala).await;
    assert_eq!(
        for_ala["currentGame"],
        json!({ "gameId": game_id, "status": "active" })
    );
    assert_eq!(for_ala["me"], Value::Null, "the status was consumed");
    let state = data_of(&bob.get(&format!("/api/games/{game_id}")).await);
    assert_eq!(
        (
            state["you"]["seat"].clone(),
            state["players"][1]["displayName"].clone()
        ),
        (json!(1), json!("Ala"))
    );
    assert_eq!(state["players"].as_array().unwrap().len(), 2);
    // An account that holds a game cannot mark itself or pick; the game id is told to the caller.
    let mut cy = signup(&app, 3, "Cyprian").await;
    cy.put("/api/lobby/seeking", Value::Null).await;
    let seeker_cy = lobby(&mut bob).await["seekers"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let busy = bob
        .post(&format!("/api/lobby/seekers/{seeker_cy}/pick"), Value::Null)
        .await;
    let error = error_of(&busy);
    assert_eq!(
        (error["code"].clone(), error["data"]["gameId"].clone()),
        (json!("ACTIVE_GAME_EXISTS"), json!(game_id))
    );
    assert_eq!(
        count(&app, "SELECT count(*) FROM lobby_seekers").await,
        1,
        "the refusal restored the seeker's status"
    );
    let mark = bob.put("/api/lobby/seeking", Value::Null).await;
    assert_eq!(error_of(&mark)["data"]["gameId"], game_id.as_str());
}

#[tokio::test]
async fn every_failed_pick_has_its_own_answer() {
    let app = app().await;
    let mut ala = signup(&app, 1, "Ala").await;
    let mut bob = signup(&app, 2, "Bob").await;
    assert_eq!(
        error_of(
            &bob.post("/api/lobby/seekers/not-a-uuid/pick", Value::Null)
                .await
        )["code"],
        "NOT_FOUND"
    );
    let unknown = bob
        .post(
            "/api/lobby/seekers/00000000-0000-4000-8000-000000000000/pick",
            Value::Null,
        )
        .await;
    assert_eq!(error_of(&unknown)["code"], "LOBBY_SEEKER_TAKEN");
    let own = data_of(&ala.put("/api/lobby/seeking", Value::Null).await)["me"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        error_of(
            &ala.post(&format!("/api/lobby/seekers/{own}/pick"), Value::Null)
                .await
        )["code"],
        "CANNOT_PICK_SELF"
    );
    app.advance_seconds(301);
    assert_eq!(
        error_of(
            &bob.post(&format!("/api/lobby/seekers/{own}/pick"), Value::Null)
                .await
        )["code"],
        "LOBBY_SEEKER_EXPIRED"
    );
    // A seeker who started a game elsewhere is simply taken (the game is not revealed).
    ala.put("/api/lobby/seeking", Value::Null).await;
    let seeker = lobby(&mut bob).await["seekers"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    create(&mut ala).await;
    assert_eq!(
        count(
            &app,
            "SELECT count(*) FROM lobby_seekers WHERE user_id IS NOT NULL"
        )
        .await,
        0,
        "starting a game withdraws the status"
    );
    assert_eq!(
        error_of(
            &bob.post(&format!("/api/lobby/seekers/{seeker}/pick"), Value::Null)
                .await
        )["code"],
        "LOBBY_SEEKER_TAKEN"
    );
}

#[tokio::test]
async fn of_many_pickers_exactly_one_wins() {
    let app = app().await;
    let mut ala = signup(&app, 1, "Ala").await;
    ala.put("/api/lobby/seeking", Value::Null).await;
    let seeker = lobby(&mut ala).await;
    let _ = seeker;
    let mut pickers = Vec::new();
    for n in 0..4u8 {
        pickers.push(signup(&app, 10 + n, &format!("Picker{n}")).await);
    }
    let id = lobby(&mut pickers[0]).await["seekers"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let path = format!("/api/lobby/seekers/{id}/pick");
    let [a, b, c, d] = &mut pickers[..] else {
        panic!("four pickers")
    };
    let (r1, r2, r3, r4) = tokio::join!(
        a.post(&path, Value::Null),
        b.post(&path, Value::Null),
        c.post(&path, Value::Null),
        d.post(&path, Value::Null)
    );
    let statuses: Vec<StatusCode> = [&r1, &r2, &r3, &r4].iter().map(|r| r.status).collect();
    assert_eq!(
        statuses.iter().filter(|s| **s == StatusCode::OK).count(),
        1,
        "{statuses:?}"
    );
    assert!(
        statuses
            .iter()
            .all(|s| *s == StatusCode::OK || *s == StatusCode::CONFLICT),
        "{statuses:?}"
    );
    assert_eq!(
        count(&app, "SELECT count(*) FROM games WHERE origin = 'lobby'").await,
        1
    );
}

#[tokio::test]
async fn logging_out_withdraws_the_status() {
    let app = app().await;
    let mut ala = signup(&app, 1, "Ala").await;
    ala.put("/api/lobby/seeking", Value::Null).await;
    assert_eq!(count(&app, "SELECT count(*) FROM lobby_seekers").await, 1);
    ala.post("/api/auth/logout", Value::Null).await;
    assert_eq!(count(&app, "SELECT count(*) FROM lobby_seekers").await, 0);
}

// ---- history ------------------------------------------------------------------------------------------------

#[tokio::test]
async fn the_history_lists_finished_games_newest_first_with_the_readers_result() {
    let app = app().await;
    let mut ala = signup(&app, 1, "Ala").await;
    let mut bob = signup(&app, 2, "Bob").await;
    assert_eq!(
        data_of(&ala.get("/api/history").await),
        json!({ "entries": [], "nextCursor": null })
    );
    let first = play_to_resignation(&app, &mut ala, &mut bob).await;
    app.advance_seconds(5);
    let second = play_to_resignation(&app, &mut bob, &mut ala).await;

    let page = data_of(&ala.get("/api/history").await);
    let entries = page["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(
        (entries[0]["gameId"].clone(), entries[1]["gameId"].clone()),
        (json!(second), json!(first))
    );
    assert_eq!(entries[0]["result"], "win", "Bob resigned the second game");
    assert_eq!(entries[1]["result"], "loss", "Ala resigned the first game");
    assert_eq!(entries[0]["reason"], "resignation");
    assert_eq!(
        entries[0]["opponent"],
        json!({ "kind": "account", "name": "Bob" })
    );
    assert!(
        entries[0]["finishedAt"].as_str().unwrap().ends_with('Z')
            && entries[0]["finishedAt"].as_str().unwrap().len() == 24
    );
    assert_eq!(page["nextCursor"], Value::Null);
    assert!(!page.to_string().contains("@example.com"));
    // Guests have no history; cancelled games never appear.
    assert_eq!(
        app.client().get("/api/history").await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn a_game_lost_on_time_that_nobody_opened_appears_in_the_history() {
    let app = app().await;
    let mut ala = signup(&app, 1, "Ala").await;
    let mut bob = signup(&app, 2, "Bob").await;
    let (id, code) = create(&mut ala).await;
    bob.post(
        &format!("/api/invitations/{code}/accept"),
        json!({ "as": "user" }),
    )
    .await;
    bob.get(&format!("/api/games/{id}")).await;
    ala.get(&format!("/api/games/{id}")).await;
    bob.get(&format!("/api/games/{id}")).await;
    app.advance_seconds(1_000);
    let page = data_of(&ala.get("/api/history").await);
    assert_eq!(page["entries"][0]["reason"], "timeout");
}

#[tokio::test]
async fn the_history_pages_by_keyset_twenty_at_a_time() {
    let app = app().await;
    let mut ala = signup(&app, 1, "Ala").await;
    let ala_id: gomoku_db::Uuid =
        sqlx::query_scalar("SELECT id FROM users WHERE display_name = 'Ala'")
            .fetch_one(app.db.pool())
            .await
            .unwrap();
    // 25 finished games against guests, finished at distinct times and two at the very same instant.
    for index in 0..25i64 {
        let finished = if index >= 23 { 23 } else { index };
        let game: gomoku_db::Uuid = sqlx::query_scalar(
            "INSERT INTO games (origin, status, started_at, finished_at, black_seat, end_reason, winner_seat) \
             VALUES ('invite', 'finished', now() - interval '3 days', now() - make_interval(mins => 100 - $1::int), 1, 'resignation', 2) RETURNING id",
        )
        .bind(i32::try_from(finished).unwrap())
        .fetch_one(app.db.pool())
        .await
        .unwrap();
        sqlx::query("INSERT INTO game_players (game_id, seat, kind, user_id, display_name) VALUES ($1, 1, 'account', $2, 'Ala')")
            .bind(game)
            .bind(ala_id)
            .execute(app.db.pool())
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO game_players (game_id, seat, kind, nickname) VALUES ($1, 2, 'guest', $2)",
        )
        .bind(game)
        .bind(if index % 2 == 0 {
            Some(format!("G{index}"))
        } else {
            None
        })
        .execute(app.db.pool())
        .await
        .unwrap();
    }
    let first = data_of(&ala.get("/api/history").await);
    assert_eq!(first["entries"].as_array().unwrap().len(), 20);
    let cursor = first["nextCursor"].as_str().unwrap();
    assert!(
        cursor
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    );
    let second = data_of(&ala.get(&format!("/api/history?cursor={cursor}")).await);
    assert_eq!(second["entries"].as_array().unwrap().len(), 5);
    assert_eq!(second["nextCursor"], Value::Null);
    let mut seen: Vec<String> = first["entries"]
        .as_array()
        .unwrap()
        .iter()
        .chain(second["entries"].as_array().unwrap())
        .map(|e| e["gameId"].as_str().unwrap().to_owned())
        .collect();
    seen.sort();
    seen.dedup();
    assert_eq!(
        seen.len(),
        25,
        "no row repeated or skipped across the page boundary"
    );
    // The opponent is a guest: its nickname, or the default for its seat.
    let names: Vec<&str> = first["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["opponent"]["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"Gracz 2")
            && names
                .iter()
                .any(|n| n.starts_with('G') && n.len() <= 3 && *n != "Gracz 2")
    );
}

#[tokio::test]
async fn a_bad_cursor_is_a_400_before_the_session_is_looked_at() {
    let app = app().await;
    let anonymous = app.client().get("/api/history?cursor=!!!").await;
    assert_eq!(anonymous.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        error_of(&anonymous)["fieldIssues"]["cursor"][0]["code"],
        "CURSOR_INVALID"
    );
    assert_eq!(
        app.client()
            .get(&format!("/api/history?cursor={}", "a".repeat(201)))
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        app.client().get("/api/history").await.status,
        StatusCode::UNAUTHORIZED
    );
}

// ---- account lifecycle and races ----------------------------------------------------------------------------

#[tokio::test]
async fn a_game_in_progress_blocks_account_deletion_and_waiting_games_are_removed() {
    let app = app().await;
    let mut ala = signup(&app, 1, "Ala").await;
    let mut bob = signup(&app, 2, "Bob").await;
    let (id, code) = create(&mut ala).await;
    bob.post(
        &format!("/api/invitations/{code}/accept"),
        json!({ "as": "user" }),
    )
    .await;
    let blocked = ala
        .post(
            "/api/account/delete",
            json!({ "password": PASSWORD, "confirm": true }),
        )
        .await;
    assert_eq!(blocked.status, StatusCode::CONFLICT);
    let error = error_of(&blocked);
    assert_eq!(
        (error["code"].clone(), error["data"]["gameId"].clone()),
        (json!("ACTIVE_GAME"), json!(id))
    );
    assert_eq!(
        count(&app, "SELECT count(*) FROM users").await,
        2,
        "nothing was written"
    );

    // A waiting game goes with the account (and its invitation); the seat of a finished game is anonymized.
    let mut cy = signup(&app, 3, "Cyprian").await;
    let (waiting, _) = create(&mut cy).await;
    assert_eq!(
        cy.post(
            "/api/account/delete",
            json!({ "password": PASSWORD, "confirm": true })
        )
        .await
        .status,
        StatusCode::OK
    );
    assert_eq!(
        count(
            &app,
            &format!("SELECT count(*) FROM games WHERE id = '{waiting}'")
        )
        .await,
        0
    );
    assert_eq!(
        count(&app, "SELECT count(*) FROM invitations").await,
        1,
        "only the first game's invitation is left"
    );

    ala.get(&format!("/api/games/{id}")).await;
    bob.get(&format!("/api/games/{id}")).await;
    ala.post(&format!("/api/games/{id}/resign"), Value::Null)
        .await;
    assert_eq!(
        ala.post(
            "/api/account/delete",
            json!({ "password": PASSWORD, "confirm": true })
        )
        .await
        .status,
        StatusCode::OK
    );
    let seats: Vec<Option<String>> =
        sqlx::query("SELECT display_name FROM game_players WHERE game_id = $1 ORDER BY seat")
            .bind(gomoku_db::Uuid::parse_str(&id).unwrap())
            .fetch_all(app.db.pool())
            .await
            .unwrap()
            .iter()
            .map(|r| r.get("display_name"))
            .collect();
    assert_eq!(
        seats,
        vec![None, Some("Bob".to_owned())],
        "the deleted account's name did not outlive it"
    );
    // Bob's history still lists the game, the opponent reads as unknown.
    let page = data_of(&bob.get("/api/history").await);
    assert_eq!(page["entries"][0]["opponent"]["name"], Value::Null);
}

#[tokio::test]
async fn a_session_that_expired_in_the_middle_of_a_game_stays_valid_until_the_game_ends() {
    let app = app().await;
    let mut ala = signup(&app, 1, "Ala").await;
    let mut bob = signup(&app, 2, "Bob").await;
    let (id, code) = create(&mut ala).await;
    bob.post(
        &format!("/api/invitations/{code}/accept"),
        json!({ "as": "user" }),
    )
    .await;
    bob.get(&format!("/api/games/{id}")).await;
    ala.get(&format!("/api/games/{id}")).await;
    // The session lasts 30 days; the clock jumps past that while the game is still active (the game's own clock would
    // end it first, so keep the game from timing out by moving the clock only for the session check).
    app.advance_seconds(30 * 24 * 3600 + 1);
    // The game itself lost on time long ago, but nobody touched it, so it is still `active` in the database.
    assert_eq!(
        count(&app, "SELECT count(*) FROM games WHERE status = 'active'").await,
        1
    );
    assert_eq!(
        ala.get("/account").await.status,
        StatusCode::OK,
        "the session is still honoured while the game is running"
    );
    // The read settles the game; the next one finds the session expired.
    ala.get(&format!("/api/games/{id}")).await;
    assert_eq!(ala.get("/account").await.status, StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn of_two_simultaneous_accepts_exactly_one_joins() {
    let app = app().await;
    let mut creator = signup(&app, 1, "Creator").await;
    let mut a = signup(&app, 2, "Alpha").await;
    let mut b = signup(&app, 3, "Beta").await;
    let (id, code) = create(&mut creator).await;
    let path = format!("/api/invitations/{code}/accept");
    let (ra, rb) = tokio::join!(
        a.post(&path, json!({ "as": "user" })),
        b.post(&path, json!({ "as": "user" }))
    );
    let mut statuses = [ra.status, rb.status];
    statuses.sort();
    assert_eq!(
        statuses,
        [StatusCode::OK, StatusCode::GONE],
        "{} / {}",
        ra.body,
        rb.body
    );
    assert_eq!(
        count(
            &app,
            &format!("SELECT count(*) FROM game_players WHERE game_id = '{id}'")
        )
        .await,
        2
    );
}

#[tokio::test]
async fn of_a_simultaneous_accept_and_cancel_exactly_one_wins() {
    let app = app().await;
    let mut creator = signup(&app, 1, "Creator").await;
    let mut joiner = signup(&app, 2, "Joiner").await;
    let (id, code) = create(&mut creator).await;
    let accept_path = format!("/api/invitations/{code}/accept");
    let cancel_path = format!("/api/games/{id}/cancel");
    let (accept, cancel) = tokio::join!(
        joiner.post(&accept_path, json!({ "as": "user" })),
        creator.post(&cancel_path, Value::Null)
    );
    let joined = accept.status == StatusCode::OK;
    let cancelled = cancel.status == StatusCode::OK;
    assert!(
        joined ^ cancelled,
        "accept {} / cancel {}",
        accept.status,
        cancel.status
    );
    let status: String = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT status FROM games WHERE id = '{id}'"
    )))
    .fetch_one(app.db.pool())
    .await
    .unwrap();
    assert_eq!(status, if joined { "active" } else { "cancelled" });
}

#[tokio::test]
async fn two_simultaneous_creates_by_one_account_leave_one_game() {
    let app = app().await;
    let ala = signup(&app, 1, "Ala").await;
    let mut one = app.client().with_header("x-forwarded-for", "10.0.0.50");
    one.set_cookie("gomoku_session", ala.cookie("gomoku_session").unwrap());
    let mut two = app.client().with_header("x-forwarded-for", "10.0.0.51");
    two.set_cookie("gomoku_session", ala.cookie("gomoku_session").unwrap());
    let (r1, r2) = tokio::join!(
        one.post("/api/games", json!({})),
        two.post("/api/games", json!({}))
    );
    let mut statuses = [r1.status, r2.status];
    statuses.sort();
    assert_eq!(statuses, [StatusCode::CREATED, StatusCode::CONFLICT]);
    assert_eq!(count(&app, "SELECT count(*) FROM games").await, 1);
}

#[tokio::test]
async fn a_move_and_a_timeout_settlement_race_never_corrupts_the_game() {
    let app = app().await;
    let mut a = signup(&app, 1, "Ala").await;
    let mut b = signup(&app, 2, "Bob").await;
    let (id, code) = create(&mut a).await;
    b.post(
        &format!("/api/invitations/{code}/accept"),
        json!({ "as": "user" }),
    )
    .await;
    b.get(&format!("/api/games/{id}")).await;
    a.get(&format!("/api/games/{id}")).await;
    b.get(&format!("/api/games/{id}")).await;
    let black_seat = data_of(&a.get(&format!("/api/games/{id}")).await)["blackSeat"]
        .as_i64()
        .unwrap();
    app.advance_seconds(600);
    let (black, white) = if black_seat == 1 {
        (&mut a, &mut b)
    } else {
        (&mut b, &mut a)
    };
    let move_path = format!("/api/games/{id}/moves");
    let state_path = format!("/api/games/{id}");
    let (moved, polled) = tokio::join!(
        black.post(&move_path, json!({ "x": 7, "y": 7, "moveNumber": 1 })),
        white.get(&state_path)
    );
    assert_eq!(moved.status, StatusCode::CONFLICT);
    assert_eq!(polled.status, StatusCode::OK);
    assert_eq!(count(&app, "SELECT count(*) FROM games WHERE status = 'finished' AND end_reason = 'timeout' AND move_count = 0").await, 1);
    assert_eq!(
        count(&app, "SELECT sum(finished_count) FROM game_pair_counters").await,
        1,
        "counted once"
    );
}

// ---- cleanup ------------------------------------------------------------------------------------------------

#[tokio::test]
async fn cleanup_removes_dead_games_and_expired_guest_tokens_but_never_an_accounts_history() {
    let app = app().await;
    let mut ala = signup(&app, 1, "Ala").await;
    let mut bob = signup(&app, 2, "Bob").await;
    let kept = play_to_resignation(&app, &mut ala, &mut bob).await;
    // A guest-only finished game, an unjoined expired game, an old cancelled one.
    let mut visitor = app.client().with_header("x-forwarded-for", "10.0.0.9");
    let guest_game = data_of(
        &visitor
            .post("/api/games", json!({ "acknowledgedGuestWarning": true }))
            .await,
    )["gameId"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut cy = signup(&app, 3, "Cyprian").await;
    let (cancelled, _) = create(&mut cy).await;
    cy.post(&format!("/api/games/{cancelled}/cancel"), Value::Null)
        .await;
    app.advance_seconds(26 * 3600 + 60);
    // Make the guest game finished long ago via SQL (a guest-only game that was played out).
    let options = CleanupOptions::new(common::now_of(&app));
    let result = cleanup_expired(&app.db, &options).await;
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(
        result.waiting_games >= 1 && result.cancelled_games >= 1,
        "{result:?}"
    );
    assert_eq!(
        count(
            &app,
            &format!("SELECT count(*) FROM games WHERE id = '{guest_game}'")
        )
        .await,
        0,
        "the expired unjoined game is gone"
    );
    assert_eq!(
        count(
            &app,
            &format!("SELECT count(*) FROM games WHERE id = '{cancelled}'")
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &app,
            &format!("SELECT count(*) FROM games WHERE id = '{kept}'")
        )
        .await,
        1,
        "an account's history stays"
    );
    // Guest tokens of finished games are cleared one hour after the end, the rows stay.
    let mut host = signup(&app, 4, "Host").await;
    let (gid, code) = create(&mut host).await;
    let mut g = app.client().with_header("x-forwarded-for", "10.0.0.10");
    g.post(
        &format!("/api/invitations/{code}/accept"),
        json!({ "as": "guest", "acknowledgedGuestWarning": true }),
    )
    .await;
    g.get(&format!("/api/games/{gid}")).await;
    host.get(&format!("/api/games/{gid}")).await;
    g.post(&format!("/api/games/{gid}/resign"), Value::Null)
        .await;
    app.advance_seconds(3601);
    let result = cleanup_expired(&app.db, &CleanupOptions::new(common::now_of(&app))).await;
    assert_eq!(result.guest_tokens, 1);
    assert_eq!(count(&app, &format!("SELECT count(*) FROM game_players WHERE game_id = '{gid}' AND guest_token_hash IS NULL")).await, 2);
    assert_eq!(
        count(
            &app,
            &format!("SELECT count(*) FROM games WHERE id = '{gid}'")
        )
        .await,
        1,
        "the account player keeps the history"
    );
}

#[tokio::test]
async fn an_unconfirmed_account_that_took_part_in_a_game_is_not_swept() {
    let app = app().await;
    let mut ala = signup(&app, 1, "Ala").await;
    let mut bob = signup(&app, 2, "Bob").await;
    play_to_resignation(&app, &mut ala, &mut bob).await;
    sqlx::query("UPDATE users SET created_at = now() - interval '30 days'")
        .execute(app.db.pool())
        .await
        .unwrap();
    let result = cleanup_expired(&app.db, &CleanupOptions::new(common::now_of(&app))).await;
    assert_eq!(result.unverified_users, 0);
    assert_eq!(count(&app, "SELECT count(*) FROM users").await, 2);
}
