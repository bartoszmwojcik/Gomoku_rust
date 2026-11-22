//! The JSON a client gets about a game (the poll and the answers of moves and resignation).

use gomoku_core::clock::{ClockView, clock_view};
use gomoku_core::game::{BoardRow, board_view, closed_reason, display_name_of};
use gomoku_core::invitation_code::{format_invitation_code, invitation_url};
use gomoku_core::paper::{Five, paper_turn_seat, score_game};
use gomoku_core::rematch::{RematchStatus, StoredRematchRow, rematch_view};
use gomoku_core::types::{GameStatus, GameVariant, RematchState, Seat};
use gomoku_db::OffsetDateTime;
use gomoku_db::games::{GameError, GameRow, PaperSnapshot, Snapshot, SpectatorState, to_ms};
use serde_json::{Value, json};

/// The bot's name and strength for a seat of kind `bot`: `{ name, bot: { level, memoryMb, tableMb } }`.
pub fn bot_json(player: &gomoku_db::games::PlayerRow) -> Option<Value> {
    use gomoku_core::bot::choose_move::BotLevel;
    use gomoku_core::bot::profile::{bot_display_name, bot_table_mb};
    let level = BotLevel::from_number(i64::from(player.bot_level?))?;
    let memory = player.bot_memory_mb.map(i64::from);
    Some(json!({
        "name": bot_display_name(level, memory),
        "bot": { "level": level.number(), "memoryMb": memory, "tableMb": bot_table_mb(level, memory) },
    }))
}

/// ISO 8601 UTC with milliseconds, like `Date.prototype.toISOString`.
pub fn iso_ms(at: OffsetDateTime) -> String {
    let at = at.to_offset(time::UtcOffset::UTC);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        at.year(),
        u8::from(at.month()),
        at.day(),
        at.hour(),
        at.minute(),
        at.second(),
        at.millisecond()
    )
}

fn rematch_json(snapshot: &Snapshot, now_ms: i64) -> Option<Value> {
    let game = &snapshot.game;
    let row = StoredRematchRow {
        state: game.rematch_state.as_deref().and_then(RematchState::parse),
        offered_by_seat: game.rematch_offered_by,
        expires_at_ms: game.rematch_expires_at.map(to_ms),
        game_id: game.rematch_game_id.map(|id| id.to_string()),
    };
    let view = rematch_view(&row, now_ms)?;
    let state = match view.state {
        RematchStatus::Pending => "pending",
        RematchStatus::Accepted => "accepted",
        RematchStatus::Declined => "declined",
        RematchStatus::Expired => "expired",
    };
    Some(json!({
        "state": state,
        "offeredBySeat": view.offered_by_seat.number(),
        "expiresAt": iso_ms(gomoku_db::games::from_ms(view.expires_at_ms)),
        "gameId": view.game_id,
    }))
}

/// The variant-dependent part of a view: the board (classic) or the sheet (paper), the result and the clock.
struct Parts {
    move_count: usize,
    black_seat: Option<Seat>,
    turn_seat: Option<Seat>,
    moves: Value,
    result: Value,
    clock: ClockView,
    paper: Option<Value>,
}

fn five_json(five: &Five) -> Value {
    json!({
        "seat": five.seat.number(),
        "from": { "x": five.from.0, "y": five.from.1 },
        "to": { "x": five.to.0, "y": five.to.1 },
    })
}

