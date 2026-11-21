//! Games: rows, lazy bookkeeping, participants and the operations on a game, in the lock order
//! `users (by id) -> lobby_seekers -> games (by id) -> game_players`.
//!
//! There is no background worker: every time-based transition (an invitation expiring, a game nobody opened, a loss on
//! time) is performed lazily by whoever touches the game next, inside the statements below.

mod create;
mod history;
mod invite;
mod lobby;
mod play;
mod rematch;

use gomoku_core::clock::{ClockState, deadline_ms, seat_out_of_time};
use gomoku_core::constants::READY_DEADLINE_MS;
use gomoku_core::rules::seat_on_turn;
use gomoku_core::types::{EndReason, GameStatus, Seat};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

pub use create::{CreateOutcome, CreatedGame, GuestCreateInput};
pub use history::{HistoryEntryRow, HistoryPageRows, ReviewRows};
pub use invite::{AcceptInput, AcceptOutcome, JoinAs, PreviewState, PreviewView, Viewer};
pub use lobby::{LobbyRows, SeekerRow, SeekingStatus};
pub use play::{MoveInput, MoveOutcome};
pub use rematch::{RematchAccepted, RematchOffered};

use crate::{Database, DbError};

/// Milliseconds since the epoch of a timestamp (what the pure clock rules work in).
pub fn to_ms(at: OffsetDateTime) -> i64 {
    i64::try_from(at.unix_timestamp_nanos() / 1_000_000).unwrap_or(i64::MAX)
}

/// Inverse of [`to_ms`].
pub fn from_ms(ms: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000)
        .unwrap_or(OffsetDateTime::UNIX_EPOCH)
}

