//! `gomoku-admin create`: creates the installation administrator's account (`ADMIN_EMAIL`) or takes an existing one
//! over: new password, confirmed e-mail, every session and unused link revoked. The password is read only from the
//! terminal (hidden) or from one line of stdin; never from an argument or a variable (they leak through `ps`, /proc
//! and `docker inspect`). Prints one JSON line without secrets; errors go to stderr. CLI texts stay Polish.

use gomoku_core::auth::{validate_display_name, validate_email, validate_new_password};
use gomoku_core::validators::normalize_display_name;
use gomoku_db::{Database, OffsetDateTime, ProvisionAction, ProvisionError};

use crate::config::PasswordParams;
use crate::password::PasswordHasher;

const PASSWORD_ATTEMPTS: usize = 3;

/// Where the operator's answers come from; the real one wraps the terminal, tests script it.
pub trait Prompter {
    /// Whether a person is at a terminal (otherwise one line of stdin is the password).
    fn interactive(&self) -> bool;
    /// Asks one question; `None` means the input ended. `secret` answers are not echoed.
    fn ask(&mut self, prompt: &str, secret: bool) -> Option<String>;
}

/// How a command ended: what to print on each stream and the exit code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CreateArgs {
    pub name: Option<String>,
    pub yes: bool,
}

/// `--name <n>` / `--name=<n>` and `--yes`; anything else is an error (a typo must not be ignored).
pub fn parse_create_args(args: &[String]) -> Result<CreateArgs, String> {
    let mut parsed = CreateArgs::default();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--yes" {
            parsed.yes = true;
        } else if arg == "--name" {
            parsed.name = Some(iter.next().ok_or("Flaga --name wymaga wartości.")?.clone());
        } else if let Some(value) = arg.strip_prefix("--name=") {
            parsed.name = Some(value.to_owned());
        } else {
            return Err(format!("Nieznany argument: {arg}."));
        }
    }
    Ok(parsed)
}

/// `host:port/database` of the target, without credentials, so the operator sees which database is about to change.
pub fn describe_database_target(database_url: Option<&str>) -> String {
    let Some(url) = database_url.filter(|u| !u.is_empty()) else {
        return "nieustawiona (DATABASE_URL)".to_owned();
    };
    let Some((_, rest)) = url.split_once("://") else {
        return "nieczytelna (DATABASE_URL)".to_owned();
    };
    let rest = rest.rsplit_once('@').map_or(rest, |(_, host)| host);
    let target = rest.split('?').next().unwrap_or("");
    if target.is_empty() {
        "nieczytelna (DATABASE_URL)".to_owned()
    } else {
        target.to_owned()
    }
}

struct OperatorError(String);

fn read_password(prompter: &mut dyn Prompter) -> Result<String, OperatorError> {
    let attempts = if prompter.interactive() {
        PASSWORD_ATTEMPTS
    } else {
        1
    };
    let mut problem = String::new();
    for attempt in 0..attempts {
        let password = prompter
            .ask("Hasło administratora: ", true)
            .ok_or_else(|| OperatorError("Brak hasła na wejściu.".to_owned()))?;
        if validate_new_password(&password).is_err() {
            problem = "Hasło musi mieć od 8 znaków do 72 bajtów.".to_owned();
        } else if !prompter.interactive()
            || prompter.ask("Powtórz hasło: ", true).as_deref() == Some(password.as_str())
        {
            return Ok(password);
        } else {
            problem = "Hasła się różnią.".to_owned();
        }
        if attempt + 1 < attempts {
            let _ = prompter.ask(
                &format!("{problem} Wciśnij Enter, aby spróbować ponownie."),
                false,
            );
        }
    }
    Err(OperatorError(problem))
}

fn parse_name(raw: &str) -> Result<String, OperatorError> {
    validate_display_name(raw)
        .map_err(|issues| OperatorError(crate::api::issue_message(issues[0]).to_owned()))
}

/// Runs `gomoku-admin create`. `env` supplies `ADMIN_EMAIL` and `DATABASE_URL` (the latter only for the printed
/// target); the account is written to `db`.
pub async fn run_create(
    env: &dyn Fn(&str) -> Option<String>,
    args: &[String],
    prompter: &mut dyn Prompter,
    db: &Database,
    password_params: PasswordParams,
    now: OffsetDateTime,
) -> Report {
    let mut stderr = String::new();
    match create(env, args, prompter, db, password_params, now, &mut stderr).await {
        Ok(stdout) => Report {
            stdout,
            stderr,
            code: 0,
        },
        Err(OperatorError(message)) => {
            stderr.push_str(&message);
            stderr.push('\n');
            Report {
                stdout: String::new(),
                stderr,
                code: 1,
            }
        }
    }
}

