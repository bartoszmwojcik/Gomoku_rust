//! Test harness: the real router over a real PostgreSQL (one container per test binary, one database per test).
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use gomoku_db::{Database, DbConfig, OffsetDateTime};
use gomoku_web::app::{AppState, build_router};
use gomoku_web::config::{AppConfig, PasswordParams};
use gomoku_web::health::HealthProbe;
use gomoku_web::i18n::Catalogs;
use gomoku_web::password::PasswordHasher;
use http_body_util::BodyExt;
use serde_json::Value;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ContainerAsync;
use testcontainers_modules::testcontainers::ImageExt;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use tokio::sync::OnceCell;
use tower::ServiceExt;

static CONTAINER: OnceCell<(ContainerAsync<Postgres>, String)> = OnceCell::const_new();

async fn admin_url() -> &'static str {
    let (_, url) = CONTAINER
        .get_or_init(|| async {
            let container = Postgres::default()
                .with_tag("17")
                .start()
                .await
                .expect("start postgres");
            let port = container.get_host_port_ipv4(5432).await.expect("port");
            (
                container,
                format!("postgres://postgres:postgres@127.0.0.1:{port}"),
            )
        })
        .await;
    url
}

pub const ORIGIN: &str = "http://localhost:3000";

pub struct FakeProbe(pub Result<(), String>);

#[async_trait]
impl HealthProbe for FakeProbe {
    async fn check(&self) -> Result<(), String> {
        self.0.clone()
    }
}

pub struct TestApp {
    pub router: Router,
    pub db: Database,
    pub clock: Arc<Mutex<OffsetDateTime>>,
}

pub fn fast_config(overrides: &[(&str, &str)]) -> AppConfig {
    let mut config = AppConfig::from_lookup(|key| {
        overrides
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| (*v).to_string())
    })
    .unwrap();
    config.password = PasswordParams {
        m_cost_kib: 8,
        t_cost: 1,
        p_cost: 1,
    };
    config.assets_dir = "../../assets".into();
    config
}

fn state_for(
    db: Database,
    config: AppConfig,
    probe: Result<(), String>,
) -> (AppState, Arc<Mutex<OffsetDateTime>>) {
    let clock = Arc::new(Mutex::new(
        OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap(),
    ));
    let clock_for_state = Arc::clone(&clock);
    let state = AppState {
        hasher: PasswordHasher::new(config.password).unwrap(),
        health: Arc::new(FakeProbe(probe)),
        db,
        clock: Arc::new(move || *clock_for_state.lock().unwrap()),
        catalogs: Catalogs::embedded().unwrap(),
        config: Arc::new(config),
    };
    (state, clock)
}

impl TestApp {
    /// A fresh, migrated database and the app on top of it.
    pub async fn start() -> Self {
        Self::start_with(&[]).await
    }

    pub async fn start_with(overrides: &[(&str, &str)]) -> Self {
        let admin = admin_url().await;
        let name = format!("t_{}", uuid_simple());
        let admin_db = Database::connect_lazy(&DbConfig {
            url: format!("{admin}/postgres"),
            pool_max: 1,
        })
        .unwrap();
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
            .execute(admin_db.pool())
            .await
            .unwrap();
        let db = Database::connect_lazy(&DbConfig {
            url: format!("{admin}/{name}"),
            pool_max: 4,
        })
        .unwrap();
        db.migrate().await.unwrap();
        let config = fast_config(overrides);
        let (state, clock) = state_for(db.clone(), config.clone(), Ok(()));
        Self {
            router: build_router(state, &config.assets_dir),
            db,
            clock,
        }
    }

    /// An app whose database is unreachable and whose health probe answers `probe`.
    pub fn without_database(probe: Result<(), String>) -> Self {
        let db = Database::connect_lazy(&DbConfig {
            url: "postgres://u:p@127.0.0.1:1/none".into(),
            pool_max: 1,
        })
        .unwrap();
        let config = fast_config(&[]);
        let (state, clock) = state_for(db.clone(), config.clone(), probe);
        Self {
            router: build_router(state, &config.assets_dir),
            db,
            clock,
        }
    }

    pub fn advance_seconds(&self, seconds: i64) {
        let mut now = self.clock.lock().unwrap();
        *now += time::Duration::seconds(seconds);
    }

    pub fn client(&self) -> Client {
        Client {
            router: self.router.clone(),
            cookies: Vec::new(),
            headers: Vec::new(),
        }
    }
}

fn uuid_simple() -> String {
    let bytes: [u8; 8] = rand::random();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub struct Reply {
    pub status: StatusCode,
    pub headers: axum::http::HeaderMap,
    pub body: String,
}

impl Reply {
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|_| panic!("not JSON: {}", self.body))
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    pub fn set_cookies(&self) -> Vec<String> {
        self.headers
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .map(str::to_owned)
            .collect()
    }
}

/// A browser-like client: keeps cookies between requests and sends the CSRF headers on API calls.
pub struct Client {
    router: Router,
    cookies: Vec<(String, String)>,
    headers: Vec<(String, String)>,
}

impl Client {
    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }

    pub fn cookie(&self, name: &str) -> Option<&str> {
        self.cookies
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn set_cookie(&mut self, name: &str, value: &str) {
        self.cookies.retain(|(n, _)| n != name);
        self.cookies.push((name.to_owned(), value.to_owned()));
    }

    fn absorb(&mut self, reply: &Reply) {
        for raw in reply.set_cookies() {
            let pair = raw.split(';').next().unwrap_or("");
            if let Some((name, value)) = pair.split_once('=') {
                self.cookies.retain(|(n, _)| n != name);
                if !value.is_empty() && !raw.to_ascii_lowercase().contains("max-age=0") {
                    self.cookies.push((name.to_owned(), value.to_owned()));
                }
            }
        }
    }

    pub async fn send(&mut self, request: axum::http::request::Builder, body: Body) -> Reply {
        let mut request = request;
        if !self.cookies.is_empty() {
            let cookie = self
                .cookies
                .iter()
                .map(|(n, v)| format!("{n}={v}"))
                .collect::<Vec<_>>()
                .join("; ");
            request = request.header(header::COOKIE, cookie);
        }
        for (name, value) in &self.headers {
            request = request.header(name.as_str(), value.as_str());
        }
        let response = self
            .router
            .clone()
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap();
        let (parts, body) = response.into_parts();
        let bytes = body.collect().await.unwrap().to_bytes();
        let reply = Reply {
            status: parts.status,
            headers: parts.headers,
            body: String::from_utf8_lossy(&bytes).into_owned(),
        };
        self.absorb(&reply);
        reply
    }

    pub async fn get(&mut self, uri: &str) -> Reply {
        self.send(Request::get(uri), Body::empty()).await
    }

    /// A JSON POST with the CSRF headers a browser form would send.
    pub async fn post(&mut self, uri: &str, body: Value) -> Reply {
        let request = Request::post(uri)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ORIGIN, ORIGIN)
            .header("x-gomoku-request", "1");
        self.send(request, Body::from(body.to_string())).await
    }

    /// A JSON POST without any CSRF header.
    pub async fn post_bare(&mut self, uri: &str, body: Value) -> Reply {
        self.send(
            Request::post(uri).header(header::CONTENT_TYPE, "application/json"),
            Body::from(body.to_string()),
        )
        .await
    }
}
