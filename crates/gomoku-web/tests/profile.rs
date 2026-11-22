//! E-mail confirmation, e-mail change and display name end to end (real router, real PostgreSQL, collecting mailbox).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::http::StatusCode;
use common::{Client, Reply, TestApp};
use gomoku_web::email::EmailMessage;
use serde_json::{Value, json};
use sqlx::Row;

const PASSWORD: &str = "correct horse";
const VERIFY_SUBJECT: &str = "Potwierdź adres e-mail w Gomoku";
const CHANGE_SUBJECT: &str = "Potwierdź nowy adres e-mail w Gomoku";
const NOTICE_SUBJECT: &str = "Adres e-mail Twojego konta w Gomoku został zmieniony";

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
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    client
}

/// Registers from a given client address (needs `TRUST_PROXY_HOPS=1`), so tests are not held back by the
/// registration limit of one address.
async fn sign_up_from(app: &TestApp, ip: &str, email: &str, name: &str) -> Client {
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

/// An account with a session created straight in the database (no registration limit, no confirmation mail).
async fn quick_player(app: &TestApp, ip: &str, email: &str, name: &str) -> Client {
    let token = format!("token-{name}");
    let key = gomoku_web::security::hash_opaque_token(&token);
    let expires_at = gomoku_db::OffsetDateTime::from_unix_timestamp(2_000_000_000).unwrap();
    app.db
        .register_user(
            &gomoku_db::NewUser {
                email,
                display_name: name,
                display_name_key: &name.to_lowercase(),
                password_hash: "$argon2id$unused",
            },
            &gomoku_db::NewSession {
                token_hash: &key,
                expires_at,
            },
        )
        .await
        .unwrap();
    let mut client = app.client().with_header("x-forwarded-for", ip);
    client.set_cookie("gomoku_session", &token);
    client
}

async fn confirm_email(app: &TestApp, email: &str) {
    sqlx::query("UPDATE users SET email_verified_at = now() WHERE email = $1")
        .bind(email)
        .execute(app.db.pool())
        .await
        .unwrap();
}

fn mails(app: &TestApp, subject: &str) -> Vec<EmailMessage> {
    app.messages()
        .into_iter()
        .filter(|m| m.subject == subject)
        .collect()
}

/// The token in the first link of the newest message with that subject.
fn token_in(app: &TestApp, subject: &str) -> String {
    let all = mails(app, subject);
    let text = &all.last().expect("a message was sent").text;
    let start = text.find("?token=").unwrap() + "?token=".len();
    text[start..]
        .split(|c: char| c.is_whitespace() || c == '&')
        .next()
        .unwrap()
        .to_owned()
}

async fn count(app: &TestApp, sql: &str) -> i64 {
    sqlx::query(sqlx::AssertSqlSafe(sql.to_owned()))
        .fetch_one(app.db.pool())
        .await
        .unwrap()
        .get::<i64, _>(0)
}

async fn verify(app: &TestApp, token: &str, next: Option<&str>) -> Reply {
    let mut body = json!({ "token": token });
    if let Some(next) = next {
        body["next"] = json!(next);
    }
    app.client()
        .post("/api/auth/email-verification/confirm", body)
        .await
}

// ---- e-mail confirmation -------------------------------------------------------------------------------------

#[tokio::test]
async fn registration_sends_a_confirmation_link_and_the_token_is_stored_hashed() {
    let app = TestApp::start().await;
    sign_up(&app, "ala@example.com", "Ala").await;
    let sent = mails(&app, VERIFY_SUBJECT);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].to, "ala@example.com");
    assert!(
        sent[0]
            .text
            .contains("http://localhost:3000/verify-email?token=")
    );
    assert!(sent[0].html.contains("Cześć Ala,"));
    let token = token_in(&app, VERIFY_SUBJECT);
    let stored: String = sqlx::query_scalar("SELECT token_hash FROM email_verification_tokens")
        .fetch_one(app.db.pool())
        .await
        .unwrap();
    assert_eq!(stored.len(), 64);
    assert!(!stored.contains(&token));
}

