//! Password change and reset end to end: the real router on a real PostgreSQL, a collecting mailbox.
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

async fn sign_up(app: &TestApp, email: &str, name: &str) -> Client {
    let mut client = app.client();
    let reply = client
        .post(
            "/api/auth/register",
            json!({ "email": email, "displayName": name, "password": PASSWORD }),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED);
    client
}

async fn confirm_email(app: &TestApp, email: &str) {
    sqlx::query("UPDATE users SET email_verified_at = now() WHERE email = $1")
        .bind(email)
        .execute(app.db.pool())
        .await
        .unwrap();
}

async fn change(client: &mut Client, current: &str, new: &str) -> Reply {
    client
        .post(
            "/api/auth/password",
            json!({ "currentPassword": current, "newPassword": new }),
        )
        .await
}

async fn request_reset(app: &TestApp, email: &str) -> Reply {
    let reply = app
        .client()
        .post(
            "/api/auth/password-reset/request",
            json!({ "email": email }),
        )
        .await;
    app.settle().await;
    reply
}

/// The token in the link of the newest message.
fn latest_token(app: &TestApp) -> String {
    let messages = app.messages();
    let text = &messages.last().expect("a message was sent").text;
    let start = text.find("/reset-password?token=").unwrap() + "/reset-password?token=".len();
    text[start..]
        .split(|c: char| c.is_whitespace())
        .next()
        .unwrap()
        .to_owned()
}

async fn confirm(app: &TestApp, token: &str, password: &str) -> Reply {
    app.client()
        .post(
            "/api/auth/password-reset/confirm",
            json!({ "token": token, "newPassword": password }),
        )
        .await
}

async fn login(app: &TestApp, email: &str, password: &str) -> Reply {
    app.client()
        .post(
            "/api/auth/login",
            json!({ "email": email, "password": password }),
        )
        .await
}

async fn count(app: &TestApp, sql: &str) -> i64 {
    sqlx::query(sqlx::AssertSqlSafe(sql.to_owned()))
        .fetch_one(app.db.pool())
        .await
        .unwrap()
        .get::<i64, _>(0)
}

