//! The paper and pencil variant in the database: the sheet lives in `paper_moves`, the counters in `paper_game_states`.
//! Time is settled lazily like in the classic game, but the end is a score and the leader may be taken over by the
//! automat, so a settlement can also materialise stones.

use gomoku_core::clock::{ClockState, spend};
use gomoku_core::paper::{
    AUTO_MOVE_DELAY_MS, PaperClockState, Refusal, Stone, clock_seat, refusal, resolve_paper_clock,
    score_game,
};
use gomoku_core::rules::seat_on_turn;
use gomoku_core::types::{GameStatus, GameVariant, Seat};
use sqlx::{PgConnection, Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use super::{
    GameError, GameRow, Snapshot, from_ms, read_game, record_finished, release_slots, seat_of,
    settle_game, snapshot, to_ms,
};

/// One stone of the sheet with the moment it was placed (an automat stone: the moment it was due).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaperMoveRow {
    pub n: i32,
    pub x: i32,
    pub y: i32,
    pub seat: Seat,
    pub auto: bool,
    pub at: OffsetDateTime,
}

/// A paper game's sheet and counters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaperSnapshot {
    pub move_count: usize,
    pub auto_seat: Option<Seat>,
    pub pending_auto_at: Option<OffsetDateTime>,
    pub moves: Vec<PaperMoveRow>,
}

impl PaperSnapshot {
    pub fn stones(&self) -> Vec<Stone> {
        self.moves
            .iter()
            .map(|m| Stone {
                x: m.x,
                y: m.y,
                seat: m.seat,
                auto: m.auto,
            })
            .collect()
    }

    /// The seat on turn and the seat whose clock runs, for a game with this black seat.
    pub fn clock_seat(&self, black: Seat) -> Seat {
        clock_seat(self.move_count, black, self.auto_seat)
    }

    /// The clock as the pure rules see it: the running seat exists only in an active game.
    pub fn clock_state(&self, game: &GameRow) -> ClockState {
        let turn_seat = match (game.status, game.black_seat) {
            (GameStatus::Active, Some(black)) => Some(self.clock_seat(black)),
            _ => None,
        };
        ClockState {
            clock_ms: game.clock_ms,
            turn_started_at_ms: game.turn_started_at.map(to_ms),
            turn_seat,
            time_control_ms: Some(game.time_control_ms),
        }
    }
}

/// A uniform number below `len` from the operating system's generator (what the automat picks its cell with).
pub(crate) fn random_below(len: usize) -> usize {
    let bytes = Uuid::new_v4().as_u128();
    (bytes % len.max(1) as u128) as usize
}

/// Creates the (empty) paper state of a new game; called in the same transaction as the game's insert.
pub(crate) async fn insert_paper_state(
    conn: &mut PgConnection,
    game_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO paper_game_states (game_id) VALUES ($1)")
        .bind(game_id)
        .execute(conn)
        .await?;
    Ok(())
}

