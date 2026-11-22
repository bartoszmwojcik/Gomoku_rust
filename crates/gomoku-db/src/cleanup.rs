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
/// Rows of `invitations` go with their game (`ON DELETE CASCADE`), so the games rules cover them.
pub const EXEMPT_TABLES: [&str; 1] = ["invitations"];

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
    /// Instead of deleting the matching rows, sets this column to `NULL` (the condition must stop matching once it is
    /// cleared, or the rule never ends).
    pub clear_column: Option<&'static str>,
}

/// The moment the running clock of an active game runs out (or a game nobody opened is due), for the row `$g` of `games`.
/// A paper game takes the seat on turn from its own counters: in the automat phase the opponent of the automat's seat.
macro_rules! game_deadline_sql {
    ($g:literal) => {
        concat!(
            "COALESCE(",
            $g,
            ".turn_started_at + (CASE WHEN (CASE WHEN ",
            $g,
            ".variant = 'paper' THEN \
               (SELECT CASE WHEN s.auto_seat IS NOT NULL THEN 3 - s.auto_seat \
                            WHEN s.move_count % 2 = 0 THEN ",
            $g,
            ".black_seat ELSE 3 - ",
            $g,
            ".black_seat END \
                FROM paper_game_states s WHERE s.game_id = ",
            $g,
            ".id) \
              ELSE CASE WHEN ",
            $g,
            ".move_count % 2 = 0 THEN ",
            $g,
            ".black_seat ELSE 3 - ",
            $g,
            ".black_seat END END) = 1 \
             THEN ",
            $g,
            ".clock_ms_1 ELSE ",
            $g,
            ".clock_ms_2 END) * interval '1 millisecond', \
             ",
            $g,
            ".started_at + 900000 * interval '1 millisecond')"
        )
    };
}

