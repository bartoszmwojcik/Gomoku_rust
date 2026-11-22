//! What an agent is told about its game: a compact view (board as text rows, no names, no ids of the other side) and the
//! chat part of `get_state`.

use gomoku_core::agent::{AGENT_CHAT_NOTICE, MCP_RETRY_HINT_MS};
use gomoku_core::board::decode_moves;
use gomoku_core::chat::ChatState;
use gomoku_core::clock::clock_view;
use gomoku_core::game::{BoardRow, board_view};
use gomoku_core::paper::{PAPER_BOARD_SIZE, PAPER_CENTER, PAPER_MAX_GAP, score_game};
use gomoku_core::types::{GameStatus, GameVariant, Seat};
use gomoku_db::OffsetDateTime;
use gomoku_db::games::{ChatRead, GameError, Snapshot, to_ms};
use serde_json::{Value, json};

use crate::game_view::iso_ms;

const SIZE: usize = 15;

fn color_name(black_seat: Option<Seat>, seat: Seat) -> Value {
    black_seat.map_or(Value::Null, |black| {
        json!(if black == seat { "black" } else { "white" })
    })
}

fn side(me: Seat, seat: Option<Seat>) -> Value {
    seat.map_or(Value::Null, |s| {
        json!(if s == me { "you" } else { "opponent" })
    })
}

fn outcome(me: Seat, winner: Option<Seat>) -> &'static str {
    match winner {
        None => "draw",
        Some(seat) if seat == me => "win",
        Some(_) => "loss",
    }
}