async fn create(
    env: &dyn Fn(&str) -> Option<String>,
    args: &[String],
    prompter: &mut dyn Prompter,
    db: &Database,
    password_params: PasswordParams,
    now: OffsetDateTime,
    stderr: &mut String,
) -> Result<String, OperatorError> {
    let raw_email = env("ADMIN_EMAIL").filter(|v| !v.trim().is_empty());
    let raw_email = raw_email.ok_or_else(|| {
        OperatorError("Ustaw ADMIN_EMAIL (adres administratora instalacji).".into())
    })?;
    let email = validate_email(&raw_email)
        .map_err(|_| OperatorError("ADMIN_EMAIL nie jest poprawnym adresem e-mail.".into()))?;
    let parsed = parse_create_args(args).map_err(OperatorError)?;
    let requested_name = parsed.name.as_deref().map(parse_name).transpose()?;
    stderr.push_str(&format!(
        "Baza danych: {}\n",
        describe_database_target(env("DATABASE_URL").as_deref())
    ));

    let password = read_password(prompter)?;
    let hasher = PasswordHasher::new(password_params)
        .map_err(|_| OperatorError("Nieprawidłowe parametry argon2.".into()))?;
    let password_hash = hasher
        .hash(&password)
        .await
        .map_err(|_| OperatorError("Nie udało się zahaszować hasła.".into()))?;
    let unavailable = |_: gomoku_db::DbError| OperatorError("Baza danych jest niedostępna.".into());

    let existing = db.find_user_by_email(&email).await.map_err(unavailable)?;
    let mut name = requested_name;
    if let Some(account) = &existing {
        if name.as_deref().is_some_and(|n| n != account.display_name) {
            stderr.push_str(&format!(
                "Uwaga: konto zostaje przy nazwie „{}”, --name zignorowano.\n",
                account.display_name
            ));
        }
        if !parsed.yes {
            if !prompter.interactive() {
                return Err(OperatorError(
                    "Konto już istnieje; bez terminala potwierdź przejęcie flagą --yes.".into(),
                ));
            }
            let answer = prompter.ask(
                &format!(
                    "Konto „{}” już istnieje. Przejąć je (nowe hasło, wylogowanie wszystkich sesji)? [t/N] ",
                    account.display_name
                ),
                false,
            );
            let accepted = answer
                .as_deref()
                .map(str::trim)
                .is_some_and(|a| ["t", "tak", "y", "yes"].contains(&a.to_lowercase().as_str()));
            if !accepted {
                return Err(OperatorError("Anulowano, nic nie zmieniono.".into()));
            }
        }
    } else if name.is_none() {
        if !prompter.interactive() {
            return Err(OperatorError("Podaj nazwę gracza flagą --name.".into()));
        }
        let answer = prompter
            .ask("Nazwa gracza: ", false)
            .ok_or_else(|| OperatorError("Brak nazwy gracza na wejściu.".into()))?;
        name = Some(parse_name(&answer)?);
    }

    let key = name.as_deref().map(normalize_display_name);
    let display = name.as_deref().zip(key.as_deref());
    match db
        .provision_admin(&email, display, &password_hash, now)
        .await
    {
        Ok(result) => {
            let action = match result.action {
                ProvisionAction::Created => "created",
                ProvisionAction::TakenOver => "taken_over",
            };
            Ok(serde_json::json!({
                "ok": true, "action": action, "userId": result.user_id, "revokedSessions": result.revoked_sessions,
            })
            .to_string())
        }
        Err(ProvisionError::NameRequired) => {
            Err(OperatorError("Podaj nazwę gracza flagą --name.".into()))
        }
        Err(ProvisionError::NameTaken) => {
            Err(OperatorError("Ta nazwa gracza jest już zajęta.".into()))
        }
        Err(ProvisionError::Db(error)) => Err(unavailable(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|a| (*a).to_owned()).collect()
    }

    #[test]
    fn parses_the_flags_and_refuses_typos() {
        assert_eq!(parse_create_args(&[]), Ok(CreateArgs::default()));
        assert_eq!(
            parse_create_args(&args(&["--yes", "--name", "Boss"])),
            Ok(CreateArgs {
                name: Some("Boss".into()),
                yes: true
            })
        );
        assert_eq!(
            parse_create_args(&args(&["--name=Boss"]))
                .unwrap()
                .name
                .as_deref(),
            Some("Boss")
        );
        assert_eq!(
            parse_create_args(&args(&["--name"])),
            Err("Flaga --name wymaga wartości.".into())
        );
        assert_eq!(
            parse_create_args(&args(&["--yess"])),
            Err("Nieznany argument: --yess.".into())
        );
    }

    #[test]
    fn describes_the_target_without_credentials() {
        assert_eq!(
            describe_database_target(Some(
                "postgresql://gomoku:secret@db.example:5432/gomoku?sslmode=x"
            )),
            "db.example:5432/gomoku"
        );
        assert_eq!(
            describe_database_target(Some("postgres://localhost/app")),
            "localhost/app"
        );
        assert_eq!(
            describe_database_target(None),
            "nieustawiona (DATABASE_URL)"
        );
        assert_eq!(
            describe_database_target(Some("")),
            "nieustawiona (DATABASE_URL)"
        );
        assert_eq!(
            describe_database_target(Some("not a url")),
            "nieczytelna (DATABASE_URL)"
        );
        assert_eq!(
            describe_database_target(Some("postgres://")),
            "nieczytelna (DATABASE_URL)"
        );
    }
}
