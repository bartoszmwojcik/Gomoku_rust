//! Account deletion, the administrator's account and the cleanup sweep on a real PostgreSQL (Docker needed).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use gomoku_db::cleanup::{CleanupOptions, RULES, cleanup_expired};
use gomoku_db::{
    Database, DbConfig, DeleteOutcome, NewSession, NewUser, OffsetDateTime, ProvisionAction,
    ProvisionError, Uuid,
};
use sqlx::Row;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ImageExt;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

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

fn now() -> OffsetDateTime {
    OffsetDateTime::now_utc()
}

async fn add_user(db: &Database, email: &str, name: &str, key: &str) -> Uuid {
    let token = format!("{:0>64}", key);
    let user = NewUser {
        email,
        display_name: name,
        display_name_key: key,
        password_hash: "$hash",
    };
    db.register_user(
        &user,
        &NewSession {
            token_hash: &token,
            expires_at: now() + time::Duration::days(30),
        },
    )
    .await
    .unwrap()
}

async fn scalar(db: &Database, sql: &str) -> i64 {
    sqlx::query(sqlx::AssertSqlSafe(sql.to_owned()))
        .fetch_one(db.pool())
        .await
        .unwrap()
        .get::<i64, _>(0)
}

#[tokio::test]
async fn deleting_an_account_removes_it_with_its_sessions_and_links() {
    let (db, _container) = database().await;
    let id = add_user(&db, "ala@example.com", "Ala", "ala").await;
    db.insert_reset_token(
        id,
        &format!("{:0>64}", "r"),
        now(),
        now() + time::Duration::hours(1),
    )
    .await
    .unwrap();
    let other = add_user(&db, "bob@example.com", "Bob", "bob").await;

    let outcome = db
        .delete_account(id, "$hash", |_| false, now())
        .await
        .unwrap();
    assert_eq!(outcome, DeleteOutcome::Deleted);
    assert!(db.find_user_by_id(id).await.unwrap().is_none());
    assert_eq!(
        scalar(&db, "SELECT count(*) FROM sessions").await,
        1,
        "only the other player's session is left"
    );
    assert_eq!(
        scalar(&db, "SELECT count(*) FROM password_reset_tokens").await,
        0
    );
    assert!(db.find_user_by_id(other).await.unwrap().is_some());
    assert_eq!(
        db.delete_account(id, "$hash", |_| false, now())
            .await
            .unwrap(),
        DeleteOutcome::Gone
    );
}

#[tokio::test]
async fn deletion_refuses_the_administrator_and_a_password_changed_meanwhile() {
    let (db, _container) = database().await;
    let admin = add_user(&db, "admin@example.com", "Boss", "boss").await;
    let ala = add_user(&db, "ala@example.com", "Ala", "ala").await;
    let protect = |email: &str| email == "admin@example.com";
    assert_eq!(
        db.delete_account(admin, "$hash", protect, now())
            .await
            .unwrap(),
        DeleteOutcome::Protected
    );
    assert!(db.find_user_by_id(admin).await.unwrap().is_some());
    assert_eq!(
        db.delete_account(ala, "$stale", protect, now())
            .await
            .unwrap(),
        DeleteOutcome::HashChanged
    );
    assert!(db.find_user_by_id(ala).await.unwrap().is_some());
}