fn paper_parts(game: &GameRow, paper: &PaperSnapshot, after: usize, now: OffsetDateTime) -> Parts {
    let scoring = score_game(&paper.stones());
    let moves: Vec<Value> = paper
        .moves
        .iter()
        .filter(|m| usize::try_from(m.n).is_ok_and(|n| n > after))
        .map(|m| {
            json!({
                "n": m.n, "x": m.x, "y": m.y, "seat": m.seat.number(), "auto": m.auto, "at": iso_ms(m.at),
            })
        })
        .collect();
    let turn_seat = match (game.status, game.black_seat) {
        (GameStatus::Active, Some(black)) => Some(paper_turn_seat(paper.move_count, black)),
        _ => None,
    };
    let result = match (game.status, game.end_reason) {
        (GameStatus::Finished, Some(reason)) => json!({
            "winnerSeat": game.winner.map(|s| s.number()),
            "reason": reason.as_str(),
            "winLine": null,
            "scores": scoring.scores,
        }),
        _ => Value::Null,
    };
    Parts {
        move_count: paper.move_count,
        black_seat: game.black_seat,
        turn_seat,
        moves: json!([]),
        result,
        clock: clock_view(&paper.clock_state(game), to_ms(now)),
        paper: Some(json!({
            "moveCount": paper.move_count,
            "moves": moves,
            "scores": scoring.scores,
            "fives": scoring.fives.iter().map(five_json).collect::<Vec<_>>(),
            "autoSeat": paper.auto_seat.map(|s| s.number()),
            "autoMoveAt": paper.pending_auto_at.map(iso_ms),
        })),
    }
}

