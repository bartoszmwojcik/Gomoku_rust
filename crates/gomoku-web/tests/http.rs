//! In-process HTTP tests: the real router, fake collaborators (no network, no database).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use gomoku_web::app::{AppState, build_router};
use gomoku_web::health::HealthProbe;
use http_body_util::BodyExt;
use tower::ServiceExt;

struct FakeProbe(Result<(), String>);

#[async_trait]
impl HealthProbe for FakeProbe {
    async fn check(&self) -> Result<(), String> {
        self.0.clone()
    }
}

fn router(probe: Result<(), String>, now: u64) -> Router {
    let state = AppState {
        health: Arc::new(FakeProbe(probe)),
        now: Arc::new(move || now),
    };
    build_router(state, std::path::Path::new("../../assets"))
}

async fn get(router: Router, uri: &str) -> (StatusCode, String) {
    let response = router
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

#[tokio::test]
async fn health_is_ok_when_database_answers() {
    let (status, body) = get(router(Ok(()), 0), "/api/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, r#"{"data":{"database":"up"},"ok":true}"#);
}

#[tokio::test]
async fn health_is_503_and_hides_the_error_detail() {
    let (status, body) = get(router(Err("secret dsn".into()), 0), "/api/health").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body.contains("DATABASE_UNAVAILABLE"));
    assert!(!body.contains("secret dsn"));
}

#[tokio::test]
async fn home_and_login_pages_render() {
    let (status, body) = get(router(Ok(()), 0), "/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("<h1>Gomoku</h1>"));

    let (status, body) = get(router(Ok(()), 0), "/login").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains(r#"data-testid="login-form""#));
}

#[tokio::test]
async fn board_page_has_225_cells() {
    let (status, body) = get(router(Ok(()), 0), "/spike/board").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.matches(r#"class="cell""#).count(), 15 * 15);
    assert!(body.contains(r#"hx-get="/spike/clock""#));
}

#[tokio::test]
async fn clock_counts_down_from_the_server_time() {
    let (_, body) = get(router(Ok(()), 0), "/spike/clock").await;
    assert_eq!(body.trim(), "10:00");
    let (_, body) = get(router(Ok(()), 541), "/spike/clock").await;
    assert_eq!(body.trim(), "0:59");
    let (_, body) = get(router(Ok(()), 590), "/spike/clock").await;
    assert_eq!(body.trim(), "0:10");
    let (_, body) = get(router(Ok(()), 595), "/spike/clock").await;
    assert_eq!(body.trim(), "0:05");
}

#[tokio::test]
async fn serves_static_assets_and_404s_unknown_paths() {
    let (status, body) = get(router(Ok(()), 0), "/assets/board.js").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("black"));

    let (status, _) = get(router(Ok(()), 0), "/nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn serve_runs_until_shutdown() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(gomoku_web::server::serve(
        listener,
        router(Ok(()), 0),
        async {
            let _ = stopped.await;
        },
    ));
    stop.send(()).unwrap();
    server.await.unwrap().unwrap();
}