#[tokio::test]
async fn provisioning_creates_the_administrator_or_takes_the_account_over() {
    let (db, _container) = database().await;
    let created = db
        .provision_admin("admin@example.com", Some(("Boss", "boss")), "$new", now())
        .await
        .unwrap();
    assert_eq!(
        (created.action, created.revoked_sessions),
        (ProvisionAction::Created, 0)
    );
    let user = db.find_user_by_id(created.user_id).await.unwrap().unwrap();
    assert!(user.email_verified_at.is_some());
    assert_eq!(user.password_hash, "$new");

    // Taking over: a new password, every session and unused link revoked, the name kept.
    let token = format!("{:0>64}", "s1");
    db.create_session(
        created.user_id,
        &NewSession {
            token_hash: &token,
            expires_at: now() + time::Duration::days(1),
        },
    )
    .await
    .unwrap();
    db.insert_reset_token(
        created.user_id,
        &format!("{:0>64}", "r"),
        now(),
        now() + time::Duration::hours(1),
    )
    .await
    .unwrap();
    let taken = db
        .provision_admin("admin@example.com", None, "$newer", now())
        .await
        .unwrap();
    assert_eq!(
        (taken.action, taken.user_id, taken.revoked_sessions),
        (ProvisionAction::TakenOver, created.user_id, 1)
    );
    let user = db.find_user_by_id(created.user_id).await.unwrap().unwrap();
    assert_eq!(
        (user.password_hash.as_str(), user.display_name.as_str()),
        ("$newer", "Boss")
    );
    assert_eq!(
        scalar(
            &db,
            "SELECT count(*) FROM password_reset_tokens WHERE used_at IS NULL"
        )
        .await,
        0
    );

    // An unconfirmed account that is taken over becomes confirmed.
    add_user(&db, "second@example.com", "Second", "second").await;
    db.provision_admin("second@example.com", None, "$x", now())
        .await
        .unwrap();
    assert!(
        db.find_user_by_email("second@example.com")
            .await
            .unwrap()
            .unwrap()
            .email_verified_at
            .is_some()
    );
}