fn classic_parts(game: &GameRow, after: usize, now: OffsetDateTime) -> Result<Parts, GameError> {
    let board = board_view(
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
    let result = board.result.as_ref().map_or(Value::Null, |r| {
        json!({
            "winnerSeat": r.winner.map(|s| s.number()),
            "reason": r.reason.as_str(),
            "winLine": r.win_line,
        })
    });
    Ok(Parts {
        move_count: board.move_count,
        black_seat: board.black_seat,
        turn_seat: board.turn_seat,
        moves: json!(board.moves),
        result,
        clock: clock_view(&game.clock_state(), to_ms(now)),
        paper: None,
    })
}

fn parts_of(
    game: &GameRow,
    paper: Option<&PaperSnapshot>,
    after: usize,
    now: OffsetDateTime,
) -> Result<Parts, GameError> {
    match (game.variant, paper) {
        (GameVariant::Paper, Some(paper)) => Ok(paper_parts(game, paper, after, now)),
        (GameVariant::Paper, None) => Err(GameError::Corrupt),
        (GameVariant::Classic, _) => classic_parts(game, after, now),
    }
}

/// Builds the view of a snapshot for a client that already knows the first `after` moves. `origin` is the public origin
/// of the app (for the invitation link).
pub fn game_view(
    snapshot: &Snapshot,
    after: usize,
    now: OffsetDateTime,
    origin: &str,
) -> Result<Value, GameError> {
    let game = &snapshot.game;
    let now_ms = to_ms(now);
    let parts = parts_of(game, snapshot.paper.as_ref(), after, now)?;
    let players: Vec<Value> = snapshot
        .players
        .iter()
        .map(|p| {
            let mut player = json!({
                "seat": p.seat.number(),
                "kind": p.kind,
                "displayName": display_name_of(p.is_guest(), p.seat, p.nickname.as_deref(), p.display_name.as_deref(), p.current_name.as_deref()),
                "seen": p.seen,
            });
            if let Some(bot) = bot_json(p) {
                player["displayName"] = bot["name"].clone();
                player["bot"] = bot["bot"].clone();
            }
            player
        })
        .collect();
    let me = snapshot.players.iter().find(|p| p.seat == snapshot.me);
    let mut view = json!({
        "status": game.status.as_str(),
        "variant": game.variant.as_str(),
        "timeControlMs": game.time_control_ms,
        "moveCount": parts.move_count,
        "blackSeat": parts.black_seat.map(|s| s.number()),
        "turnSeat": parts.turn_seat.map(|s| s.number()),
        "moves": parts.moves,
        "result": parts.result,
        "clock": {
            "remainingMs": parts.clock.remaining_ms,
            "running": parts.clock.running.map(|s| s.number()),
            "now": parts.clock.now,
        },
        "players": players,
        "you": { "seat": snapshot.me.number(), "kind": me.map(|p| p.kind.clone()) },
    });
    let object = view.as_object_mut().ok_or(GameError::Corrupt)?;
    if let Some(paper) = parts.paper {
        object.insert("paper".into(), paper);
    }
    if let Some(invitation) = snapshot
        .invitation
        .as_ref()
        .filter(|_| game.status == GameStatus::Waiting)
    {
        object.insert(
            "invitation".into(),
            json!({
                "code": format_invitation_code(&invitation.code),
                "url": invitation_url(origin, &invitation.code),
                "expiresAt": iso_ms(invitation.expires_at),
            }),
        );
    }
    if game.status == GameStatus::Cancelled
        && let Some(reason) = closed_reason(
            snapshot.players.len(),
            snapshot.invitation.as_ref().map(|i| i.consumed),
        )
    {
        object.insert("closedReason".into(), json!(reason.as_str()));
    }
    if let Some(count) = snapshot.audience {
        object.insert("audience".into(), json!({ "spectators": count }));
    }
    if snapshot.me == gomoku_core::types::Seat::One
        && game.is_public
        && matches!(game.status, GameStatus::Active | GameStatus::Finished)
    {
        object.insert("canCloseAudience".into(), json!(true));
    }
    if game.audience_closed {
        object.insert("audienceClosed".into(), json!(true));
    }
    if let Some(rematch) = rematch_json(snapshot, now_ms) {
        object.insert("rematch".into(), rematch);
    }
    if let (GameStatus::Finished, Some(at)) = (game.status, game.finished_at) {
        object.insert("finishedAt".into(), json!(iso_ms(at)));
    }
    if game.origin == "rematch" {
        object.insert("fromRematch".into(), json!(true));
    }
    Ok(view)
}

/// What a spectator reads: the public part of the game, without the viewer's seat, the invitation, the rematch or any
/// `seen` mark.
pub fn spectator_view(
    state: &SpectatorState,
    after: usize,
    now: OffsetDateTime,
) -> Result<Value, GameError> {
    let game = &state.game;
    let parts = parts_of(game, state.paper.as_ref(), after, now)?;
    let players: Vec<Value> = state
        .players
        .iter()
        .map(|p| {
            json!({
                "seat": p.seat.number(),
                "kind": p.kind,
                "displayName": display_name_of(p.is_guest(), p.seat, p.nickname.as_deref(), p.display_name.as_deref(), p.current_name.as_deref()),
            })
        })
        .collect();
    let mut view = json!({
        "status": game.status.as_str(),
        "variant": game.variant.as_str(),
        "timeControlMs": game.time_control_ms,
        "moveCount": parts.move_count,
        "blackSeat": parts.black_seat.map(|s| s.number()),
        "turnSeat": parts.turn_seat.map(|s| s.number()),
        "moves": parts.moves,
        "result": parts.result,
        "clock": {
            "remainingMs": parts.clock.remaining_ms,
            "running": parts.clock.running.map(|s| s.number()),
            "now": parts.clock.now,
        },
        "players": players,
        "canChat": state.can_chat,
    });
    let object = view.as_object_mut().ok_or(GameError::Corrupt)?;
    if let Some(paper) = parts.paper {
        object.insert("paper".into(), paper);
    }
    if let Some(count) = state.audience {
        object.insert("audience".into(), json!({ "spectators": count }));
    }
    if let (GameStatus::Finished, Some(at)) = (game.status, game.finished_at) {
        object.insert("finishedAt".into(), json!(iso_ms(at)));
    }
    Ok(view)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_times_have_milliseconds_and_a_z() {
        let at = OffsetDateTime::from_unix_timestamp_nanos(1_800_000_000_123_456_789).unwrap();
        assert_eq!(iso_ms(at), "2027-01-15T08:00:00.123Z");
        assert_eq!(
            iso_ms(OffsetDateTime::UNIX_EPOCH),
            "1970-01-01T00:00:00.000Z"
        );
    }
}
