//! Cleanup of expired rows: short autocommit batches until a rule runs out of rows or its time budget passes.
//!
//! Every table with an expiry column needs a rule here or an entry in `EXEMPT_TABLES` (a test checks the
//! schema). Each new expiring table adds its rule together with its migration.

use std::time::{Duration, Instant};

use serde::Serialize;
use time::OffsetDateTime;

use crate::Database;

pub const DEFAULT_BATCH_SIZE: i64 = 1000;
/// Time budget of one rule; a slow rule cannot starve the later ones.
pub const DEFAULT_RULE_TIME_LIMIT: Duration = Duration::from_secs(15);

const HOUR_MS: i64 = 60 * 60 * 1000;
const DAY_MS: i64 = 24 * HOUR_MS;
/// The longest rate-limit window the application uses: the daily e-mail pools. Counters are kept 24 h past it,
/// because the limiter measures time with the database clock and the cleanup with the application clock.
pub const LONGEST_RATE_LIMIT_WINDOW_MS: i64 = 24 * HOUR_MS;

/// Tables with an expiry-like column that deliberately have no rule (none yet).
pub const EXEMPT_TABLES: [&str; 0] = [];

/// One cleanup rule. `condition` and the names are static text; the cutoff is the bound `$1`, the batch size `$2`.
#[derive(Debug, Clone, Copy)]
pub struct Rule {
    /// Key of the result field.
    pub key: &'static str,
    pub table: &'static str,
    pub primary_key: &'static str,
    pub condition: &'static str,
    /// Rows matching the condition for cutoff = `now - grace_ms` are deleted.
    pub grace_ms: i64,
    /// The rule skips the account of the protected e-mail (the installation administrator) when one is given.
    pub protects_email: bool,
}

pub const RULES: [Rule; 6] = [
    Rule {
        // 1 h margin: a session that expired mid-game stays until the game ends (games last well under an hour).
        key: "sessions",
        table: "sessions",
        primary_key: "id",
        condition: "expires_at < $1",
        grace_ms: HOUR_MS,
        protects_email: false,
    },
    Rule {
        key: "rateLimitBuckets",
        table: "rate_limit_buckets",
        primary_key: "key",
        condition: "window_start < $1",
        grace_ms: LONGEST_RATE_LIMIT_WINDOW_MS + 24 * HOUR_MS,
        protects_email: false,
    },
    Rule {
        key: "emailVerificationTokens",
        table: "email_verification_tokens",
        primary_key: "id",
        condition: "expires_at < $1 OR used_at < $1",
        grace_ms: DAY_MS,
        protects_email: false,
    },
    Rule {
        key: "passwordResetTokens",
        table: "password_reset_tokens",
        primary_key: "id",
        condition: "expires_at < $1 OR used_at < $1",
        grace_ms: DAY_MS,
        protects_email: false,
    },
    Rule {
        key: "emailChangeTokens",
        table: "email_change_tokens",
        primary_key: "id",
        condition: "expires_at < $1 OR used_at < $1",
        grace_ms: DAY_MS,
        protects_email: false,
    },
    Rule {
        // Unconfirmed accounts older than 14 days. Last on purpose: by then the sweep of the same run is done. The
        // "without any game" condition arrives with the games table.
        key: "unverifiedUsers",
        table: "users",
        primary_key: "id",
        condition: "email_verified_at IS NULL AND created_at < $1",
        grace_ms: 14 * DAY_MS,
        protects_email: true,
    },
];

/// A rule that failed: its key and the SQL state code only, never a message (it may carry values).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuleError {
    pub rule: String,
    pub code: Option<String>,
}

/// Deleted rows per rule (rows removed before a failure included), the failed rules and whether a time budget
/// stopped a rule early (the next run continues).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupResult {
    pub sessions: u64,
    pub rate_limit_buckets: u64,
    pub email_verification_tokens: u64,
    pub password_reset_tokens: u64,
    pub email_change_tokens: u64,
    pub unverified_users: u64,
    pub errors: Vec<RuleError>,
    pub truncated: bool,
}

