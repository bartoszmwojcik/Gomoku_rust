//! Integration tests on a real PostgreSQL (Testcontainers). They need a running Docker daemon.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use gomoku_db::{Database, DbConfig};
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

#[tokio::test]
async fn migrates_and_pings_a_real_postgres() {
    let container = Postgres::default().start().await.expect("start postgres");
    let port = container.get_host_port_ipv4(5432).await.expect("port");
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let config = DbConfig { url, pool_max: 2 };

    let db = Database::connect_lazy(&config).expect("pool");
    db.ping().await.expect("ping");
    db.migrate().await.expect("migrate");
    db.migrate().await.expect("migrate is idempotent");

    sqlx::query("INSERT INTO app_meta (key, value) VALUES ('schema', 'f0')")
        .execute(db.pool())
        .await
        .expect("insert");
}

#[tokio::test]
async fn ping_fails_when_database_is_down() {
    let config = DbConfig {
        url: "postgres://u:p@127.0.0.1:1/none".into(),
        pool_max: 1,
    };
    let db = Database::connect_lazy(&config).expect("lazy pool never fails to build");
    assert!(db.ping().await.is_err());
}
