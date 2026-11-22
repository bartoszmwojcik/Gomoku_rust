//! Invitations: the preview behind the invitation page and accepting one.
//!
//! Every "no" is the same `InvalidInvitation` (malformed, unknown, used, expired, cancelled, a lost race): the answer is no
//! oracle for which codes exist.

use gomoku_core::paper::GameSettings;
use gomoku_core::types::{GameStatus, GameVariant, Seat};
use sqlx::{PgConnection, Row};
use time::OffsetDateTime;
use uuid::Uuid;

use super::create::cancel_waiting_game;
use super::{
    ACTIVE_SLOT_INDEX, GameError, SEAT_UNIQUE, Scope, cancel_not_started, close_stale,
    expire_waiting, release_slots, unique_violation,
};
use crate::Database;

/// Who is looking at (or accepting) an invitation.
#[derive(Debug, Clone, Copy)]
pub enum Viewer<'a> {
    Anonymous,
    Account(Uuid),
    /// By the hash of the guest cookie's token.
    Guest(&'a str),
}

/// What the invitation page shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewState {
    /// The viewer is the creator of this very invitation (the page sends them to the game).
    OwnInvitation {
        game_id: Uuid,
    },
    /// The viewer already plays the game.
    Participant {
        game_id: Uuid,
    },
    Joinable,
    /// The viewer occupies a slot in another game.
    HasActiveGame {
        game_id: Uuid,
    },
    /// The viewer has a waiting game of their own.
    HasOwnWaitingGame {
        game_id: Uuid,
    },
    /// A public game in progress: anybody with the code may watch.
    Watchable {
        game_id: Uuid,
    },
    /// A finished public game: the code opens its review.
    Reviewable {
        game_id: Uuid,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewView {
    pub state: PreviewState,
    /// `None` for a deleted account.
    pub creator_name: Option<String>,
    pub creator_is_guest: bool,
    pub is_public: bool,
    pub settings: GameSettings,
}

/// How the second player joins.
#[derive(Debug, Clone, Copy)]
pub enum JoinAs<'a> {
    Account(Uuid),
    Guest {
        /// Hash of the token the browser already carries, if any (to find the guest's own slot).
        cookie_hash: Option<&'a str>,
        /// Hash of the fresh token of the new seat; the raw token goes only into the cookie.
        new_hash: &'a str,
        nickname: Option<&'a str>,
    },
    /// An MCP agent: no account, no cookie; the hash of its token and the declared model name.
    Agent {
        token_hash: &'a str,
        name: Option<&'a str>,
    },
}

#[derive(Debug, Clone, Copy)]
pub struct AcceptInput<'a> {
    pub join_as: JoinAs<'a>,
    pub cancel_own_waiting_game: bool,
    /// The seat that plays black, drawn by the caller (a CSPRNG in production).
    pub black_seat: Seat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcceptOutcome {
    pub game_id: Uuid,
    /// A guest really joined: the fresh token must be set as the cookie. `false` for a returning participant.
    pub new_guest_seat: bool,
    /// The creator (seat 1) is a guest, not an account.
    pub creator_is_guest: bool,
}

struct Found {
    game_id: Uuid,
    consumed: bool,
    expired: bool,
    status: GameStatus,
    creator_user_id: Option<Uuid>,
    creator_name: Option<String>,
    creator_is_guest: bool,
    is_public: bool,
    settings: GameSettings,
}

async fn find_invitation(
    conn: &mut PgConnection,
    code: &str,
    now: OffsetDateTime,
) -> Result<Option<Found>, GameError> {
    let row = sqlx::query(
        "SELECT i.game_id, i.consumed_at IS NOT NULL AS consumed, i.expires_at <= $2 AS expired, g.status, g.is_public, g.variant, g.time_control_ms, \
                c.user_id AS creator_user_id, COALESCE(c.display_name, u.display_name) AS creator_account_name, \
                c.kind AS creator_kind, c.nickname AS creator_nickname \
         FROM invitations i JOIN games g ON g.id = i.game_id \
         JOIN game_players c ON c.game_id = g.id AND c.seat = 1 LEFT JOIN users u ON u.id = c.user_id \
         WHERE i.code = $1",
    )
    .bind(code)
    .bind(now)
    .fetch_optional(conn)
    .await?;
    let Some(row) = row else { return Ok(None) };
    let status: String = row.try_get("status")?;
    let kind: String = row.try_get("creator_kind")?;
    let is_guest = kind == "guest";
    let nickname: Option<String> = row.try_get("creator_nickname")?;
    Ok(Some(Found {
        game_id: row.try_get("game_id")?,
        consumed: row.try_get("consumed")?,
        expired: row.try_get("expired")?,
        status: GameStatus::parse(&status).ok_or(GameError::Corrupt)?,
        creator_user_id: row.try_get("creator_user_id")?,
        creator_name: if is_guest {
            Some(nickname.unwrap_or_else(|| gomoku_core::nickname::default_nickname(Seat::One)))
        } else {
            row.try_get("creator_account_name")?
        },
        creator_is_guest: is_guest,
        is_public: row.try_get("is_public")?,
        settings: GameSettings {
            variant: GameVariant::parse(&row.try_get::<String, _>("variant")?)
                .ok_or(GameError::Corrupt)?,
            time_control_ms: i64::from(row.try_get::<i32, _>("time_control_ms")?),
        },
    }))
}