pub const RULES: [Rule; 15] = [
    Rule {
        // 1 h margin: a session that expired mid-game stays until the game ends (games last well under an hour).
        key: "sessions",
        table: "sessions",
        primary_key: "id",
        condition: "expires_at < $1",
        grace_ms: HOUR_MS,
        protects_email: false,
        clear_column: None,
    },
    Rule {
        key: "rateLimitBuckets",
        table: "rate_limit_buckets",
        primary_key: "key",
        condition: "window_start < $1",
        grace_ms: LONGEST_RATE_LIMIT_WINDOW_MS + 24 * HOUR_MS,
        protects_email: false,
        clear_column: None,
    },
    Rule {
        key: "emailVerificationTokens",
        table: "email_verification_tokens",
        primary_key: "id",
        condition: "expires_at < $1 OR used_at < $1",
        grace_ms: DAY_MS,
        protects_email: false,
        clear_column: None,
    },
    Rule {
        key: "passwordResetTokens",
        table: "password_reset_tokens",
        primary_key: "id",
        condition: "expires_at < $1 OR used_at < $1",
        grace_ms: DAY_MS,
        protects_email: false,
        clear_column: None,
    },
    Rule {
        key: "emailChangeTokens",
        table: "email_change_tokens",
        primary_key: "id",
        condition: "expires_at < $1 OR used_at < $1",
        grace_ms: DAY_MS,
        protects_email: false,
        clear_column: None,
    },
    Rule {
        // "Looking for a game" statuses live 5 minutes and reads filter on `expires_at`, so a dead row never matters for
        // correctness; the 1 h margin only absorbs clock skew between the application and the database.
        key: "lobbySeekers",
        table: "lobby_seekers",
        primary_key: "id",
        condition: "expires_at < $1",
        grace_ms: HOUR_MS,
        protects_email: false,
        clear_column: None,
    },
    Rule {
        // The chat is temporary: it goes 1 h after the game ended (the read hides it at exactly 1 h anyway), 1 h after an
        // unjoined or cancelled game, and 1 h after the clock of an abandoned active game ran out. It is swept before
        // the games rules so that a game batch never carries a heavy cascade; games that stay as history lose it too.
        key: "chatMessages",
        table: "chat_messages",
        primary_key: "id",
        condition: concat!(
            "EXISTS (SELECT 1 FROM games g WHERE g.id = chat_messages.game_id AND ( \
                      (g.status = 'finished' AND g.finished_at < $1) \
                      OR (g.status = 'cancelled' AND g.created_at < $1) \
                      OR (g.status = 'active' AND ",
            game_deadline_sql!("g"),
            " < $1)))"
        ),
        grace_ms: HOUR_MS,
        protects_email: false,
        clear_column: None,
    },
    Rule {
        // Spectator access lives 1 h from the last read and reads filter on `expires_at`; the margin absorbs clock skew.
        key: "gameSpectators",
        table: "game_spectators",
        primary_key: "id",
        condition: "expires_at < $1",
        grace_ms: HOUR_MS,
        protects_email: false,
        clear_column: None,
    },
    Rule {
        // An unjoined game whose invitation expired: kept for 1 h after the expiry, because the guest creator's token works
        // that long and shows "invitation expired". Players and the invitation go with the game (cascade).
        key: "waitingGames",
        table: "games",
        primary_key: "id",
        condition: "status = 'waiting' AND EXISTS (SELECT 1 FROM invitations i WHERE i.game_id = games.id AND i.expires_at < $1)",
        grace_ms: HOUR_MS,
        protects_email: false,
        clear_column: None,
    },
    Rule {
        // Cancelled games: 24 h after creation, plus the 1 h window of an unjoined one.
        key: "cancelledGames",
        table: "games",
        primary_key: "id",
        condition: "status = 'cancelled' AND created_at < $1",
        grace_ms: 25 * HOUR_MS,
        protects_email: false,
        clear_column: None,
    },
    Rule {
        // Dead active games of guests only (nobody has a history of them): removed 1 h after the clock ran out, without
        // writing a result, so a returning player can still open the game and see the lazy loss on time.
        key: "guestOnlyActiveGames",
        table: "games",
        primary_key: "id",
        condition: concat!(
            "status = 'active' AND NOT EXISTS (SELECT 1 FROM game_players p WHERE p.game_id = games.id AND p.kind NOT IN ('guest', 'bot')) \
                    AND ",
            game_deadline_sql!("games"),
            " < $1"
        ),
        grace_ms: HOUR_MS,
        protects_email: false,
        clear_column: None,
    },
    Rule {
        // Finished games of guests only: removed 1 h after the end (the guest token window). `kind`, not `user_id IS NULL`:
        // a deleted account has no user but stays `account`, which protects its opponent's history.
        key: "guestOnlyFinishedGames",
        table: "games",
        primary_key: "id",
        condition: "status = 'finished' AND NOT EXISTS (SELECT 1 FROM game_players p WHERE p.game_id = games.id AND p.kind NOT IN ('guest', 'bot')) \
                    AND finished_at < $1",
        grace_ms: HOUR_MS,
        protects_email: false,
        clear_column: None,
    },
    Rule {
        // Finished games with no living account (every account deleted): nobody has a history of them.
        key: "orphanedFinishedGames",
        table: "games",
        primary_key: "id",
        condition: "status = 'finished' AND finished_at < $1 \
                    AND NOT EXISTS (SELECT 1 FROM game_players p WHERE p.game_id = games.id AND p.user_id IS NOT NULL)",
        grace_ms: HOUR_MS,
        protects_email: false,
        clear_column: None,
    },
    Rule {
        // A guest's token dies 1 h after the game finished: the hash is cleared (the rows stay for the opponent's history).
        key: "guestTokens",
        table: "game_players",
        primary_key: "id",
        condition: "guest_token_hash IS NOT NULL AND game_id IN (SELECT id FROM games WHERE finished_at < $1)",
        grace_ms: HOUR_MS,
        protects_email: false,
        clear_column: Some("guest_token_hash"),
    },
    Rule {
        // Unconfirmed accounts older than 14 days. Last on purpose: by then the sweep of the same run is done. The
        // "without any game" condition arrives with the games table.
        key: "unverifiedUsers",
        table: "users",
        primary_key: "id",
        condition: "email_verified_at IS NULL AND created_at < $1 \
                    AND NOT EXISTS (SELECT 1 FROM game_players gp WHERE gp.user_id = users.id)",
        grace_ms: 14 * DAY_MS,
        protects_email: true,
        clear_column: None,
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
    pub lobby_seekers: u64,
    pub chat_messages: u64,
    pub game_spectators: u64,
    pub waiting_games: u64,
    pub cancelled_games: u64,
    pub guest_only_active_games: u64,
    pub guest_only_finished_games: u64,
    pub orphaned_finished_games: u64,
    pub guest_tokens: u64,
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
            "lobbySeekers" => &mut self.lobby_seekers,
            "chatMessages" => &mut self.chat_messages,
            "gameSpectators" => &mut self.game_spectators,
            "waitingGames" => &mut self.waiting_games,
            "cancelledGames" => &mut self.cancelled_games,
            "guestOnlyActiveGames" => &mut self.guest_only_active_games,
            "guestOnlyFinishedGames" => &mut self.guest_only_finished_games,
            "orphanedFinishedGames" => &mut self.orphaned_finished_games,
            "guestTokens" => &mut self.guest_tokens,
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
    let action = match rule.clear_column {
        Some(column) => format!("UPDATE {} SET {column} = NULL", rule.table),
        None => format!("DELETE FROM {}", rule.table),
    };
    format!(
        "{action} WHERE {pk} IN (SELECT {pk} FROM {table} WHERE ({condition}){protection} \
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
