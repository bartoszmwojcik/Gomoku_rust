//! Users, sessions and rate-limit counters on a real PostgreSQL (Testcontainers; needs Docker).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use gomoku_db::{Database, DbConfig, NewSession, NewUser, OffsetDateTime, RegisterError};
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ImageExt;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

/// One fresh, migrated database per test; the container lives as long as the returned guard.
async fn database() -> (Database, impl Sized) {
    let container = Postgres::default().with_tag("17").start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let db = Database::connect_lazy(&DbConfig {
        url: format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres"),
        pool_max: 4,
    })
    .unwrap();
    db.migrate().await.unwrap();
    (db, container)
}

fn user<'a>(email: &'a str, name: &'a str, key: &'a str) -> NewUser<'a> {
    NewUser {
        email,
        display_name: name,
        display_name_key: key,
        password_hash: "$argon2id$hash",
    }
}

fn session(hash: &str, hours: i64) -> (String, OffsetDateTime) {
    (
        hash.to_owned(),
        OffsetDateTime::now_utc() + time::Duration::hours(hours),
    )
}

async fn register(
    db: &Database,
    email: &str,
    name: &str,
    key: &str,
) -> Result<gomoku_db::Uuid, RegisterError> {
    let (hash, expires_at) = session(&format!("{:0>64}", key), 1);
    db.register_user(
        &user(email, name, key),
        &NewSession {
            token_hash: &hash,
            expires_at,
        },
    )
    .await
}

#[tokio::test]
async fn registers_users_and_maps_unique_violations() {
    let (db, _container) = database().await;
    let id = register(&db, "ala@example.com", "Ala", "ala")
        .await
        .unwrap();

    let found = db
        .find_user_by_email("ala@example.com")
        .await
        .unwrap()
        .unwrap();
    assert_eq!((found.id, found.display_name.as_str()), (id, "Ala"));
    assert!(found.email_verified_at.is_none() && found.display_name_changed_at.is_none());
    assert_eq!(db.find_user_by_id(id).await.unwrap(), Some(found));
    assert!(
        db.find_user_by_email("nobody@example.com")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        db.find_user_by_id(gomoku_db::Uuid::nil())
            .await
            .unwrap()
            .is_none()
    );

    assert!(matches!(
        register(&db, "ala@example.com", "Inna", "inna").await,
        Err(RegisterError::EmailTaken)
    ));
    assert!(matches!(
        register(&db, "new@example.com", "ALA", "ala").await,
        Err(RegisterError::DisplayNameTaken)
    ));
    // Both taken: the e-mail answer wins.
    assert!(matches!(
        register(&db, "ala@example.com", "ALA", "ala").await,
        Err(RegisterError::EmailTaken)
    ));
    // Other failures are plain database errors, not conflicts.
    let too_long = format!("{}@example.com", "a".repeat(260));
    assert!(matches!(
        register(&db, &too_long, "Zed", "zed").await,
        Err(RegisterError::Db(_))
    ));
}

#[tokio::test]
async fn a_failed_registration_leaves_no_session_behind() {
    let (db, _container) = database().await;
    register(&db, "ala@example.com", "Ala", "ala")
        .await
        .unwrap();
    let _ = register(&db, "ala@example.com", "Inna", "inna").await;
    let count: i64 = sqlx_count(&db, "sessions").await;
    assert_eq!(count, 1);
}

async fn sqlx_count(db: &Database, table: &str) -> i64 {
    // Table names are literals from this file.
    let sql = format!("SELECT count(*) FROM {table}");
    sqlx_scalar(db, &sql).await
}

async fn sqlx_scalar(db: &Database, sql: &str) -> i64 {
    use sqlx::Row;
    sqlx::query(sqlx::AssertSqlSafe(sql.to_owned()))
        .fetch_one(db.pool())
        .await
        .unwrap()
        .get::<i64, _>(0)
}

