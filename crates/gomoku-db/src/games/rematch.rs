//! Rematch: the offer lives on the finished game's row; accepting creates the new game with the colours swapped.

use gomoku_core::constants::REMATCH_TTL_MS;
use gomoku_core::game::{GUEST_REMATCH_FRESH_MS, derive_rematch_token};
use gomoku_core::rematch::rematch_black_seat;
use gomoku_core::types::{GameStatus, GameVariant, Seat};
use sqlx::Row;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use super::paper::insert_paper_state;
use super::{
    ACTIVE_SLOT_INDEX, GameError, Scope, close_stale, from_ms, read_game, to_ms, unique_violation,
};
use crate::Database;
use crate::security_hash::sha256_hex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RematchOffered {
    pub offered_by_seat: Seat,
    pub expires_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RematchAccepted {
    pub game_id: Uuid,
    /// A guest's token for the new game (derived from the cookie's token); it goes only into the cookie.
    pub guest_token: Option<String>,
}

struct Seat0 {
    id: Uuid,
    seat: Seat,
    kind: String,
    user_id: Option<Uuid>,
    token_hash: Option<String>,
    nickname: Option<String>,
}

async fn seats(db: &Database, game_id: Uuid) -> Result<Vec<Seat0>, GameError> {
    let rows = sqlx::query("SELECT id, seat, kind, user_id, guest_token_hash, nickname FROM game_players WHERE game_id = $1 ORDER BY seat")
        .bind(game_id)
        .fetch_all(db.pool())
        .await?;
    rows.iter()
        .map(|r| {
            let seat: i16 = r.try_get("seat")?;
            Ok(Seat0 {
                id: r.try_get("id")?,
                seat: u8::try_from(seat)
                    .ok()
                    .and_then(Seat::from_number)
                    .ok_or(GameError::Corrupt)?,
                kind: r.try_get("kind")?,
                user_id: r.try_get("user_id")?,
                token_hash: r.try_get("guest_token_hash")?,
                nickname: r.try_get("nickname")?,
            })
        })
        .collect()
}

fn account_is_gone(seat: &Seat0) -> bool {
    seat.kind == "account" && seat.user_id.is_none()
}

impl Database {
    /// The checks that precede an offer, and the subject of the rate limit (the account, or a guest's seat). Nothing is
    /// written. The caller counts the attempt between this and [`Database::offer_rematch`].
    pub async fn rematch_subject(&self, player_id: Uuid, game_id: Uuid) -> Result<Uuid, GameError> {
        let players = seats(self, game_id).await?;
        let me = players
            .iter()
            .find(|p| p.id == player_id)
            .ok_or(GameError::NotFound)?;
        let mut conn = self.pool().acquire().await?;
        let game = read_game(&mut conn, game_id)
            .await?
            .ok_or(GameError::NotFound)?;
        if game.status != GameStatus::Finished {
            return Err(GameError::GameNotFinished);
        }
        if players.iter().any(account_is_gone) {
            return Err(GameError::RematchUnavailable);
        }
        Ok(me.user_id.unwrap_or(me.id))
    }

    /// Writes the offer; of two simultaneous offers exactly one wins (`None` for the loser, who gives the attempt back).
    pub async fn offer_rematch(
        &self,
        player_id: Uuid,
        game_id: Uuid,
        now: OffsetDateTime,
    ) -> Result<Option<RematchOffered>, GameError> {
        let seat: i16 = sqlx::query("SELECT seat FROM game_players WHERE id = $1 AND game_id = $2")
            .bind(player_id)
            .bind(game_id)
            .fetch_optional(self.pool())
            .await?
            .ok_or(GameError::NotFound)?
            .try_get("seat")?;
        let expires_at = now + Duration::milliseconds(REMATCH_TTL_MS as i64);
        let row = sqlx::query(
            "UPDATE games SET rematch_state = 'pending', rematch_offered_by_seat = $2, rematch_expires_at = $3, rematch_game_id = NULL \
             WHERE id = $1 AND status = 'finished' \
               AND (rematch_state IS NULL OR rematch_state = 'declined' OR (rematch_state = 'pending' AND rematch_expires_at <= $4)) \
             RETURNING id",
        )
        .bind(game_id)
        .bind(seat)
        .bind(expires_at)
        .bind(now)
        .fetch_optional(self.pool())
        .await?;
        Ok(row.map(|_| RematchOffered {
            offered_by_seat: u8::try_from(seat)
                .ok()
                .and_then(Seat::from_number)
                .unwrap_or(Seat::One),
            expires_at,
        }))
    }

    /// Declines (or withdraws) the pending offer; idempotent once declined.
    pub async fn decline_rematch(
        &self,
        player_id: Uuid,
        game_id: Uuid,
        now: OffsetDateTime,
    ) -> Result<(), GameError> {
        sqlx::query("SELECT 1 FROM game_players WHERE id = $1 AND game_id = $2")
            .bind(player_id)
            .bind(game_id)
            .fetch_optional(self.pool())
            .await?
            .ok_or(GameError::NotFound)?;
        let done = sqlx::query(
            "UPDATE games SET rematch_state = 'declined' WHERE id = $1 AND status = 'finished' AND rematch_state = 'pending' \
             AND rematch_expires_at > $2 RETURNING id",
        )
        .bind(game_id)
        .bind(now)
        .fetch_optional(self.pool())
        .await?;
        if done.is_some() {
            return Ok(());
        }
        let mut conn = self.pool().acquire().await?;
        let game = read_game(&mut conn, game_id)
            .await?
            .ok_or(GameError::NotFound)?;
        match game.rematch_state.as_deref() {
            Some("declined") => Ok(()),
            Some("pending") => Err(GameError::RematchExpired),
            _ => Err(GameError::RematchNotOffered),
        }
    }

    /// Accepts the opponent's offer: a new active game, same seats, colours swapped, nobody has seen it yet. Idempotent
    /// for the same offer. A guest's rematch can only be accepted in the first 55 minutes, so the proposer still has an
    /// hour-long window to claim their seat.
    pub async fn accept_rematch(
        &self,
        player_id: Uuid,
        game_id: Uuid,
        cookie_token: Option<&str>,
        now: OffsetDateTime,
    ) -> Result<RematchAccepted, GameError> {
        let players = seats(self, game_id).await?;
        let me = players
            .iter()
            .find(|p| p.id == player_id)
            .ok_or(GameError::NotFound)?;
        if me.kind == "guest" && cookie_token.is_none() {
            return Err(GameError::NotFound);
        }
        for user in players.iter().filter_map(|p| p.user_id) {
            self.settle_for(user, now).await?;
        }
        let mut user_ids: Vec<Uuid> = players.iter().filter_map(|p| p.user_id).collect();
        user_ids.sort();

        let mut tx = self.pool().begin().await?;
        if !user_ids.is_empty() {
            sqlx::query("SELECT id FROM users WHERE id = ANY($1) ORDER BY id FOR NO KEY UPDATE")
                .bind(&user_ids)
                .fetch_all(&mut *tx)
                .await?;
        }
        sqlx::query("SELECT id FROM games WHERE id = $1 FOR UPDATE")
            .bind(game_id)
            .fetch_optional(&mut *tx)
            .await?;
        let game = read_game(&mut tx, game_id)
            .await?
            .ok_or(GameError::NotFound)?;
        if game.status != GameStatus::Finished {
            return Err(GameError::GameNotFinished);
        }
        let own = game.rematch_offered_by == Some(me.seat);
        let derived = |new_game: Uuid| {
            cookie_token.map(|token| derive_rematch_token(token, &new_game.to_string()))
        };
        if game.rematch_state.as_deref() == Some("accepted")
            && let Some(new_game) = game.rematch_game_id
            && !own
        {
            return Ok(RematchAccepted {
                game_id: new_game,
                guest_token: if me.kind == "guest" {
                    derived(new_game)
                } else {
                    None
                },
            });
        }
        if game.rematch_state.as_deref() != Some("pending") || own {
            return Err(GameError::RematchNotOffered);
        }
        if game.rematch_expires_at.is_none_or(|expires| expires <= now) {
            return Err(GameError::RematchExpired);
        }
        // Re-read the seats under the lock: a deleted account, a cleared guest token or a stale guest window ends it.
        let players = seats_in(&mut tx, game_id).await?;
        let token_fresh = game
            .finished_at
            .is_some_and(|at| to_ms(at) > to_ms(now) - GUEST_REMATCH_FRESH_MS);
        let unavailable = players.iter().any(|p| {
            account_is_gone(p) || (p.kind == "guest" && (p.token_hash.is_none() || !token_fresh))
        });
        if unavailable {
            return Err(GameError::RematchUnavailable);
        }
        // One slot per player: stale slots are closed first, then any held slot refuses the rematch.
        for user in players.iter().filter_map(|p| p.user_id) {
            close_stale(&mut tx, now, Scope::User(user)).await?;
        }
        let held = sqlx::query(
            "SELECT game_id, user_id FROM game_players WHERE user_id = ANY($1) AND is_active",
        )
        .bind(players.iter().filter_map(|p| p.user_id).collect::<Vec<_>>())
        .fetch_all(&mut *tx)
        .await?;
        if let Some(row) = held.first() {
            // The game of the caller is told; the opponent's is never revealed. The offer stays pending.
            let holder: Uuid = row.try_get("user_id")?;
            let game_id_of_holder: Uuid = row.try_get("game_id")?;
            let caller_holds = me.user_id == Some(holder);
            return Err(GameError::ActiveGameExists {
                game_id: caller_holds.then_some(game_id_of_holder),
            });
        }

        let black = rematch_black_seat(game.black_seat.ok_or(GameError::Corrupt)?);
        let new_game = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO games (id, origin, status, created_at, started_at, black_seat, variant, time_control_ms, clock_ms_1, clock_ms_2) \
             VALUES ($1, 'rematch', 'active', $2, $2, $3, $4, $5, $5, $5)",
        )
        .bind(new_game)
        .bind(now)
        .bind(i16::from(black.number()))
        .bind(game.variant.as_str())
        .bind(game.time_control_ms as i32)
        .execute(&mut *tx)
        .await?;
        if game.variant == GameVariant::Paper {
            insert_paper_state(&mut tx, new_game).await?;
        }
        let caller_hash = derived(new_game).map(|token| sha256_hex(&token));
        for p in &players {
            let hash = if p.id == player_id {
                caller_hash.clone()
            } else {
                None
            };
            let inserted = sqlx::query(
                "INSERT INTO game_players (game_id, seat, kind, user_id, guest_token_hash, nickname, is_active, joined_at, display_name) \
                 VALUES ($1, $2, $3, $4, $5, $6, true, $7, (SELECT display_name FROM users WHERE id = $4))",
            )
            .bind(new_game)
            .bind(i16::from(p.seat.number()))
            .bind(&p.kind)
            .bind(p.user_id)
            .bind(if p.kind == "guest" { hash } else { None })
            .bind(&p.nickname)
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
        let user_ids: Vec<Uuid> = players.iter().filter_map(|p| p.user_id).collect();
        sqlx::query("DELETE FROM lobby_seekers WHERE user_id = ANY($1)")
            .bind(&user_ids)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE games SET rematch_state = 'accepted', rematch_game_id = $2 WHERE id = $1 AND rematch_state = 'pending'")
            .bind(game_id)
            .bind(new_game)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(RematchAccepted {
            game_id: new_game,
            guest_token: if me.kind == "guest" {
                derived(new_game)
            } else {
                None
            },
        })
    }

    /// The proposing guest gets a token for the new game while polling the old one: the derived token of the accepted
    /// rematch, claimed for their seat. Idempotent (the same call gives the same token); `None` when there is nothing to
    /// claim or the seat already holds another token.
    pub async fn claim_guest_seat(
        &self,
        player_id: Uuid,
        game_id: Uuid,
        cookie_token: &str,
    ) -> Result<Option<String>, GameError> {
        let source = sqlx::query(
            "SELECT g.rematch_game_id, p.seat FROM game_players p JOIN games g ON g.id = p.game_id \
             WHERE p.id = $1 AND g.id = $2 AND p.kind = 'guest' AND g.rematch_state = 'accepted' AND g.rematch_game_id IS NOT NULL",
        )
        .bind(player_id)
        .bind(game_id)
        .fetch_optional(self.pool())
        .await?;
        let Some(source) = source else {
            return Ok(None);
        };
        let new_game: Uuid = source.try_get("rematch_game_id")?;
        let seat: i16 = source.try_get("seat")?;
        let token = derive_rematch_token(cookie_token, &new_game.to_string());
        let claimed = sqlx::query(
            "UPDATE game_players SET guest_token_hash = $3 WHERE game_id = $1 AND seat = $2 AND kind = 'guest' \
             AND (guest_token_hash IS NULL OR guest_token_hash = $3) RETURNING id",
        )
        .bind(new_game)
        .bind(seat)
        .bind(sha256_hex(&token))
        .fetch_optional(self.pool())
        .await?;
        Ok(claimed.map(|_| token))
    }
}

async fn seats_in(conn: &mut sqlx::PgConnection, game_id: Uuid) -> Result<Vec<Seat0>, GameError> {
    let rows = sqlx::query("SELECT id, seat, kind, user_id, guest_token_hash, nickname FROM game_players WHERE game_id = $1 ORDER BY seat")
        .bind(game_id)
        .fetch_all(conn)
        .await?;
    rows.iter()
        .map(|r| {
            let seat: i16 = r.try_get("seat")?;
            Ok(Seat0 {
                id: r.try_get("id")?,
                seat: u8::try_from(seat)
                    .ok()
                    .and_then(Seat::from_number)
                    .ok_or(GameError::Corrupt)?,
                kind: r.try_get("kind")?,
                user_id: r.try_get("user_id")?,
                token_hash: r.try_get("guest_token_hash")?,
                nickname: r.try_get("nickname")?,
            })
        })
        .collect()
}

#[allow(dead_code)]
fn _unused(_: i64) -> OffsetDateTime {
    from_ms(0)
}