#[tokio::test]
async fn a_failed_confirmation_send_never_fails_the_registration() {
    let app = TestApp::start().await;
    *app.mail.fail_with.lock().unwrap() = Some("BREVO_HTTP_500".into());
    let client = sign_up(&app, "ala@example.com", "Ala").await;
    assert!(client.cookie("gomoku_session").is_some());
    assert_eq!(
        count(&app, "SELECT count(*) FROM email_verification_tokens").await,
        0,
        "the unsent token was removed"
    );
}

#[tokio::test]
async fn confirming_marks_the_address_without_logging_anyone_in_and_works_once() {
    let app = TestApp::start().await;
    let mut player = sign_up(&app, "ala@example.com", "Ala").await;
    assert!(
        player
            .get("/account")
            .await
            .body
            .contains("Niepotwierdzony")
    );
    let token = token_in(&app, VERIFY_SUBJECT);

    let reply = verify(&app, &token, Some("/lobby")).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(
        reply.json(),
        json!({ "ok": true, "data": { "next": "/lobby" } })
    );
    assert!(reply.set_cookies().is_empty());
    assert!(player.get("/account").await.body.contains("Potwierdzony"));

    let again = verify(&app, &token, None).await;
    assert_eq!(again.status, StatusCode::BAD_REQUEST);
    assert_eq!(error_of(&again)["code"], "INVALID_TOKEN");
}

#[tokio::test]
async fn confirmation_rejects_unknown_expired_and_malformed_input_and_sanitizes_next() {
    let app = TestApp::start().await;
    sign_up(&app, "ala@example.com", "Ala").await;
    let token = token_in(&app, VERIFY_SUBJECT);
    assert_eq!(
        error_of(&verify(&app, "unknown", None).await)["code"],
        "INVALID_TOKEN"
    );
    let empty = app
        .client()
        .post("/api/auth/email-verification/confirm", json!({}))
        .await;
    assert_eq!(
        error_of(&empty)["fieldIssues"]["token"][0]["code"],
        "TOKEN_REQUIRED"
    );
    assert_eq!(
        error_of(&verify(&app, &"t".repeat(513), None).await)["fieldIssues"]["token"][0]["code"],
        "TOKEN_TOO_LONG"
    );

    // An open redirect target is replaced by `/` (the link works).
    let evil = verify(&app, &token, Some("https://evil.example")).await;
    assert_eq!(evil.json()["data"]["next"], "/");

    sign_up(&app, "bob@example.com", "Bob").await;
    let bob_token = token_in(&app, VERIFY_SUBJECT);
    app.advance_seconds(24 * 3600);
    assert_eq!(
        error_of(&verify(&app, &bob_token, None).await)["code"],
        "INVALID_TOKEN"
    );
}

#[tokio::test]
async fn resending_needs_a_session_an_unconfirmed_address_and_respects_the_limits() {
    let app = TestApp::start().await;
    let anonymous = app
        .client()
        .post("/api/auth/email-verification/resend", Value::Null)
        .await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);

    let mut player = sign_up(&app, "ala@example.com", "Ala").await;
    // The registration mail goes out in the background: it must be in the box before the resends overtake it.
    app.settle().await;
    let first = token_in(&app, VERIFY_SUBJECT);
    for _ in 0..3 {
        // The clock is frozen in tests: without a step every link has the same age and the newest is decided by a random id.
        app.advance_seconds(1);
        let reply = player
            .post("/api/auth/email-verification/resend", Value::Null)
            .await;
        assert_eq!(reply.status, StatusCode::OK);
        assert_eq!(reply.json(), json!({ "ok": true, "data": null }));
    }
    let limited = player
        .post("/api/auth/email-verification/resend", Value::Null)
        .await;
    assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(limited.header("retry-after").is_some());
    // The e-mails go out in the background: wait for them (a loaded machine may be late).
    app.settle().await;
    assert_eq!(
        mails(&app, VERIFY_SUBJECT).len(),
        4,
        "registration + 3 resends"
    );
    // Only the newest link works.
    assert_eq!(
        error_of(&verify(&app, &first, None).await)["code"],
        "INVALID_TOKEN"
    );
    let newest = token_in(&app, VERIFY_SUBJECT);
    assert_eq!(verify(&app, &newest, None).await.status, StatusCode::OK);

    let done = player
        .post("/api/auth/email-verification/resend", Value::Null)
        .await;
    assert_eq!(done.status, StatusCode::CONFLICT);
    assert_eq!(error_of(&done)["code"], "ALREADY_VERIFIED");
}