#[tokio::test]
async fn sessions_are_found_until_they_expire_and_can_be_revoked() {
    let (db, _container) = database().await;
    let id = register(&db, "ala@example.com", "Ala", "ala")
        .await
        .unwrap();
    let (hash, expires_at) = session("a1", 1);
    db.create_session(
        id,
        &NewSession {
            token_hash: &hash,
            expires_at,
        },
    )
    .await
    .unwrap();

    let now = OffsetDateTime::now_utc();
    let found = db.find_valid_session(&hash, now).await.unwrap().unwrap();
    assert_eq!(
        (
            found.user_id,
            found.display_name.as_str(),
            found.email.as_str()
        ),
        (id, "Ala", "ala@example.com")
    );
    assert!(found.email_verified_at.is_none());
    assert!(
        db.find_valid_session(&hash, now + time::Duration::hours(2))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        db.find_valid_session("unknown", now)
            .await
            .unwrap()
            .is_none()
    );

    db.revoke_session(&hash).await.unwrap();
    assert!(db.find_valid_session(&hash, now).await.unwrap().is_none());
    db.revoke_session(&hash).await.unwrap();
    // Revoking one session leaves the user's other sessions alone (the registration one).
    assert_eq!(sqlx_count(&db, "sessions").await, 1);
}

#[tokio::test]
async fn rate_limit_counts_within_a_window_and_starts_a_new_one_after_it() {
    let (db, _container) = database().await;
    let first = db.rate_limit_hit("k", 60_000).await.unwrap();
    let second = db.rate_limit_hit("k", 60_000).await.unwrap();
    assert_eq!((first.count, second.count), (1, 2));
    assert!((1..=60).contains(&second.retry_after_seconds));
    assert_eq!(db.rate_limit_hit("other", 60_000).await.unwrap().count, 1);

    db.rate_limit_release("k", 60_000).await.unwrap();
    assert_eq!(db.rate_limit_hit("k", 60_000).await.unwrap().count, 2);
    // Never below zero, and nothing to release in an expired window.
    db.rate_limit_release("fresh", 60_000).await.unwrap();
    assert_eq!(db.rate_limit_hit("fresh", 60_000).await.unwrap().count, 1);

    db.rate_limit_reset("k").await.unwrap();
    assert_eq!(db.rate_limit_hit("k", 60_000).await.unwrap().count, 1);

    assert_eq!(db.rate_limit_hit("short", 50).await.unwrap().count, 1);
    tokio::time::sleep(Duration::from_millis(120)).await;
    assert_eq!(
        db.rate_limit_hit("short", 50).await.unwrap().count,
        1,
        "the window elapsed: a new one starts"
    );
    tokio::time::sleep(Duration::from_millis(120)).await;
    db.rate_limit_release("short", 50).await.unwrap();
    assert_eq!(db.rate_limit_hit("short", 50).await.unwrap().count, 1);
}

async fn count(db: &Database, sql: &str) -> i64 {
    sqlx_scalar(db, sql).await
}

async fn verified_user(db: &Database, email: &str, name: &str, key: &str) -> gomoku_db::Uuid {
    let id = register(db, email, name, key).await.unwrap();
    sqlx::query("UPDATE users SET email_verified_at = now() WHERE id = $1")
        .bind(id)
        .execute(db.pool())
        .await
        .unwrap();
    id
}

#[tokio::test]
async fn changing_a_password_is_conditional_and_ends_other_sessions_and_links() {
    let (db, _container) = database().await;
    let id = verified_user(&db, "ala@example.com", "Ala", "ala").await;
    let now = OffsetDateTime::now_utc();
    let (other_hash, expires_at) = session("other", 1);
    db.create_session(
        id,
        &NewSession {
            token_hash: &other_hash,
            expires_at,
        },
    )
    .await
    .unwrap();
    let current = db
        .find_valid_session(&format!("{:0>64}", "ala"), now)
        .await
        .unwrap()
        .unwrap();
    db.insert_reset_token(
        id,
        &format!("{:0>64}", "t1"),
        now,
        now + time::Duration::hours(1),
    )
    .await
    .unwrap();

    assert!(
        db.change_password(
            id,
            "$argon2id$hash",
            "$argon2id$new",
            current.session_id,
            now
        )
        .await
        .unwrap()
    );
    assert_eq!(
        db.find_user_by_id(id).await.unwrap().unwrap().password_hash,
        "$argon2id$new"
    );
    assert_eq!(
        count(&db, "SELECT count(*) FROM sessions").await,
        1,
        "only the current session stays"
    );
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM password_reset_tokens WHERE used_at IS NULL"
        )
        .await,
        0
    );

    // The second change with the stale hash loses the race and writes nothing.
    assert!(
        !db.change_password(
            id,
            "$argon2id$hash",
            "$argon2id$other",
            current.session_id,
            now
        )
        .await
        .unwrap()
    );
    assert_eq!(
        db.find_user_by_id(id).await.unwrap().unwrap().password_hash,
        "$argon2id$new"
    );
}

