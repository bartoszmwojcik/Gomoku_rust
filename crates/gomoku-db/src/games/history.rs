//! A player's history and the review of a finished game.

use gomoku_core::history::HISTORY_PAGE_SIZE;
use gomoku_core::types::{EndReason, GameVariant, Seat};
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

use super::{GAME_COLUMNS, GameError, GameRow, PLAYER_SELECT, PlayerRow};
use crate::Database;

/// One finished game in the history of an account. Never an e-mail or an account id of the opponent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntryRow {
    pub game_id: Uuid,
    /// ISO 8601 UTC with microseconds, formatted by the database: a client-side date would lose them and the next page
    /// would repeat or skip a row.
    pub finished_at: String,
    pub reason: EndReason,
    pub winner: Option<Seat>,
    pub my_seat: Seat,
    pub opponent_kind: String,
    pub opponent_seat: Seat,
    /// `None` for a deleted account; a guest's name is derived by the caller when this is `None`.
    pub opponent_name: Option<String>,
    pub opponent_nickname: Option<String>,
    pub variant: GameVariant,
    /// `[mine, theirs]` for a paper game (from the cache the game keeps), `None` for a classic one.
    pub scores: Option<[u32; 2]>,
}

#[derive(Debug, Clone)]
pub struct HistoryPageRows {
    pub entries: Vec<HistoryEntryRow>,
    /// The keyset cursor of the last returned row when more exist.
    pub next: Option<(String, Uuid)>,
}

/// A finished game for the review screen.
#[derive(Debug, Clone)]
pub struct ReviewRows {
    pub game: GameRow,
    pub players: Vec<PlayerRow>,
    /// `None` for a reader who came with the code and did not play.
    pub viewer_seat: Option<Seat>,
}

fn seat(value: i16) -> Result<Seat, GameError> {
    u8::try_from(value)
        .ok()
        .and_then(Seat::from_number)
        .ok_or(GameError::Corrupt)
}

impl Database {
    /// The account's finished games, newest first, 20 per page (keyset on `(finished_at, id)`). Games lost on time that
    /// nobody opened are settled first, so they appear.
    pub async fn history_page(
        &self,
        user_id: Uuid,
        cursor: Option<(&str, Uuid)>,
        now: OffsetDateTime,
    ) -> Result<HistoryPageRows, GameError> {
        self.settle_for(user_id, now).await?;
        let sql = format!(
            "SELECT g.id, to_char(g.finished_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS finished_at, \
                    g.end_reason, g.winner_seat, me.seat AS my_seat, op.kind AS opponent_kind, op.seat AS opponent_seat, \
                    op.nickname AS opponent_nickname, COALESCE(op.display_name, u.display_name) AS opponent_name, g.variant, ps.score_1, ps.score_2 \
             FROM game_players me JOIN games g ON g.id = me.game_id AND g.status = 'finished' \
             JOIN game_players op ON op.game_id = g.id AND op.seat <> me.seat LEFT JOIN users u ON u.id = op.user_id \
             LEFT JOIN paper_game_states ps ON ps.game_id = g.id \
             WHERE me.user_id = $1 {} ORDER BY g.finished_at DESC, g.id DESC LIMIT {}",
            if cursor.is_some() {
                "AND (g.finished_at, g.id) < ($2::timestamptz, $3::uuid)"
            } else {
                ""
            },
            HISTORY_PAGE_SIZE + 1
        );
        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql)).bind(user_id);
        if let Some((finished_at, id)) = cursor {
            query = query.bind(finished_at).bind(id);
        }
        let rows = query.fetch_all(self.pool()).await?;
        let more = rows.len() > HISTORY_PAGE_SIZE;
        let mut entries = Vec::new();
        for row in rows.iter().take(HISTORY_PAGE_SIZE) {
            let reason: String = row.try_get("end_reason")?;
            let winner: Option<i16> = row.try_get("winner_seat")?;
            let my_seat = seat(row.try_get("my_seat")?)?;
            let variant = GameVariant::parse(&row.try_get::<String, _>("variant")?)
                .ok_or(GameError::Corrupt)?;
            let scores = match (
                variant,
                row.try_get::<Option<i32>, _>("score_1")?,
                row.try_get::<Option<i32>, _>("score_2")?,
            ) {
                (GameVariant::Paper, Some(one), Some(two)) => {
                    let (one, two) = (
                        u32::try_from(one).unwrap_or(0),
                        u32::try_from(two).unwrap_or(0),
                    );
                    Some(if my_seat == Seat::One {
                        [one, two]
                    } else {
                        [two, one]
                    })
                }
                _ => None,
            };
            entries.push(HistoryEntryRow {
                variant,
                scores,
                game_id: row.try_get("id")?,
                finished_at: row.try_get("finished_at")?,
                reason: EndReason::parse(&reason).ok_or(GameError::Corrupt)?,
                winner: winner.map(seat).transpose()?,
                my_seat,
                opponent_kind: row.try_get("opponent_kind")?,
                opponent_seat: seat(row.try_get("opponent_seat")?)?,
                opponent_name: row.try_get("opponent_name")?,
                opponent_nickname: row.try_get("opponent_nickname")?,
            });
        }
        let next = if more {
            entries.last().map(|e| (e.finished_at.clone(), e.game_id))
        } else {
            None
        };
        Ok(HistoryPageRows { entries, next })
    }

    /// A finished game for one of its account players, or `None` for everything else (missing, not finished, somebody
    /// else's, cleaned up): the same answer, so it is no oracle.
    pub async fn open_review(
        &self,
        user_id: Uuid,
        game_id: Uuid,
    ) -> Result<Option<ReviewRows>, GameError> {
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {GAME_COLUMNS} FROM games WHERE id = $1 AND status = 'finished'"
        )))
        .bind(game_id)
        .fetch_optional(self.pool())
        .await?;
        let Some(row) = row else { return Ok(None) };
        let game = GameRow::from_row(&row)?;
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "{PLAYER_SELECT} WHERE p.game_id = $1 ORDER BY p.seat"
        )))
        .bind(game_id)
        .fetch_all(self.pool())
        .await?;
        let players = rows
            .iter()
            .map(PlayerRow::from_row)
            .collect::<Result<Vec<_>, _>>()?;
        let viewer = players
            .iter()
            .find(|p| p.kind == "account" && p.user_id == Some(user_id));
        Ok(viewer.map(|v| ReviewRows {
            viewer_seat: Some(v.seat),
            game,
            players: players.clone(),
        }))
    }

    /// A finished game of a public audience for a reader with the invitation code: the game must still be public and the
    /// code must be this game's code. The reader writes nothing and has no seat.
    pub async fn open_review_by_code(
        &self,
        game_id: Uuid,
        code: &str,
    ) -> Result<Option<ReviewRows>, GameError> {
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {GAME_COLUMNS} FROM games g WHERE id = $1 AND status = 'finished' AND is_public \
             AND EXISTS (SELECT 1 FROM invitations i WHERE i.game_id = g.id AND i.code = $2)"
        )))
        .bind(game_id)
        .bind(code)
        .fetch_optional(self.pool())
        .await?;
        let Some(row) = row else { return Ok(None) };
        let game = GameRow::from_row(&row)?;
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "{PLAYER_SELECT} WHERE p.game_id = $1 ORDER BY p.seat"
        )))
        .bind(game_id)
        .fetch_all(self.pool())
        .await?;
        let players = rows
            .iter()
            .map(PlayerRow::from_row)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some(ReviewRows {
            viewer_seat: None,
            game,
            players,
        }))
    }
}
