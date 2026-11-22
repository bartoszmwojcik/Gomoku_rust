//! The lobby: accounts mark themselves as looking for a game and others pick them.

use gomoku_core::paper::GameSettings;
use gomoku_core::types::{GameVariant, Seat};
use sqlx::Row;
use sqlx::postgres::PgRow;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use super::paper::insert_paper_state;
use super::{ACTIVE_SLOT_INDEX, CurrentGame, GameError, Scope, close_stale, unique_violation};
use crate::Database;

/// An account's own "looking for a game" status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeekingStatus {
    pub id: Uuid,
    pub created_at: OffsetDateTime,
    pub expires_in_ms: i32,
    pub settings: GameSettings,
}

/// Another seeker as the list shows them: never an e-mail or an account id. `id` is the handle a pick uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeekerRow {
    pub id: Uuid,
    pub display_name: String,
    pub since_ms: i32,
    pub expires_in_ms: i32,
    pub settings: GameSettings,
}

#[derive(Debug, Clone)]
pub struct LobbyRows {
    pub me: Option<SeekingStatus>,
    /// The game that holds the account's slot, so the client can send them back to it.
    pub current_game: Option<CurrentGame>,
    pub seekers: Vec<SeekerRow>,
}

fn settings_of(row: &PgRow) -> Result<GameSettings, sqlx::Error> {
    Ok(GameSettings {
        variant: GameVariant::parse(&row.try_get::<String, _>("variant")?)
            .ok_or_else(|| sqlx::Error::Decode("unknown variant".into()))?,
        time_control_ms: i64::from(row.try_get::<i32, _>("time_control_ms")?),
    })
}

async fn own_status(
    db: &Database,
    user_id: Uuid,
    now: OffsetDateTime,
) -> Result<Option<SeekingStatus>, GameError> {
    let row = sqlx::query(
        "SELECT id, created_at, variant, time_control_ms, (extract(epoch FROM (expires_at - $2::timestamptz)) * 1000)::int AS expires_in_ms \
         FROM lobby_seekers WHERE user_id = $1 AND expires_at > $2",
    )
    .bind(user_id)
    .bind(now)
    .fetch_optional(db.pool())
    .await?;
    row.map(|r| -> Result<SeekingStatus, sqlx::Error> {
        Ok(SeekingStatus {
            id: r.try_get("id")?,
            created_at: r.try_get("created_at")?,
            expires_in_ms: r.try_get("expires_in_ms")?,
            settings: settings_of(&r)?,
        })
    })
    .transpose()
    .map_err(Into::into)
}