/// Why a game operation was refused. Everything a client can act on is a variant; the rest is a database failure.
#[derive(Debug, thiserror::Error)]
pub enum GameError {
    #[error("not found")]
    NotFound,
    #[error("the account is gone")]
    UserGone,
    #[error("the player already occupies a game slot")]
    ActiveGameExists { game_id: Option<Uuid> },
    #[error("the player has a waiting game of their own")]
    OwnWaitingGame { game_id: Uuid, on_join: bool },
    #[error("own invitation")]
    OwnInvitation { game_id: Uuid },
    #[error("invalid invitation")]
    InvalidInvitation,
    #[error("the game can no longer be cancelled")]
    GameNotCancellable,
    #[error("the game is not in progress")]
    GameNotActive,
    #[error("the game is finished")]
    GameFinished,
    #[error("the opponent has not opened the game")]
    GameNotStarted,
    #[error("not your turn")]
    NotYourTurn,
    #[error("stale move")]
    StaleMove,
    #[error("cell occupied")]
    CellOccupied,
    #[error("`after` is beyond the move count")]
    BadAfter,
    #[error("the game is not finished")]
    GameNotFinished,
    #[error("a rematch was already offered")]
    RematchAlreadyOffered,
    #[error("no rematch is offered")]
    RematchNotOffered,
    #[error("the rematch offer expired")]
    RematchExpired,
    #[error("the opponent is gone")]
    RematchUnavailable,
    #[error("the seeker was taken")]
    LobbySeekerTaken,
    #[error("the seeker's status expired")]
    LobbySeekerExpired,
    #[error("cannot pick oneself")]
    CannotPickSelf,
    #[error("a stored game is corrupt")]
    Corrupt,
    #[error(transparent)]
    Db(#[from] DbError),
}

impl From<sqlx::Error> for GameError {
    fn from(error: sqlx::Error) -> Self {
        GameError::Db(DbError::Sqlx(error))
    }
}

/// Unique-violation helper: the constraint that fired, if the error is one.
pub(crate) fn unique_violation(error: &sqlx::Error) -> Option<String> {
    error
        .as_database_error()
        .filter(|db| db.is_unique_violation())
        .map(|db| db.constraint().unwrap_or_default().to_owned())
}

pub(crate) const ACTIVE_SLOT_INDEX: &str = "game_players_user_id_active_unique";
pub(crate) const SEAT_UNIQUE: &str = "game_players_game_id_seat_unique";

/// A game as stored.
#[derive(Debug, Clone)]
pub struct GameRow {
    pub id: Uuid,
    pub origin: String,
    pub status: GameStatus,
    pub created_at: OffsetDateTime,
    pub started_at: Option<OffsetDateTime>,
    pub finished_at: Option<OffsetDateTime>,
    pub black_seat: Option<Seat>,
    pub move_count: usize,
    pub moves: Vec<u8>,
    pub winner: Option<Seat>,
    pub end_reason: Option<EndReason>,
    pub clock_ms: [i64; 2],
    pub turn_started_at: Option<OffsetDateTime>,
    pub rematch_state: Option<String>,
    pub rematch_offered_by: Option<Seat>,
    pub rematch_expires_at: Option<OffsetDateTime>,
    pub rematch_game_id: Option<Uuid>,
}

pub(crate) const GAME_COLUMNS: &str = "id, origin, status, created_at, started_at, finished_at, black_seat, move_count, moves, \
     winner_seat, end_reason, clock_ms_1, clock_ms_2, turn_started_at, rematch_state, rematch_offered_by_seat, \
     rematch_expires_at, rematch_game_id";

fn seat_of(value: Option<i16>) -> Option<Seat> {
    value
        .and_then(|n| u8::try_from(n).ok())
        .and_then(Seat::from_number)
}

impl GameRow {
    pub(crate) fn from_row(row: &PgRow) -> Result<Self, GameError> {
        let status: String = row.try_get("status")?;
        let end_reason: Option<String> = row.try_get("end_reason")?;
        let move_count: i16 = row.try_get("move_count")?;
        Ok(Self {
            id: row.try_get("id")?,
            origin: row.try_get("origin")?,
            status: GameStatus::parse(&status).ok_or(GameError::Corrupt)?,
            created_at: row.try_get("created_at")?,
            started_at: row.try_get("started_at")?,
            finished_at: row.try_get("finished_at")?,
            black_seat: seat_of(row.try_get("black_seat")?),
            move_count: usize::try_from(move_count).map_err(|_| GameError::Corrupt)?,
            moves: row.try_get("moves")?,
            winner: seat_of(row.try_get("winner_seat")?),
            end_reason: end_reason
                .as_deref()
                .map(EndReason::parse)
                .map(|r| r.ok_or(GameError::Corrupt))
                .transpose()?,
            clock_ms: [
                i64::from(row.try_get::<i32, _>("clock_ms_1")?),
                i64::from(row.try_get::<i32, _>("clock_ms_2")?),
            ],
            turn_started_at: row.try_get("turn_started_at")?,
            rematch_state: row.try_get("rematch_state")?,
            rematch_offered_by: seat_of(row.try_get("rematch_offered_by_seat")?),
            rematch_expires_at: row.try_get("rematch_expires_at")?,
            rematch_game_id: row.try_get("rematch_game_id")?,
        })
    }

    /// The clock as the pure rules see it: the seat on turn exists only in an active game.
    pub fn clock_state(&self) -> ClockState {
        let turn_seat = match (self.status, self.black_seat) {
            (GameStatus::Active, Some(black)) => Some(seat_on_turn(self.move_count, black)),
            _ => None,
        };
        ClockState {
            clock_ms: self.clock_ms,
            turn_started_at_ms: self.turn_started_at.map(to_ms),
            turn_seat,
            time_control_ms: None,
        }
    }
}

/// A seat of a game.
#[derive(Debug, Clone)]
pub struct PlayerRow {
    pub id: Uuid,
    pub seat: Seat,
    pub kind: String,
    pub user_id: Option<Uuid>,
    pub guest_token_hash: Option<String>,
    pub nickname: Option<String>,
    /// The name when the seat was created.
    pub display_name: Option<String>,
    /// The account's current name (for seats created before the snapshot existed).
    pub current_name: Option<String>,
    pub seen: bool,
    pub is_active: bool,
}

pub(crate) const PLAYER_SELECT: &str = "SELECT p.id, p.seat, p.kind, p.user_id, p.guest_token_hash, p.nickname, p.display_name, \
     u.display_name AS current_name, p.seen_at IS NOT NULL AS seen, p.is_active \
     FROM game_players p LEFT JOIN users u ON u.id = p.user_id";

impl PlayerRow {
    pub(crate) fn from_row(row: &PgRow) -> Result<Self, GameError> {
        let seat: i16 = row.try_get("seat")?;
        Ok(Self {
            id: row.try_get("id")?,
            seat: seat_of(Some(seat)).ok_or(GameError::Corrupt)?,
            kind: row.try_get("kind")?,
            user_id: row.try_get("user_id")?,
            guest_token_hash: row.try_get("guest_token_hash")?,
            nickname: row.try_get("nickname")?,
            display_name: row.try_get("display_name")?,
            current_name: row.try_get("current_name")?,
            seen: row.try_get("seen")?,
            is_active: row.try_get("is_active")?,
        })
    }

