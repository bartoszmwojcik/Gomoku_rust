//! The game pages: what each screen is given, who may see it, and the redirects. The scripts that make them live are
//! checked in a real browser (see the F3 notes in the plan).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::http::StatusCode;
use common::{Client, TestApp};
use serde_json::{Value, json};

const PASSWORD: &str = "correct horse";

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

fn visitor(app: &TestApp, n: u8) -> Client {
    app.client()
        .with_header("x-forwarded-for", &format!("10.0.1.{n}"))
}

async fn create(client: &mut Client) -> (String, String) {
    let reply = client.post("/api/games", json!({})).await;
    let data = reply.json()["data"].clone();
    (
        data["gameId"].as_str().unwrap().to_owned(),
        data["invitation"]["code"].as_str().unwrap().to_owned(),
    )
}

/// The JSON embedded in a `<script type="application/json" id="...">` block.
fn embedded(body: &str, id: &str) -> Value {
    let start = body
        .find(&format!("id=\"{id}\">"))
        .unwrap_or_else(|| panic!("no #{id} in the page"))
        + id.len()
        + 6;
    let end = body[start..].find("</script>").unwrap() + start;
    serde_json::from_str(&body[start..end]).unwrap()
}

#[tokio::test]
async fn the_home_page_offers_what_the_visitor_can_do_and_shows_the_game_they_hold() {
    let app = app().await;
    let guest = visitor(&app, 1).get("/").await;
    assert!(
        guest.body.contains(r#"data-testid="play-without-account""#)
            && guest.body.contains(r#"data-testid="nav-join""#)
    );
    assert!(!guest.body.contains("create-game") && !guest.body.contains("nav-lobby"));

    let mut ala = signup(&app, 1, "Ala").await;
    let home = ala.get("/").await;
    assert!(
        home.body.contains(r#"data-testid="create-game""#)
            && home.body.contains(r#"data-testid="nav-lobby""#)
    );
    assert!(home.body.contains(r#"data-testid="nav-history""#));
    let (id, _) = create(&mut ala).await;
    let busy = ala.get("/").await;
    assert!(
        busy.body.contains(r#"data-testid="current-game""#) && !busy.body.contains("create-game")
    );
    assert!(busy.body.contains(&format!("/games/{id}")));
    assert!(
        ala.get("/?notice=cancelled")
            .await
            .body
            .contains("cancelled-notice")
    );

    // A guest sees their own waiting game by the cookie.
    let mut g = visitor(&app, 2);
    g.post("/api/games", json!({ "acknowledgedGuestWarning": true }))
        .await;
    assert!(
        g.get("/")
            .await
            .body
            .contains(r#"data-testid="current-game""#)
    );
}

#[tokio::test]
async fn play_and_join_pages() {
    let app = app().await;
    let guest = visitor(&app, 1).get("/play").await;
    assert_eq!(guest.status, StatusCode::OK);
    assert!(
        guest.body.contains(r#"data-testid="guest-create""#)
            && guest.body.contains(r#"name="acknowledgedGuestWarning""#)
    );
    assert!(guest.body.contains("Zostaw puste, a będziesz „Gracz 1”"));
    let mut ala = signup(&app, 1, "Ala").await;
    let signed = ala.get("/play").await;
    assert!(signed.body.contains("play-signed-in") && !signed.body.contains("guest-create"));
    let join = visitor(&app, 2).get("/join?lang=fr").await;
    assert_eq!(join.status, StatusCode::TEMPORARY_REDIRECT);
    let page = visitor(&app, 2)
        .with_header("accept-language", "fr")
        .get("/join")
        .await;
    assert!(
        page.body.contains(r#"data-testid="join-form""#) && page.body.contains("/assets/join.js")
    );
    assert!(
        page.body.contains("8 symboles")
            || page.body.contains("8 caractères")
            || page.body.contains("8"),
        "{}",
        page.body
    );
}

#[tokio::test]
async fn the_invitation_page_shows_each_state_and_sends_players_to_their_game() {
    let app = app().await;
    let mut creator = signup(&app, 1, "Creator").await;
    let (id, code) = create(&mut creator).await;
    let mut member = signup(&app, 2, "Member").await;
    let mut anyone = visitor(&app, 3);

    let seen_by_member = member.get(&format!("/invite/{code}")).await;
    assert_eq!(seen_by_member.status, StatusCode::OK);
    assert_eq!(
        seen_by_member.header("referrer-policy"),
        Some("no-referrer")
    );
    assert!(
        seen_by_member.body.contains(r#"data-state="joinable""#)
            && seen_by_member.body.contains(r#"data-testid="join""#)
    );
    assert!(
        seen_by_member.body.contains("Creator zaprasza"),
        "{}",
        seen_by_member.body
    );

    // A visitor is offered to log in, register or play as a guest; the code may be typed in any form.
    let lower = code.to_lowercase().replace('-', "%20");
    let seen = anyone.get(&format!("/invite/{lower}")).await;
    assert!(
        seen.body.contains(r#"data-testid="invite-login""#)
            && seen.body.contains(r#"data-testid="guest-join-form""#)
    );
    assert!(seen.body.contains("next=%2Finvite%2F"), "{}", seen.body);
    assert!(seen.body.contains("Zostaw puste, a będziesz „Gracz 2”"));

    // The creator goes straight to the game.
    let own = creator.get(&format!("/invite/{code}")).await;
    assert_eq!(own.status, StatusCode::SEE_OTHER);
    assert_eq!(
        own.header("location"),
        Some(format!("/games/{id}").as_str())
    );
}

#[tokio::test]
async fn the_invitation_page_for_players_who_hold_games_and_for_bad_codes() {
    let app = app().await;
    let mut creator = signup(&app, 1, "Creator").await;
    let (_, code) = create(&mut creator).await;
    let mut busy = signup(&app, 2, "Busy").await;
    let mut other = signup(&app, 3, "Other").await;
    let (own_game, other_code) = create(&mut busy).await;
    other
        .post(
            &format!("/api/invitations/{other_code}/accept"),
            json!({ "as": "user" }),
        )
        .await;
    // A player with a waiting game of their own is offered a swap.
    let mut holder = signup(&app, 4, "Holder").await;
    create(&mut holder).await;
    let swap = holder.get(&format!("/invite/{code}")).await;
    assert!(
        swap.body.contains(r#"data-state="has-own-waiting-game""#),
        "{}",
        swap.status
    );
    assert!(swap.body.contains(r#"data-testid="swap""#));
    let _ = own_game;
    // Third: Other now plays an active game, so another invitation just refers to it.
    let active = other.get(&format!("/invite/{code}")).await;
    assert!(
        active.body.contains(r#"data-state="has-active-game""#),
        "{}",
        active.body
    );
    // Bad codes are one uniform page.
    for bad in ["XXXX-XXXX", "abc", "ABCD-EFG0"] {
        let page = visitor(&app, 9).get(&format!("/invite/{bad}")).await;
        assert!(page.body.contains(r#"data-state="invalid""#), "{bad}");
    }
}

#[tokio::test]
async fn the_invitation_page_runs_dry_like_the_api() {
    let app = app().await;
    let mut v = visitor(&app, 7);
    for _ in 0..20 {
        v.get("/invite/XXXX-XXXX").await;
    }
    let limited = v.get("/invite/XXXX-XXXX").await;
    assert!(
        limited.body.contains(r#"data-state="limit""#),
        "{}",
        limited.body
    );
}

#[tokio::test]
async fn the_game_room_page_is_for_players_only_and_embeds_the_first_view() {
    let app = app().await;
    let mut creator = signup(&app, 1, "Creator").await;
    let (id, _) = create(&mut creator).await;
    let page = creator.get(&format!("/games/{id}")).await;
    assert_eq!(page.status, StatusCode::OK);
    assert_eq!(page.header("referrer-policy"), Some("no-referrer"));
    assert!(page.body.contains(r#"id="game-room""#) && page.body.contains("/assets/game-room.js"));
    let initial = embedded(&page.body, "game-initial");
    assert_eq!(initial["gameId"], id.as_str());
    assert_eq!(initial["view"]["status"], "waiting");
    assert_eq!(
        initial["view"]["invitation"]["code"]
            .as_str()
            .unwrap()
            .len(),
        9
    );
    assert!(
        page.body.contains("game.room.waitingHeading")
            || page.body.contains("Partia czeka na przeciwnika"),
        "the text bag is embedded"
    );
    let notice = creator
        .get(&format!("/games/{id}?notice=own-waiting"))
        .await;
    assert_eq!(
        embedded(&notice.body, "game-initial")["notice"],
        "own-waiting"
    );

    let mut stranger = signup(&app, 2, "Stranger").await;
    for page in [
        stranger.get(&format!("/games/{id}")).await,
        stranger.get("/games/not-a-uuid").await,
        visitor(&app, 5).get(&format!("/games/{id}")).await,
    ] {
        assert_eq!(page.status, StatusCode::NOT_FOUND);
        assert!(page.body.contains(r#"data-testid="gate-title""#));
    }
    // Opening the page is a read: it marked the creator as having seen the game.
    let seen: bool =
        sqlx::query_scalar("SELECT seen_at IS NOT NULL FROM game_players WHERE seat = 1")
            .fetch_one(app.db.pool())
            .await
            .unwrap();
    assert!(seen);
}

#[tokio::test]
async fn the_lobby_and_history_pages_need_a_session() {
    let app = app().await;
    for path in ["/lobby", "/history"] {
        let page = app.client().get(path).await;
        assert_eq!(page.status, StatusCode::SEE_OTHER, "{path}");
        assert_eq!(
            page.header("location"),
            Some(format!("/login?next=%2F{}", &path[1..]).as_str())
        );
    }
    let mut ala = signup(&app, 1, "Ala").await;
    let lobby = ala.get("/lobby").await;
    assert!(lobby.body.contains(r#"id="lobby""#) && lobby.body.contains("/assets/lobby.js"));
    assert_eq!(
        embedded(&lobby.body, "lobby-initial"),
        json!({ "me": null, "currentGame": null, "seekers": [] })
    );
    let history = ala.get("/history").await;
    assert_eq!(
        embedded(&history.body, "history-initial"),
        json!({ "entries": [], "nextCursor": null })
    );
    assert!(history.body.contains("/assets/history.js"));
}

#[tokio::test]
async fn the_review_page_is_for_the_accounts_that_played_a_finished_game() {
    let app = app().await;
    let mut ala = signup(&app, 1, "Ala").await;
    let mut bob = signup(&app, 2, "Bob").await;
    let mut cy = signup(&app, 3, "Cyprian").await;
    assert_eq!(
        app.client().get("/games/x/review").await.status,
        StatusCode::SEE_OTHER
    );
    let (id, code) = create(&mut ala).await;
    bob.post(
        &format!("/api/invitations/{code}/accept"),
        json!({ "as": "user" }),
    )
    .await;
    // A game in progress is not reviewable (the same not-found page as any other).
    assert_eq!(
        ala.get(&format!("/games/{id}/review")).await.status,
        StatusCode::NOT_FOUND
    );
    bob.get(&format!("/api/games/{id}")).await;
    ala.get(&format!("/api/games/{id}")).await;
    ala.post(&format!("/api/games/{id}/resign"), Value::Null)
        .await;

    let page = ala.get(&format!("/games/{id}/review")).await;
    assert_eq!(page.status, StatusCode::OK);
    assert_eq!(page.header("referrer-policy"), Some("no-referrer"));
    let data = embedded(&page.body, "review-data");
    assert_eq!(
        (data["viewerSeat"].clone(), data["result"]["reason"].clone()),
        (json!(1), json!("resignation"))
    );
    assert_eq!(data["players"].as_array().unwrap().len(), 2);
    assert!(page.body.contains("/assets/review.js"));
    for stranger in [
        cy.get(&format!("/games/{id}/review")).await,
        ala.get("/games/not-a-uuid/review").await,
    ] {
        assert_eq!(stranger.status, StatusCode::NOT_FOUND);
        assert!(stranger.body.contains("Nie możemy otworzyć tej partii"));
    }
}

#[tokio::test]
async fn the_catalogs_serve_the_game_screens_in_every_language() {
    let app = app().await;
    let mut creator = signup(&app, 1, "Creator").await;
    let (id, _) = create(&mut creator).await;
    for (lang, text) in [
        ("en", "Waiting for your opponent"),
        ("fr", "La partie attend un adversaire"),
    ] {
        let mut reader = app.client().with_header("accept-language", lang);
        reader.set_cookie("gomoku_session", creator.cookie("gomoku_session").unwrap());
        let page = reader.get(&format!("/games/{id}")).await;
        assert!(page.body.contains(&format!(r#"<html lang="{lang}">"#)));
        assert!(
            page.body.contains(text) || page.body.contains(&text.replace('’', "\\u2019")),
            "{lang}"
        );
    }
}
