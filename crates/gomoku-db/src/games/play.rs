//! Reading a game (the poll), playing a move and resigning.

use gomoku_core::apply_move::{MoveRejection, apply_move};
use gomoku_core::clock::spend;
use gomoku_core::game::{is_valid_after, replay};
use gomoku_core::resign::{ResignRejection, resolve_resignation};
use gomoku_core::rules::seat_on_turn;
use gomoku_core::types::{EndReason, GameResult, GameStatus, Seat};
use sqlx::{PgConnection, Row};
use time::OffsetDateTime;
use uuid::Uuid;

use super::{
    GAME_COLUMNS, GameError, GameRow, Scope, Snapshot, cancel_not_started, expire_waiting,
    read_game, record_finished, release_slots, settle_expired, settle_game, snapshot, to_ms,
};
use crate::Database;

/// A move as the client sent it, already validated against the board.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MoveInput {
    pub cell: usize,
    /// The number of the move the client means to play: `move_count + 1` of the position it sees.
    pub move_number: usize,
}

/// Outcome alias kept for readability at call sites.
pub type MoveOutcome = Result<Snapshot, GameError>;

fn small(value: usize) -> Result<i16, GameError> {
    i16::try_from(value).map_err(|_| GameError::Corrupt)
}

/// The game row locked against concurrent moves, together with the caller's seat.
async fn lock_for_play(
    conn: &mut PgConnection,
    game_id: Uuid,
    player_id: Uuid,
) -> Result<(GameRow, Seat), GameError> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {GAME_COLUMNS} FROM games WHERE id = $1 FOR NO KEY UPDATE"
    )))
    .bind(game_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(GameError::NotFound)?;
    let game = GameRow::from_row(&row)?;
    let seat: i16 = sqlx::query("SELECT seat FROM game_players WHERE id = $1 AND game_id = $2")
        .bind(player_id)
        .bind(game_id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or(GameError::NotFound)?
        .try_get("seat")?;
    let seat = u8::try_from(seat)
        .ok()
        .and_then(Seat::from_number)
        .ok_or(GameError::Corrupt)?;
    Ok((game, seat))
}

impl Database {
    /// The state of a game for one of its players: also the poll. All the lazy bookkeeping happens here, in one
    /// transaction and in the lock order games -> game_players: unjoined invitations expire, games nobody opened are
    /// cancelled (before the clock start, so a late first visit cannot revive them), the clock starts once everybody has
    /// seen the game, a clock that ran out loses, and the caller's `seen_at` is written last.
    pub async fn game_state(
        &self,
        player_id: Uuid,
        game_id: Uuid,
        after: usize,
        now: OffsetDateTime,
    ) -> Result<Snapshot, GameError> {
        let mut tx = self.pool().begin().await?;
        let row = sqlx::query("SELECT g.move_count FROM game_players p JOIN games g ON g.id = p.game_id WHERE p.id = $1 AND g.id = $2")
            .bind(player_id)
            .bind(game_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(GameError::NotFound)?;
        let move_count = usize::try_from(row.try_get::<i16, _>("move_count")?)
            .map_err(|_| GameError::Corrupt)?;
        if !is_valid_after(after, move_count) {
            return Err(GameError::BadAfter);
        }
        expire_waiting(&mut tx, now, Scope::Game(game_id)).await?;
        cancel_not_started(&mut tx, now, Scope::Game(game_id)).await?;
        sqlx::query(
            "UPDATE games SET turn_started_at = $1 WHERE id = $2 AND status = 'active' AND turn_started_at IS NULL \
             AND NOT EXISTS (SELECT 1 FROM game_players p WHERE p.game_id = games.id AND p.seen_at IS NULL AND p.id <> $3)",
        )
        .bind(now)
        .bind(game_id)
        .bind(player_id)
        .execute(&mut *tx)
        .await?;
        settle_expired(&mut tx, game_id, now).await?;
        sqlx::query(
            "UPDATE game_players p SET seen_at = $1 FROM games g \
             WHERE p.id = $2 AND g.id = p.game_id AND p.seen_at IS NULL AND g.status IN ('waiting', 'active')",
        )
        .bind(now)
        .bind(player_id)
        .execute(&mut *tx)
        .await?;
        let state = snapshot(&mut tx, game_id, player_id, now).await?;
        tx.commit().await?;
        Ok(state)
    }

    /// Plays a move. One transaction with the game row locked; a refusal is returned, and a loss on time found on the way
    /// is committed first (rolling it back would undo the settlement). The answer to a repeat of the move just played
    /// is the state, with nothing written (the client retries after a lost answer).
    pub async fn play_move(
        &self,
        player_id: Uuid,
        game_id: Uuid,
        input: MoveInput,
        now: OffsetDateTime,
    ) -> MoveOutcome {
        let mut tx = self.pool().begin().await?;
        let (game, seat) = lock_for_play(&mut tx, game_id, player_id).await?;
        let after = input.move_number.saturating_sub(1);

        let moves =
            gomoku_core::board::decode_moves(&game.moves).map_err(|_| GameError::Corrupt)?;
        if let Some(black) = game.black_seat
            && moves.get(after) == Some(&input.cell)
            && seat_on_turn(after, black) == seat
        {
            return snapshot(&mut tx, game_id, player_id, now).await;
        }

        if settle_game(&mut tx, game_id, now).await? {
            tx.commit().await?;
            return Err(GameError::GameFinished);
        }
        let game = read_game(&mut tx, game_id)
            .await?
            .ok_or(GameError::NotFound)?;
        match game.status {
            GameStatus::Finished => return Err(GameError::GameFinished),
            GameStatus::Active => {}
            _ => return Err(GameError::GameNotActive),
        }
        let black = game.black_seat.ok_or(GameError::GameNotActive)?;
        if game.turn_started_at.is_none() {
            return Err(GameError::GameNotStarted);
        }
        if seat_on_turn(game.move_count, black) != seat {
            return Err(GameError::NotYourTurn);
        }
        if input.move_number != game.move_count + 1 {
            return Err(GameError::StaleMove);
        }
        let state = replay(black, &moves);
        let applied = match apply_move(&state, input.cell) {
            Ok(applied) => applied,
            Err(MoveRejection::Occupied | MoveRejection::OutOfBoard) => {
                return Err(GameError::CellOccupied);
            }
        };
        // The mover's time is charged at the moment the move arrived; the opponent's clock starts now.
        let spent = spend(&game.clock_state(), seat, to_ms(now));
        if spent.expired {
            return Err(GameError::GameFinished);
        }
        let mut clocks = game.clock_ms;
        clocks[seat.index()] = spent.clock_ms;
        let result: Option<GameResult> = applied.result;
        let cell_byte = vec![u8::try_from(input.cell).map_err(|_| GameError::Corrupt)?];
        let updated = sqlx::query(
            "UPDATE games SET moves = moves || $2, move_count = move_count + 1, clock_ms_1 = $3, clock_ms_2 = $4, \
             turn_started_at = $5, status = CASE WHEN $6::text IS NULL THEN status ELSE 'finished' END, \
             winner_seat = $7, end_reason = $6, finished_at = CASE WHEN $6::text IS NULL THEN NULL ELSE $5 END \
             WHERE id = $1 AND status = 'active' AND move_count = $8 RETURNING id",
        )
        .bind(game_id)
        .bind(cell_byte)
        .bind(i32::try_from(clocks[0]).map_err(|_| GameError::Corrupt)?)
        .bind(i32::try_from(clocks[1]).map_err(|_| GameError::Corrupt)?)
        .bind(now)
        .bind(result.map(|r| r.reason.as_str()))
        .bind(result.and_then(|r| r.winner).map(|s| i16::from(s.number())))
        .bind(small(game.move_count)?)
        .fetch_optional(&mut *tx)
        .await?;
        if updated.is_none() {
            return Err(GameError::StaleMove);
        }
        if result.is_some() {
            record_finished(&mut tx, game_id).await?;
            release_slots(&mut tx, game_id).await?;
        }
        let state = snapshot(&mut tx, game_id, player_id, now).await?;
        tx.commit().await?;
        Ok(state)
    }

    /// Resigns the game: allowed any time while it is active (before the first move, out of turn, before the clock
    /// started). A loss on time wins over a resignation; repeating the resignation is not an error.
    pub async fn resign(
        &self,
        player_id: Uuid,
        game_id: Uuid,
        now: OffsetDateTime,
    ) -> Result<Snapshot, GameError> {
        let mut tx = self.pool().begin().await?;
        let (game, seat) = lock_for_play(&mut tx, game_id, player_id).await?;
        if game.end_reason == Some(EndReason::Resignation) && game.winner != Some(seat) {
            return snapshot(&mut tx, game_id, player_id, now).await;
        }
        if settle_game(&mut tx, game_id, now).await? {
            tx.commit().await?;
            return Err(GameError::GameFinished);
        }
        let result = match resolve_resignation(game.status, seat) {
            Ok(result) => result,
            Err(ResignRejection::Finished) => return Err(GameError::GameFinished),
            Err(ResignRejection::NotActive) => return Err(GameError::GameNotActive),
        };
        let updated = sqlx::query(
            "UPDATE games SET status = 'finished', winner_seat = $2, end_reason = 'resignation', finished_at = $3 \
             WHERE id = $1 AND status = 'active' RETURNING id",
        )
        .bind(game_id)
        .bind(result.winner.map(|s| i16::from(s.number())))
        .bind(now)
        .fetch_optional(&mut *tx)
        .await?;
        if updated.is_none() {
            return Err(GameError::GameFinished);
        }
        record_finished(&mut tx, game_id).await?;
        release_slots(&mut tx, game_id).await?;
        let state = snapshot(&mut tx, game_id, player_id, now).await?;
        tx.commit().await?;
        Ok(state)
    }
}
