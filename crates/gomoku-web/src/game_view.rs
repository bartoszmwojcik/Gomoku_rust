//! The JSON a client gets about a game (the poll and the answers of moves and resignation).

use gomoku_core::clock::clock_view;
use gomoku_core::game::{BoardRow, board_view, closed_reason, display_name_of};
use gomoku_core::invitation_code::{format_invitation_code, invitation_url};
use gomoku_core::rematch::{RematchStatus, StoredRematchRow, rematch_view};
use gomoku_core::types::{GameStatus, RematchState};
use gomoku_db::OffsetDateTime;
use gomoku_db::games::{GameError, Snapshot, to_ms};
use serde_json::{Value, json};

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
    let clock = clock_view(&game.clock_state(), now_ms);
    let result = board.result.as_ref().map(|r| {
        json!({
            "winnerSeat": r.winner.map(|s| s.number()),
            "reason": r.reason.as_str(),
            "winLine": r.win_line,
        })
    });
    let players: Vec<Value> = snapshot
        .players
        .iter()
        .map(|p| {
            json!({
                "seat": p.seat.number(),
                "kind": p.kind,
                "displayName": display_name_of(p.is_guest(), p.seat, p.nickname.as_deref(), p.display_name.as_deref(), p.current_name.as_deref()),
                "seen": p.seen,
            })
        })
        .collect();
    let me = snapshot.players.iter().find(|p| p.seat == snapshot.me);
    let mut view = json!({
        "status": game.status.as_str(),
        "variant": "classic",
        "timeControlMs": 600_000,
        "moveCount": board.move_count,
        "blackSeat": board.black_seat.map(|s| s.number()),
        "turnSeat": board.turn_seat.map(|s| s.number()),
        "moves": board.moves,
        "result": result,
        "clock": {
            "remainingMs": clock.remaining_ms,
            "running": clock.running.map(|s| s.number()),
            "now": clock.now,
        },
        "players": players,
        "you": { "seat": snapshot.me.number(), "kind": me.map(|p| p.kind.clone()) },
    });
    let object = view.as_object_mut().ok_or(GameError::Corrupt)?;
    if let Some(invitation) = &snapshot.invitation {
        if game.status == GameStatus::Waiting {
            object.insert(
                "invitation".into(),
                json!({
                    "code": format_invitation_code(&invitation.code),
                    "url": invitation_url(origin, &invitation.code),
                    "expiresAt": iso_ms(invitation.expires_at),
                }),
            );
        }
        if game.status == GameStatus::Cancelled {
            // handled below with the closed reason
        }
    }
    if game.status == GameStatus::Cancelled
        && let Some(reason) = closed_reason(
            snapshot.players.len(),
            snapshot.invitation.as_ref().map(|i| i.consumed),
        )
    {
        object.insert("closedReason".into(), json!(reason.as_str()));
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