#[tokio::test]
async fn changing_the_password_ends_other_sessions_and_keeps_the_current_one() {
    let app = TestApp::start().await;
    let mut here = sign_up(&app, "ala@example.com", "Ala").await;
    let mut elsewhere = app.client();
    assert_eq!(
        login(&app, "ala@example.com", PASSWORD).await.status,
        StatusCode::OK
    );
    elsewhere
        .post(
            "/api/auth/login",
            json!({ "email": "ala@example.com", "password": PASSWORD }),
        )
        .await;
    assert_eq!(elsewhere.get("/account").await.status, StatusCode::OK);

    let reply = change(&mut here, PASSWORD, "brand new secret").await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.json(), json!({ "ok": true, "data": null }));

    assert_eq!(
        here.get("/account").await.status,
        StatusCode::OK,
        "the current session stays"
    );
    assert_eq!(
        elsewhere.get("/account").await.status,
        StatusCode::SEE_OTHER,
        "the other one ended"
    );
    assert_eq!(
        login(&app, "ala@example.com", PASSWORD).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        login(&app, "ala@example.com", "brand new secret")
            .await
            .status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn a_password_change_needs_a_session_and_valid_input() {
    let app = TestApp::start().await;
    let anonymous = change(&mut app.client(), PASSWORD, "brand new secret").await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
    assert_eq!(error_of(&anonymous)["code"], "UNAUTHORIZED");

    let mut client = sign_up(&app, "ala@example.com", "Ala").await;
    let empty = client.post("/api/auth/password", json!({})).await;
    let error = error_of(&empty);
    assert_eq!(empty.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        error["fieldIssues"]["currentPassword"][0]["code"],
        "CURRENT_PASSWORD_REQUIRED"
    );
    assert_eq!(
        error["fieldIssues"]["newPassword"][0]["code"],
        "PASSWORD_REQUIRED"
    );

    let short = error_of(&change(&mut client, PASSWORD, "short").await);
    assert_eq!(
        short["fieldIssues"]["newPassword"][0]["code"],
        "PASSWORD_TOO_SHORT"
    );
    let same = error_of(&change(&mut client, PASSWORD, PASSWORD).await);
    assert_eq!(
        same["fieldIssues"]["newPassword"][0]["code"],
        "PASSWORD_UNCHANGED"
    );
    let blank = error_of(&change(&mut client, "", "brand new secret").await);
    assert_eq!(
        blank["fieldIssues"]["currentPassword"][0]["code"],
        "CURRENT_PASSWORD_REQUIRED"
    );
    // Invalid input spends no attempt.
    assert_eq!(
        count(
            &app,
            "SELECT count(*) FROM rate_limit_buckets WHERE key LIKE 'password-change:%'"
        )
        .await,
        0
    );
}

#[tokio::test]
async fn a_wrong_current_password_is_limited_per_account_and_success_gives_the_budget_back() {
    let app = TestApp::start().await;
    let mut client = sign_up(&app, "ala@example.com", "Ala").await;
    let wrong = change(&mut client, "not my password", "brand new secret").await;
    assert_eq!(wrong.status, StatusCode::BAD_REQUEST);
    let error = error_of(&wrong);
    assert_eq!(error["code"], "INVALID_CURRENT_PASSWORD");
    assert_eq!(
        error["fieldIssues"]["currentPassword"][0]["code"],
        "INVALID_CURRENT_PASSWORD"
    );
    for _ in 0..8 {
        assert_eq!(
            change(&mut client, "not my password", "brand new secret")
                .await
                .status,
            StatusCode::BAD_REQUEST
        );
    }
    // Ten failures are allowed; a success resets the counter.
    assert_eq!(
        change(&mut client, PASSWORD, "brand new secret")
            .await
            .status,
        StatusCode::OK
    );
    for _ in 0..10 {
        assert_eq!(
            change(&mut client, "not my password", "another secret")
                .await
                .status,
            StatusCode::BAD_REQUEST
        );
    }
    let blocked = change(&mut client, "brand new secret", "another secret").await;
    assert_eq!(blocked.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(blocked.header("retry-after").is_some());
}

#[tokio::test]
async fn a_password_change_invalidates_pending_reset_links() {
    let app = TestApp::start().await;
    let mut client = sign_up(&app, "ala@example.com", "Ala").await;
    confirm_email(&app, "ala@example.com").await;
    request_reset(&app, "ala@example.com").await;
    let token = latest_token(&app);
    assert_eq!(
        change(&mut client, PASSWORD, "brand new secret")
            .await
            .status,
        StatusCode::OK
    );
    let reply = confirm(&app, &token, "yet another one").await;
    assert_eq!(error_of(&reply)["code"], "INVALID_TOKEN");
}

#[tokio::test]
async fn a_reset_request_always_answers_the_same_and_mails_only_confirmed_owners() {
    let app = TestApp::start().await;
    sign_up(&app, "confirmed@example.com", "Confirmed").await;
    sign_up(&app, "pending@example.com", "Pending").await;
    confirm_email(&app, "confirmed@example.com").await;

    let mut answers = Vec::new();
    for email in [
        "nobody@example.com",
        "pending@example.com",
        "confirmed@example.com",
    ] {
        let reply = request_reset(&app, email).await;
        assert_eq!(reply.status, StatusCode::OK);
        answers.push(reply.body);
    }
    assert!(
        answers.iter().all(|body| body == &answers[0]),
        "{answers:?}"
    );
    assert_eq!(answers[0], r#"{"data":null,"ok":true}"#);

    let messages = app.messages();
    assert_eq!(messages.len(), 1, "only the confirmed owner got a message");
    let message = &messages[0];
    assert_eq!(message.to, "confirmed@example.com");
    assert!(
        message
            .text
            .contains("http://localhost:3000/reset-password?token=")
    );
    assert!(message.html.contains("Cześć Confirmed,"));
    // Only the hash of the token is stored.
    let token = latest_token(&app);
    let stored: String = sqlx::query_scalar("SELECT token_hash FROM password_reset_tokens")
        .fetch_one(app.db.pool())
        .await
        .unwrap();
    assert_eq!(stored.len(), 64);
    assert!(!stored.contains(&token));
}

#[tokio::test]
async fn a_malformed_reset_request_is_a_client_error_and_spends_the_address_budget() {
    let app = TestApp::start().await;
    let mut client = app.client();
    let bad = client
        .post(
            "/api/auth/password-reset/request",
            json!({ "email": "nope" }),
        )
        .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        error_of(&bad)["fieldIssues"]["email"][0]["code"],
        "EMAIL_INVALID"
    );
    let missing = client
        .post("/api/auth/password-reset/request", json!({}))
        .await;
    assert_eq!(
        error_of(&missing)["fieldIssues"]["email"][0]["code"],
        "EMAIL_REQUIRED"
    );
    for _ in 0..8 {
        client
            .post("/api/auth/password-reset/request", json!({}))
            .await;
    }
    let tenth = client
        .post(
            "/api/auth/password-reset/request",
            json!({ "email": "a@example.com" }),
        )
        .await;
    assert_eq!(tenth.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(tenth.header("retry-after").is_some());
}

#[tokio::test]
async fn at_most_three_messages_per_hour_per_address_and_the_daily_pool_is_shared() {
    let app = TestApp::start_with(&[("EMAIL_DAILY_LIMIT", "5")]).await;
    sign_up(&app, "ala@example.com", "Ala").await;
    confirm_email(&app, "ala@example.com").await;
    for _ in 0..5 {
        assert_eq!(
            request_reset(&app, "ala@example.com").await.status,
            StatusCode::OK
        );
    }
    assert_eq!(
        app.messages().len(),
        3,
        "the fourth and fifth request send nothing"
    );
    // The throttled address did not use up the daily pool (3 sends = 3 units, not 5).
    let used: i32 =
        sqlx::query_scalar("SELECT count FROM rate_limit_buckets WHERE key = 'email-daily:reset'")
            .fetch_one(app.db.pool())
            .await
            .unwrap();
    assert_eq!(used, 3);
}

#[tokio::test]
async fn the_daily_pool_stops_sending_for_everyone() {
    let app = TestApp::start_with(&[("EMAIL_DAILY_LIMIT", "2")]).await;
    for index in 0..3 {
        let email = format!("user{index}@example.com");
        sign_up(&app, &email, &format!("User{index}")).await;
        confirm_email(&app, &email).await;
        assert_eq!(request_reset(&app, &email).await.status, StatusCode::OK);
    }
    assert_eq!(app.messages().len(), 2);
}

#[tokio::test]
async fn a_failed_send_removes_the_new_link_and_keeps_the_previous_one_valid() {
    let app = TestApp::start().await;
    sign_up(&app, "ala@example.com", "Ala").await;
    confirm_email(&app, "ala@example.com").await;
    request_reset(&app, "ala@example.com").await;
    let first = latest_token(&app);

    *app.mail.fail_with.lock().unwrap() = Some("BREVO_HTTP_500".into());
    assert_eq!(
        request_reset(&app, "ala@example.com").await.status,
        StatusCode::OK
    );
    assert_eq!(
        count(&app, "SELECT count(*) FROM password_reset_tokens").await,
        1,
        "the undelivered link was removed"
    );
    assert_eq!(
        count(
            &app,
            "SELECT count(*) FROM password_reset_tokens WHERE used_at IS NULL"
        )
        .await,
        1
    );

    assert_eq!(
        confirm(&app, &first, "brand new secret").await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn a_newer_link_kills_the_older_one() {
    let app = TestApp::start().await;
    sign_up(&app, "ala@example.com", "Ala").await;
    confirm_email(&app, "ala@example.com").await;
    request_reset(&app, "ala@example.com").await;
    let old = latest_token(&app);
    app.advance_seconds(5);
    request_reset(&app, "ala@example.com").await;
    let new = latest_token(&app);
    assert_ne!(old, new);
    assert_eq!(
        error_of(&confirm(&app, &old, "brand new secret").await)["code"],
        "INVALID_TOKEN"
    );
    assert_eq!(
        confirm(&app, &new, "brand new secret").await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn confirming_sets_the_password_ends_every_session_and_works_once() {
    let app = TestApp::start().await;
    let mut session = sign_up(&app, "ala@example.com", "Ala").await;
    confirm_email(&app, "ala@example.com").await;
    request_reset(&app, "ala@example.com").await;
    let token = latest_token(&app);

    let reply = confirm(&app, &token, "brand new secret").await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(
        reply.set_cookies().is_empty(),
        "nobody is logged in afterwards"
    );
    assert_eq!(
        session.get("/account").await.status,
        StatusCode::SEE_OTHER,
        "every session ended"
    );
    assert_eq!(count(&app, "SELECT count(*) FROM sessions").await, 0);
    assert_eq!(
        login(&app, "ala@example.com", PASSWORD).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        login(&app, "ala@example.com", "brand new secret")
            .await
            .status,
        StatusCode::OK
    );
    // The address stays confirmed and the link works once.
    assert_eq!(
        count(
            &app,
            "SELECT count(*) FROM users WHERE email_verified_at IS NOT NULL"
        )
        .await,
        1
    );
    let again = confirm(&app, &token, "once more secret").await;
    assert_eq!(again.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        error_of(&again)["message"],
        "Link wygasł lub został już użyty."
    );
}

#[tokio::test]
async fn expired_and_unknown_links_and_bad_input_are_rejected() {
    let app = TestApp::start().await;
    sign_up(&app, "ala@example.com", "Ala").await;
    confirm_email(&app, "ala@example.com").await;
    request_reset(&app, "ala@example.com").await;
    let token = latest_token(&app);

    assert_eq!(
        error_of(&confirm(&app, "unknown-token", "brand new secret").await)["code"],
        "INVALID_TOKEN"
    );
    // Invalid input does not consume the link.
    let weak = confirm(&app, &token, "short").await;
    assert_eq!(weak.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        error_of(&weak)["fieldIssues"]["newPassword"][0]["code"],
        "PASSWORD_TOO_SHORT"
    );
    let empty = app
        .client()
        .post("/api/auth/password-reset/confirm", json!({}))
        .await;
    let error = error_of(&empty);
    assert_eq!(error["fieldIssues"]["token"][0]["code"], "TOKEN_REQUIRED");
    assert_eq!(
        error["fieldIssues"]["newPassword"][0]["code"],
        "PASSWORD_REQUIRED"
    );
    let huge = confirm(&app, &"t".repeat(513), "brand new secret").await;
    assert_eq!(
        error_of(&huge)["fieldIssues"]["token"][0]["code"],
        "TOKEN_TOO_LONG"
    );

    app.advance_seconds(3600);
    assert_eq!(
        error_of(&confirm(&app, &token, "brand new secret").await)["code"],
        "INVALID_TOKEN"
    );
}

#[tokio::test]
async fn an_unconfirmed_address_cannot_reset_even_with_a_link() {
    let app = TestApp::start().await;
    sign_up(&app, "ala@example.com", "Ala").await;
    confirm_email(&app, "ala@example.com").await;
    request_reset(&app, "ala@example.com").await;
    let token = latest_token(&app);
    sqlx::query("UPDATE users SET email_verified_at = NULL")
        .execute(app.db.pool())
        .await
        .unwrap();
    assert_eq!(
        error_of(&confirm(&app, &token, "brand new secret").await)["code"],
        "INVALID_TOKEN"
    );
}

#[tokio::test]
async fn confirmations_are_limited_per_address_whatever_the_outcome() {
    let app = TestApp::start().await;
    let mut client = app.client();
    for _ in 0..10 {
        let reply = client
            .post(
                "/api/auth/password-reset/confirm",
                json!({ "token": "x", "newPassword": "brand new secret" }),
            )
            .await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    }
    let reply = client
        .post(
            "/api/auth/password-reset/confirm",
            json!({ "token": "x", "newPassword": "brand new secret" }),
        )
        .await;
    assert_eq!(reply.status, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn the_reset_pages_render_and_do_not_leak_the_token() {
    let app = TestApp::start().await;
    let forgot = app.client().get("/forgot-password").await;
    assert_eq!(forgot.status, StatusCode::OK);
    assert!(forgot.body.contains("forgot-form") && forgot.body.contains("Reset hasła"));
    let page = app.client().get("/reset-password?token=abc123").await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains(r#"name="token" value="abc123""#));
    assert_eq!(page.header("referrer-policy"), Some("no-referrer"));
    let none = app.client().get("/reset-password").await;
    assert!(none.body.contains(r#"name="token" value="""#));
    let login = app.client().get("/login").await;
    assert!(login.body.contains(r#"href="/forgot-password""#));
    let mut player = sign_up(&app, "ala@example.com", "Ala").await;
    assert!(player.get("/account").await.body.contains("password-form"));
}