#[tokio::test]
async fn provisioning_needs_a_free_name_for_a_new_account() {
    let (db, _container) = database().await;
    add_user(&db, "ala@example.com", "Ala", "ala").await;
    let nameless = db
        .provision_admin("admin@example.com", None, "$x", now())
        .await;
    assert!(matches!(nameless, Err(ProvisionError::NameRequired)));
    let taken = db
        .provision_admin("admin@example.com", Some(("ALA", "ala")), "$x", now())
        .await;
    assert!(matches!(taken, Err(ProvisionError::NameTaken)));
    assert!(
        db.find_user_by_email("admin@example.com")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn cleanup_sweeps_expired_rows_and_keeps_the_live_ones() {
    let (db, _container) = database().await;
    let id = add_user(&db, "ala@example.com", "Ala", "ala").await; // a live session
    let long_ago = now() - time::Duration::days(30);
    let hash = |n: &str| format!("{:0>64}", n);
    // Expired sessions (past the 1 h margin) and a fresh-expired one that must stay.
    for (n, expires) in [
        ("e1", long_ago),
        ("e2", long_ago),
        ("recent", now() - time::Duration::minutes(10)),
    ] {
        db.create_session(
            id,
            &NewSession {
                token_hash: &hash(n),
                expires_at: expires,
            },
        )
        .await
        .unwrap();
    }
    // Tokens: expired long ago, used long ago, still valid.
    db.insert_reset_token(
        id,
        &hash("r-old"),
        long_ago,
        long_ago + time::Duration::hours(1),
    )
    .await
    .unwrap();
    db.insert_verification_token(
        id,
        &hash("v-old"),
        long_ago,
        long_ago + time::Duration::hours(24),
    )
    .await
    .unwrap();
    db.insert_email_change_token(
        id,
        "x@example.com",
        &hash("c-old"),
        long_ago,
        long_ago + time::Duration::hours(24),
    )
    .await
    .unwrap();
    db.insert_reset_token(id, &hash("r-live"), now(), now() + time::Duration::hours(1))
        .await
        .unwrap();
    // Counters: one stale, one fresh.
    sqlx::query("INSERT INTO rate_limit_buckets (key, window_start, count) VALUES ('old', now() - interval '3 days', 1), ('new', now(), 1)")
        .execute(db.pool())
        .await
        .unwrap();
    // Unconfirmed accounts: an old one goes, a young one and the administrator stay.
    for (email, name, age_days) in [
        ("old@example.com", "Old", 30),
        ("young@example.com", "Young", 1),
        ("admin@example.com", "Boss", 30),
    ] {
        let user = add_user(&db, email, name, &name.to_lowercase()).await;
        sqlx::query(
            "UPDATE users SET created_at = now() - make_interval(days => $2) WHERE id = $1",
        )
        .bind(user)
        .bind(age_days)
        .execute(db.pool())
        .await
        .unwrap();
    }
    let mut options = CleanupOptions::new(now());
    options.protected_email = Some("admin@example.com".to_owned());
    let result = cleanup_expired(&db, &options).await;

    assert_eq!(result.errors, vec![]);
    assert!(!result.truncated);
    assert_eq!(
        (
            result.sessions,
            result.rate_limit_buckets,
            result.email_verification_tokens
        ),
        (2, 1, 1)
    );
    assert_eq!(
        (
            result.password_reset_tokens,
            result.email_change_tokens,
            result.unverified_users
        ),
        (1, 1, 1)
    );
    assert!(
        db.find_user_by_email("old@example.com")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        db.find_user_by_email("young@example.com")
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        db.find_user_by_email("admin@example.com")
            .await
            .unwrap()
            .is_some(),
        "the administrator is never swept"
    );
    assert!(
        db.find_user_by_email("ala@example.com")
            .await
            .unwrap()
            .is_some(),
        "unconfirmed but recent accounts stay too"
    );
    assert_eq!(
        scalar(&db, "SELECT count(*) FROM password_reset_tokens").await,
        1
    );
    // A second run finds nothing.
    let again = cleanup_expired(&db, &options).await;
    assert_eq!(
        (again.sessions, again.unverified_users, again.errors.len()),
        (0, 0, 0)
    );
}

#[tokio::test]
async fn cleanup_works_in_batches_and_stops_at_the_time_budget() {
    let (db, _container) = database().await;
    for index in 0..5 {
        sqlx::query("INSERT INTO rate_limit_buckets (key, window_start, count) VALUES ($1, now() - interval '9 days', 1)")
            .bind(format!("k{index}"))
            .execute(db.pool())
            .await
            .unwrap();
    }
    let mut options = CleanupOptions::new(now());
    options.batch_size = 2;
    options.rule_time_limit = Duration::ZERO;
    let first = cleanup_expired(&db, &options).await;
    assert!(first.truncated, "the budget stopped the rule early");
    assert_eq!(
        first.rate_limit_buckets, 2,
        "one batch ran, the rest is left for the next run"
    );
    options.rule_time_limit = Duration::from_secs(30);
    let second = cleanup_expired(&db, &options).await;
    assert_eq!((second.rate_limit_buckets, second.truncated), (3, false));
}

#[tokio::test]
async fn one_failing_rule_does_not_stop_the_others() {
    let (db, _container) = database().await;
    sqlx::query("DROP TABLE email_change_tokens")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO rate_limit_buckets (key, window_start, count) VALUES ('old', now() - interval '9 days', 1)")
        .execute(db.pool())
        .await
        .unwrap();
    let result = cleanup_expired(&db, &CleanupOptions::new(now())).await;
    assert_eq!(result.errors.len(), 1);
    assert_eq!(result.errors[0].rule, "emailChangeTokens");
    assert_eq!(result.errors[0].code.as_deref(), Some("42P01"));
    assert_eq!(
        result.rate_limit_buckets, 1,
        "the rules after the failed one still ran"
    );
}

/// Every table with an expiry-like column needs a cleanup rule (or an entry in `EXEMPT_TABLES`).
#[tokio::test]
async fn every_expiring_table_has_a_cleanup_rule() {
    let (db, _container) = database().await;
    let rows = sqlx::query(
        "SELECT DISTINCT table_name FROM information_schema.columns \
         WHERE table_schema = 'public' AND column_name IN ('expires_at', 'window_start') ORDER BY table_name",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    for row in rows {
        let table: String = row.get("table_name");
        let covered = RULES.iter().any(|rule| rule.table == table)
            || gomoku_db::cleanup::EXEMPT_TABLES.contains(&table.as_str());
        assert!(
            covered,
            "table {table} has an expiry column but no cleanup rule"
        );
    }
}