/// The parts that depend on the variant.
struct Parts {
    move_count: usize,
    board: Vec<String>,
    board_origin: Option<(i32, i32)>,
    moves: Vec<Value>,
    turn_seat: Option<Seat>,
    result: Value,
    clock: Value,
    /// Milliseconds until the paper automat places its next stone.
    auto_in_ms: Option<i64>,
    extra: Vec<(&'static str, Value)>,
}

fn classic_parts(
    snapshot: &Snapshot,
    after: usize,
    now: OffsetDateTime,
) -> Result<Parts, GameError> {
    let game = &snapshot.game;
    let me = snapshot.me;
    let view = board_view(
        &BoardRow {
            status: game.status,
            black_seat: game.black_seat,
            move_count: game.move_count,
            moves: &game.moves,
            winner: game.winner,
            end_reason: game.end_reason,
        },
        after,
    )
    .map_err(|_| GameError::Corrupt)?;
    let all = decode_moves(&game.moves).map_err(|_| GameError::Corrupt)?;
    let mut grid = vec![vec!['.'; SIZE]; SIZE];
    for (index, &cell) in all.iter().enumerate() {
        grid[cell / SIZE][cell % SIZE] = if index % 2 == 0 { 'B' } else { 'W' };
    }
    let moves = view
        .moves
        .iter()
        .enumerate()
        .map(|(offset, &cell)| {
            let n = after + offset + 1;
            json!({
                "n": n, "x": cell % SIZE, "y": cell / SIZE,
                "color": if n % 2 == 1 { "black" } else { "white" },
            })
        })
        .collect();
    let result = view.result.as_ref().map_or(Value::Null, |r| {
        let mut result = json!({ "outcome": outcome(me, r.winner), "reason": r.reason.as_str() });
        if let Some(line) = &r.win_line {
            result["winLine"] = json!(
                line.iter()
                    .map(|&c| json!({ "x": c % SIZE, "y": c / SIZE }))
                    .collect::<Vec<_>>()
            );
        }
        result
    });
    let clock = clock_view(&game.clock_state(), to_ms(now));
    Ok(Parts {
        move_count: view.move_count,
        board: grid.iter().map(|row| row.iter().collect()).collect(),
        board_origin: None,
        moves,
        turn_seat: view.turn_seat,
        result,
        clock: clock_json(me, &clock),
        auto_in_ms: None,
        extra: Vec::new(),
    })
}

fn clock_json(me: Seat, clock: &gomoku_core::clock::ClockView) -> Value {
    json!({
        "youMs": clock.remaining_ms[me.index()],
        "opponentMs": clock.remaining_ms[me.other().index()],
        "runningSide": side(me, clock.running),
        "serverNow": clock.now,
    })
}

fn paper_parts(snapshot: &Snapshot, after: usize, now: OffsetDateTime) -> Result<Parts, GameError> {
    let game = &snapshot.game;
    let me = snapshot.me;
    let paper = snapshot.paper.as_ref().ok_or(GameError::Corrupt)?;
    let stones = paper.stones();
    let size = PAPER_BOARD_SIZE as i32;
    let (min_x, max_x, min_y, max_y) = if stones.is_empty() {
        (PAPER_CENTER, PAPER_CENTER, PAPER_CENTER, PAPER_CENTER)
    } else {
        stones.iter().fold((size, -1, size, -1), |(a, b, c, d), s| {
            (a.min(s.x), b.max(s.x), c.min(s.y), d.max(s.y))
        })
    };
    let (x0, x1) = (
        (min_x - PAPER_MAX_GAP).max(0),
        (max_x + PAPER_MAX_GAP).min(size - 1),
    );
    let (y0, y1) = (
        (min_y - PAPER_MAX_GAP).max(0),
        (max_y + PAPER_MAX_GAP).min(size - 1),
    );
    let mut grid: Vec<Vec<char>> = (y0..=y1)
        .map(|_| vec!['.'; usize::try_from(x1 - x0 + 1).unwrap_or(0)])
        .collect();
    for stone in &stones {
        let ch = if Some(stone.seat) == game.black_seat {
            'B'
        } else {
            'W'
        };
        grid[usize::try_from(stone.y - y0).unwrap_or(0)]
            [usize::try_from(stone.x - x0).unwrap_or(0)] = ch;
    }
    let moves = paper
        .moves
        .iter()
        .filter(|m| usize::try_from(m.n).is_ok_and(|n| n > after))
        .map(|m| {
            json!({
                "n": m.n, "x": m.x, "y": m.y,
                "color": if Some(m.seat) == game.black_seat { "black" } else { "white" },
                "auto": m.auto,
            })
        })
        .collect();
    let turn_seat = match (game.status, game.black_seat) {
        (GameStatus::Active, Some(black)) => {
            Some(gomoku_core::paper::paper_turn_seat(paper.move_count, black))
        }
        _ => None,
    };
    let scoring = score_game(&stones);
    let result = match (game.status, game.end_reason) {
        (GameStatus::Finished, Some(reason)) => {
            json!({ "outcome": outcome(me, game.winner), "reason": reason.as_str() })
        }
        _ => Value::Null,
    };
    let clock = clock_view(&paper.clock_state(game), to_ms(now));
    let auto_in_ms = paper
        .pending_auto_at
        .map(|at| (to_ms(at) - to_ms(now)).max(0));
    Ok(Parts {
        move_count: paper.move_count,
        board: grid.iter().map(|row| row.iter().collect()).collect(),
        board_origin: Some((x0, y0)),
        moves,
        turn_seat,
        result,
        clock: clock_json(me, &clock),
        auto_in_ms,
        extra: vec![
            ("variant", json!("paper")),
            (
                "scores",
                json!({ "you": scoring.scores[me.index()], "opponent": scoring.scores[me.other().index()] }),
            ),
            (
                "autoSeat",
                paper.auto_seat.map_or(Value::Null, |s| json!(s.number())),
            ),
            (
                "autoMoveAt",
                paper
                    .pending_auto_at
                    .map_or(Value::Null, |at| json!(iso_ms(at))),
            ),
        ],
    })
}

/// The agent's view of its game for an agent that already knows the first `after` moves.
pub fn agent_view(
    snapshot: &Snapshot,
    after: usize,
    now: OffsetDateTime,
) -> Result<Value, GameError> {
    let game = &snapshot.game;
    let me = snapshot.me;
    let parts = match game.variant {
        GameVariant::Classic => classic_parts(snapshot, after, now)?,
        GameVariant::Paper => paper_parts(snapshot, after, now)?,
    };
    let auto_for_me = snapshot
        .paper
        .as_ref()
        .is_some_and(|p| p.auto_seat == Some(me));
    let turn = match parts.turn_seat {
        Some(seat) if seat == me && !auto_for_me => json!("you"),
        Some(_) => json!("opponent"),
        None => Value::Null,
    };
    let opponent_kind = snapshot
        .players
        .iter()
        .find(|p| p.seat != me)
        .map_or(Value::Null, |p| json!(p.kind));
    let retry_after = match (game.status, &turn) {
        (GameStatus::Finished | GameStatus::Cancelled, _) => Value::Null,
        (_, t) if t == "you" => json!(0),
        _ => json!(parts.auto_in_ms.map_or(MCP_RETRY_HINT_MS as i64, |ms| {
            ms.min(MCP_RETRY_HINT_MS as i64)
        })),
    };
    let mut view = json!({
        "status": game.status.as_str(),
        "you": { "seat": me.number(), "color": color_name(game.black_seat, me) },
        "opponent": { "kind": opponent_kind },
        "turn": turn,
        "nextMoveNumber": parts.move_count + 1,
        "board": parts.board,
        "moves": parts.moves,
        "clock": parts.clock,
        "result": parts.result,
        "retryAfterMs": retry_after,
    });
    if let Some(object) = view.as_object_mut() {
        if let Some((x, y)) = parts.board_origin {
            object.insert("boardOrigin".into(), json!({ "x": x, "y": y }));
        }
        for (key, value) in parts.extra {
            object.insert(key.into(), value);
        }
    }
    Ok(view)
}

/// The chat part of `get_state`. Only an open or a one-sided disabled chat shows messages.
pub fn chat_part(read: &ChatRead, me: Seat) -> Value {
    let status = match read.state {
        ChatState::DisabledByYou => ChatState::Unavailable,
        other => other,
    };
    let messages: Vec<Value> = if read.state.readable() {
        read.messages
            .iter()
            .map(|m| {
                let from = match m.seat {
                    Some(seat) if seat == me => "you",
                    Some(_) => "opponent",
                    None => "spectator",
                };
                json!({ "id": m.seq, "from": from, "text": m.body, "untrusted": m.seat != Some(me) })
            })
            .collect()
    } else {
        Vec::new()
    };
    json!({
        "chat": messages,
        "chatLastId": read.last_id,
        "chatStatus": status.as_str(),
        "chatNotice": AGENT_CHAT_NOTICE,
    })
}