#[tokio::test]
async fn reset_links_are_consumed_once_and_only_for_confirmed_addresses() {
    let (db, _container) = database().await;
    let id = verified_user(&db, "ala@example.com", "Ala", "ala").await;
    let bob = register(&db, "bob@example.com", "Bob", "bob")
        .await
        .unwrap();
    let now = OffsetDateTime::now_utc();
    let later = now + time::Duration::hours(1);
    let hash = |n: &str| format!("{:0>64}", n);
    db.insert_reset_token(id, &hash("good"), now, later)
        .await
        .unwrap();
    db.insert_reset_token(
        id,
        &hash("expired"),
        now - time::Duration::hours(2),
        now - time::Duration::hours(1),
    )
    .await
    .unwrap();
    db.insert_reset_token(bob, &hash("unconfirmed"), now, later)
        .await
        .unwrap();

    assert_eq!(
        db.confirm_password_reset(&hash("nope"), "$new", now)
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        db.confirm_password_reset(&hash("expired"), "$new", now)
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        db.confirm_password_reset(&hash("unconfirmed"), "$new", now)
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        db.find_user_by_id(bob)
            .await
            .unwrap()
            .unwrap()
            .password_hash,
        "$argon2id$hash"
    );

    assert_eq!(
        db.confirm_password_reset(&hash("good"), "$new", now)
            .await
            .unwrap(),
        Some(id)
    );
    assert_eq!(
        db.find_user_by_id(id).await.unwrap().unwrap().password_hash,
        "$new"
    );
    assert_eq!(
        count(
            &db,
            &format!("SELECT count(*) FROM sessions WHERE user_id = '{id}'")
        )
        .await,
        0
    );
    // One use only.
    assert_eq!(
        db.confirm_password_reset(&hash("good"), "$again", now)
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn a_newer_reset_link_invalidates_older_ones_only() {
    let (db, _container) = database().await;
    let id = verified_user(&db, "ala@example.com", "Ala", "ala").await;
    let now = OffsetDateTime::now_utc();
    let later = now + time::Duration::hours(1);
    let hash = |n: &str| format!("{:0>64}", n);
    let old = db
        .insert_reset_token(id, &hash("old"), now - time::Duration::minutes(5), later)
        .await
        .unwrap();
    let new = db
        .insert_reset_token(id, &hash("new"), now, later)
        .await
        .unwrap();
    let newest = db
        .insert_reset_token(id, &hash("newest"), now + time::Duration::seconds(1), later)
        .await
        .unwrap();
    db.invalidate_older_reset_tokens(id, now, new.id, now)
        .await
        .unwrap();
    let used: Vec<String> = {
        use sqlx::Row;
        sqlx::query("SELECT token_hash FROM password_reset_tokens WHERE used_at IS NOT NULL")
            .fetch_all(db.pool())
            .await
            .unwrap()
            .iter()
            .map(|row| row.get::<String, _>(0))
            .collect()
    };
    assert_eq!(used, vec![hash("old")]);
    // A link that was never delivered can be removed.
    db.delete_reset_token(newest.id).await.unwrap();
    db.delete_reset_token(old.id).await.unwrap();
    assert_eq!(
        count(&db, "SELECT count(*) FROM password_reset_tokens").await,
        1
    );
}
