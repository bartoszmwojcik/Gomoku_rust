//! In-process HTTP tests without a database: health, static pages, language switch.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::http::StatusCode;
use common::TestApp;

#[tokio::test]
async fn health_is_ok_when_the_probe_answers() {
    let app = TestApp::without_database(Ok(()));
    let reply = app.client().get("/api/health").await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body, r#"{"data":{"database":"up"},"ok":true}"#);
}

#[tokio::test]
async fn health_is_503_and_hides_the_error_detail() {
    let app = TestApp::without_database(Err("secret dsn".into()));
    let reply = app.client().get("/api/health").await;
    assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(reply.body.contains("DATABASE_UNAVAILABLE"));
    assert!(!reply.body.contains("secret dsn"));
}

#[tokio::test]
async fn serves_static_assets_and_404s_unknown_paths() {
    let app = TestApp::without_database(Ok(()));
    let reply = app.client().get("/assets/forms.js").await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(reply.body.contains("data-api"));
    assert_eq!(
        app.client().get("/assets/gomoku.js").await.status,
        StatusCode::OK
    );
    assert_eq!(
        app.client().get("/nope").await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn home_renders_in_polish_by_default_and_follows_accept_language() {
    let app = TestApp::without_database(Ok(()));
    let pl = app.client().get("/").await;
    assert!(pl.body.contains(r#"<html lang="pl">"#));
    assert!(pl.body.contains("Załóż konto"));
    let fr = app
        .client()
        .with_header("accept-language", "fr-CA,fr;q=0.9,en;q=0.5")
        .get("/")
        .await;
    assert!(fr.body.contains(r#"<html lang="fr">"#));
    assert!(fr.body.contains("Créer un compte"));
    let de = app
        .client()
        .with_header("accept-language", "de")
        .get("/")
        .await;
    assert!(de.body.contains(r#"<html lang="de">"#));
    assert!(de.body.contains("Registrieren"));
}

#[tokio::test]
async fn the_lang_parameter_sets_a_cookie_and_is_removed_from_the_address() {
    let app = TestApp::without_database(Ok(()));
    let mut client = app.client();
    let reply = client.get("/login?next=%2Faccount&lang=en").await;
    assert_eq!(reply.status, StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(reply.header("location"), Some("/login?next=%2Faccount"));
    let cookie = reply.set_cookies().join(";");
    assert!(
        cookie.contains("gomoku_lang=en")
            && cookie.contains("HttpOnly")
            && cookie.contains("SameSite=Lax")
    );
    assert!(cookie.contains("Max-Age=31536000"));
    // The cookie now decides, whatever the browser asks for.
    let page = client.with_header("accept-language", "fr").get("/").await;
    assert!(page.body.contains(r#"<html lang="en">"#));
}

#[tokio::test]
async fn an_unsupported_lang_is_only_dropped_and_api_and_assets_are_left_alone() {
    let app = TestApp::without_database(Ok(()));
    let reply = app.client().get("/?lang=xx").await;
    assert_eq!(reply.status, StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(reply.header("location"), Some("/"));
    assert!(reply.set_cookies().is_empty());
    assert_eq!(
        app.client().get("/api/health?lang=fr").await.status,
        StatusCode::OK
    );
    assert_eq!(
        app.client().get("/assets/gomoku.js?lang=fr").await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn serve_runs_until_shutdown() {
    let app = TestApp::without_database(Ok(()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(gomoku_web::server::serve(
        listener,
        app.router.clone(),
        async {
            let _ = stopped.await;
        },
    ));
    stop.send(()).unwrap();
    server.await.unwrap().unwrap();
}

#[test]
fn the_system_clock_tells_the_current_time() {
    let before = gomoku_db::OffsetDateTime::now_utc();
    let now = (gomoku_web::clock::system_clock())();
    assert!(now >= before && now - before < time::Duration::seconds(5));
}

#[tokio::test]
async fn german_theme_switch_and_skip_link_are_on_every_page() {
    let app = TestApp::without_database(Ok(()));
    let page = app
        .client()
        .with_header("accept-language", "de")
        .get("/")
        .await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains(r#"<html lang="de">"#));
    assert!(
        page.body.contains("Zum Inhalt springen"),
        "the skip link is German"
    );
    assert!(
        page.body.contains(r#"data-testid="theme-switch""#) && page.body.contains("Helles Design")
    );
    assert!(page.body.contains(r##"href="#main""##) && page.body.contains(r#"<main id="main""#));
    assert!(
        page.body.contains("gomoku_theme=(dark|light)"),
        "the head script applies the stored theme"
    );
    assert!(page.body.contains("?lang=de") && page.body.contains("Deutsch"));
    let polish = app.client().get("/").await;
    assert!(polish.body.contains("Jasny motyw") && polish.body.contains("Przejdź do treści"));
}