    pub fn is_guest(&self) -> bool {
        self.kind == "guest"
    }
}

/// An invitation as shown to its creator.
#[derive(Debug, Clone)]
pub struct InvitationRow {
    pub code: String,
    pub expires_at: OffsetDateTime,
    pub consumed: bool,
}

/// Everything a client is told about a game, read in one transaction. The view is built from it by the web layer.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub game: GameRow,
    pub players: Vec<PlayerRow>,
    /// The caller's own seat.
    pub me: Seat,
    /// Seat 1's invitation while the game waits; for a cancelled one-player game it tells why it was cancelled.
    pub invitation: Option<InvitationRow>,
}

/// What a lazy-closing statement is scoped to.
#[derive(Clone, Copy)]
pub(crate) enum Scope<'a> {
    User(Uuid),
    Game(Uuid),
    Guest(&'a str),
}

impl Scope<'_> {
    fn predicate(self) -> &'static str {
        match self {
            Scope::User(_) => {
                "exists (select 1 from game_players m where m.game_id = g.id and m.user_id = $2)"
            }
            Scope::Game(_) => "g.id = $2",
            Scope::Guest(_) => {
                "exists (select 1 from game_players m where m.game_id = g.id and m.guest_token_hash = $2)"
            }
        }
    }
}

/// Releases the single game slot of every player of the game.
pub(crate) async fn release_slots(
    conn: &mut PgConnection,
    game_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE game_players SET is_active = false WHERE game_id = $1 AND is_active")
        .bind(game_id)
        .execute(conn)
        .await?;
    Ok(())
}

async fn run_scoped(
    conn: &mut PgConnection,
    sql: &str,
    when: OffsetDateTime,
    scope: Scope<'_>,
) -> Result<(), sqlx::Error> {
    // SAFETY of the text: only static fragments are interpolated; every value is bound.
    let query = sqlx::query(sqlx::AssertSqlSafe(sql.to_owned())).bind(when);
    match scope {
        Scope::User(id) | Scope::Game(id) => query.bind(id).execute(conn).await?,
        Scope::Guest(hash) => query.bind(hash).execute(conn).await?,
    };
    Ok(())
}

/// Cancels unjoined waiting games whose invitation expired, and frees their slots.
pub(crate) async fn expire_waiting(
    conn: &mut PgConnection,
    now: OffsetDateTime,
    scope: Scope<'_>,
) -> Result<(), sqlx::Error> {
    let sql = format!(
        "WITH expired AS (UPDATE games g SET status = 'cancelled' WHERE g.status = 'waiting' \
         AND EXISTS (SELECT 1 FROM invitations i WHERE i.game_id = g.id AND i.expires_at <= $1) AND {} RETURNING g.id) \
         UPDATE game_players SET is_active = false WHERE is_active AND game_id IN (SELECT id FROM expired)",
        scope.predicate()
    );
    run_scoped(conn, &sql, now, scope).await
}

/// Cancels active games somebody never opened within the ready deadline, and frees their slots.
pub(crate) async fn cancel_not_started(
    conn: &mut PgConnection,
    now: OffsetDateTime,
    scope: Scope<'_>,
) -> Result<(), sqlx::Error> {
    let cutoff = now - Duration::milliseconds(READY_DEADLINE_MS as i64);
    let sql = format!(
        "WITH stale AS (UPDATE games g SET status = 'cancelled' WHERE g.status = 'active' AND g.started_at <= $1 \
         AND EXISTS (SELECT 1 FROM game_players p WHERE p.game_id = g.id AND p.seen_at IS NULL) AND {} RETURNING g.id) \
         UPDATE game_players SET is_active = false WHERE is_active AND game_id IN (SELECT id FROM stale)",
        scope.predicate()
    );
    run_scoped(conn, &sql, cutoff, scope).await
}