impl Database {
    /// Marks the account as looking for a game for `ttl_ms`. A live status is not renewed (repeating returns the same
    /// one); an expired one is overwritten. A player who already occupies a slot cannot look for another game.
    pub async fn mark_seeking(
        &self,
        user_id: Uuid,
        now: OffsetDateTime,
        ttl_ms: i64,
        settings: GameSettings,
    ) -> Result<SeekingStatus, GameError> {
        self.settle_for(user_id, now).await?;
        let mut tx = self.pool().begin().await?;
        sqlx::query("SELECT id FROM users WHERE id = $1 FOR NO KEY UPDATE")
            .bind(user_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(GameError::UserGone)?;
        close_stale(&mut tx, now, Scope::User(user_id)).await?;
        let slot = sqlx::query("SELECT game_id FROM game_players WHERE user_id = $1 AND is_active")
            .bind(user_id)
            .fetch_optional(&mut *tx)
            .await?;
        if let Some(slot) = slot {
            // Decided inside the transaction, thrown after it committed so the lazy closings are kept.
            let game_id: Uuid = slot.try_get("game_id")?;
            tx.commit().await?;
            return Err(GameError::ActiveGameExists {
                game_id: Some(game_id),
            });
        }
        sqlx::query(
            "INSERT INTO lobby_seekers (user_id, created_at, expires_at, variant, time_control_ms) VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT (user_id) DO UPDATE SET created_at = excluded.created_at, expires_at = excluded.expires_at, \
               variant = excluded.variant, time_control_ms = excluded.time_control_ms \
             WHERE lobby_seekers.expires_at <= $2",
        )
        .bind(user_id)
        .bind(now)
        .bind(now + Duration::milliseconds(ttl_ms))
        .bind(settings.variant.as_str())
        .bind(settings.time_control_ms as i32)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        own_status(self, user_id, now)
            .await?
            .ok_or(GameError::Corrupt)
    }

    /// Withdraws the status; also fine without one.
    pub async fn withdraw_seeking(&self, user_id: Uuid) -> Result<(), GameError> {
        sqlx::query("DELETE FROM lobby_seekers WHERE user_id = $1")
            .bind(user_id)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// The lobby as one account sees it: the oldest statuses first, never their own, never an expired one and never a
    /// player who already occupies a slot.
    pub async fn lobby_view(
        &self,
        user_id: Uuid,
        now: OffsetDateTime,
        limit: i64,
    ) -> Result<LobbyRows, GameError> {
        let current_game = self.current_game(user_id, now).await?;
        let me = own_status(self, user_id, now).await?;
        let rows = sqlx::query(
            "SELECT s.id, u.display_name, s.variant, s.time_control_ms, \
                    greatest(0, (extract(epoch FROM ($2::timestamptz - s.created_at)) * 1000))::int AS since_ms, \
                    (extract(epoch FROM (s.expires_at - $2::timestamptz)) * 1000)::int AS expires_in_ms \
             FROM lobby_seekers s JOIN users u ON u.id = s.user_id \
             WHERE s.expires_at > $2::timestamptz AND s.user_id <> $1 \
               AND NOT EXISTS (SELECT 1 FROM game_players p WHERE p.user_id = s.user_id AND p.is_active) \
             ORDER BY s.created_at, s.id LIMIT $3",
        )
        .bind(user_id)
        .bind(now)
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        let seekers = rows
            .iter()
            .map(|r| -> Result<SeekerRow, sqlx::Error> {
                Ok(SeekerRow {
                    id: r.try_get("id")?,
                    display_name: r.try_get("display_name")?,
                    since_ms: r.try_get("since_ms")?,
                    expires_in_ms: r.try_get("expires_in_ms")?,
                    settings: settings_of(r)?,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(LobbyRows {
            me,
            current_game,
            seekers,
        })
    }

    /// Starts a game with a seeker. The picker need not be looking themselves. Of many pickers of one seeker exactly one
    /// wins (the delete of the status is the gate); every refusal rolls the transaction back, restoring the status.
    pub async fn pick_seeker(
        &self,
        picker: Uuid,
        seeker_id: Uuid,
        now: OffsetDateTime,
        black_seat: Seat,
    ) -> Result<Uuid, GameError> {
        self.settle_for(picker, now).await?;
        {
            let mut conn = self.pool().acquire().await?;
            close_stale(&mut conn, now, Scope::User(picker)).await?;
        }
        let seeker = sqlx::query(
            "SELECT user_id, variant, time_control_ms FROM lobby_seekers WHERE id = $1",
        )
        .bind(seeker_id)
        .fetch_optional(self.pool())
        .await?
        .ok_or(GameError::LobbySeekerTaken)?;
        let owner: Uuid = seeker.try_get("user_id")?;
        let settings = settings_of(&seeker)?;
        if owner == picker {
            return Err(GameError::CannotPickSelf);
        }
        let mut tx = self.pool().begin().await?;
        let users =
            sqlx::query("SELECT id FROM users WHERE id IN ($1, $2) ORDER BY id FOR NO KEY UPDATE")
                .bind(picker)
                .bind(owner)
                .fetch_all(&mut *tx)
                .await?;
        let present: Vec<Uuid> = users
            .iter()
            .map(|r| r.try_get("id"))
            .collect::<Result<_, _>>()?;
        if !present.contains(&picker) {
            return Err(GameError::UserGone);
        }
        if !present.contains(&owner) {
            return Err(GameError::LobbySeekerTaken);
        }
        let taken = sqlx::query("DELETE FROM lobby_seekers WHERE id = $1 AND user_id = $2 AND expires_at > $3 RETURNING id")
            .bind(seeker_id)
            .bind(owner)
            .bind(now)
            .fetch_optional(&mut *tx)
            .await?;
        if taken.is_none() {
            let still_there =
                sqlx::query("SELECT expires_at <= $2 AS expired FROM lobby_seekers WHERE id = $1")
                    .bind(seeker_id)
                    .bind(now)
                    .fetch_optional(&mut *tx)
                    .await?;
            return Err(if still_there.is_some() {
                GameError::LobbySeekerExpired
            } else {
                GameError::LobbySeekerTaken
            });
        }
        let slots = sqlx::query(
            "SELECT game_id, user_id FROM game_players WHERE user_id IN ($1, $2) AND is_active",
        )
        .bind(picker)
        .bind(owner)
        .fetch_all(&mut *tx)
        .await?;
        // The picker's own slot is reported first (with the game); the seeker's game is never revealed.
        for slot in &slots {
            if slot.try_get::<Uuid, _>("user_id")? == picker {
                return Err(GameError::ActiveGameExists {
                    game_id: Some(slot.try_get("game_id")?),
                });
            }
        }
        if !slots.is_empty() {
            return Err(GameError::LobbySeekerTaken);
        }
        sqlx::query("DELETE FROM lobby_seekers WHERE user_id = $1")
            .bind(picker)
            .execute(&mut *tx)
            .await?;
        let game_id: Uuid = sqlx::query(
            "INSERT INTO games (origin, status, created_at, started_at, black_seat, variant, time_control_ms, clock_ms_1, clock_ms_2) \
             VALUES ('lobby', 'active', $1, $1, $2, $3, $4, $4, $4) RETURNING id",
        )
        .bind(now)
        .bind(i16::from(black_seat.number()))
        .bind(settings.variant.as_str())
        .bind(settings.time_control_ms as i32)
        .fetch_one(&mut *tx)
        .await?
        .try_get("id")?;
        if settings.variant == GameVariant::Paper {
            insert_paper_state(&mut tx, game_id).await?;
        }
        for (seat, user) in [(1_i16, picker), (2_i16, owner)] {
            let inserted = sqlx::query(
                "INSERT INTO game_players (game_id, seat, kind, user_id, is_active, joined_at, display_name) \
                 VALUES ($1, $2, 'account', $3, true, $4, (SELECT display_name FROM users WHERE id = $3))",
            )
            .bind(game_id)
            .bind(seat)
            .bind(user)
            .bind(now)
            .execute(&mut *tx)
            .await;
            if let Err(error) = inserted {
                return Err(match unique_violation(&error).as_deref() {
                    Some(ACTIVE_SLOT_INDEX) => GameError::ActiveGameExists { game_id: None },
                    _ => error.into(),
                });
            }
        }
        tx.commit().await?;
        Ok(game_id)
    }
}