#[tokio::test]
async fn resending_is_limited_per_client_address() {
    let app = TestApp::start_with(&[("TRUST_PROXY_HOPS", "1")]).await;
    // Eleven accounts behind one address, each using one resend: the eleventh hits the address limit.
    for index in 0..11 {
        let mut player = quick_player(
            &app,
            "203.0.113.9",
            &format!("u{index}@example.com"),
            &format!("User{index}"),
        )
        .await;
        let reply = player
            .post("/api/auth/email-verification/resend", Value::Null)
            .await;
        let expected = if index < 10 {
            StatusCode::OK
        } else {
            StatusCode::TOO_MANY_REQUESTS
        };
        assert_eq!(reply.status, expected, "resend {index}");
    }
}

#[tokio::test]
async fn a_failed_resend_is_a_502_and_keeps_the_previous_link_valid() {
    let app = TestApp::start().await;
    let mut player = sign_up(&app, "ala@example.com", "Ala").await;
    let first = token_in(&app, VERIFY_SUBJECT);
    *app.mail.fail_with.lock().unwrap() = Some("BREVO_NETWORK".into());
    let reply = player
        .post("/api/auth/email-verification/resend", Value::Null)
        .await;
    assert_eq!(reply.status, StatusCode::BAD_GATEWAY);
    assert_eq!(error_of(&reply)["code"], "EMAIL_SEND_FAILED");
    assert_eq!(
        count(&app, "SELECT count(*) FROM email_verification_tokens").await,
        1
    );
    assert_eq!(verify(&app, &first, None).await.status, StatusCode::OK);
}

#[tokio::test]
async fn verification_uses_at_most_seventy_percent_of_the_daily_limit() {
    // 10 per day -> 7 for verification; with 8 registrations the eighth gets no mail (and still succeeds).
    let app = TestApp::start_with(&[("EMAIL_DAILY_LIMIT", "10"), ("TRUST_PROXY_HOPS", "1")]).await;
    for index in 0..8 {
        sign_up_from(
            &app,
            &format!("203.0.113.{index}"),
            &format!("u{index}@example.com"),
            &format!("User{index}"),
        )
        .await;
    }
    assert_eq!(mails(&app, VERIFY_SUBJECT).len(), 7);
    // A resend over the quota is a 503, not a silent success.
    let mut player = quick_player(&app, "198.51.100.1", "late@example.com", "Late").await;
    let reply = player
        .post("/api/auth/email-verification/resend", Value::Null)
        .await;
    assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(error_of(&reply)["code"], "EMAIL_QUOTA_EXCEEDED");
    // With EMAIL_DAILY_LIMIT=1 verification still gets one send a day (floor(0.7) would be 0).
    let tiny = TestApp::start_with(&[("EMAIL_DAILY_LIMIT", "1")]).await;
    sign_up(&tiny, "a@example.com", "Aaa").await;
    assert_eq!(mails(&tiny, VERIFY_SUBJECT).len(), 1);
}