impl CleanupResult {
    fn slot(&mut self, key: &str) -> Option<&mut u64> {
        Some(match key {
            "sessions" => &mut self.sessions,
            "rateLimitBuckets" => &mut self.rate_limit_buckets,
            "emailVerificationTokens" => &mut self.email_verification_tokens,
            "passwordResetTokens" => &mut self.password_reset_tokens,
            "emailChangeTokens" => &mut self.email_change_tokens,
            "unverifiedUsers" => &mut self.unverified_users,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone)]
pub struct CleanupOptions {
    pub now: OffsetDateTime,
    pub batch_size: i64,
    pub rule_time_limit: Duration,
    /// Normalized e-mail of an account no rule may delete.
    pub protected_email: Option<String>,
}

impl CleanupOptions {
    pub fn new(now: OffsetDateTime) -> Self {
        Self {
            now,
            batch_size: DEFAULT_BATCH_SIZE,
            rule_time_limit: DEFAULT_RULE_TIME_LIMIT,
            protected_email: None,
        }
    }
}

/// The batch statement of a rule. Identifiers and conditions come from the static rules, never from callers;
/// values are bound parameters.
fn batch_sql(rule: &Rule, protected: bool) -> String {
    let protection = if protected && rule.protects_email {
        " AND email <> $3"
    } else {
        ""
    };
    format!(
        "DELETE FROM {table} WHERE {pk} IN (SELECT {pk} FROM {table} WHERE ({condition}){protection} \
         LIMIT $2 FOR UPDATE SKIP LOCKED)",
        table = rule.table,
        pk = rule.primary_key,
        condition = rule.condition,
    )
}

fn error_code(error: &sqlx::Error) -> Option<String> {
    error
        .as_database_error()
        .and_then(|db| db.code().map(|c| c.into_owned()))
}

async fn run_rule(
    db: &Database,
    rule: &Rule,
    options: &CleanupOptions,
    deleted: &mut u64,
) -> Result<bool, sqlx::Error> {
    let cutoff = options.now - time::Duration::milliseconds(rule.grace_ms);
    let protected = options
        .protected_email
        .as_deref()
        .filter(|_| rule.protects_email);
    let sql = batch_sql(rule, protected.is_some());
    let started = Instant::now();
    loop {
        // SAFETY of the text: the table, key and condition are static; the cutoff, limit and e-mail are bound.
        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql.clone()))
            .bind(cutoff)
            .bind(options.batch_size);
        if let Some(email) = protected {
            query = query.bind(email);
        }
        let done = query.execute(db.pool()).await?.rows_affected();
        *deleted += done;
        if i64::try_from(done).unwrap_or(i64::MAX) < options.batch_size {
            return Ok(false);
        }
        if started.elapsed() >= options.rule_time_limit {
            return Ok(true);
        }
    }
}

/// Deletes expired and used rows. An error in one rule is recorded in `errors` and the following rules still run.
pub async fn cleanup_expired(db: &Database, options: &CleanupOptions) -> CleanupResult {
    let mut result = CleanupResult::default();
    for rule in &RULES {
        let mut deleted = 0;
        match run_rule(db, rule, options, &mut deleted).await {
            Ok(truncated) => result.truncated |= truncated,
            Err(error) => result.errors.push(RuleError {
                rule: rule.key.to_owned(),
                code: error_code(&error),
            }),
        }
        if let Some(slot) = result.slot(rule.key) {
            *slot = deleted;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_worst_case_of_a_run_under_the_hourly_cron_interval() {
        assert!(
            DEFAULT_RULE_TIME_LIMIT * u32::try_from(RULES.len()).unwrap()
                < Duration::from_secs(3600)
        );
    }

    #[test]
    fn every_rule_has_a_slot_in_the_result_and_the_batch_statement_is_static_text() {
        let mut result = CleanupResult::default();
        for rule in &RULES {
            assert!(result.slot(rule.key).is_some(), "{}", rule.key);
            let sql = batch_sql(rule, true);
            assert!(sql.contains("FOR UPDATE SKIP LOCKED") && sql.contains("LIMIT $2"));
            assert_eq!(sql.contains("email <> $3"), rule.protects_email);
            assert!(!batch_sql(rule, false).contains("$3"));
        }
        assert!(result.slot("nope").is_none());
    }

    #[test]
    fn the_unconfirmed_accounts_rule_runs_last_and_the_counter_window_has_a_margin() {
        assert_eq!(RULES.last().map(|r| r.key), Some("unverifiedUsers"));
        let buckets = RULES.iter().find(|r| r.key == "rateLimitBuckets").unwrap();
        assert!(buckets.grace_ms >= 2 * LONGEST_RATE_LIMIT_WINDOW_MS);
    }
}
