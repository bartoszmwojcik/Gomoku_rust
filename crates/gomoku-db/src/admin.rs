//! Reads for the read-only administrator panel. Nothing here writes.

use gomoku_core::admin::{
    ADMIN_PAGE_SIZE, AccountsQuery, SortDir, SortKey, like_prefix, search_key,
};
use gomoku_core::finished_stats::{FinishedGameStats, PairRow, build_finished_game_stats};
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{Database, DbError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountTotals {
    pub total: i64,
    pub verified: i64,
}

/// Games in the database now (shows excluded), by status, by origin and by reason of the end.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GameCounters {
    pub waiting: i64,
    pub active: i64,
    pub finished: i64,
    pub cancelled: i64,
    pub invite: i64,
    pub lobby: i64,
    pub rematch: i64,
    pub bot: i64,
    pub five_in_row: i64,
    pub draw: i64,
    pub timeout: i64,
    pub resignation: i64,
    pub points: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountRow {
    pub id: Uuid,
    pub email: String,
    pub display_name: String,
    pub verified: bool,
    pub created_at: OffsetDateTime,
    pub finished_games: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountPage {
    pub total: i64,
    /// The page actually shown (cut to the last one).
    pub page: i64,
    pub page_count: i64,
    pub rows: Vec<AccountRow>,
}

impl Database {
    pub async fn admin_account_totals(&self) -> Result<AccountTotals, DbError> {
        let row = sqlx::query(
            "SELECT count(*)::int8 AS total, count(*) FILTER (WHERE email_verified_at IS NOT NULL)::int8 AS verified FROM users",
        )
        .fetch_one(self.pool())
        .await?;
        Ok(AccountTotals {
            total: row.try_get("total")?,
            verified: row.try_get("verified")?,
        })
    }

    pub async fn admin_game_counters(&self) -> Result<GameCounters, DbError> {
        let row = sqlx::query(
            "SELECT \
               count(*) FILTER (WHERE status = 'waiting')::int8 AS waiting, \
               count(*) FILTER (WHERE status = 'active')::int8 AS active, \
               count(*) FILTER (WHERE status = 'finished')::int8 AS finished, \
               count(*) FILTER (WHERE status = 'cancelled')::int8 AS cancelled, \
               count(*) FILTER (WHERE status = 'finished' AND origin = 'invite')::int8 AS invite, \
               count(*) FILTER (WHERE status = 'finished' AND origin = 'lobby')::int8 AS lobby, \
               count(*) FILTER (WHERE status = 'finished' AND origin = 'rematch')::int8 AS rematch, \
               count(*) FILTER (WHERE status = 'finished' AND origin = 'bot')::int8 AS bot, \
               count(*) FILTER (WHERE status = 'finished' AND end_reason = 'five_in_row')::int8 AS five_in_row, \
               count(*) FILTER (WHERE status = 'finished' AND end_reason = 'draw')::int8 AS draw, \
               count(*) FILTER (WHERE status = 'finished' AND end_reason = 'timeout')::int8 AS timeout, \
               count(*) FILTER (WHERE status = 'finished' AND end_reason = 'resignation')::int8 AS resignation, \
               count(*) FILTER (WHERE status = 'finished' AND end_reason = 'points')::int8 AS points \
             FROM games WHERE origin <> 'show'",
        )
        .fetch_one(self.pool())
        .await?;
        Ok(GameCounters {
            waiting: row.try_get("waiting")?,
            active: row.try_get("active")?,
            finished: row.try_get("finished")?,
            cancelled: row.try_get("cancelled")?,
            invite: row.try_get("invite")?,
            lobby: row.try_get("lobby")?,
            rematch: row.try_get("rematch")?,
            bot: row.try_get("bot")?,
            five_in_row: row.try_get("five_in_row")?,
            draw: row.try_get("draw")?,
            timeout: row.try_get("timeout")?,
            resignation: row.try_get("resignation")?,
            points: row.try_get("points")?,
        })
    }

    /// The persistent pair counters: total, today and the last seven days (Warsaw calendar), shows never counted.
    pub async fn admin_finished_stats(
        &self,
        now: OffsetDateTime,
    ) -> Result<FinishedGameStats, DbError> {
        let rows = sqlx::query(
            "SELECT c.kind_a, c.kind_b, sum(c.finished_count)::int8 AS total, \
               coalesce(sum(c.finished_count) FILTER (WHERE c.day = t.today), 0)::int8 AS today, \
               coalesce(sum(c.finished_count) FILTER (WHERE c.day BETWEEN t.today - 6 AND t.today), 0)::int8 AS last7_days \
             FROM game_pair_counters c \
             CROSS JOIN (SELECT ($1::timestamptz AT TIME ZONE 'Europe/Warsaw')::date AS today) t \
             GROUP BY c.kind_a, c.kind_b",
        )
        .bind(now)
        .fetch_all(self.pool())
        .await?;
        let pairs: Vec<PairRow> = rows
            .iter()
            .map(|r| {
                Ok::<_, sqlx::Error>(PairRow {
                    kind_a: r.try_get("kind_a")?,
                    kind_b: r.try_get("kind_b")?,
                    total: r.try_get("total")?,
                    today: r.try_get("today")?,
                    last_7_days: r.try_get("last7_days")?,
                })
            })
            .collect::<Result<_, _>>()?;
        Ok(build_finished_game_stats(&pairs))
    }

    /// One page of accounts, searched by a display-name prefix (never by e-mail), with the count of finished games of
    /// the accounts on the page.
    pub async fn admin_account_page(&self, query: &AccountsQuery) -> Result<AccountPage, DbError> {
        let pattern = search_key(&query.q).map(|key| like_prefix(&key));
        let filter = if pattern.is_some() {
            " WHERE display_name_key LIKE $1 ESCAPE '\\'"
        } else {
            ""
        };
        let count_sql = format!("SELECT count(*)::int8 AS total FROM users{filter}");
        let count_query = sqlx::query(sqlx::AssertSqlSafe(count_sql));
        let count_query = match &pattern {
            Some(p) => count_query.bind(p),
            None => count_query,
        };
        let total: i64 = count_query.fetch_one(self.pool()).await?.try_get("total")?;
        let page_count = ((total + ADMIN_PAGE_SIZE - 1) / ADMIN_PAGE_SIZE).max(1);
        let page = query.page.min(page_count);
        if total == 0 {
            return Ok(AccountPage {
                total,
                page,
                page_count,
                rows: Vec::new(),
            });
        }
        let direction = match query.dir {
            SortDir::Asc => "ASC",
            SortDir::Desc => "DESC",
        };
        let key = match query.sort {
            SortKey::Created => "created_at",
            SortKey::Name => "display_name_key",
        };
        let offset = (page - 1) * ADMIN_PAGE_SIZE;
        let limit_placeholder = if pattern.is_some() {
            ("$2", "$3")
        } else {
            ("$1", "$2")
        };
        let sql = format!(
            "SELECT id, email, display_name, email_verified_at IS NOT NULL AS verified, created_at FROM users{filter} \
             ORDER BY {key} {direction}, id {direction} LIMIT {} OFFSET {}",
            limit_placeholder.0, limit_placeholder.1
        );
        let rows_query = sqlx::query(sqlx::AssertSqlSafe(sql));
        let rows_query = match &pattern {
            Some(p) => rows_query.bind(p),
            None => rows_query,
        };
        let users = rows_query
            .bind(ADMIN_PAGE_SIZE)
            .bind(offset)
            .fetch_all(self.pool())
            .await?;
        let ids: Vec<Uuid> = users
            .iter()
            .map(|r| r.try_get("id"))
            .collect::<Result<_, _>>()?;
        let finished = sqlx::query(
            "SELECT gp.user_id, count(*)::int8 AS finished FROM game_players gp JOIN games g ON g.id = gp.game_id \
             WHERE gp.user_id = ANY($1) AND g.status = 'finished' GROUP BY gp.user_id",
        )
        .bind(&ids)
        .fetch_all(self.pool())
        .await?;
        let mut counts = std::collections::HashMap::new();
        for row in &finished {
            let id: Uuid = row.try_get("user_id")?;
            counts.insert(id, row.try_get::<i64, _>("finished")?);
        }
        let rows = users
            .iter()
            .map(|r| {
                let id: Uuid = r.try_get("id")?;
                Ok::<_, sqlx::Error>(AccountRow {
                    id,
                    email: r.try_get("email")?,
                    display_name: r.try_get("display_name")?,
                    verified: r.try_get("verified")?,
                    created_at: r.try_get("created_at")?,
                    finished_games: counts.get(&id).copied().unwrap_or(0),
                })
            })
            .collect::<Result<_, _>>()?;
        Ok(AccountPage {
            total,
            page,
            page_count,
            rows,
        })
    }
}