struct Slot {
    game_id: Uuid,
    status: String,
    seat: i16,
}

async fn slot_of(conn: &mut PgConnection, viewer: Viewer<'_>) -> Result<Option<Slot>, GameError> {
    let (sql, bind_user, bind_hash) = match viewer {
        Viewer::Anonymous => return Ok(None),
        Viewer::Account(id) => (
            "SELECT p.game_id, g.status, p.seat FROM game_players p JOIN games g ON g.id = p.game_id WHERE p.user_id = $1 AND p.is_active",
            Some(id),
            None,
        ),
        Viewer::Guest(hash) => (
            "SELECT p.game_id, g.status, p.seat FROM game_players p JOIN games g ON g.id = p.game_id WHERE p.guest_token_hash = $1 AND p.is_active",
            None,
            Some(hash),
        ),
    };
    let query = sqlx::query(sql);
    let row = match (bind_user, bind_hash) {
        (Some(id), _) => query.bind(id).fetch_optional(conn).await?,
        (_, Some(hash)) => query.bind(hash).fetch_optional(conn).await?,
        _ => None,
    };
    row.map(|r| {
        Ok(Slot {
            game_id: r.try_get("game_id")?,
            status: r.try_get("status")?,
            seat: r.try_get("seat")?,
        })
    })
    .transpose()
}

/// The viewer's seat in the game, if they play it (a guest by a token that is still valid for the game).
async fn member_of(
    conn: &mut PgConnection,
    viewer: Viewer<'_>,
    game_id: Uuid,
    now: OffsetDateTime,
) -> Result<Option<i16>, GameError> {
    let row = match viewer {
        Viewer::Anonymous => None,
        Viewer::Account(user) => {
            sqlx::query("SELECT seat FROM game_players WHERE game_id = $1 AND user_id = $2")
                .bind(game_id)
                .bind(user)
                .fetch_optional(conn)
                .await?
        }
        Viewer::Guest(hash) => {
            let cutoff = now
                - time::Duration::milliseconds(gomoku_core::constants::GUEST_TOKEN_GRACE_MS as i64);
            sqlx::query(
                "SELECT p.seat FROM game_players p JOIN games g ON g.id = p.game_id \
                 WHERE p.game_id = $1 AND p.guest_token_hash = $2 AND (g.status = 'active' OR (g.status IN ('finished', 'waiting', 'cancelled') \
                   AND (CASE WHEN g.finished_at IS NOT NULL THEN g.finished_at \
                             ELSE (SELECT max(i.expires_at) FROM invitations i WHERE i.game_id = g.id AND i.consumed_at IS NULL) END) > $3))",
            )
            .bind(game_id)
            .bind(hash)
            .bind(cutoff)
            .fetch_optional(conn)
            .await?
        }
    };
    row.map(|r| r.try_get("seat"))
        .transpose()
        .map_err(Into::into)
}

async fn settle_viewer(
    db: &Database,
    viewer: Viewer<'_>,
    now: OffsetDateTime,
) -> Result<(), GameError> {
    match viewer {
        Viewer::Anonymous => Ok(()),
        Viewer::Account(user) => db.settle_for(user, now).await,
        Viewer::Guest(hash) => db.settle_for_guest(hash, now).await,
    }
}

fn scope_of<'a>(viewer: Viewer<'a>) -> Option<Scope<'a>> {
    match viewer {
        Viewer::Anonymous => None,
        Viewer::Account(user) => Some(Scope::User(user)),
        Viewer::Guest(hash) => Some(Scope::Guest(hash)),
    }
}

