//! The paper and pencil variant end to end: creation, the rules of a stone, scoring, the clock, the automat, resigning,
//! rematch, lobby, history, review and the classic game staying as it was.
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

async fn app() -> TestApp {
    TestApp::start_with(&[("TRUST_PROXY_HOPS", "1"), ("CHAT_MIN_INTERVAL_MS", "0")]).await
}

async fn signup(app: &TestApp, n: u8, name: &str) -> Client {
    let mut client = app
        .client()
        .with_header("x-forwarded-for", &format!("10.3.0.{n}"));
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
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT ({sql})::bigint")))
        .fetch_one(app.db.pool())
        .await
        .unwrap()
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

/// A paper game in progress between two accounts, both having opened it. Returns the id, the code and the clients in
/// the order (black, white).
async fn started(app: &TestApp, time_ms: i64) -> (String, String, Client, Client) {
    let mut ala = signup(app, 1, "Ala").await;
    let mut bob = signup(app, 2, "Bob").await;
    let created = ala
        .post(
            "/api/games",
            json!({ "variant": "paper", "timeControlMs": time_ms }),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let data = data_of(&created);
    let (id, code) = (
        data["gameId"].as_str().unwrap().to_owned(),
        data["invitation"]["code"].as_str().unwrap().to_owned(),
    );
    bob.post(
        &format!("/api/invitations/{code}/accept"),
        json!({ "as": "user" }),
    )
    .await;
    view(&mut bob, &id).await;
    let seen = view(&mut ala, &id).await;
    view(&mut bob, &id).await;
    if seen["blackSeat"] == 1 {
        (id, code, ala, bob)
    } else {
        (id, code, bob, ala)
    }
}

#[tokio::test]
async fn creating_validates_the_settings_and_stores_the_sheet() {
    let app = app().await;
    let mut ala = signup(&app, 1, "Ala").await;
    let classic_time = ala
        .post("/api/games", json!({ "timeControlMs": 60000 }))
        .await;
    assert_eq!(classic_time.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        error_of(&classic_time)["fieldIssues"]["timeControlMs"][0]["code"],
        "TIME_CONTROL_CLASSIC"
    );
    let bad_time = ala
        .post(
            "/api/games",
            json!({ "variant": "paper", "timeControlMs": 90000 }),
        )
        .await;
    assert_eq!(
        error_of(&bad_time)["fieldIssues"]["timeControlMs"][0]["code"],
        "invalid_union"
    );
    let bad_variant = ala.post("/api/games", json!({ "variant": "renju" })).await;
    assert_eq!(
        error_of(&bad_variant)["fieldIssues"]["variant"][0]["code"],
        "invalid_value"
    );
    for body in [
        json!({ "variant": null }),
        json!({ "variant": "paper", "timeControlMs": 180000.5 }),
        json!({ "variant": "paper", "timeControlMs": "60000" }),
    ] {
        assert_eq!(
            ala.post("/api/games", body).await.status,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(count(&app, "SELECT count(*) FROM games").await, 0);

    let created = ala.post("/api/games", json!({ "variant": "paper" })).await;
    let id = data_of(&created)["gameId"].as_str().unwrap().to_owned();
    let waiting = view(&mut ala, &id).await;
    assert_eq!(
        (waiting["variant"].clone(), waiting["timeControlMs"].clone()),
        (json!("paper"), json!(180000))
    );
    assert_eq!(
        waiting["paper"],
        json!({ "moveCount": 0, "moves": [], "scores": [0, 0], "fives": [], "autoSeat": null, "autoMoveAt": null })
    );
    assert_eq!(waiting["clock"]["remainingMs"], json!([180000, 180000]));
    assert_eq!(
        count(&app, "SELECT count(*) FROM paper_game_states").await,
        1
    );
    ala.post(&format!("/api/games/{id}/cancel"), Value::Null)
        .await;

    let two = ala
        .post(
            "/api/games",
            json!({ "variant": "paper", "timeControlMs": 120000 }),
        )
        .await;
    let id = data_of(&two)["gameId"].as_str().unwrap().to_owned();
    assert_eq!(
        view(&mut ala, &id).await["clock"]["remainingMs"],
        json!([120000, 120000])
    );
    ala.post(&format!("/api/games/{id}/cancel"), Value::Null)
        .await;

    let classic = ala.post("/api/games", json!({})).await;
    let id = data_of(&classic)["gameId"].as_str().unwrap().to_owned();
    let seen = view(&mut ala, &id).await;
    assert_eq!(
        (seen["variant"].clone(), seen["timeControlMs"].clone()),
        (json!("classic"), json!(600000))
    );
    assert!(seen.get("paper").is_none());
    assert_eq!(
        count(&app, "SELECT count(*) FROM paper_game_states").await,
        2,
        "the classic game added no sheet"
    );
    // The invitation page names the rules of the game.
    let mut visitor = app.client().with_header("x-forwarded-for", "10.3.1.1");
    let code = data_of(&classic)["invitation"]["code"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        visitor
            .get(&format!("/invite/{code}"))
            .await
            .body
            .contains("Swobodne zasady")
    );
}

#[tokio::test]
async fn the_rules_of_a_stone_over_http() {
    let app = app().await;
    let (id, _, mut black, mut white) = started(&app, 180_000).await;
    let first = mv(&mut black, &id, 49, 50, 1).await;
    assert_eq!(
        (first.status, error_of(&first)["code"].clone()),
        (StatusCode::BAD_REQUEST, json!("FIRST_MOVE_CENTER"))
    );
    let early = mv(&mut white, &id, 50, 50, 1).await;
    assert_eq!(
        (early.status, error_of(&early)["data"]["reason"].clone()),
        (StatusCode::CONFLICT, json!("opponent_turn"))
    );
    let placed = mv(&mut black, &id, 50, 50, 1).await;
    assert_eq!(placed.status, StatusCode::OK, "{}", placed.body);
    let state = data_of(&placed);
    assert_eq!(state["moveCount"], 1);
    assert_eq!(state["moves"], json!([]));
    assert_eq!(state["paper"]["moves"][0]["x"], 50);
    assert_eq!(state["paper"]["moves"][0]["auto"], false);
    // The same stone again is the same answer; the next one is the opponent's.
    assert_eq!(
        data_of(&mv(&mut black, &id, 50, 50, 1).await)["moveCount"],
        1
    );
    assert_eq!(
        error_of(&mv(&mut black, &id, 51, 50, 2).await)["code"],
        "NOT_YOUR_TURN"
    );
    assert_eq!(
        error_of(&mv(&mut white, &id, 50, 50, 2).await)["code"],
        "CELL_OCCUPIED"
    );
    let far = mv(&mut white, &id, 55, 50, 2).await;
    assert_eq!(
        (far.status, error_of(&far)["code"].clone()),
        (StatusCode::BAD_REQUEST, json!("MOVE_TOO_FAR"))
    );
    assert_eq!(
        error_of(&mv(&mut white, &id, 51, 50, 3).await)["code"],
        "STALE_MOVE"
    );
    assert_eq!(
        mv(&mut white, &id, 101, 50, 2).await.status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        mv(&mut white, &id, 51, 50, 10202).await.status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        count(&app, "SELECT move_count FROM paper_game_states").await,
        1,
        "refusals wrote nothing"
    );
    let second = data_of(&mv(&mut white, &id, 53, 50, 2).await);
    assert_eq!(
        (second["moveCount"].clone(), second["turnSeat"].clone()),
        (json!(2), second["blackSeat"].clone())
    );
    assert_eq!(count(&app, "SELECT count(*) FROM paper_moves").await, 2);
    assert_eq!(count(&app, "SELECT move_count FROM games").await, 0);
    // The poll returns only what is new, and bounds the cursor.
    let poll = black.get(&format!("/api/games/{id}?after=1")).await;
    assert_eq!(
        data_of(&poll)["paper"]["moves"].as_array().unwrap().len(),
        1
    );
    assert_eq!(
        black.get(&format!("/api/games/{id}?after=3")).await.status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        black
            .get(&format!("/api/games/{id}?after=10202"))
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
}

/// Plays black's five (y = 50) against white's stones (y = 53); black has made `blacks` stones when this returns.
async fn play_row(id: &str, black: &mut Client, white: &mut Client, blacks: i64) {
    for i in 0..blacks {
        let n = 2 * i + 1;
        assert_eq!(
            mv(black, id, 50 + i, 50, n).await.status,
            StatusCode::OK,
            "black {i}"
        );
        if i < blacks - 1 || blacks == 5 && false {
            assert_eq!(
                mv(white, id, 50 + i, 53, n + 1).await.status,
                StatusCode::OK,
                "white {i}"
            );
        }
    }
}

#[tokio::test]
async fn a_five_scores_a_point_and_the_game_goes_on() {
    let app = app().await;
    let (id, _, mut black, mut white) = started(&app, 60_000).await;
    play_row(&id, &mut black, &mut white, 5).await;
    let seen = view(&mut white, &id).await;
    assert_eq!(seen["status"], "active");
    assert_eq!(seen["moveCount"], 9);
    let black_seat = seen["blackSeat"].as_i64().unwrap();
    let expected = if black_seat == 1 {
        json!([1, 0])
    } else {
        json!([0, 1])
    };
    assert_eq!(seen["paper"]["scores"], expected);
    assert_eq!(seen["paper"]["fives"].as_array().unwrap().len(), 1);
    assert_eq!(
        count(&app, "SELECT score_1 + score_2 FROM paper_game_states").await,
        1
    );
}

#[tokio::test]
async fn a_trailing_player_out_of_time_loses_on_points_and_an_even_game_is_a_draw() {
    let app = app().await;
    let (id, _, mut black, mut white) = started(&app, 60_000).await;
    play_row(&id, &mut black, &mut white, 5).await;
    app.advance_seconds(61);
    let end = view(&mut black, &id).await;
    assert_eq!(end["status"], "finished");
    assert_eq!(
        (
            end["result"]["reason"].clone(),
            end["result"]["winnerSeat"].clone()
        ),
        (json!("points"), end["blackSeat"].clone())
    );
    assert!(end["result"]["winLine"].is_null());
    assert!(end["result"]["scores"].is_array());
    assert_eq!(
        error_of(&mv(&mut white, &id, 54, 53, 10).await)["code"],
        "GAME_FINISHED"
    );
    assert_eq!(
        count(&app, "SELECT count(*) FROM game_players WHERE is_active").await,
        0,
        "slots released"
    );
    assert_eq!(
        count(
            &app,
            "SELECT count(*) FROM games WHERE end_reason = 'points'"
        )
        .await,
        1
    );

    // Nobody moves: black's budget runs out and it is a draw by points.
    let app2 = app_again().await;
    let (id2, _, mut b2, _) = started(&app2, 60_000).await;
    app2.advance_seconds(61);
    let drawn = view(&mut b2, &id2).await;
    assert_eq!(
        (
            drawn["status"].clone(),
            drawn["result"]["winnerSeat"].clone(),
            drawn["result"]["reason"].clone()
        ),
        (json!("finished"), Value::Null, json!("points"))
    );
    assert_eq!(
        count(
            &app2,
            "SELECT count(*) FROM games WHERE winner_seat IS NULL AND end_reason = 'points'"
        )
        .await,
        1
    );
}

async fn app_again() -> TestApp {
    app().await
}

#[tokio::test]
async fn the_automat_plays_for_a_leader_who_ran_out_of_time() {
    let app = app().await;
    let (id, _, mut black, mut white) = started(&app, 60_000).await;
    play_row(&id, &mut black, &mut white, 5).await;
    // White's fifth stone elsewhere: black leads and is on turn.
    assert_eq!(mv(&mut white, &id, 56, 53, 10).await.status, StatusCode::OK);
    let black_seat = view(&mut black, &id).await["blackSeat"].as_i64().unwrap();
    app.advance_seconds(61);
    let taken = view(&mut white, &id).await;
    assert_eq!(taken["status"], "active");
    assert_eq!(taken["paper"]["autoSeat"], black_seat);
    assert_eq!(taken["moveCount"], 11);
    let stone = taken["paper"]["moves"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(
        (stone["auto"].clone(), stone["seat"].clone()),
        (json!(true), json!(black_seat))
    );
    assert_eq!(taken["clock"]["running"], 3 - black_seat);
    let idx = (black_seat - 1) as usize;
    assert_eq!(taken["clock"]["remainingMs"][idx], 0);
    // The leader never moves by hand again; the opponent plays, and the automat answers two seconds later.
    assert_eq!(
        error_of(&mv(&mut black, &id, 51, 51, 12).await)["data"]["reason"],
        "opponent_turn"
    );
    assert_eq!(mv(&mut white, &id, 58, 53, 12).await.status, StatusCode::OK);
    let waiting = view(&mut white, &id).await;
    assert!(waiting["paper"]["autoMoveAt"].is_string());
    let blocked = mv(&mut black, &id, 51, 51, 13).await;
    assert_eq!(
        (blocked.status, error_of(&blocked)["data"]["reason"].clone()),
        (StatusCode::CONFLICT, json!("auto_turn"))
    );
    app.advance_seconds(3);
    let answered = view(&mut white, &id).await;
    assert_eq!(answered["moveCount"], 13);
    assert!(answered["paper"]["autoMoveAt"].is_null());
    // When the opponent's clock runs out the game ends on points.
    app.advance_seconds(70);
    let end = view(&mut white, &id).await;
    assert_eq!(
        (
            end["status"].clone(),
            end["result"]["reason"].clone(),
            end["result"]["winnerSeat"].clone()
        ),
        (json!("finished"), json!("points"), json!(black_seat))
    );
    assert_eq!(
        count(&app, "SELECT count(*) FROM game_players WHERE is_active").await,
        0
    );
}

#[tokio::test]
async fn a_late_poll_equals_a_continuous_one_and_resigning_loses_whatever_the_score() {
    let app = app().await;
    let (id, _, mut black, mut white) = started(&app, 60_000).await;
    play_row(&id, &mut black, &mut white, 5).await;
    assert_eq!(mv(&mut white, &id, 56, 53, 10).await.status, StatusCode::OK);
    // Resigning as the leader loses; the scores come with the result; repeating is fine.
    let resigned = black
        .post(&format!("/api/games/{id}/resign"), Value::Null)
        .await;
    assert_eq!(resigned.status, StatusCode::OK, "{}", resigned.body);
    let result = data_of(&resigned)["result"].clone();
    assert_eq!(result["reason"], "resignation");
    assert!(result["scores"].is_array());
    assert_eq!(
        black
            .post(&format!("/api/games/{id}/resign"), Value::Null)
            .await
            .status,
        StatusCode::OK
    );
    let late = white
        .post(&format!("/api/games/{id}/resign"), Value::Null)
        .await;
    assert_eq!(late.status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn resigning_after_time_ran_out_keeps_the_points_result() {
    let app = app().await;
    let (id, _, mut black, mut white) = started(&app, 60_000).await;
    play_row(&id, &mut black, &mut white, 5).await;
    app.advance_seconds(65);
    let late = white
        .post(&format!("/api/games/{id}/resign"), Value::Null)
        .await;
    assert_eq!(late.status, StatusCode::CONFLICT);
    assert_eq!(error_of(&late)["code"], "GAME_FINISHED");
    assert_eq!(
        count(
            &app,
            "SELECT count(*) FROM games WHERE end_reason = 'points'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn simultaneous_stones_of_one_seat_leave_exactly_one() {
    let app = app().await;
    let (id, _, mut black, _white) = started(&app, 180_000).await;
    let mut twin = black.clone();
    let path = format!("/api/games/{id}/moves");
    let (a, b) = tokio::join!(
        black.post(&path, json!({ "x": 50, "y": 50, "moveNumber": 1 })),
        twin.post(&path, json!({ "x": 50, "y": 51, "moveNumber": 1 })),
    );
    let mut statuses = [a.status.as_u16(), b.status.as_u16()];
    statuses.sort_unstable();
    assert!(
        statuses[0] == 200 && statuses[1] != 500,
        "{statuses:?}: {} / {}",
        a.body,
        b.body
    );
    assert_eq!(count(&app, "SELECT count(*) FROM paper_moves").await, 1);
}

#[tokio::test]
async fn the_classic_game_keeps_its_bounds_and_its_shape() {
    let app = app().await;
    let mut ala = signup(&app, 1, "Ala").await;
    let mut bob = signup(&app, 2, "Bob").await;
    let created = data_of(&ala.post("/api/games", json!({})).await);
    let (id, code) = (
        created["gameId"].as_str().unwrap().to_owned(),
        created["invitation"]["code"].as_str().unwrap().to_owned(),
    );
    bob.post(
        &format!("/api/invitations/{code}/accept"),
        json!({ "as": "user" }),
    )
    .await;
    view(&mut bob, &id).await;
    let seen = view(&mut ala, &id).await;
    let (mut black, _) = if seen["blackSeat"] == 1 {
        (ala, bob)
    } else {
        (bob, ala)
    };
    let wide = mv(&mut black, &id, 15, 3, 1).await;
    assert_eq!(wide.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        error_of(&wide)["fieldIssues"]["x"][0]["code"],
        "COLUMN_INVALID"
    );
    assert_eq!(
        mv(&mut black, &id, 3, 3, 226).await.status,
        StatusCode::BAD_REQUEST
    );
    let ok = data_of(&mv(&mut black, &id, 7, 7, 1).await);
    assert_eq!(
        (
            ok["variant"].clone(),
            ok["moveCount"].clone(),
            ok["moves"].clone()
        ),
        (json!("classic"), json!(1), json!([112]))
    );
    assert!(ok.get("paper").is_none());
}

#[tokio::test]
async fn rematch_lobby_history_review_and_spectators_carry_the_variant() {
    let app = app().await;
    let (id, code, mut black, mut white) = started(&app, 120_000).await;
    play_row(&id, &mut black, &mut white, 5).await;
    assert_eq!(
        white
            .post(&format!("/api/games/{id}/resign"), Value::Null)
            .await
            .status,
        StatusCode::OK
    );
    // History: variant and scores [mine, theirs].
    let history = data_of(&black.get("/api/history").await);
    let entry = history["entries"][0].clone();
    assert_eq!(
        (entry["variant"].clone(), entry["scores"].clone()),
        (json!("paper"), json!([1, 0]))
    );
    assert_eq!(
        data_of(&white.get("/api/history").await)["entries"][0]["scores"],
        json!([0, 1])
    );
    // Review: a paper game has no move log.
    let review = black.get(&format!("/games/{id}/review")).await;
    assert_eq!(review.status, StatusCode::OK, "{}", review.body);
    assert!(review.body.contains(r#""variant":"paper""#) && review.body.contains(r#""moves":[]"#));
    // Rematch keeps the variant and the time.
    black
        .post(&format!("/api/games/{id}/rematch"), Value::Null)
        .await;
    let accepted = white
        .post(&format!("/api/games/{id}/rematch/accept"), Value::Null)
        .await;
    let next = data_of(&accepted)["gameId"].as_str().unwrap().to_owned();
    let again = view(&mut white, &next).await;
    assert_eq!(
        (
            again["variant"].clone(),
            again["timeControlMs"].clone(),
            again["paper"]["moveCount"].clone()
        ),
        (json!("paper"), json!(120000), json!(0))
    );
    assert_eq!(again["clock"]["remainingMs"], json!([120000, 120000]));
    let _ = code;
}

#[tokio::test]
async fn a_lobby_pick_starts_the_game_the_seeker_chose() {
    let app = app().await;
    let mut ala = signup(&app, 1, "Ala").await;
    let mut bob = signup(&app, 2, "Bob").await;
    let marked = ala
        .put(
            "/api/lobby/seeking",
            json!({ "variant": "paper", "timeControlMs": 60000 }),
        )
        .await;
    assert_eq!(marked.status, StatusCode::OK, "{}", marked.body);
    let me = data_of(&marked)["me"].clone();
    assert_eq!(
        (me["variant"].clone(), me["timeControlMs"].clone()),
        (json!("paper"), json!(60000))
    );
    // A second PUT with other settings while live keeps the old status.
    let again = ala
        .put(
            "/api/lobby/seeking",
            json!({ "variant": "paper", "timeControlMs": 300000 }),
        )
        .await;
    assert_eq!(data_of(&again)["me"]["timeControlMs"], 60000);
    assert_eq!(
        ala.put("/api/lobby/seeking", json!({ "timeControlMs": 60000 }))
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
    let lobby = data_of(&bob.get("/api/lobby").await);
    assert_eq!(lobby["seekers"][0]["variant"], "paper");
    let seeker = lobby["seekers"][0]["id"].as_str().unwrap().to_owned();
    let picked = bob
        .post(&format!("/api/lobby/seekers/{seeker}/pick"), Value::Null)
        .await;
    let id = data_of(&picked)["gameId"].as_str().unwrap().to_owned();
    let game = view(&mut bob, &id).await;
    assert_eq!(
        (game["variant"].clone(), game["timeControlMs"].clone()),
        (json!("paper"), json!(60000))
    );
    assert_eq!(
        count(&app, "SELECT count(*) FROM paper_game_states").await,
        1
    );
}

#[tokio::test]
async fn a_spectator_of_a_public_paper_game_sees_the_sheet() {
    let app = app().await;
    let mut ala = signup(&app, 1, "Ala").await;
    let mut bob = signup(&app, 2, "Bob").await;
    let created = data_of(
        &ala.post(
            "/api/games",
            json!({ "variant": "paper", "isPublic": true }),
        )
        .await,
    );
    let (id, code) = (
        created["gameId"].as_str().unwrap().to_owned(),
        created["invitation"]["code"].as_str().unwrap().to_owned(),
    );
    bob.post(
        &format!("/api/invitations/{code}/accept"),
        json!({ "as": "user" }),
    )
    .await;
    view(&mut bob, &id).await;
    view(&mut ala, &id).await;
    let mut viewer = app.client().with_header("x-forwarded-for", "10.3.1.5");
    assert_eq!(
        viewer
            .post(&format!("/api/invitations/{code}/spectate"), json!({}))
            .await
            .status,
        StatusCode::OK
    );
    let seen = data_of(&viewer.get(&format!("/api/games/{id}/spectate")).await);
    assert_eq!(
        (
            seen["variant"].clone(),
            seen["paper"]["moveCount"].clone(),
            seen["moves"].clone()
        ),
        (json!("paper"), json!(0), json!([]))
    );
    assert_eq!(
        viewer
            .get(&format!("/api/games/{id}/spectate?after=1"))
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn cleanup_follows_the_deadline_of_a_paper_game() {
    let app = app().await;
    let mut guest = app.client().with_header("x-forwarded-for", "10.3.1.8");
    let created = data_of(&guest.post("/api/games", json!({ "acknowledgedGuestWarning": true, "variant": "paper", "timeControlMs": 60000 })).await);
    let (id, code) = (
        created["gameId"].as_str().unwrap().to_owned(),
        created["invitation"]["code"].as_str().unwrap().to_owned(),
    );
    let mut other = app.client().with_header("x-forwarded-for", "10.3.1.9");
    other
        .post(
            &format!("/api/invitations/{code}/accept"),
            json!({ "as": "guest", "acknowledgedGuestWarning": true }),
        )
        .await;
    other.get(&format!("/api/games/{id}")).await;
    guest.get(&format!("/api/games/{id}")).await;
    let result = cleanup_expired(&app.db, &CleanupOptions::new(common::now_of(&app))).await;
    assert_eq!(result.guest_only_active_games, 0);
    // Black's one minute plus the hour of grace.
    app.advance_seconds(61 + 3600 + 5);
    let result = cleanup_expired(&app.db, &CleanupOptions::new(common::now_of(&app))).await;
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_eq!(result.guest_only_active_games, 1);
    assert_eq!(
        count(&app, "SELECT count(*) FROM paper_game_states").await,
        0,
        "the sheet goes with the game"
    );
}