/// Loads the sheet; `None` for a game that is not a paper game.
pub(crate) async fn load_paper(
    conn: &mut PgConnection,
    game_id: Uuid,
) -> Result<Option<PaperSnapshot>, GameError> {
    let Some(state) = sqlx::query(
        "SELECT move_count, auto_seat, pending_auto_at FROM paper_game_states WHERE game_id = $1",
    )
    .bind(game_id)
    .fetch_optional(&mut *conn)
    .await?
    else {
        return Ok(None);
    };
    let rows = sqlx::query(
        "SELECT move_number, x, y, seat, is_auto, played_at FROM paper_moves WHERE game_id = $1 ORDER BY move_number",
    )
    .bind(game_id)
    .fetch_all(&mut *conn)
    .await?;
    let moves = rows
        .iter()
        .map(|r| -> Result<PaperMoveRow, GameError> {
            Ok(PaperMoveRow {
                n: r.try_get("move_number")?,
                x: i32::from(r.try_get::<i16, _>("x")?),
                y: i32::from(r.try_get::<i16, _>("y")?),
                seat: seat_of(Some(r.try_get("seat")?)).ok_or(GameError::Corrupt)?,
                auto: r.try_get("is_auto")?,
                at: r.try_get("played_at")?,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let move_count: i32 = state.try_get("move_count")?;
    Ok(Some(PaperSnapshot {
        move_count: usize::try_from(move_count).map_err(|_| GameError::Corrupt)?,
        auto_seat: seat_of(state.try_get("auto_seat")?),
        pending_auto_at: state.try_get("pending_auto_at")?,
        moves,
    }))
}

/// Whether something is due: the running clock ran out or the automat's pending stone is due. A normal poll stays a
/// plain read; the game is locked only when this says yes.
pub(crate) async fn paper_is_due(
    conn: &mut PgConnection,
    game_id: Uuid,
    now: OffsetDateTime,
) -> Result<bool, GameError> {
    let Some(game) = read_game(conn, game_id).await? else {
        return Ok(false);
    };
    let (GameStatus::Active, GameVariant::Paper, Some(black), Some(started)) = (
        game.status,
        game.variant,
        game.black_seat,
        game.turn_started_at,
    ) else {
        return Ok(false);
    };
    let Some(paper) = load_paper(conn, game_id).await? else {
        return Ok(false);
    };
    let seat = paper.clock_seat(black);
    let deadline = to_ms(started) + game.clock_ms[seat.index()];
    let due = paper
        .pending_auto_at
        .map_or(deadline, |pending| deadline.min(to_ms(pending)));
    Ok(to_ms(now) >= due)
}

/// Settles what is due in a paper game whose row the caller has locked: stones of the automat, and the end by points.
/// Returns whether the game finished.
pub(crate) async fn settle_paper(
    conn: &mut PgConnection,
    game_id: Uuid,
    now: OffsetDateTime,
) -> Result<bool, GameError> {
    let Some(game) = read_game(conn, game_id).await? else {
        return Ok(false);
    };
    let (GameStatus::Active, GameVariant::Paper, Some(black), Some(started)) = (
        game.status,
        game.variant,
        game.black_seat,
        game.turn_started_at,
    ) else {
        return Ok(false);
    };
    let Some(paper) = load_paper(conn, game_id).await? else {
        return Ok(false);
    };
    let state = PaperClockState {
        stones: paper.stones(),
        clock_ms: game.clock_ms,
        turn_seat: paper.clock_seat(black),
        turn_started_at_ms: to_ms(started),
        auto_seat: paper.auto_seat,
        pending_auto_at_ms: paper.pending_auto_at.map(to_ms),
    };
    let resolved = resolve_paper_clock(&state, to_ms(now), &mut random_below);
    if resolved.auto_moves.is_empty() && resolved.finish.is_none() {
        return Ok(false);
    }
    for (index, mv) in resolved.auto_moves.iter().enumerate() {
        let number = i32::try_from(paper.move_count + 1 + index).map_err(|_| GameError::Corrupt)?;
        sqlx::query(
            "INSERT INTO paper_moves (game_id, move_number, x, y, seat, is_auto, played_at) VALUES ($1, $2, $3, $4, $5, true, $6)",
        )
        .bind(game_id)
        .bind(number)
        .bind(mv.x as i16)
        .bind(mv.y as i16)
        .bind(i16::from(mv.seat.number()))
        .bind(from_ms(mv.at_ms))
        .execute(&mut *conn)
        .await?;
    }
    let scores = score_game(&resolved.state.stones).scores;
    let new_count = i32::try_from(paper.move_count + resolved.auto_moves.len())
        .map_err(|_| GameError::Corrupt)?;
    sqlx::query(
        "UPDATE paper_game_states SET move_count = $2, score_1 = $3, score_2 = $4, auto_seat = $5, pending_auto_at = NULL \
         WHERE game_id = $1",
    )
    .bind(game_id)
    .bind(new_count)
    .bind(scores[0] as i32)
    .bind(scores[1] as i32)
    .bind(resolved.state.auto_seat.map(|s| i16::from(s.number())))
    .execute(&mut *conn)
    .await?;
    if let (None, Some(auto)) = (paper.auto_seat, resolved.state.auto_seat) {
        // The take-over: the automat's own clock is spent for good and the other player's runs without a break from here.
        let sql = format!(
            "UPDATE games SET clock_ms_{} = 0, turn_started_at = $2 WHERE id = $1 AND status = 'active'",
            auto.number()
        );
        sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(game_id)
            .bind(from_ms(resolved.state.turn_started_at_ms))
            .execute(&mut *conn)
            .await?;
    }
    let Some(finish) = resolved.finish else {
        return Ok(false);
    };
    let sql = format!(
        "UPDATE games SET status = 'finished', winner_seat = $2, end_reason = 'points', finished_at = $3, clock_ms_{} = 0 \
         WHERE id = $1 AND status = 'active' RETURNING id",
        resolved.state.turn_seat.number()
    );
    let finished = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(game_id)
        .bind(finish.winner.map(|s| i16::from(s.number())))
        .bind(from_ms(finish.at_ms))
        .fetch_optional(&mut *conn)
        .await?;
    if finished.is_none() {
        return Ok(false);
    }
    record_finished(conn, game_id).await?;
    release_slots(conn, game_id).await?;
    Ok(true)
}

/// A paper move as the client sent it: absolute coordinates and the number of the move it means to play.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaperMove {
    pub x: i32,
    pub y: i32,
    pub move_number: usize,
}

fn small(value: i32) -> Result<i16, GameError> {
    i16::try_from(value).map_err(|_| GameError::Corrupt)
}

async fn paper_move_inner(
    conn: &mut PgConnection,
    game_id: Uuid,
    player_id: Uuid,
    seat: Seat,
    mv: PaperMove,
    now: OffsetDateTime,
) -> Result<Snapshot, GameError> {
    let number = i32::try_from(mv.move_number).map_err(|_| GameError::Corrupt)?;
    // The answer to a repeat of the move just played is the state, with nothing written.
    let same =
        sqlx::query("SELECT x, y, seat FROM paper_moves WHERE game_id = $1 AND move_number = $2")
            .bind(game_id)
            .bind(number)
            .fetch_optional(&mut *conn)
            .await?;
    if let Some(row) = same
        && i32::from(row.try_get::<i16, _>("x")?) == mv.x
        && i32::from(row.try_get::<i16, _>("y")?) == mv.y
        && seat_of(Some(row.try_get("seat")?)) == Some(seat)
    {
        return snapshot(conn, game_id, player_id, now).await;
    }
    if settle_game(conn, game_id, now).await? {
        return Err(GameError::GameFinished);
    }
    let game = read_game(conn, game_id).await?.ok_or(GameError::NotFound)?;
    match game.status {
        GameStatus::Finished => return Err(GameError::GameFinished),
        GameStatus::Active => {}
        _ => return Err(GameError::GameNotActive),
    }
    let black = game.black_seat.ok_or(GameError::GameNotActive)?;
    if game.turn_started_at.is_none() {
        return Err(GameError::GameNotStarted);
    }
    let paper = load_paper(conn, game_id).await?.ok_or(GameError::Corrupt)?;
    let on_turn = seat_on_turn(paper.move_count, black);
    if on_turn != seat || paper.auto_seat == Some(seat) {
        return Err(GameError::NotYourTurn {
            auto: paper.auto_seat == Some(on_turn),
        });
    }
    if mv.move_number != paper.move_count + 1 {
        return Err(GameError::StaleMove);
    }
    let mut stones = paper.stones();
    match refusal(&stones, mv.x, mv.y) {
        Some(Refusal::OffBoard) => return Err(GameError::OffBoard),
        Some(Refusal::CellOccupied) => return Err(GameError::CellOccupied),
        Some(Refusal::FirstMoveCenter) => return Err(GameError::FirstMoveCenter),
        Some(Refusal::MoveTooFar) => return Err(GameError::MoveTooFar),
        None => {}
    }
    stones.push(Stone {
        x: mv.x,
        y: mv.y,
        seat,
        auto: false,
    });
    let scores = score_game(&stones).scores;
    let pending = paper
        .auto_seat
        .map(|_| now + time::Duration::milliseconds(AUTO_MOVE_DELAY_MS));
    let counted = sqlx::query(
        "UPDATE paper_game_states SET move_count = move_count + 1, score_1 = $2, score_2 = $3, pending_auto_at = $4 \
         WHERE game_id = $1 AND move_count = $5 RETURNING game_id",
    )
    .bind(game_id)
    .bind(scores[0] as i32)
    .bind(scores[1] as i32)
    .bind(pending)
    .bind(i32::try_from(paper.move_count).map_err(|_| GameError::Corrupt)?)
    .fetch_optional(&mut *conn)
    .await?;
    if counted.is_none() {
        return Err(GameError::StaleMove);
    }
    sqlx::query(
        "INSERT INTO paper_moves (game_id, move_number, x, y, seat, is_auto, played_at) VALUES ($1, $2, $3, $4, $5, false, $6)",
    )
    .bind(game_id)
    .bind(number)
    .bind(small(mv.x)?)
    .bind(small(mv.y)?)
    .bind(i16::from(seat.number()))
    .bind(now)
    .execute(&mut *conn)
    .await
    .map_err(|error| {
        if error.as_database_error().is_some_and(|db| db.is_unique_violation()) {
            GameError::StaleMove
        } else {
            error.into()
        }
    })?;
    if paper.auto_seat.is_none() {
        // The mover's time is charged at the moment the move arrived; the opponent's clock starts now. In the automat
        // phase nothing is charged: the human's clock runs without a break from the take-over.
        let spent = spend(&paper.clock_state(&game), seat, to_ms(now));
        if spent.expired {
            return Err(GameError::GameFinished);
        }
        let mut clocks = game.clock_ms;
        clocks[seat.index()] = spent.clock_ms;
        sqlx::query(
            "UPDATE games SET clock_ms_1 = $2, clock_ms_2 = $3, turn_started_at = $4 WHERE id = $1 AND status = 'active'",
        )
        .bind(game_id)
        .bind(i32::try_from(clocks[0]).map_err(|_| GameError::Corrupt)?)
        .bind(i32::try_from(clocks[1]).map_err(|_| GameError::Corrupt)?)
        .bind(now)
        .execute(&mut *conn)
        .await?;
    }
    snapshot(conn, game_id, player_id, now).await
}

/// Plays a stone of a paper game inside the transaction the caller opened and locked. Refusals are committed (a settlement
/// found on the way, such as stones of the automat, must not be rolled back with them); only database failures roll back.
pub(crate) async fn play_paper_move(
    mut tx: Transaction<'static, Postgres>,
    game_id: Uuid,
    player_id: Uuid,
    seat: Seat,
    mv: PaperMove,
    now: OffsetDateTime,
) -> Result<Snapshot, GameError> {
    match paper_move_inner(&mut tx, game_id, player_id, seat, mv, now).await {
        Ok(state) => {
            tx.commit().await?;
            Ok(state)
        }
        Err(GameError::Db(error)) => Err(GameError::Db(error)),
        Err(refusal) => {
            tx.commit().await?;
            Err(refusal)
        }
    }
}
