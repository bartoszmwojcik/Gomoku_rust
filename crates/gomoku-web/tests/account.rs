//! Account deletion end to end: the real router on a real PostgreSQL.
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

async fn delete(client: &mut Client, password: &str, confirm: Value) -> Reply {
    client
        .post(
            "/api/account/delete",
            json!({ "password": password, "confirm": confirm }),
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
async fn deleting_the_account_removes_it_for_good_and_ends_the_session() {
    let app = TestApp::start().await;
    let mut ala = sign_up(&app, "ala@example.com", "Ala").await;
    let mut elsewhere = app.client();
    elsewhere
        .post(
            "/api/auth/login",
            json!({ "email": "ala@example.com", "password": PASSWORD }),
        )
        .await;
    sign_up(&app, "bob@example.com", "Bob").await;

    // A wrong attempt first, so the counter exists and a success must forget it.
    assert_eq!(
        delete(&mut ala, "nope nope", json!(true)).await.status,
        StatusCode::BAD_REQUEST
    );
    let reply = delete(&mut ala, PASSWORD, json!(true)).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.json(), json!({ "ok": true, "data": null }));
    assert!(reply.set_cookies().join(";").contains("Max-Age=0"));

    assert_eq!(
        count(
            &app,
            "SELECT count(*) FROM users WHERE email = 'ala@example.com'"
        )
        .await,
        0
    );
    assert_eq!(
        count(&app, "SELECT count(*) FROM sessions").await,
        1,
        "only Bob's session is left"
    );
    assert_eq!(
        count(
            &app,
            "SELECT count(*) FROM rate_limit_buckets WHERE key LIKE 'account-delete:%'"
        )
        .await,
        0
    );
    assert_eq!(ala.get("/account").await.status, StatusCode::SEE_OTHER);
    assert_eq!(
        elsewhere.get("/account").await.status,
        StatusCode::SEE_OTHER
    );
    let login = app
        .client()
        .post(
            "/api/auth/login",
            json!({ "email": "ala@example.com", "password": PASSWORD }),
        )
        .await;
    assert_eq!(login.status, StatusCode::UNAUTHORIZED);
    // The e-mail and the name are free again.
    let again = app
        .client()
        .post(
            "/api/auth/register",
            json!({ "email": "ala@example.com", "displayName": "Ala", "password": PASSWORD }),
        )
        .await;
    assert_eq!(again.status, StatusCode::CREATED);
}

#[tokio::test]
async fn deletion_needs_a_session_a_password_and_the_explicit_confirmation() {
    let app = TestApp::start().await;
    let anonymous = delete(&mut app.client(), PASSWORD, json!(true)).await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);

    let mut ala = sign_up(&app, "ala@example.com", "Ala").await;
    for confirm in [json!(false), json!("yes"), Value::Null] {
        let reply = delete(&mut ala, PASSWORD, confirm).await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST);
        assert_eq!(
            error_of(&reply)["fieldIssues"]["confirm"][0]["code"],
            "CONFIRM_REQUIRED"
        );
    }
    let empty = error_of(&ala.post("/api/account/delete", json!({})).await);
    assert_eq!(
        empty["fieldIssues"]["password"][0]["code"],
        "PASSWORD_REQUIRED"
    );
    assert_eq!(
        empty["fieldIssues"]["confirm"][0]["code"],
        "CONFIRM_REQUIRED"
    );
    let blank = error_of(&delete(&mut ala, "", json!(true)).await);
    assert_eq!(
        blank["fieldIssues"]["password"][0]["code"],
        "PASSWORD_REQUIRED"
    );
    // Invalid input spends no attempt and nothing was deleted.
    assert_eq!(
        count(
            &app,
            "SELECT count(*) FROM rate_limit_buckets WHERE key LIKE 'account-delete:%'"
        )
        .await,
        0
    );
    assert_eq!(count(&app, "SELECT count(*) FROM users").await, 1);
}

#[tokio::test]
async fn wrong_passwords_stay_counted_and_block_the_account_after_ten() {
    let app = TestApp::start().await;
    let mut ala = sign_up(&app, "ala@example.com", "Ala").await;
    for _ in 0..10 {
        let reply = delete(&mut ala, "nope nope", json!(true)).await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST);
        let error = error_of(&reply);
        assert_eq!(error["code"], "INVALID_PASSWORD");
        assert_eq!(
            error["fieldIssues"]["password"][0]["code"],
            "INVALID_PASSWORD"
        );
        assert!(
            reply.set_cookies().is_empty(),
            "the session stays so the user can retry"
        );
    }
    let blocked = delete(&mut ala, PASSWORD, json!(true)).await;
    assert_eq!(blocked.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(blocked.header("retry-after").is_some());
    assert_eq!(count(&app, "SELECT count(*) FROM users").await, 1);
}

#[tokio::test]
async fn the_administrators_account_is_never_deleted_and_the_attempt_is_given_back() {
    let app = TestApp::start_with(&[("ADMIN_EMAIL", "admin@example.com")]).await;
    // The administrator's account is created out of band (here: registered under another address, then moved).
    let mut boss = sign_up(&app, "boss@example.com", "Boss").await;
    sqlx::query("UPDATE users SET email = 'admin@example.com'")
        .execute(app.db.pool())
        .await
        .unwrap();
    for _ in 0..12 {
        let reply = delete(&mut boss, PASSWORD, json!(true)).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN);
        assert_eq!(error_of(&reply)["code"], "ADMIN_ACCOUNT_PROTECTED");
    }
    assert_eq!(count(&app, "SELECT count(*) FROM users").await, 1);
    let page = boss.get("/account").await;
    assert!(page.body.contains("delete-locked") && !page.body.contains("delete-form"));
}

#[tokio::test]
async fn the_account_page_offers_the_deletion_form_with_a_checkbox() {
    let app = TestApp::start().await;
    let mut ala = sign_up(&app, "ala@example.com", "Ala").await;
    let page = ala.get("/account").await;
    assert!(page.body.contains(r#"data-testid="delete-form""#));
    assert!(page.body.contains(r#"type="checkbox" name="confirm""#));
    assert!(page.body.contains("Usunięcie konta"));
}

#[tokio::test]
async fn an_unavailable_database_does_not_leak_details_or_delete_anything() {
    let app = TestApp::start().await;
    let mut ala = sign_up(&app, "ala@example.com", "Ala").await;
    sqlx::query("ALTER TABLE users RENAME TO users_gone")
        .execute(app.db.pool())
        .await
        .unwrap();
    let reply = delete(&mut ala, PASSWORD, json!(true)).await;
    // The session lookup fails closed first: nobody is signed in.
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    assert!(!reply.body.contains("users_gone"));
}
