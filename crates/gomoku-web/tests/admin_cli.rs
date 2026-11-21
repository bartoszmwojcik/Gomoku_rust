//! `gomoku-admin create` against a real PostgreSQL with a scripted terminal.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::VecDeque;

use common::TestApp;
use gomoku_web::admin_cli::{Prompter, Report, run_create};
use gomoku_web::config::PasswordParams;
use serde_json::{Value, json};

struct Script {
    interactive: bool,
    answers: VecDeque<String>,
    prompts: Vec<String>,
}

impl Script {
    fn terminal(answers: &[&str]) -> Self {
        Self {
            interactive: true,
            answers: answers.iter().map(|a| (*a).to_owned()).collect(),
            prompts: vec![],
        }
    }

    fn piped(answers: &[&str]) -> Self {
        Self {
            interactive: false,
            ..Self::terminal(answers)
        }
    }
}

impl Prompter for Script {
    fn interactive(&self) -> bool {
        self.interactive
    }

    fn ask(&mut self, prompt: &str, _secret: bool) -> Option<String> {
        self.prompts.push(prompt.to_owned());
        self.answers.pop_front()
    }
}

fn params() -> PasswordParams {
    PasswordParams {
        m_cost_kib: 8,
        t_cost: 1,
        p_cost: 1,
    }
}

async fn create(app: &TestApp, env: &[(&str, &str)], args: &[&str], script: &mut Script) -> Report {
    let lookup = |key: &str| {
        env.iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| (*v).to_owned())
    };
    let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
    run_create(
        &lookup,
        &args,
        script,
        &app.db,
        params(),
        gomoku_db::OffsetDateTime::now_utc(),
    )
    .await
}

const ENV: [(&str, &str); 2] = [
    ("ADMIN_EMAIL", "Admin@Example.com"),
    (
        "DATABASE_URL",
        "postgres://gomoku:topsecret@db.internal:5432/gomoku",
    ),
];

fn json_of(report: &Report) -> Value {
    serde_json::from_str(&report.stdout).unwrap_or_else(|_| panic!("not JSON: {report:?}"))
}

#[tokio::test]
async fn creates_the_administrator_from_a_piped_password() {
    let app = TestApp::start().await;
    let report = create(
        &app,
        &ENV,
        &["--name", "Boss"],
        &mut Script::piped(&["a good password"]),
    )
    .await;
    assert_eq!(report.code, 0, "{report:?}");
    let out = json_of(&report);
    assert_eq!(
        (
            out["ok"].clone(),
            out["action"].clone(),
            out["revokedSessions"].clone()
        ),
        (json!(true), json!("created"), json!(0))
    );
    assert!(
        report
            .stderr
            .contains("Baza danych: db.internal:5432/gomoku")
    );
    assert!(!report.stderr.contains("topsecret") && !report.stdout.contains("a good password"));

    let user = app
        .db
        .find_user_by_email("admin@example.com")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(user.display_name, "Boss");
    assert!(user.email_verified_at.is_some() && user.password_hash.starts_with("$argon2id$"));
    // The password works at login.
    let login = app
        .client()
        .post(
            "/api/auth/login",
            json!({ "email": "admin@example.com", "password": "a good password" }),
        )
        .await;
    assert_eq!(login.status, axum::http::StatusCode::OK);
}

#[tokio::test]
async fn refuses_without_the_administrator_address_or_with_bad_arguments() {
    let app = TestApp::start().await;
    let nothing = create(&app, &[], &[], &mut Script::piped(&["a good password"])).await;
    assert_eq!(
        (nothing.code, nothing.stderr.as_str()),
        (1, "Ustaw ADMIN_EMAIL (adres administratora instalacji).\n")
    );
    let bad_email = create(
        &app,
        &[("ADMIN_EMAIL", "nope")],
        &[],
        &mut Script::piped(&[]),
    )
    .await;
    assert!(
        bad_email
            .stderr
            .contains("ADMIN_EMAIL nie jest poprawnym adresem")
    );
    let typo = create(&app, &ENV, &["--yess"], &mut Script::piped(&[])).await;
    assert!(typo.stderr.contains("Nieznany argument: --yess."));
    let bad_name = create(&app, &ENV, &["--name", "ab"], &mut Script::piped(&[])).await;
    assert!(bad_name.stderr.contains("co najmniej 3 znaki"));
    let unset = create(
        &app,
        &[("ADMIN_EMAIL", "a@example.com")],
        &["--name", "Boss"],
        &mut Script::piped(&["a good password"]),
    )
    .await;
    assert!(
        unset
            .stderr
            .contains("Baza danych: nieustawiona (DATABASE_URL)")
    );
}