/// Both lazy closings for one scope, in the order games -> game_players. Idempotent; used before every judgement of
/// "does this player occupy a slot".
pub(crate) async fn close_stale(
    conn: &mut PgConnection,
    now: OffsetDateTime,
    scope: Scope<'_>,
) -> Result<(), sqlx::Error> {
    expire_waiting(conn, now, scope).await?;
    cancel_not_started(conn, now, scope).await
}

/// Both lazy closings for the games of one account (used by account deletion).
pub async fn close_stale_of_user(
    conn: &mut PgConnection,
    now: OffsetDateTime,
    user_id: Uuid,
) -> Result<(), sqlx::Error> {
    close_stale(conn, now, Scope::User(user_id)).await
}

/// The finished-games counter: one row per day, pair of participant kinds. Written by every finish path.
pub(crate) async fn record_finished(
    conn: &mut PgConnection,
    game_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO game_pair_counters (day, kind_a, kind_b, finished_count) \
         SELECT (g.finished_at AT TIME ZONE 'Europe/Warsaw')::date, k.kind_a, k.kind_b, 1 \
         FROM games g JOIN (SELECT game_id, min(kind) AS kind_a, max(kind) AS kind_b FROM game_players \
                            WHERE game_id = $1 GROUP BY game_id) k ON k.game_id = g.id \
         WHERE g.id = $1 AND g.status = 'finished' \
         ON CONFLICT (day, kind_a, kind_b) DO UPDATE SET finished_count = game_pair_counters.finished_count + 1",
    )
    .bind(game_id)
    .execute(conn)
    .await?;
    Ok(())
}

/// The game row, without a lock.
pub(crate) async fn read_game(
    conn: &mut PgConnection,
    game_id: Uuid,
) -> Result<Option<GameRow>, GameError> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {GAME_COLUMNS} FROM games WHERE id = $1"
    )))
    .bind(game_id)
    .fetch_optional(conn)
    .await?;
    row.as_ref().map(GameRow::from_row).transpose()
}

/// Loses on time if the clock of the seat on turn ran out; the game row must be locked by the caller (or the caller
/// must be willing to lose the race: the update is conditional on the move count). `finished_at` is the deadline, not
/// the moment this is noticed. Returns whether the game was finished by it.
pub(crate) async fn settle_game(
    conn: &mut PgConnection,
    game_id: Uuid,
    now: OffsetDateTime,
) -> Result<bool, GameError> {
    let Some(game) = read_game(conn, game_id).await? else {
        return Ok(false);
    };
    let state = game.clock_state();
    let Some(loser) = seat_out_of_time(&state, to_ms(now)) else {
        return Ok(false);
    };
    let Some(deadline) = deadline_ms(&state).map(from_ms) else {
        return Ok(false);
    };
    let winner = loser.other();
    let sql = format!(
        "UPDATE games SET status = 'finished', winner_seat = $2, end_reason = 'timeout', finished_at = $3, clock_ms_{} = 0 \
         WHERE id = $1 AND status = 'active' AND move_count = $4 RETURNING id",
        loser.number()
    );
    let finished = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(game_id)
        .bind(i16::from(winner.number()))
        .bind(deadline)
        .bind(i16::try_from(game.move_count).map_err(|_| GameError::Corrupt)?)
        .fetch_optional(&mut *conn)
        .await?;
    if finished.is_none() {
        return Ok(false);
    }
    record_finished(conn, game_id).await?;
    release_slots(conn, game_id).await?;
    Ok(true)
}

