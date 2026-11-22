//! Registration, login, logout and sessions end to end: the real router on a real PostgreSQL.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{ORIGIN, TestApp};
use gomoku_db::OffsetDateTime;
use serde_json::{Value, json};
use sqlx::Row;

fn register_body(email: &str, name: &str) -> Value {
    json!({ "email": email, "displayName": name, "password": "correct horse" })
}

async fn register(app: &TestApp, email: &str, name: &str) -> common::Reply {
    app.client()
        .post("/api/auth/register", register_body(email, name))
        .await
}

fn error_of(reply: &common::Reply) -> Value {
    let body = reply.json();
    assert_eq!(body["ok"], false, "{body}");
    body["error"].clone()
}

#[tokio::test]
async fn registration_creates_an_account_a_hashed_session_and_a_cookie() {
    let app = TestApp::start().await;
    let mut client = app.client();
    let reply = client
        .post(
            "/api/auth/register",
            register_body("  Ala@Example.com ", "Ala_Ma-Kota"),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED);
    assert_eq!(reply.header("cache-control"), Some("private, no-store"));
    let body = reply.json();
    assert_eq!(body["ok"], true);
    assert_eq!(body["data"]["user"]["displayName"], "Ala_Ma-Kota");

    let cookie = reply.set_cookies().join(";");
    assert!(cookie.starts_with("gomoku_session="), "{cookie}");
    for attribute in ["HttpOnly", "SameSite=Lax", "Path=/"] {
        assert!(
            cookie.contains(attribute),
            "{attribute} missing in {cookie}"
        );
    }
    assert!(!cookie.contains("Secure"));

    // The row has the normalized e-mail, the name key and an argon2id hash; the token itself is not stored.
    let row =
        sqlx::query("SELECT email, display_name_key, password_hash, email_verified_at FROM users")
            .fetch_one(app.db.pool())
            .await
            .unwrap();
    assert_eq!(row.get::<String, _>("email"), "ala@example.com");
    assert_eq!(row.get::<String, _>("display_name_key"), "alamakota");
    assert!(
        row.get::<String, _>("password_hash")
            .starts_with("$argon2id$")
    );
    assert!(
        row.get::<Option<OffsetDateTime>, _>("email_verified_at")
            .is_none()
    );
    let token = client.cookie("gomoku_session").unwrap().to_owned();
    let stored: String = sqlx::query_scalar("SELECT token_hash FROM sessions")
        .fetch_one(app.db.pool())
        .await
        .unwrap();
    assert_eq!(stored.len(), 64);
    assert_ne!(stored, token);
    assert!(!stored.contains(&token));

    let account = client.get("/account").await;
    assert_eq!(account.status, StatusCode::OK);
    assert!(account.body.contains("Ala_Ma-Kota") && account.body.contains("ala@example.com"));
    assert!(account.body.contains("Niepotwierdzony"));
}