#[tokio::test]
async fn a_piped_password_must_be_valid_and_a_new_account_needs_a_name() {
    let app = TestApp::start().await;
    let short = create(
        &app,
        &ENV,
        &["--name", "Boss"],
        &mut Script::piped(&["short"]),
    )
    .await;
    assert_eq!(short.code, 1);
    assert!(short.stderr.contains("od 8 znaków"));
    let nothing = create(&app, &ENV, &["--name", "Boss"], &mut Script::piped(&[])).await;
    assert!(nothing.stderr.contains("Brak hasła na wejściu."));
    let nameless = create(&app, &ENV, &[], &mut Script::piped(&["a good password"])).await;
    assert!(nameless.stderr.contains("Podaj nazwę gracza flagą --name."));
    assert!(
        app.db
            .find_user_by_email("admin@example.com")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn taking_an_existing_account_over_needs_consent_and_revokes_its_sessions() {
    let app = TestApp::start().await;
    let mut player = app.client();
    player
        .post("/api/auth/register", json!({ "email": "admin@example.com", "displayName": "Boss", "password": "old password" }))
        .await;
    assert_eq!(
        player.get("/account").await.status,
        axum::http::StatusCode::OK
    );

    let piped = create(&app, &ENV, &[], &mut Script::piped(&["a good password"])).await;
    assert!(
        piped
            .stderr
            .contains("bez terminala potwierdź przejęcie flagą --yes.")
    );
    let refused = create(
        &app,
        &ENV,
        &[],
        &mut Script::terminal(&["a good password", "a good password", "n"]),
    )
    .await;
    assert!(refused.stderr.contains("Anulowano, nic nie zmieniono."));
    assert_eq!(
        player.get("/account").await.status,
        axum::http::StatusCode::OK,
        "nothing changed"
    );

    let taken = create(
        &app,
        &ENV,
        &["--name", "Other"],
        &mut Script::terminal(&["a good password", "a good password", "TAK"]),
    )
    .await;
    assert_eq!(taken.code, 0, "{taken:?}");
    assert!(
        taken
            .stderr
            .contains("konto zostaje przy nazwie „Boss”, --name zignorowano.")
    );
    let out = json_of(&taken);
    assert_eq!(
        (out["action"].clone(), out["revokedSessions"].clone()),
        (json!("taken_over"), json!(1))
    );
    assert_eq!(
        player.get("/account").await.status,
        axum::http::StatusCode::SEE_OTHER
    );

    let forced = create(
        &app,
        &ENV,
        &["--yes"],
        &mut Script::piped(&["another good one"]),
    )
    .await;
    assert_eq!(forced.code, 0, "{forced:?}");
}

#[tokio::test]
async fn the_terminal_asks_twice_retries_and_prompts_for_the_name() {
    let app = TestApp::start().await;
    let mut script = Script::terminal(&[
        "short",
        "",
        "a good password",
        "different one",
        "",
        "a good password",
        "a good password",
        "Boss",
    ]);
    let report = create(&app, &ENV, &[], &mut script).await;
    assert_eq!(report.code, 0, "{report:?}");
    assert!(
        script
            .prompts
            .iter()
            .any(|p| p.contains("Hasła się różnią."))
    );
    assert!(script.prompts.iter().any(|p| p.contains("od 8 znaków")));
    assert!(script.prompts.iter().any(|p| p == "Nazwa gracza: "));
    assert_eq!(
        app.db
            .find_user_by_email("admin@example.com")
            .await
            .unwrap()
            .unwrap()
            .display_name,
        "Boss"
    );

    // Three failed attempts give up with the last problem.
    let app = TestApp::start().await;
    let mut script = Script::terminal(&["x", "", "x", "", "x"]);
    let failed = create(&app, &ENV, &["--name", "Boss"], &mut script).await;
    assert_eq!(failed.code, 1);
    // A terminal that ends while asking for the name.
    let mut script = Script::terminal(&["a good password", "a good password"]);
    let no_name = create(&app, &ENV, &[], &mut script).await;
    assert!(no_name.stderr.contains("Brak nazwy gracza na wejściu."));
    let mut script = Script::terminal(&["a good password", "a good password", "ab"]);
    let bad_name = create(&app, &ENV, &[], &mut script).await;
    assert!(bad_name.stderr.contains("co najmniej 3 znaki"));
}

#[tokio::test]
async fn a_taken_name_and_an_unreachable_database_are_clear_errors() {
    let app = TestApp::start().await;
    app.client()
        .post("/api/auth/register", json!({ "email": "ala@example.com", "displayName": "Boss", "password": "old password" }))
        .await;
    let taken = create(
        &app,
        &ENV,
        &["--name", "BOSS"],
        &mut Script::piped(&["a good password"]),
    )
    .await;
    assert!(taken.stderr.contains("Ta nazwa gracza jest już zajęta."));

    let down = TestApp::without_database(Ok(()));
    let unreachable = create(
        &down,
        &ENV,
        &["--name", "Boss"],
        &mut Script::piped(&["a good password"]),
    )
    .await;
    assert_eq!(unreachable.code, 1);
    assert!(
        unreachable.stderr.contains("Baza danych jest niedostępna.")
            && !unreachable.stderr.contains("topsecret")
    );
}
