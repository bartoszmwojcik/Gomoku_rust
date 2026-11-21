//! Process entry point of the administrator CLI: terminal I/O and exit code only; the logic lives in
//! `gomoku_web::admin_cli`.

use std::io::{BufRead, IsTerminal, Write};

use gomoku_db::{Database, DbConfig, OffsetDateTime};
use gomoku_web::admin_cli::{Prompter, run_create};
use gomoku_web::config::AppConfig;

struct Terminal {
    interactive: bool,
}

impl Prompter for Terminal {
    fn interactive(&self) -> bool {
        self.interactive
    }

    fn ask(&mut self, prompt: &str, secret: bool) -> Option<String> {
        if self.interactive {
            if secret {
                return rpassword::prompt_password(prompt).ok();
            }
            eprint!("{prompt}");
            let _ = std::io::stderr().flush();
        }
        let mut line = String::new();
        match std::io::stdin().lock().read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(line.trim_end_matches(['\n', '\r']).to_owned()),
        }
    }
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let _ = dotenvy::dotenv();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let env = |key: &str| std::env::var(key).ok();
    let rest = match args.split_first() {
        Some((command, rest)) if command == "create" => rest,
        _ => {
            eprintln!("usage: gomoku-admin create [--name <name>] [--yes]");
            return std::process::ExitCode::from(2);
        }
    };
    let setup = AppConfig::from_lookup(env)
        .map_err(|e| e.to_string())
        .and_then(|config| {
            let db_config = DbConfig::from_lookup(env).map_err(|e| e.to_string())?;
            let db = Database::connect_lazy(&db_config)
                .map_err(|_| "Nieprawidłowy DATABASE_URL.".to_owned())?;
            Ok((config, db))
        });
    let (config, db) = match setup {
        Ok(parts) => parts,
        Err(message) => {
            eprintln!("{message}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let mut terminal = Terminal {
        interactive: std::io::stdin().is_terminal(),
    };
    let report = run_create(
        &env,
        rest,
        &mut terminal,
        &db,
        config.password,
        OffsetDateTime::now_utc(),
    )
    .await;
    if !report.stdout.is_empty() {
        println!("{}", report.stdout);
    }
    eprint!("{}", report.stderr);
    std::process::ExitCode::from(u8::try_from(report.code).unwrap_or(1))
}