impl Database {
    /// What the invitation page shows. Consumes nothing (link previewers open it), but lazily settles and closes the
    /// viewer's stale games first.
    pub async fn preview_invitation(
        &self,
        code: &str,
        viewer: Viewer<'_>,
        now: OffsetDateTime,
    ) -> Result<PreviewView, GameError> {
        settle_viewer(self, viewer, now).await?;
        let mut conn = self.pool().acquire().await?;
        let found = find_invitation(&mut conn, code, now)
            .await?
            .ok_or(GameError::InvalidInvitation)?;
        let view = |state| PreviewView {
            state,
            creator_name: found.creator_name.clone(),
            creator_is_guest: found.creator_is_guest,
            is_public: found.is_public,
            settings: found.settings,
        };

        if let Some(seat) = member_of(&mut conn, viewer, found.game_id, now).await? {
            if found.status == GameStatus::Waiting && !found.expired {
                return Ok(view(PreviewState::OwnInvitation {
                    game_id: found.game_id,
                }));
            }
            if seat == 2 || found.status != GameStatus::Cancelled {
                return Ok(view(PreviewState::Participant {
                    game_id: found.game_id,
                }));
            }
            // The creator of a cancelled game falls through: for them the invitation is simply dead.
        }
        // Watching takes no slot, so a viewer who plays elsewhere may watch too.
        if found.is_public && found.status == GameStatus::Active {
            return Ok(view(PreviewState::Watchable {
                game_id: found.game_id,
            }));
        }
        if found.is_public && found.status == GameStatus::Finished {
            return Ok(view(PreviewState::Reviewable {
                game_id: found.game_id,
            }));
        }
        if found.status != GameStatus::Waiting || found.consumed || found.expired {
            return Err(GameError::InvalidInvitation);
        }
        if let Some(scope) = scope_of(viewer) {
            close_stale(&mut conn, now, scope).await?;
        }
        let state = match slot_of(&mut conn, viewer).await? {
            None => PreviewState::Joinable,
            Some(slot) if slot.status == "waiting" && slot.seat == 1 => {
                PreviewState::HasOwnWaitingGame {
                    game_id: slot.game_id,
                }
            }
            Some(slot) => PreviewState::HasActiveGame {
                game_id: slot.game_id,
            },
        };
        Ok(view(state))
    }