/// A normal poll stays a plain read: the game is locked only when its clock really ran out.
pub(crate) async fn settle_expired(
    conn: &mut PgConnection,
    game_id: Uuid,
    now: OffsetDateTime,
) -> Result<bool, GameError> {
    let Some(game) = read_game(conn, game_id).await? else {
        return Ok(false);
    };
    if seat_out_of_time(&game.clock_state(), to_ms(now)).is_none() {
        return Ok(false);
    }
    sqlx::query("SELECT id FROM games WHERE id = $1 FOR UPDATE")
        .bind(game_id)
        .fetch_optional(&mut *conn)
        .await?;
    settle_game(conn, game_id, now).await
}

/// Reads a game for a caller already known to be its participant.
pub(crate) async fn snapshot(
    conn: &mut PgConnection,
    game_id: Uuid,
    caller: Uuid,
) -> Result<Snapshot, GameError> {
    let game = read_game(conn, game_id).await?.ok_or(GameError::NotFound)?;
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "{PLAYER_SELECT} WHERE p.game_id = $1 ORDER BY p.seat"
    )))
    .bind(game_id)
    .fetch_all(&mut *conn)
    .await?;
    let players = rows
        .iter()
        .map(PlayerRow::from_row)
        .collect::<Result<Vec<_>, _>>()?;
    let me = players
        .iter()
        .find(|p| p.id == caller)
        .map(|p| p.seat)
        .ok_or(GameError::NotFound)?;
    // The creator sees the invitation while the game waits; for a cancelled one-player game it tells why.
    let wants_invitation = (game.status == GameStatus::Waiting && me == Seat::One)
        || (game.status == GameStatus::Cancelled && players.len() == 1);
    let invitation = if wants_invitation {
        let row = sqlx::query("SELECT code, expires_at, consumed_at IS NOT NULL AS consumed FROM invitations WHERE game_id = $1")
            .bind(game_id)
            .fetch_optional(&mut *conn)
            .await?;
        row.map(|r| -> Result<InvitationRow, sqlx::Error> {
            Ok(InvitationRow {
                code: r.try_get("code")?,
                expires_at: r.try_get("expires_at")?,
                consumed: r.try_get("consumed")?,
            })
        })
        .transpose()?
    } else {
        None
    };
    Ok(Snapshot {
        game,
        players,
        me,
        invitation,
    })
}

/// Where a player currently stands in the game slot sense.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CurrentGame {
    pub game_id: Uuid,
    pub status: GameStatus,
    pub seat: Seat,
}

fn current_game_of(row: Option<&PgRow>) -> Result<Option<CurrentGame>, GameError> {
    let Some(row) = row else { return Ok(None) };
    let status: String = row.try_get("status")?;
    let seat: i16 = row.try_get("seat")?;
    Ok(Some(CurrentGame {
        game_id: row.try_get("game_id")?,
        status: GameStatus::parse(&status).ok_or(GameError::Corrupt)?,
        seat: seat_of(Some(seat)).ok_or(GameError::Corrupt)?,
    }))
}

impl Database {
    /// The game that holds the account's slot (after closing the stale ones), if any.
    pub async fn current_game(
        &self,
        user_id: Uuid,
        now: OffsetDateTime,
    ) -> Result<Option<CurrentGame>, GameError> {
        let mut conn = self.pool().acquire().await?;
        close_stale(&mut conn, now, Scope::User(user_id)).await?;
        let row = sqlx::query(
            "SELECT p.game_id, g.status, p.seat FROM game_players p JOIN games g ON g.id = p.game_id \
             WHERE p.user_id = $1 AND p.is_active AND g.status IN ('waiting', 'active') LIMIT 1",
        )
        .bind(user_id)
        .fetch_optional(&mut *conn)
        .await?;
        current_game_of(row.as_ref())
    }

    /// The same for a guest, by the hash of the cookie token.
    pub async fn current_game_of_guest(
        &self,
        token_hash: &str,
        now: OffsetDateTime,
    ) -> Result<Option<CurrentGame>, GameError> {
        let mut conn = self.pool().acquire().await?;
        close_stale(&mut conn, now, Scope::Guest(token_hash)).await?;
        let row = sqlx::query(
            "SELECT p.game_id, g.status, p.seat FROM game_players p JOIN games g ON g.id = p.game_id \
             WHERE p.guest_token_hash = $1 AND p.is_active AND g.status IN ('waiting', 'active') LIMIT 1",
        )
        .bind(token_hash)
        .fetch_optional(&mut *conn)
        .await?;
        current_game_of(row.as_ref())
    }