#[tokio::test]
async fn invalid_registration_reports_every_field_with_codes() {
    let app = TestApp::start().await;
    let reply = app
        .client()
        .post(
            "/api/auth/register",
            json!({ "email": "nope", "displayName": "ab", "password": "short" }),
        )
        .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    let error = error_of(&reply);
    assert_eq!(error["code"], "VALIDATION");
    assert_eq!(error["fieldIssues"]["email"][0]["code"], "EMAIL_INVALID");
    assert_eq!(
        error["fieldIssues"]["displayName"][0]["code"],
        "DISPLAY_NAME_TOO_SHORT"
    );
    assert_eq!(
        error["fieldIssues"]["password"][0]["code"],
        "PASSWORD_TOO_SHORT"
    );
    assert_eq!(
        error["fieldErrors"]["email"][0],
        "Podaj poprawny adres e-mail."
    );

    let missing = error_of(&app.client().post("/api/auth/register", json!({})).await);
    assert_eq!(missing["fieldIssues"]["email"][0]["code"], "EMAIL_REQUIRED");
    assert_eq!(
        missing["fieldIssues"]["displayName"][0]["code"],
        "DISPLAY_NAME_REQUIRED"
    );
    assert_eq!(
        missing["fieldIssues"]["password"][0]["code"],
        "PASSWORD_REQUIRED"
    );

    let reserved = error_of(
        &app.client()
            .post("/api/auth/register", register_body("a@b.pl", "Admin"))
            .await,
    );
    assert_eq!(
        reserved["fieldIssues"]["displayName"][0]["code"],
        "DISPLAY_NAME_RESERVED"
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(app.db.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn a_taken_email_gets_a_uniform_answer_and_a_taken_name_its_own() {
    let app = TestApp::start().await;
    assert_eq!(
        register(&app, "ala@example.com", "Ala").await.status,
        StatusCode::CREATED
    );

    let email_taken = register(&app, "ALA@example.com", "Inna").await;
    assert_eq!(email_taken.status, StatusCode::CONFLICT);
    let error = error_of(&email_taken);
    assert_eq!(error["code"], "CONFLICT");
    assert_eq!(error["data"]["reason"], "email_taken");
    assert!(error.get("fieldErrors").is_none());

    let name_taken = register(&app, "new@example.com", "a_l-a").await;
    assert_eq!(name_taken.status, StatusCode::CONFLICT);
    let error = error_of(&name_taken);
    assert_eq!(error["data"]["reason"], "display_name_taken");
    assert_eq!(
        error["fieldIssues"]["displayName"][0]["code"],
        "DISPLAY_NAME_TAKEN"
    );

    // Both taken: the uniform e-mail answer wins, so a name conflict never confirms an e-mail.
    let both = error_of(&register(&app, "ala@example.com", "ALA").await);
    assert_eq!(both["data"]["reason"], "email_taken");
}

#[tokio::test]
async fn registering_while_signed_in_is_refused_without_spending_the_budget() {
    let app = TestApp::start().await;
    let mut client = app.client();
    assert_eq!(
        client
            .post("/api/auth/register", register_body("a@example.com", "Ala"))
            .await
            .status,
        StatusCode::CREATED
    );
    let again = client
        .post("/api/auth/register", register_body("b@example.com", "Bob"))
        .await;
    assert_eq!(again.status, StatusCode::CONFLICT);
    assert_eq!(error_of(&again)["code"], "ALREADY_AUTHENTICATED");
    let used: i32 =
        sqlx::query_scalar("SELECT count FROM rate_limit_buckets WHERE key LIKE 'register:%'")
            .fetch_one(app.db.pool())
            .await
            .unwrap();
    assert_eq!(used, 1);
}

#[tokio::test]
async fn registration_is_limited_to_five_attempts_per_hour_per_address() {
    let app = TestApp::start().await;
    for index in 0..5 {
        let reply = register(
            &app,
            &format!("u{index}@example.com"),
            &format!("User{index}"),
        )
        .await;
        assert_eq!(reply.status, StatusCode::CREATED, "attempt {index}");
    }
    let sixth = register(&app, "u6@example.com", "User6").await;
    assert_eq!(sixth.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(error_of(&sixth)["code"], "RATE_LIMITED");
    let retry: u64 = sixth.header("retry-after").unwrap().parse().unwrap();
    assert!((1..=3600).contains(&retry));
}

#[tokio::test]
async fn csrf_checks_origin_and_the_custom_header() {
    let app = TestApp::start().await;
    let body = || Body::from(register_body("a@example.com", "Ala").to_string());
    let json = |builder: axum::http::request::Builder| {
        builder.header(header::CONTENT_TYPE, "application/json")
    };

    let mut client = app.client();
    let no_headers = client
        .send(json(Request::post("/api/auth/register")), body())
        .await;
    assert_eq!(no_headers.status, StatusCode::FORBIDDEN);
    assert_eq!(error_of(&no_headers)["code"], "FORBIDDEN");
    let foreign = client
        .send(
            json(Request::post("/api/auth/register"))
                .header(header::ORIGIN, "https://evil.example")
                .header("x-gomoku-request", "1"),
            body(),
        )
        .await;
    assert_eq!(foreign.status, StatusCode::FORBIDDEN);
    let no_custom = client
        .send(
            json(Request::post("/api/auth/register")).header(header::ORIGIN, ORIGIN),
            body(),
        )
        .await;
    assert_eq!(no_custom.status, StatusCode::FORBIDDEN);
    let bad_port = client
        .send(
            json(Request::post("/api/auth/register"))
                .header(header::ORIGIN, "http://localhost:")
                .header("x-gomoku-request", "1"),
            body(),
        )
        .await;
    assert_eq!(bad_port.status, StatusCode::FORBIDDEN);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(app.db.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
    // Reads are not state-changing.
    assert_eq!(client.get("/api/health").await.status, StatusCode::OK);
}

#[tokio::test]
async fn csrf_with_a_configured_origin_requires_an_exact_match() {
    let app = TestApp::start_with(&[
        ("APP_ORIGIN", "https://gomoku.example"),
        ("SESSION_COOKIE_SECURE", "true"),
    ])
    .await;
    let request = |origin: &'static str| {
        Request::post("/api/auth/logout")
            .header(header::ORIGIN, origin)
            .header("x-gomoku-request", "1")
    };
    let mut client = app.client();
    assert_eq!(
        client
            .send(request("https://gomoku.example"), Body::empty())
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        client.send(request(ORIGIN), Body::empty()).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        client
            .send(request("https://gomoku.example.evil"), Body::empty())
            .await
            .status,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn bodies_that_are_too_big_or_not_json_are_client_errors() {
    let app = TestApp::start().await;
    let mut client = app.client();
    let big = client
        .send(
            Request::post("/api/auth/register")
                .header(header::ORIGIN, ORIGIN)
                .header("x-gomoku-request", "1"),
            Body::from("x".repeat(70 * 1024)),
        )
        .await;
    assert_eq!(big.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(error_of(&big)["code"], "PAYLOAD_TOO_LARGE");
    let declared = client
        .send(
            Request::post("/api/auth/register")
                .header(header::ORIGIN, ORIGIN)
                .header("x-gomoku-request", "1")
                .header(header::CONTENT_LENGTH, "999999"),
            Body::from("{}"),
        )
        .await;
    assert_eq!(declared.status, StatusCode::PAYLOAD_TOO_LARGE);
    let junk = client
        .send(
            Request::post("/api/auth/register")
                .header(header::ORIGIN, ORIGIN)
                .header("x-gomoku-request", "1"),
            Body::from("not json"),
        )
        .await;
    assert_eq!(junk.status, StatusCode::BAD_REQUEST);
    assert_eq!(error_of(&junk)["code"], "VALIDATION");
}

#[tokio::test]
async fn login_logout_and_the_session_lifecycle() {
    let app = TestApp::start().await;
    assert_eq!(
        register(&app, "ala@example.com", "Ala").await.status,
        StatusCode::CREATED
    );

    let mut client = app.client();
    let login = client
        .post(
            "/api/auth/login",
            json!({ "email": " ALA@example.com", "password": "correct horse" }),
        )
        .await;
    assert_eq!(login.status, StatusCode::OK);
    assert_eq!(login.json()["data"]["user"]["displayName"], "Ala");
    assert!(client.cookie("gomoku_session").is_some());
    assert_eq!(client.get("/account").await.status, StatusCode::OK);

    // A signed-in client gets a dedicated code.
    let again = client
        .post(
            "/api/auth/login",
            json!({ "email": "ala@example.com", "password": "correct horse" }),
        )
        .await;
    assert_eq!(error_of(&again)["code"], "ALREADY_AUTHENTICATED");

    let logout = client.post("/api/auth/logout", Value::Null).await;
    assert_eq!(logout.status, StatusCode::OK);
    assert_eq!(logout.json(), json!({ "ok": true, "data": null }));
    assert!(logout.set_cookies().join(";").contains("Max-Age=0"));
    let sessions: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions")
        .fetch_one(app.db.pool())
        .await
        .unwrap();
    assert_eq!(
        sessions, 1,
        "only the logged-out session is gone; the one from registration stays"
    );
    assert_eq!(client.get("/account").await.status, StatusCode::SEE_OTHER);

    // Idempotent, also without a cookie.
    assert_eq!(
        client.post("/api/auth/logout", Value::Null).await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn login_failures_are_uniform_for_unknown_accounts_and_wrong_passwords() {
    let app = TestApp::start().await;
    register(&app, "ala@example.com", "Ala").await;
    let wrong = app
        .client()
        .post(
            "/api/auth/login",
            json!({ "email": "ala@example.com", "password": "wrong password" }),
        )
        .await;
    let unknown = app
        .client()
        .post(
            "/api/auth/login",
            json!({ "email": "nobody@example.com", "password": "wrong password" }),
        )
        .await;
    for reply in [&wrong, &unknown] {
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
        assert_eq!(error_of(reply)["code"], "INVALID_CREDENTIALS");
    }
    assert_eq!(error_of(&wrong)["message"], error_of(&unknown)["message"]);
    assert!(wrong.set_cookies().is_empty());
}

#[tokio::test]
async fn login_validates_shape_only() {
    let app = TestApp::start().await;
    let reply = app
        .client()
        .post("/api/auth/login", json!({ "email": "bad", "password": "" }))
        .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    let error = error_of(&reply);
    assert_eq!(error["fieldIssues"]["email"][0]["code"], "EMAIL_INVALID");
    assert_eq!(
        error["fieldIssues"]["password"][0]["code"],
        "PASSWORD_REQUIRED"
    );
    // A short password is a wrong password at login, not a validation error (login reveals no password rules).
    let short = app
        .client()
        .post(
            "/api/auth/login",
            json!({ "email": "a@example.com", "password": "x" }),
        )
        .await;
    assert_eq!(short.status, StatusCode::UNAUTHORIZED);
    let long = app
        .client()
        .post(
            "/api/auth/login",
            json!({ "email": "a@example.com", "password": "x".repeat(1025) }),
        )
        .await;
    assert_eq!(
        error_of(&long)["fieldIssues"]["password"][0]["code"],
        "PASSWORD_TOO_LONG"
    );
    let missing = app.client().post("/api/auth/login", json!({})).await;
    assert_eq!(
        error_of(&missing)["fieldIssues"]["email"][0]["code"],
        "EMAIL_REQUIRED"
    );
}

#[tokio::test]
async fn failed_logins_are_limited_per_email_and_address_and_success_gives_the_budget_back() {
    let app = TestApp::start().await;
    register(&app, "ala@example.com", "Ala").await;
    let attempt = |password: &'static str| {
        let mut client = app.client();
        async move {
            client
                .post(
                    "/api/auth/login",
                    json!({ "email": "ala@example.com", "password": password }),
                )
                .await
        }
    };
    for index in 0..9 {
        assert_eq!(
            attempt("wrong password").await.status,
            StatusCode::UNAUTHORIZED,
            "failure {index}"
        );
    }
    // A success resets the e-mail counters.
    assert_eq!(attempt("correct horse").await.status, StatusCode::OK);
    for index in 0..10 {
        assert_eq!(
            attempt("wrong password").await.status,
            StatusCode::UNAUTHORIZED,
            "failure {index} after reset"
        );
    }
    let blocked = attempt("correct horse").await;
    assert_eq!(blocked.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(blocked.header("retry-after").is_some());
}

#[tokio::test]
async fn the_address_limit_is_checked_before_anything_else() {
    let app = TestApp::start().await;
    let mut client = app.client();
    for _ in 0..30 {
        assert_eq!(
            client.post("/api/auth/login", json!({})).await.status,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        client.post("/api/auth/login", json!({})).await.status,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn expired_and_forged_sessions_do_not_authenticate() {
    let app = TestApp::start().await;
    let mut client = app.client();
    client
        .post(
            "/api/auth/register",
            register_body("ala@example.com", "Ala"),
        )
        .await;
    assert_eq!(client.get("/account").await.status, StatusCode::OK);
    app.advance_seconds(30 * 24 * 3600 + 1);
    let expired = client.get("/account").await;
    assert_eq!(expired.status, StatusCode::SEE_OTHER);
    assert_eq!(expired.header("location"), Some("/login?next=%2Faccount"));

    let mut forger = app.client();
    forger.set_cookie("gomoku_session", "forged-token");
    assert_eq!(forger.get("/account").await.status, StatusCode::SEE_OTHER);
    let mut empty = app.client();
    empty.set_cookie("gomoku_session", "");
    assert_eq!(empty.get("/account").await.status, StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn pages_for_guests_and_signed_in_players() {
    let app = TestApp::start().await;
    let mut guest = app.client();
    for (path, marker) in [("/login", "login-form"), ("/register", "register-form")] {
        let page = guest.get(path).await;
        assert_eq!(page.status, StatusCode::OK);
        assert!(page.body.contains(marker));
    }
    // `next` is validated: an absolute URL falls back to `/`.
    let evil = guest.get("/login?next=https%3A%2F%2Fevil.example").await;
    assert!(evil.body.contains(r#"data-redirect="/""#));
    let good = guest.get("/login?next=%2Flobby").await;
    assert!(good.body.contains(r#"data-redirect="/lobby""#));

    let mut player = app.client();
    player
        .post(
            "/api/auth/register",
            register_body("ala@example.com", "Ala"),
        )
        .await;
    let login = player.get("/login?next=%2Flobby").await;
    assert_eq!(login.status, StatusCode::SEE_OTHER);
    assert_eq!(login.header("location"), Some("/lobby"));
    assert_eq!(player.get("/register").await.header("location"), Some("/"));
    let home = player.get("/").await;
    assert!(home.body.contains("Zalogowano jako Ala."));
    assert!(home.body.contains(r#"data-testid="logout""#));
    assert!(
        guest
            .get("/")
            .await
            .body
            .contains(r#"data-testid="nav-login""#)
    );
}

#[tokio::test]
async fn a_secure_deployment_uses_the_host_prefixed_cookie() {
    let app = TestApp::start_with(&[
        ("APP_ENV", "production"),
        ("APP_ORIGIN", "https://gomoku.example"),
        ("TRUST_PROXY_HOPS", "1"),
        ("ADMIN_EMAIL", "admin@example.com"),
    ])
    .await;
    let mut client = app.client();
    let request = Request::post("/api/auth/register")
        .header(header::ORIGIN, "https://gomoku.example")
        .header("x-gomoku-request", "1")
        .header("x-forwarded-for", "203.0.113.7")
        .header(header::CONTENT_TYPE, "application/json");
    let reply = client
        .send(
            request,
            Body::from(register_body("ala@example.com", "Ala").to_string()),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED);
    let cookie = reply.set_cookies().join(";");
    assert!(cookie.starts_with("__Host-gomoku_session="), "{cookie}");
    assert!(cookie.contains("Secure") && cookie.contains("Path=/"));
    assert!(!cookie.contains("Domain"));
    // Without a trusted client address production fails closed instead of sharing one counter.
    let mut anonymous = app.client();
    let unknown = anonymous
        .send(
            Request::post("/api/auth/login")
                .header(header::ORIGIN, "https://gomoku.example")
                .header("x-gomoku-request", "1"),
            Body::from("{}"),
        )
        .await;
    assert_eq!(unknown.status, StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn an_unavailable_database_is_a_generic_500_and_pages_fail_closed() {
    let app = TestApp::without_database(Ok(()));
    let reply = app
        .client()
        .post(
            "/api/auth/register",
            register_body("ala@example.com", "Ala"),
        )
        .await;
    assert_eq!(reply.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(error_of(&reply)["code"], "INTERNAL");
    assert!(!reply.body.contains("postgres"));
    let mut client = app.client();
    client.set_cookie("gomoku_session", "token");
    assert_eq!(client.get("/").await.status, StatusCode::OK);
    assert_eq!(client.get("/account").await.status, StatusCode::SEE_OTHER);
    // Logout still removes the cookie when the revocation fails.
    let logout = client.post("/api/auth/logout", Value::Null).await;
    assert_eq!(logout.status, StatusCode::OK);
    assert!(logout.set_cookies().join(";").contains("Max-Age=0"));
}