#[tokio::test]
async fn the_confirmation_page_changes_nothing_on_get_and_does_not_leak_the_token() {
    let app = TestApp::start().await;
    let mut player = sign_up(&app, "ala@example.com", "Ala").await;
    let token = token_in(&app, VERIFY_SUBJECT);

    let page = player
        .get(&format!("/verify-email?token={token}&next=%2Flobby"))
        .await;
    assert_eq!(page.status, StatusCode::OK);
    assert_eq!(page.header("referrer-policy"), Some("no-referrer"));
    assert!(
        page.body
            .contains(&format!(r#"name="token" value="{token}""#))
    );
    assert!(page.body.contains(r#"data-redirect="/lobby""#));
    assert!(
        page.body.contains("verify-resend"),
        "a signed-in unconfirmed player can ask for a new link"
    );
    assert!(
        player
            .get("/account")
            .await
            .body
            .contains("Niepotwierdzony"),
        "a GET confirms nothing"
    );

    let guest = app
        .client()
        .get("/verify-email?token=x&next=https%3A%2F%2Fevil.example")
        .await;
    assert!(guest.body.contains(r#"data-redirect="/""#));
    assert!(guest.body.contains("verify-login") && !guest.body.contains("verify-resend"));
}

#[tokio::test]
async fn the_reminder_shows_for_unconfirmed_players_on_home_and_account_only() {
    let app = TestApp::start().await;
    let mut player = sign_up(&app, "ala@example.com", "Ala").await;
    for path in ["/", "/account"] {
        let page = player.get(path).await;
        assert!(page.body.contains("verify-banner"), "{path}");
        assert!(page.body.contains("/assets/banner.js"));
    }
    assert!(!player.get("/login").await.body.contains("verify-banner"));
    assert!(!app.client().get("/").await.body.contains("verify-banner"));
    confirm_email(&app, "ala@example.com").await;
    assert!(!player.get("/").await.body.contains("verify-banner"));
    assert!(!player.get("/account").await.body.contains("verify-banner"));
}

// ---- e-mail change -------------------------------------------------------------------------------------------

async fn request_change(
    client: &mut Client,
    app: &TestApp,
    new_email: &str,
    password: &str,
) -> Reply {
    let reply = client
        .post(
            "/api/auth/email-change",
            json!({ "newEmail": new_email, "currentPassword": password }),
        )
        .await;
    app.settle().await;
    reply
}

#[tokio::test]
async fn an_email_change_needs_a_session_valid_input_and_the_current_password() {
    let app = TestApp::start().await;
    let anonymous = app.client().post("/api/auth/email-change", json!({})).await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);

    let mut player = sign_up(&app, "ala@example.com", "Ala").await;
    let empty = error_of(&player.post("/api/auth/email-change", json!({})).await);
    assert_eq!(
        empty["fieldIssues"]["newEmail"][0]["code"],
        "EMAIL_REQUIRED"
    );
    assert_eq!(
        empty["fieldIssues"]["currentPassword"][0]["code"],
        "CURRENT_PASSWORD_REQUIRED"
    );
    let bad = error_of(&request_change(&mut player, &app, "nope", PASSWORD).await);
    assert_eq!(bad["fieldIssues"]["newEmail"][0]["code"], "EMAIL_INVALID");

    let wrong = request_change(&mut player, &app, "new@example.com", "not my password").await;
    assert_eq!(wrong.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        error_of(&wrong)["fieldIssues"]["currentPassword"][0]["code"],
        "INVALID_CURRENT_PASSWORD"
    );
    let same = request_change(&mut player, &app, " ALA@example.com ", PASSWORD).await;
    assert_eq!(
        error_of(&same)["fieldIssues"]["newEmail"][0]["code"],
        "EMAIL_UNCHANGED"
    );
    assert!(mails(&app, CHANGE_SUBJECT).is_empty());
}

#[tokio::test]
async fn the_wrong_password_shares_its_attempt_pool_with_the_password_change() {
    let app = TestApp::start().await;
    let mut player = sign_up(&app, "ala@example.com", "Ala").await;
    for _ in 0..5 {
        request_change(&mut player, &app, "new@example.com", "nope nope").await;
    }
    for _ in 0..5 {
        player
            .post(
                "/api/auth/password",
                json!({ "currentPassword": "nope nope", "newPassword": "brand new secret" }),
            )
            .await;
    }
    let blocked = request_change(&mut player, &app, "new@example.com", PASSWORD).await;
    assert_eq!(blocked.status, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn a_change_request_is_always_accepted_and_the_link_goes_to_the_new_address_only() {
    let app = TestApp::start().await;
    let mut ala = sign_up(&app, "ala@example.com", "Ala").await;
    sign_up(&app, "taken@example.com", "Taken").await;

    let free = request_change(&mut ala, &app, "free@example.com", PASSWORD).await;
    let taken = request_change(&mut ala, &app, "taken@example.com", PASSWORD).await;
    for reply in [&free, &taken] {
        assert_eq!(reply.status, StatusCode::ACCEPTED);
        assert_eq!(reply.json(), json!({ "ok": true, "data": null }));
    }
    assert_eq!(
        free.body, taken.body,
        "the answer does not tell whether the address is free"
    );
    let sent = mails(&app, CHANGE_SUBJECT);
    assert_eq!(sent.len(), 1, "nothing goes to a taken address");
    assert_eq!(sent[0].to, "free@example.com");
    assert!(sent[0].text.contains("/confirm-email-change?token="));
    // The account still uses the old address until the link is used.
    let row = sqlx::query("SELECT email FROM users WHERE display_name = 'Ala'")
        .fetch_one(app.db.pool())
        .await
        .unwrap();
    assert_eq!(row.get::<String, _>("email"), "ala@example.com");
}

#[tokio::test]
async fn confirming_a_change_swaps_the_address_ends_every_session_and_notifies_the_old_one() {
    let app = TestApp::start().await;
    let mut ala = sign_up(&app, "ala@example.com", "Ala").await;
    confirm_email(&app, "ala@example.com").await;
    request_change(&mut ala, &app, "New.Ala@example.com", PASSWORD).await;
    let token = token_in(&app, CHANGE_SUBJECT);

    let reply = app
        .client()
        .post("/api/auth/email-change/confirm", json!({ "token": token }))
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(reply.set_cookies().is_empty());
    assert_eq!(
        ala.get("/account").await.status,
        StatusCode::SEE_OTHER,
        "every session ended"
    );
    let row = sqlx::query("SELECT email, email_verified_at FROM users")
        .fetch_one(app.db.pool())
        .await
        .unwrap();
    assert_eq!(row.get::<String, _>("email"), "new.ala@example.com");
    assert!(
        row.get::<Option<gomoku_db::OffsetDateTime>, _>("email_verified_at")
            .is_some(),
        "the new address counts as confirmed"
    );

    let notice = mails(&app, NOTICE_SUBJECT);
    assert_eq!(notice.len(), 1);
    assert_eq!(notice[0].to, "ala@example.com");
    assert!(notice[0].text.contains("n***@example.com") && !notice[0].text.contains("new.ala"));
    let login = app
        .client()
        .post(
            "/api/auth/login",
            json!({ "email": "new.ala@example.com", "password": PASSWORD }),
        )
        .await;
    assert_eq!(login.status, StatusCode::OK);
    // One use only.
    let again = app
        .client()
        .post("/api/auth/email-change/confirm", json!({ "token": token }))
        .await;
    assert_eq!(error_of(&again)["code"], "INVALID_TOKEN");
}

#[tokio::test]
async fn an_unconfirmed_old_address_gets_no_notice() {
    let app = TestApp::start().await;
    let mut ala = sign_up(&app, "ala@example.com", "Ala").await;
    request_change(&mut ala, &app, "new@example.com", PASSWORD).await;
    let token = token_in(&app, CHANGE_SUBJECT);
    assert_eq!(
        app.client()
            .post("/api/auth/email-change/confirm", json!({ "token": token }))
            .await
            .status,
        StatusCode::OK
    );
    assert!(mails(&app, NOTICE_SUBJECT).is_empty());
    // A failing notice never fails the change.
    let mut bob = sign_up(&app, "bob@example.com", "Bob").await;
    confirm_email(&app, "bob@example.com").await;
    request_change(&mut bob, &app, "bob2@example.com", PASSWORD).await;
    let token = token_in(&app, CHANGE_SUBJECT);
    *app.mail.fail_with.lock().unwrap() = Some("BREVO_NETWORK".into());
    assert_eq!(
        app.client()
            .post("/api/auth/email-change/confirm", json!({ "token": token }))
            .await
            .status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn change_links_expire_get_replaced_and_die_when_the_address_gets_taken() {
    let app = TestApp::start().await;
    let mut ala = sign_up(&app, "ala@example.com", "Ala").await;
    request_change(&mut ala, &app, "one@example.com", PASSWORD).await;
    let old = token_in(&app, CHANGE_SUBJECT);
    app.advance_seconds(5);
    request_change(&mut ala, &app, "two@example.com", PASSWORD).await;
    let new = token_in(&app, CHANGE_SUBJECT);
    let confirm = |token: String| {
        let client = app.client();
        async move {
            let mut client = client;
            client
                .post("/api/auth/email-change/confirm", json!({ "token": token }))
                .await
        }
    };
    assert_eq!(
        error_of(&confirm(old).await)["code"],
        "INVALID_TOKEN",
        "replaced by the newer link"
    );
    // The address got taken meanwhile: the unique index makes the link invalid.
    sign_up(&app, "two@example.com", "Two").await;
    assert_eq!(error_of(&confirm(new).await)["code"], "INVALID_TOKEN");
    assert_eq!(
        error_of(&confirm("unknown".into()).await)["code"],
        "INVALID_TOKEN"
    );
    let empty = app
        .client()
        .post("/api/auth/email-change/confirm", json!({}))
        .await;
    assert_eq!(
        error_of(&empty)["fieldIssues"]["token"][0]["code"],
        "TOKEN_REQUIRED"
    );

    request_change(&mut ala, &app, "three@example.com", PASSWORD).await;
    let late = token_in(&app, CHANGE_SUBJECT);
    app.advance_seconds(24 * 3600);
    assert_eq!(error_of(&confirm(late).await)["code"], "INVALID_TOKEN");
}

#[tokio::test]
async fn a_failed_send_keeps_the_previous_change_link_and_limits_apply() {
    let app = TestApp::start().await;
    let mut ala = sign_up(&app, "ala@example.com", "Ala").await;
    request_change(&mut ala, &app, "one@example.com", PASSWORD).await;
    let first = token_in(&app, CHANGE_SUBJECT);
    *app.mail.fail_with.lock().unwrap() = Some("BREVO_HTTP_500".into());
    assert_eq!(
        request_change(&mut ala, &app, "two@example.com", PASSWORD)
            .await
            .status,
        StatusCode::ACCEPTED
    );
    *app.mail.fail_with.lock().unwrap() = None;
    assert_eq!(
        count(&app, "SELECT count(*) FROM email_change_tokens").await,
        1
    );
    // The third request in the hour is the last one; the fourth is a 429 for the caller.
    assert_eq!(
        request_change(&mut ala, &app, "three@example.com", PASSWORD)
            .await
            .status,
        StatusCode::ACCEPTED
    );
    let fourth = request_change(&mut ala, &app, "four@example.com", PASSWORD).await;
    assert_eq!(fourth.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(fourth.header("retry-after").is_some());
    assert_ne!(first, token_in(&app, CHANGE_SUBJECT));
}

#[tokio::test]
async fn at_most_three_change_messages_per_hour_go_to_one_address() {
    let app = TestApp::start().await;
    for index in 0..4 {
        let mut player = quick_player(
            &app,
            "203.0.113.9",
            &format!("u{index}@example.com"),
            &format!("User{index}"),
        )
        .await;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE users SET password_hash = '{}' WHERE email = 'u{index}@example.com'",
            app.hash_password(PASSWORD).await
        )))
        .execute(app.db.pool())
        .await
        .unwrap();
        let reply = request_change(&mut player, &app, "target@example.com", PASSWORD).await;
        assert_eq!(reply.status, StatusCode::ACCEPTED);
    }
    assert_eq!(
        mails(&app, CHANGE_SUBJECT).len(),
        3,
        "the fourth request to the same address sends nothing"
    );
}

// ---- the installation administrator --------------------------------------------------------------------------

#[tokio::test]
async fn nobody_can_register_on_or_move_to_the_administrators_address_and_it_never_changes() {
    let app = TestApp::start_with(&[("ADMIN_EMAIL", "Admin@Example.com")]).await;
    let reply = app
        .client()
        .post(
            "/api/auth/register",
            json!({ "email": "admin@example.com", "displayName": "Boss", "password": PASSWORD }),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CONFLICT);
    assert_eq!(
        error_of(&reply)["data"]["reason"],
        "email_taken",
        "the same answer as for a taken e-mail"
    );

    let mut ala = sign_up(&app, "ala@example.com", "Ala").await;
    assert_eq!(
        request_change(&mut ala, &app, "admin@example.com", PASSWORD)
            .await
            .status,
        StatusCode::ACCEPTED
    );
    assert!(
        mails(&app, CHANGE_SUBJECT).is_empty(),
        "nothing is sent to the administrator's address"
    );

    // The administrator's own account (created out of band) cannot change its address, and the form is hidden.
    sqlx::query("UPDATE users SET email = 'admin@example.com'")
        .execute(app.db.pool())
        .await
        .unwrap();
    let locked = request_change(&mut ala, &app, "other@example.com", PASSWORD).await;
    assert_eq!(locked.status, StatusCode::FORBIDDEN);
    assert_eq!(error_of(&locked)["code"], "ADMIN_EMAIL_LOCKED");
    assert!(ala.get("/account").await.body.contains("email-locked"));
}

#[tokio::test]
async fn a_link_issued_before_the_role_existed_is_refused_when_used() {
    let app = TestApp::start().await;
    let mut ala = sign_up(&app, "ala@example.com", "Ala").await;
    request_change(&mut ala, &app, "later-admin@example.com", PASSWORD).await;
    let token = token_in(&app, CHANGE_SUBJECT);
    let with_admin = TestAppWithAdmin::sharing(&app, "later-admin@example.com");
    let reply = with_admin
        .client()
        .post("/api/auth/email-change/confirm", json!({ "token": token }))
        .await;
    assert_eq!(error_of(&reply)["code"], "INVALID_TOKEN");
    let row = sqlx::query("SELECT email FROM users")
        .fetch_one(app.db.pool())
        .await
        .unwrap();
    assert_eq!(
        row.get::<String, _>("email"),
        "ala@example.com",
        "nothing was written"
    );
}

/// A second router over the same database, with an administrator configured.
struct TestAppWithAdmin(axum::Router);

impl TestAppWithAdmin {
    fn sharing(app: &TestApp, admin: &str) -> Self {
        Self(app.router_with(&[("ADMIN_EMAIL", admin)]))
    }

    fn client(&self) -> Client {
        Client::for_router(self.0.clone())
    }
}

// ---- display name --------------------------------------------------------------------------------------------

async fn rename(client: &mut Client, name: &str) -> Reply {
    client
        .patch("/api/auth/display-name", json!({ "displayName": name }))
        .await
}

#[tokio::test]
async fn renaming_needs_a_session_and_valid_input_and_changes_nothing_for_the_same_name() {
    let app = TestApp::start().await;
    assert_eq!(
        rename(&mut app.client(), "Alicja").await.status,
        StatusCode::UNAUTHORIZED
    );
    let mut player = sign_up(&app, "ala@example.com", "Ala").await;
    let empty = error_of(&player.patch("/api/auth/display-name", json!({})).await);
    assert_eq!(
        empty["fieldIssues"]["displayName"][0]["code"],
        "DISPLAY_NAME_REQUIRED"
    );
    let reserved = error_of(&rename(&mut player, "Admin").await);
    assert_eq!(
        reserved["fieldIssues"]["displayName"][0]["code"],
        "DISPLAY_NAME_RESERVED"
    );
    let same = rename(&mut player, "Ala").await;
    assert_eq!(same.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        error_of(&same)["fieldIssues"]["displayName"][0]["code"],
        "DISPLAY_NAME_UNCHANGED"
    );
    // None of those spent the once-a-day window.
    assert_eq!(rename(&mut player, "Alicja").await.status, StatusCode::OK);
}

#[tokio::test]
async fn a_player_may_rename_once_a_day_and_the_old_name_becomes_free() {
    let app = TestApp::start().await;
    let mut ala = sign_up(&app, "ala@example.com", "Ala").await;
    let done = rename(&mut ala, "Alicja").await;
    assert_eq!(
        done.json(),
        json!({ "ok": true, "data": { "displayName": "Alicja" } })
    );
    assert!(ala.get("/account").await.body.contains("Alicja"));

    let too_soon = rename(&mut ala, "Ola").await;
    assert_eq!(too_soon.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(error_of(&too_soon)["code"], "DISPLAY_NAME_COOLDOWN");
    let retry: u64 = too_soon.header("retry-after").unwrap().parse().unwrap();
    assert!((86_000..=86_400).contains(&retry), "{retry}");
    let account = ala.get("/account").await;
    assert!(account.body.contains("name-cooldown") && account.body.contains("UTC"));

    // The old name is free again, for another player.
    let other = app
        .client()
        .post(
            "/api/auth/register",
            json!({ "email": "o@example.com", "displayName": "ALA", "password": PASSWORD }),
        )
        .await;
    assert_eq!(other.status, StatusCode::CREATED);
    app.advance_seconds(24 * 3600);
    assert_eq!(rename(&mut ala, "Ola").await.status, StatusCode::OK);
    // A new window started with that rename.
    assert!(ala.get("/account").await.body.contains("name-cooldown"));
    app.advance_seconds(24 * 3600);
    assert!(!ala.get("/account").await.body.contains("name-cooldown"));
}

#[tokio::test]
async fn a_taken_name_is_a_conflict_and_spends_the_window_of_nobody() {
    let app = TestApp::start().await;
    let mut ala = sign_up(&app, "ala@example.com", "Ala").await;
    sign_up(&app, "bob@example.com", "Bob").await;
    let taken = rename(&mut ala, "b_o-b").await;
    assert_eq!(taken.status, StatusCode::CONFLICT);
    let error = error_of(&taken);
    assert_eq!(
        error["fieldIssues"]["displayName"][0]["code"],
        "DISPLAY_NAME_TAKEN"
    );
    assert_eq!(error["data"]["reason"], "display_name_taken");
    assert_eq!(rename(&mut ala, "Alicja").await.status, StatusCode::OK);
}

#[tokio::test]
async fn rename_attempts_are_limited_per_account() {
    let app = TestApp::start().await;
    let mut ala = sign_up(&app, "ala@example.com", "Ala").await;
    sign_up(&app, "bob@example.com", "Bob").await;
    for _ in 0..20 {
        assert_eq!(rename(&mut ala, "Bob").await.status, StatusCode::CONFLICT);
    }
    assert_eq!(
        rename(&mut ala, "Bob").await.status,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[test]
fn the_verification_budget_and_the_rename_window_follow_the_rules() {
    use gomoku_web::routes_email::{display_name_available_at, verification_daily_budget};
    assert_eq!(verification_daily_budget(1), 1);
    assert_eq!(verification_daily_budget(2), 1);
    assert_eq!(verification_daily_budget(10), 7);
    assert_eq!(verification_daily_budget(176), 123);
    assert_eq!(verification_daily_budget(u32::MAX), 3_006_477_106);
    let now = gomoku_db::OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap();
    assert_eq!(display_name_available_at(None, now), None);
    assert_eq!(
        display_name_available_at(Some(now - time::Duration::hours(24)), now),
        None
    );
    assert_eq!(
        display_name_available_at(Some(now - time::Duration::hours(23)), now),
        Some(now + time::Duration::hours(1))
    );
}
