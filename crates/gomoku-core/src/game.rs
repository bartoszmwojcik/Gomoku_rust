//! Pure rules around a stored game: who may see it, how it is served, which transitions are due.
//!
//! Times are epoch milliseconds (`i64`); the services pass the application clock.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

use crate::apply_move::MoveState;
use crate::board::{Bitboard, BoardError, decode_moves};
use crate::constants::{GUEST_TOKEN_GRACE_MS, READY_DEADLINE_MS, REMATCH_TTL_MS};
use crate::rules::{check_win, seat_on_turn, turn_color};
use crate::types::{Color, EndReason, GameStatus, Seat};

/// Default `Max-Age` of the guest cookie: two hours, slid forward by every guest request.
pub const GUEST_COOKIE_MAX_AGE_SECONDS: i64 = 2 * 60 * 60;
/// A guest's rematch can only be accepted in the first 55 minutes after the game, so the proposer still has an
/// hour-long window to claim their seat of the new game.
pub const GUEST_REMATCH_FRESH_MS: i64 = GUEST_TOKEN_GRACE_MS as i64 - REMATCH_TTL_MS as i64;
/// After this the client stops polling a finished game of a guest (the token dies at the full hour).
pub const GUEST_RESULT_WINDOW_MS: i64 = GUEST_REMATCH_FRESH_MS;

/// Name of the guest cookie: `__Host-` prefixed when the cookie is `Secure`.
pub fn guest_cookie_name(secure: bool) -> &'static str {
    if secure {
        "__Host-gomoku_guest"
    } else {
        "gomoku_guest"
    }
}

/// Name of the spectator cookie: a visitor's access to a public game, never interchangeable with the guest cookie.
pub fn spectator_cookie_name(secure: bool) -> &'static str {
    if secure {
        "__Host-gomoku_spectator"
    } else {
        "gomoku_spectator"
    }
}

/// `Max-Age` of the guest cookie: for a waiting game with an invitation it lasts until the invitation expires plus the
/// token grace (the creator must see "invitation expired" instead of nothing); every other case is two hours.
pub fn guest_cookie_max_age(waiting_invitation_expires_ms: Option<i64>, now_ms: i64) -> i64 {
    match waiting_invitation_expires_ms {
        Some(expires) => {
            let remaining_ms = expires + GUEST_TOKEN_GRACE_MS as i64 - now_ms;
            ((remaining_ms + 999) / 1000).max(1)
        }
        None => GUEST_COOKIE_MAX_AGE_SECONDS,
    }
}

/// The colour draw, injected so tests are deterministic: black goes to seat 1 or 2 with equal chance.
pub fn draw_black_seat(mut random_int: impl FnMut(usize) -> usize) -> Seat {
    if random_int(2) == 0 {
        Seat::One
    } else {
        Seat::Two
    }
}

/// Whether a guest's token still opens the game: valid while the game is `active`; for a finished game until the grace
/// after `finished_at`; for a waiting or cancelled game until the grace after the latest UNCONSUMED invitation expires
/// (a game closed unplayed after a join, or cancelled by its creator, has none: the token is void at once).
pub fn is_token_valid(
    status: GameStatus,
    finished_at_ms: Option<i64>,
    unconsumed_invitation_expires_ms: Option<i64>,
    now_ms: i64,
) -> bool {
    if status == GameStatus::Active {
        return true;
    }
    let anchor = finished_at_ms.or(unconsumed_invitation_expires_ms);
    anchor.is_some_and(|at| at > now_ms - GUEST_TOKEN_GRACE_MS as i64)
}

/// An invitation is dead from the moment it expires.
pub fn is_invitation_expired(expires_ms: i64, now_ms: i64) -> bool {
    expires_ms <= now_ms
}

/// An active game where somebody never opened the page is cancelled 15 minutes after it started (at the boundary too).
pub fn is_not_opened_in_time(started_ms: i64, someone_unseen: bool, now_ms: i64) -> bool {
    someone_unseen && started_ms <= now_ms - READY_DEADLINE_MS as i64
}