    /// Whether the account has a game in progress (the logout warning counts only `active`; a waiting game does not).
    pub async fn has_active_game(&self, user_id: Uuid) -> Result<bool, DbError> {
        let row = sqlx::query(
            "SELECT EXISTS (SELECT 1 FROM game_players p JOIN games g ON g.id = p.game_id \
             WHERE p.user_id = $1 AND p.is_active AND g.status = 'active') AS playing",
        )
        .bind(user_id)
        .fetch_one(self.pool())
        .await?;
        Ok(row.try_get("playing")?)
    }

    /// Settles the games of the account whose clock ran out, so "is the slot free" is judged on settled games. Without
    /// any such candidate nothing is locked.
    pub async fn settle_for(&self, user_id: Uuid, now: OffsetDateTime) -> Result<(), GameError> {
        let candidates = sqlx::query(
            "SELECT g.id FROM games g JOIN game_players p ON p.game_id = g.id \
             WHERE p.user_id = $1 AND p.is_active AND g.status = 'active' AND g.turn_started_at IS NOT NULL ORDER BY g.id",
        )
        .bind(user_id)
        .fetch_all(self.pool())
        .await?;
        if candidates.is_empty() {
            return Ok(());
        }
        let mut tx = self.pool().begin().await?;
        sqlx::query("SELECT id FROM users WHERE id = $1 FOR NO KEY UPDATE")
            .bind(user_id)
            .fetch_optional(&mut *tx)
            .await?;
        for row in &candidates {
            settle_expired(&mut tx, row.try_get("id")?, now).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// The same for a guest's games.
    pub async fn settle_for_guest(
        &self,
        token_hash: &str,
        now: OffsetDateTime,
    ) -> Result<(), GameError> {
        let candidates = sqlx::query(
            "SELECT g.id FROM games g JOIN game_players p ON p.game_id = g.id \
             WHERE p.guest_token_hash = $1 AND p.is_active AND g.status = 'active' AND g.turn_started_at IS NOT NULL ORDER BY g.id",
        )
        .bind(token_hash)
        .fetch_all(self.pool())
        .await?;
        if candidates.is_empty() {
            return Ok(());
        }
        let mut tx = self.pool().begin().await?;
        for row in &candidates {
            settle_expired(&mut tx, row.try_get("id")?, now).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Resolves who the caller is in the game: an account first (any status of the game), else a guest token that is
    /// still valid for the game's status. The same `None` answers "no such game" and "not your game".
    pub async fn find_participant(
        &self,
        game_id: Uuid,
        user_id: Option<Uuid>,
        guest_token_hash: Option<&str>,
        now: OffsetDateTime,
    ) -> Result<Option<Uuid>, DbError> {
        if let Some(user) = user_id {
            let row =
                sqlx::query("SELECT id FROM game_players WHERE game_id = $1 AND user_id = $2")
                    .bind(game_id)
                    .bind(user)
                    .fetch_optional(self.pool())
                    .await?;
            if let Some(row) = row {
                return Ok(Some(row.try_get("id")?));
            }
        }
        let Some(hash) = guest_token_hash.filter(|h| !h.is_empty()) else {
            return Ok(None);
        };
        let cutoff =
            now - Duration::milliseconds(gomoku_core::constants::GUEST_TOKEN_GRACE_MS as i64);
        let row = sqlx::query(
            "SELECT p.id FROM game_players p JOIN games g ON g.id = p.game_id \
             WHERE p.game_id = $1 AND p.guest_token_hash = $2 AND (g.status = 'active' OR (g.status IN ('finished', 'waiting', 'cancelled') \
               AND (CASE WHEN g.finished_at IS NOT NULL THEN g.finished_at \
                         ELSE (SELECT max(i.expires_at) FROM invitations i WHERE i.game_id = g.id AND i.consumed_at IS NULL) END) > $3))",
        )
        .bind(game_id)
        .bind(hash)
        .bind(cutoff)
        .fetch_optional(self.pool())
        .await?;
        Ok(row.map(|r| r.try_get("id")).transpose()?)
    }
}