    /// Accepts an invitation: the viewer becomes the second player and the game starts. Locks in the global order (users by
    /// id, then games by id: the invitation's game and the viewer's own waiting game) so it cannot deadlock with create,
    /// cancel, pick, rematch or account deletion; every write is conditional, so of two simultaneous accepts exactly one wins.
    pub async fn accept_invitation(
        &self,
        code: &str,
        input: &AcceptInput<'_>,
        now: OffsetDateTime,
    ) -> Result<AcceptOutcome, GameError> {
        let viewer = match input.join_as {
            JoinAs::Account(user) => Viewer::Account(user),
            JoinAs::Guest {
                cookie_hash: Some(hash),
                ..
            } => Viewer::Guest(hash),
            JoinAs::Guest {
                cookie_hash: None, ..
            }
            | JoinAs::Agent { .. } => Viewer::Anonymous,
        };
        settle_viewer(self, viewer, now).await?;
        let mut tx = self.pool().begin().await?;
        let peek = find_invitation(&mut tx, code, now)
            .await?
            .ok_or(GameError::InvalidInvitation)?;
        let is_guest = matches!(input.join_as, JoinAs::Guest { .. });

        // The viewer's own game that may have to be cancelled is locked together with the invitation's game.
        let extra_game = if is_guest || input.cancel_own_waiting_game {
            slot_of(&mut tx, viewer)
                .await?
                .filter(|slot| is_guest || (slot.status == "waiting" && slot.seat == 1))
                .map(|slot| slot.game_id)
        } else {
            None
        };
        let mut user_ids: Vec<Uuid> = [viewer_user(viewer), peek.creator_user_id]
            .into_iter()
            .flatten()
            .collect();
        user_ids.sort();
        user_ids.dedup();
        let mut game_ids: Vec<Uuid> = [Some(peek.game_id), extra_game]
            .into_iter()
            .flatten()
            .collect();
        game_ids.sort();
        game_ids.dedup();
        if !user_ids.is_empty() {
            sqlx::query("SELECT id FROM users WHERE id = ANY($1) ORDER BY id FOR NO KEY UPDATE")
                .bind(&user_ids)
                .fetch_all(&mut *tx)
                .await?;
        }
        sqlx::query("SELECT id FROM games WHERE id = ANY($1) ORDER BY id FOR UPDATE")
            .bind(&game_ids)
            .fetch_all(&mut *tx)
            .await?;

        expire_waiting(&mut tx, now, Scope::Game(peek.game_id)).await?;
        cancel_not_started(&mut tx, now, Scope::Game(peek.game_id)).await?;
        if let Viewer::Account(user) = viewer {
            close_stale(&mut tx, now, Scope::User(user)).await?;
        }
        if let Some(extra) = extra_game {
            close_stale(&mut tx, now, Scope::Game(extra)).await?;
        }
        let found = find_invitation(&mut tx, code, now)
            .await?
            .ok_or(GameError::InvalidInvitation)?;

        // (1) The viewer already plays here: their own invitation, or a repeat (nobody joins twice, no new token).
        if let Some(seat) = member_of(&mut tx, viewer, found.game_id, now).await? {
            if found.status == GameStatus::Waiting {
                return Err(GameError::OwnInvitation {
                    game_id: found.game_id,
                });
            }
            if seat == 2 || found.status != GameStatus::Cancelled {
                return Ok(AcceptOutcome {
                    game_id: found.game_id,
                    new_guest_seat: false,
                    creator_is_guest: found.creator_is_guest,
                });
            }
            return Err(GameError::InvalidInvitation);
        }
        // (2) Not joinable any more.
        if found.status != GameStatus::Waiting || found.consumed {
            return Err(GameError::InvalidInvitation);
        }
        // (3) One slot per player: a waiting game of their own can be swapped away on request, nothing else.
        if let Some(slot) = slot_of(&mut tx, viewer).await? {
            let own_waiting = slot.status == "waiting" && slot.seat == 1;
            if !own_waiting {
                return Err(GameError::ActiveGameExists {
                    game_id: Some(slot.game_id),
                });
            }
            if !input.cancel_own_waiting_game || Some(slot.game_id) != extra_game {
                return Err(GameError::OwnWaitingGame {
                    game_id: slot.game_id,
                    on_join: true,
                });
            }
            if !cancel_waiting_game(&mut tx, slot.game_id, now).await? {
                return Err(GameError::ActiveGameExists {
                    game_id: Some(slot.game_id),
                });
            }
            release_slots(&mut tx, slot.game_id).await?;
        }
        // (4) Seat the second player; every statement is conditional and a miss is the uniform invalid answer.
        let invitation_id: Uuid = sqlx::query("UPDATE invitations SET consumed_at = $2 WHERE code = $1 AND consumed_at IS NULL AND expires_at > $2 RETURNING id")
            .bind(code)
            .bind(now)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(GameError::InvalidInvitation)?
            .try_get("id")?;
        let seated = match input.join_as {
            JoinAs::Agent { token_hash, name } => {
                sqlx::query(
                    "INSERT INTO game_players (game_id, seat, kind, agent_token_hash, agent_name, is_active, joined_at, seen_at) \
                     VALUES ($1, 2, 'agent', $2, $3, true, $4, $4) RETURNING id",
                )
                .bind(found.game_id)
                .bind(token_hash)
                .bind(name)
                .bind(now)
                .fetch_one(&mut *tx)
                .await
            }
            JoinAs::Account(user) => {
                sqlx::query(
                    "INSERT INTO game_players (game_id, seat, kind, user_id, is_active, joined_at, seen_at, display_name) \
                     VALUES ($1, 2, 'account', $2, true, $3, $3, (SELECT display_name FROM users WHERE id = $2)) RETURNING id",
                )
                .bind(found.game_id)
                .bind(user)
                .bind(now)
                .fetch_one(&mut *tx)
                .await
            }
            JoinAs::Guest { new_hash, nickname, .. } => {
                sqlx::query(
                    "INSERT INTO game_players (game_id, seat, kind, guest_token_hash, nickname, is_active, joined_at, seen_at) \
                     VALUES ($1, 2, 'guest', $2, $3, true, $4, $4) RETURNING id",
                )
                .bind(found.game_id)
                .bind(new_hash)
                .bind(nickname)
                .bind(now)
                .fetch_one(&mut *tx)
                .await
            }
        };
        let player_id: Uuid = match seated {
            Ok(row) => row.try_get("id")?,
            Err(error) => {
                return Err(match unique_violation(&error).as_deref() {
                    Some(ACTIVE_SLOT_INDEX) => GameError::ActiveGameExists { game_id: None },
                    Some(SEAT_UNIQUE) => GameError::InvalidInvitation,
                    _ => error.into(),
                });
            }
        };
        sqlx::query("UPDATE invitations SET consumed_by_id = $2 WHERE id = $1")
            .bind(invitation_id)
            .bind(player_id)
            .execute(&mut *tx)
            .await?;
        let started = sqlx::query(
            "UPDATE games SET status = 'active', started_at = $2, black_seat = $3 WHERE id = $1 AND status = 'waiting' RETURNING id",
        )
        .bind(found.game_id)
        .bind(now)
        .bind(i16::from(input.black_seat.number()))
        .fetch_optional(&mut *tx)
        .await?;
        if started.is_none() {
            return Err(GameError::InvalidInvitation);
        }
        if let Viewer::Account(user) = viewer {
            sqlx::query("DELETE FROM lobby_seekers WHERE user_id = $1")
                .bind(user)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(AcceptOutcome {
            game_id: found.game_id,
            new_guest_seat: is_guest,
            creator_is_guest: found.creator_is_guest,
        })
    }
}

fn viewer_user(viewer: Viewer<'_>) -> Option<Uuid> {
    match viewer {
        Viewer::Account(user) => Some(user),
        _ => None,
    }
}