/// Token of the rematch game derived from the previous one: the proposer's cookie keeps working after the cookie of the
/// old game is claimed for the new seat, and the same inputs always give the same token.
pub fn derive_rematch_token(previous_token: &str, new_game_id: &str) -> String {
    let digest = Sha256::digest(format!("rematch:{new_game_id}:{previous_token}").as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClosedReason {
    /// The creator cancelled the invitation.
    Cancelled,
    /// The invitation expired unjoined.
    Expired,
    /// Both joined but somebody never opened the game in time.
    NotOpened,
}

impl ClosedReason {
    pub fn as_str(self) -> &'static str {
        match self {
            ClosedReason::Cancelled => "cancelled",
            ClosedReason::Expired => "expired",
            ClosedReason::NotOpened => "not_opened",
        }
    }
}

/// Why a cancelled game was cancelled: two players mean it was never opened; otherwise the invitation tells whether
/// the creator voided it (consumed without a joiner) or it simply expired.
pub fn closed_reason(
    player_count: usize,
    invitation_consumed: Option<bool>,
) -> Option<ClosedReason> {
    if player_count > 1 {
        return Some(ClosedReason::NotOpened);
    }
    invitation_consumed.map(|consumed| {
        if consumed {
            ClosedReason::Cancelled
        } else {
            ClosedReason::Expired
        }
    })
}

/// The name shown for a player: a guest's nickname or the default `Gracz {seat}`; an account's snapshot, falling back
/// to the current name when the seat predates the snapshot; `None` for a deleted account.
pub fn display_name_of(
    is_guest: bool,
    seat: Seat,
    nickname: Option<&str>,
    snapshot: Option<&str>,
    current: Option<&str>,
) -> Option<String> {
    if is_guest {
        return Some(
            nickname.map_or_else(|| crate::nickname::default_nickname(seat), str::to_owned),
        );
    }
    snapshot.or(current).map(str::to_owned)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultView {
    pub winner: Option<Seat>,
    pub reason: EndReason,
    /// Cells of the winning line (ascending); only for a five.
    pub win_line: Option<Vec<usize>>,
}

/// What the API tells a client about the board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardView {
    pub move_count: usize,
    pub black_seat: Option<Seat>,
    /// Seat to move; `None` unless the game is active.
    pub turn_seat: Option<Seat>,
    /// Cells of the moves after the first `after` ones.
    pub moves: Vec<usize>,
    pub result: Option<ResultView>,
}

/// The columns of a game row that make up the board.
#[derive(Debug, Clone)]
pub struct BoardRow<'a> {
    pub status: GameStatus,
    pub black_seat: Option<Seat>,
    pub move_count: usize,
    pub moves: &'a [u8],
    pub winner: Option<Seat>,
    pub end_reason: Option<EndReason>,
}

/// Replays the move log into the two boards (black plays the even indices).
pub fn replay(black_seat: Seat, moves: &[usize]) -> MoveState {
    let mut state = MoveState::start(black_seat);
    for (index, &cell) in moves.iter().enumerate() {
        match turn_color(index) {
            Color::Black => state.black = state.black.with(cell),
            Color::White => state.white = state.white.with(cell),
        }
        state.move_count = index + 1;
    }
    state
}

/// The board part of the game state for a client that already knows the first `after` moves. The winning line is not
/// stored: it is recomputed from the winner's stones and the last move.
pub fn board_view(row: &BoardRow<'_>, after: usize) -> Result<BoardView, BoardError> {
    let moves = decode_moves(row.moves)?;
    let turn_seat = match (row.status, row.black_seat) {
        (GameStatus::Active, Some(black)) => Some(seat_on_turn(row.move_count, black)),
        _ => None,
    };
    let result = match (row.status, row.end_reason) {
        (GameStatus::Finished, Some(reason)) => Some(ResultView {
            winner: row.winner,
            reason,
            win_line: if reason == EndReason::FiveInRow {
                win_line_of(row, &moves)
            } else {
                None
            },
        }),
        _ => None,
    };
    Ok(BoardView {
        move_count: row.move_count,
        black_seat: row.black_seat,
        turn_seat,
        moves: moves
            .get(after..)
            .map(<[usize]>::to_vec)
            .unwrap_or_default(),
        result,
    })
}

fn win_line_of(row: &BoardRow<'_>, moves: &[usize]) -> Option<Vec<usize>> {
    let black_seat = row.black_seat?;
    let last = *moves.last()?;
    let state = replay(black_seat, moves);
    let own: &Bitboard = match turn_color(moves.len() - 1) {
        Color::Black => &state.black,
        Color::White => &state.white,
    };
    check_win(|cell| own.has(cell), last)
}

/// Whether `after` (the number of moves the client knows) is possible for a game with `move_count` moves.
pub fn is_valid_after(after: usize, move_count: usize) -> bool {
    after <= move_count
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::{cell_of, encode_moves};

    const HOUR: i64 = 3_600_000;

    #[test]
    fn guest_cookie_names_and_lifetimes() {
        assert_eq!(guest_cookie_name(true), "__Host-gomoku_guest");
        assert_eq!(guest_cookie_name(false), "gomoku_guest");
        assert_eq!(spectator_cookie_name(true), "__Host-gomoku_spectator");
        assert_eq!(spectator_cookie_name(false), "gomoku_spectator");
        assert_eq!(guest_cookie_max_age(None, 5), 7200);
        // 24 h invitation + 1 h grace, 100 ms short of a whole second: rounded up.
        let now = 1_000;
        assert_eq!(guest_cookie_max_age(Some(now + 24 * HOUR), now), 25 * 3600);
        assert_eq!(
            guest_cookie_max_age(Some(now + 24 * HOUR - 100), now),
            25 * 3600
        );
        assert_eq!(
            guest_cookie_max_age(Some(now - 10 * HOUR), now),
            1,
            "never below one second"
        );
        assert_eq!(GUEST_REMATCH_FRESH_MS, 55 * 60 * 1000);
        assert_eq!(GUEST_RESULT_WINDOW_MS, GUEST_REMATCH_FRESH_MS);
    }

    #[test]
    fn the_colour_draw_uses_the_injected_source() {
        assert_eq!(
            draw_black_seat(|max| {
                assert_eq!(max, 2);
                0
            }),
            Seat::One
        );
        assert_eq!(draw_black_seat(|_| 1), Seat::Two);
    }

    #[test]
    fn a_guest_token_follows_the_status_of_the_game() {
        let now = 10 * HOUR;
        // Active: always valid.
        assert!(is_token_valid(GameStatus::Active, None, None, now));
        // Finished: until finished_at + 1 h (exclusive).
        assert!(is_token_valid(
            GameStatus::Finished,
            Some(now - HOUR + 1),
            None,
            now
        ));
        assert!(!is_token_valid(
            GameStatus::Finished,
            Some(now - HOUR),
            None,
            now
        ));
        // Waiting / cancelled: until the unconsumed invitation expires + 1 h.
        assert!(is_token_valid(GameStatus::Waiting, None, Some(now), now));
        assert!(is_token_valid(
            GameStatus::Cancelled,
            None,
            Some(now - HOUR + 1),
            now
        ));
        assert!(!is_token_valid(
            GameStatus::Cancelled,
            None,
            Some(now - HOUR),
            now
        ));
        // Closed unplayed or cancelled by the creator: no unconsumed invitation, void at once.
        assert!(!is_token_valid(GameStatus::Cancelled, None, None, now));
        assert!(!is_token_valid(GameStatus::Waiting, None, None, now));
        // A finish time wins over an invitation.
        assert!(!is_token_valid(
            GameStatus::Finished,
            Some(now - 2 * HOUR),
            Some(now + HOUR),
            now
        ));
    }

    #[test]
    fn lazy_deadlines() {
        assert!(is_invitation_expired(100, 100));
        assert!(!is_invitation_expired(101, 100));
        let started = 1_000_000;
        let ready = 15 * 60 * 1000;
        assert!(is_not_opened_in_time(started, true, started + ready));
        assert!(!is_not_opened_in_time(started, true, started + ready - 1));
        assert!(!is_not_opened_in_time(started, false, started + 10 * ready));
    }

    #[test]
    fn the_rematch_token_is_deterministic_and_depends_on_both_inputs() {
        let a = derive_rematch_token("old-token", "game-1");
        assert_eq!(a, derive_rematch_token("old-token", "game-1"));
        assert_ne!(a, derive_rematch_token("old-token", "game-2"));
        assert_ne!(a, derive_rematch_token("other", "game-1"));
        assert_eq!(a.len(), 43);
        assert!(
            a.bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        );
    }

    #[test]
    fn closed_reasons() {
        assert_eq!(closed_reason(2, None), Some(ClosedReason::NotOpened));
        assert_eq!(closed_reason(2, Some(true)), Some(ClosedReason::NotOpened));
        assert_eq!(closed_reason(1, Some(true)), Some(ClosedReason::Cancelled));
        assert_eq!(closed_reason(1, Some(false)), Some(ClosedReason::Expired));
        assert_eq!(closed_reason(1, None), None);
        assert_eq!(ClosedReason::Cancelled.as_str(), "cancelled");
        assert_eq!(ClosedReason::Expired.as_str(), "expired");
        assert_eq!(ClosedReason::NotOpened.as_str(), "not_opened");
    }

    #[test]
    fn display_names() {
        assert_eq!(
            display_name_of(true, Seat::Two, None, None, None).as_deref(),
            Some("Gracz 2")
        );
        assert_eq!(
            display_name_of(true, Seat::One, Some("Ola"), None, None).as_deref(),
            Some("Ola")
        );
        assert_eq!(
            display_name_of(false, Seat::One, None, Some("Ala"), Some("Alicja")).as_deref(),
            Some("Ala")
        );
        assert_eq!(
            display_name_of(false, Seat::One, None, None, Some("Alicja")).as_deref(),
            Some("Alicja")
        );
        assert_eq!(display_name_of(false, Seat::One, None, None, None), None);
    }

    fn row(
        status: GameStatus,
        moves: &[u8],
        winner: Option<Seat>,
        reason: Option<EndReason>,
    ) -> BoardRow<'_> {
        BoardRow {
            status,
            black_seat: Some(Seat::Two),
            move_count: moves.len(),
            moves,
            winner,
            end_reason: reason,
        }
    }

    #[test]
    fn the_board_view_of_a_running_game_names_the_seat_on_turn_and_slices_the_moves() {
        let moves = encode_moves(&[112, 113, 114]).unwrap();
        let view = board_view(&row(GameStatus::Active, &moves, None, None), 1).unwrap();
        assert_eq!(
            (view.move_count, view.black_seat, view.turn_seat),
            (3, Some(Seat::Two), Some(Seat::One))
        );
        assert_eq!(view.moves, vec![113, 114]);
        assert_eq!(view.result, None);
        assert_eq!(
            board_view(&row(GameStatus::Active, &moves, None, None), 3)
                .unwrap()
                .moves,
            Vec::<usize>::new()
        );
        assert_eq!(
            board_view(&row(GameStatus::Active, &moves, None, None), 99)
                .unwrap()
                .moves,
            Vec::<usize>::new()
        );
        // No turn before the game is active, or without a black seat.
        let waiting = BoardRow {
            black_seat: None,
            ..row(GameStatus::Waiting, &[], None, None)
        };
        let view = board_view(&waiting, 0).unwrap();
        assert_eq!((view.turn_seat, view.black_seat), (None, None));
        assert_eq!(
            board_view(&row(GameStatus::Finished, &moves, None, None), 0)
                .unwrap()
                .turn_seat,
            None
        );
    }

    #[test]
    fn a_finished_game_recomputes_the_winning_line() {
        // Black (seat 2) plays row 7, x = 0..=4; white plays row 0, x = 0..=3.
        let black: Vec<usize> = (0..5).map(|x| cell_of(x, 7)).collect();
        let white: Vec<usize> = (0..4).map(|x| cell_of(x, 0)).collect();
        let mut cells = Vec::new();
        for i in 0..5 {
            cells.push(black[i]);
            if i < 4 {
                cells.push(white[i]);
            }
        }
        let bytes = encode_moves(&cells).unwrap();
        let five = board_view(
            &row(
                GameStatus::Finished,
                &bytes,
                Some(Seat::Two),
                Some(EndReason::FiveInRow),
            ),
            0,
        )
        .unwrap();
        let result = five.result.unwrap();
        assert_eq!(
            (result.winner, result.reason),
            (Some(Seat::Two), EndReason::FiveInRow)
        );
        assert_eq!(result.win_line, Some(black));
        // White winning by five: the last move is white's.
        let white_win: Vec<usize> = (0..5).map(|x| cell_of(x, 0)).collect();
        let black_stones: Vec<usize> = (0..5).map(|x| cell_of(x * 2, 7)).collect();
        let mut cells = Vec::new();
        for i in 0..5 {
            cells.push(black_stones[i]);
            cells.push(white_win[i]);
        }
        let bytes = encode_moves(&cells).unwrap();
        let view = board_view(
            &row(
                GameStatus::Finished,
                &bytes,
                Some(Seat::One),
                Some(EndReason::FiveInRow),
            ),
            0,
        )
        .unwrap();
        assert_eq!(view.result.unwrap().win_line, Some(white_win));
    }

    #[test]
    fn other_endings_have_no_winning_line_and_odd_rows_degrade() {
        let moves = encode_moves(&[1, 2]).unwrap();
        for reason in [EndReason::Timeout, EndReason::Resignation, EndReason::Draw] {
            let winner = (reason != EndReason::Draw).then_some(Seat::One);
            let view =
                board_view(&row(GameStatus::Finished, &moves, winner, Some(reason)), 0).unwrap();
            assert_eq!(view.result.unwrap().win_line, None);
        }
        // A five claimed with no moves, no black seat or no line on the board has no line to show.
        let claimed = row(
            GameStatus::Finished,
            &[],
            Some(Seat::One),
            Some(EndReason::FiveInRow),
        );
        assert_eq!(
            board_view(&claimed, 0).unwrap().result.unwrap().win_line,
            None
        );
        let no_black = BoardRow {
            black_seat: None,
            ..row(
                GameStatus::Finished,
                &moves,
                Some(Seat::One),
                Some(EndReason::FiveInRow),
            )
        };
        assert_eq!(
            board_view(&no_black, 0).unwrap().result.unwrap().win_line,
            None
        );
        let no_five = row(
            GameStatus::Finished,
            &moves,
            Some(Seat::One),
            Some(EndReason::FiveInRow),
        );
        assert_eq!(
            board_view(&no_five, 0).unwrap().result.unwrap().win_line,
            None
        );
        // A finished game without an end reason shows no result.
        assert_eq!(
            board_view(&row(GameStatus::Finished, &moves, None, None), 0)
                .unwrap()
                .result,
            None
        );
    }

    #[test]
    fn a_corrupt_move_log_is_an_error_and_after_is_bounded() {
        assert!(board_view(&row(GameStatus::Active, &[225], None, None), 0).is_err());
        assert!(is_valid_after(0, 0) && is_valid_after(5, 5));
        assert!(!is_valid_after(6, 5));
    }

    #[test]
    fn replay_puts_black_on_the_even_moves() {
        let state = replay(Seat::One, &[10, 11, 12]);
        assert!(state.black.has(10) && state.black.has(12) && state.white.has(11));
        assert_eq!(state.move_count, 3);
    }
}
