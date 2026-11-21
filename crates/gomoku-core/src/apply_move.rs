//! One step of a game: placing the next stone.

use crate::board::{BOARD_CELLS, Bitboard, is_cell};
use crate::rules::{check_win, seat_on_turn, turn_color};
use crate::types::{Color, EndReason, GameResult, Seat};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveState {
    pub black: Bitboard,
    pub white: Bitboard,
    pub move_count: usize,
    pub black_seat: Seat,
}

impl MoveState {
    /// An empty board with `black_seat` playing black.
    pub fn start(black_seat: Seat) -> Self {
        Self {
            black: Bitboard::new(),
            white: Bitboard::new(),
            move_count: 0,
            black_seat,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveRejection {
    OutOfBoard,
    Occupied,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveApplied {
    pub state: MoveState,
    /// The colour that just moved and the seat that played it.
    pub color: Color,
    pub seat: Seat,
    /// `None` while the game goes on.
    pub result: Option<GameResult>,
    pub win_line: Option<Vec<usize>>,
}

/// Places the next stone: a pure step of the game. A five wins even with the last, 225th, stone;
/// a full board without a five is a draw.
pub fn apply_move(state: &MoveState, cell: usize) -> Result<MoveApplied, MoveRejection> {
    if !is_cell(cell) {
        return Err(MoveRejection::OutOfBoard);
    }
    if state.black.has(cell) || state.white.has(cell) {
        return Err(MoveRejection::Occupied);
    }
    let color = turn_color(state.move_count);
    let seat = seat_on_turn(state.move_count, state.black_seat);
    let mut next = state.clone();
    next.move_count += 1;
    let own = match color {
        Color::Black => {
            next.black = next.black.with(cell);
            &next.black
        }
        Color::White => {
            next.white = next.white.with(cell);
            &next.white
        }
    };
    let win_line = check_win(|c| own.has(c), cell);
    let result = if win_line.is_some() {
        Some(GameResult {
            winner: Some(seat),
            reason: EndReason::FiveInRow,
        })
    } else if next.move_count == BOARD_CELLS {
        Some(GameResult {
            winner: None,
            reason: EndReason::Draw,
        })
    } else {
        None
    };
    Ok(MoveApplied {
        state: next,
        color,
        seat,
        result,
        win_line,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::{assert_board_consistent, cell_of};
    use crate::rules::BOARD_SIZE;

    fn play(state: &MoveState, cells: &[usize]) -> MoveApplied {
        let mut current = state.clone();
        let mut last = None;
        for &cell in cells {
            let applied = apply_move(&current, cell).unwrap();
            current = applied.state.clone();
            last = Some(applied);
        }
        last.unwrap()
    }

    fn board_of(cells: impl IntoIterator<Item = usize>) -> Bitboard {
        cells.into_iter().fold(Bitboard::new(), |b, c| b.with(c))
    }

    #[test]
    fn places_black_then_white_and_counts_moves() {
        let first = apply_move(&MoveState::start(Seat::Two), 112).unwrap();
        assert_eq!((first.color, first.seat), (Color::Black, Seat::Two));
        assert_eq!((first.result, first.win_line.clone()), (None, None));
        assert_eq!(first.state.move_count, 1);
        assert!(first.state.black.has(112));

        let second = apply_move(&first.state, 113).unwrap();
        assert_eq!((second.color, second.seat), (Color::White, Seat::One));
        assert!(second.state.white.has(113));
    }

    #[test]
    fn does_not_mutate_the_previous_state() {
        let before = MoveState::start(Seat::One);
        apply_move(&before, 5).unwrap();
        assert_eq!(before.move_count, 0);
        assert!(!before.black.has(5));
    }

    #[test]
    fn rejects_an_occupied_cell_of_either_colour_and_cells_off_the_board() {
        let state = play(&MoveState::start(Seat::One), &[10, 11]).state;
        assert_eq!(apply_move(&state, 10), Err(MoveRejection::Occupied));
        assert_eq!(apply_move(&state, 11), Err(MoveRejection::Occupied));
        assert_eq!(apply_move(&state, 225), Err(MoveRejection::OutOfBoard));
    }

    #[test]
    fn a_five_wins_for_the_seat_that_moved() {
        let row = 7 * BOARD_SIZE;
        let blacks: Vec<usize> = (0..5).map(|i| row + i).collect();
        let sequence = [
            blacks[0], 0, blacks[1], 1, blacks[2], 2, blacks[3], 3, blacks[4],
        ];
        let last = play(&MoveState::start(Seat::Two), &sequence);
        assert_eq!(
            last.result,
            Some(GameResult {
                winner: Some(Seat::Two),
                reason: EndReason::FiveInRow
            })
        );
        assert_eq!(last.win_line, Some(blacks));
    }

    #[test]
    fn a_white_win_belongs_to_the_other_seat() {
        let whites: Vec<usize> = (0..5).map(|i| 3 * BOARD_SIZE + i).collect();
        let blacks = [200, 202, 204, 206, 208];
        let sequence: Vec<usize> = blacks
            .iter()
            .zip(&whites)
            .flat_map(|(&b, &w)| [b, w])
            .collect();
        let last = play(&MoveState::start(Seat::One), &sequence);
        assert_eq!(
            last.result,
            Some(GameResult {
                winner: Some(Seat::Two),
                reason: EndReason::FiveInRow
            })
        );
        assert_eq!(last.color, Color::White);
    }

    /// A full board with no run of five: colour = parity of (floor(x / 2) + y). 113 black, 112 white.
    fn draw_layout() -> (Vec<usize>, Vec<usize>) {
        let (mut black, mut white) = (Vec::new(), Vec::new());
        for cell in 0..BOARD_CELLS {
            let (x, y) = (cell % BOARD_SIZE, cell / BOARD_SIZE);
            if (x / 2 + y) % 2 == 0 {
                black.push(cell)
            } else {
                white.push(cell)
            }
        }
        (black, white)
    }

    #[test]
    fn a_five_with_the_225th_stone_is_a_win_not_a_draw() {
        let (black_cells, white_cells) = draw_layout();
        let mut black: std::collections::HashSet<usize> = black_cells.into_iter().collect();
        let mut white: std::collections::HashSet<usize> = white_cells.into_iter().collect();
        // Row 0 gets black at x = 2, 3; two other black stones turn white to keep 112 / 112.
        black.extend([cell_of(2, 0), cell_of(3, 0)]);
        white.remove(&cell_of(2, 0));
        white.remove(&cell_of(3, 0));
        for cell in [cell_of(0, 2), cell_of(1, 2)] {
            black.remove(&cell);
            white.insert(cell);
        }
        let last_cell = cell_of(2, 0);
        black.remove(&last_cell);
        assert_eq!((black.len(), white.len()), (112, 112));
        let state = MoveState {
            black: board_of(black),
            white: board_of(white),
            move_count: 224,
            black_seat: Seat::One,
        };
        let applied = apply_move(&state, last_cell).unwrap();
        assert_eq!(applied.state.move_count, BOARD_CELLS);
        assert_eq!(
            applied.result,
            Some(GameResult {
                winner: Some(Seat::One),
                reason: EndReason::FiveInRow
            })
        );
        assert_eq!(applied.win_line, Some(vec![0, 1, 2, 3, 4, 5]));
    }

    #[test]
    fn a_full_board_without_a_five_is_a_draw() {
        let (black_cells, white_cells) = draw_layout();
        let last_cell = *black_cells.last().unwrap();
        let state = MoveState {
            black: board_of(black_cells[..black_cells.len() - 1].iter().copied()),
            white: board_of(white_cells.iter().copied()),
            move_count: 224,
            black_seat: Seat::Two,
        };
        let applied = apply_move(&state, last_cell).unwrap();
        assert_eq!(
            applied.result,
            Some(GameResult {
                winner: None,
                reason: EndReason::Draw
            })
        );
        assert_eq!(applied.win_line, None);
        let moves: Vec<usize> = black_cells
            .iter()
            .enumerate()
            .flat_map(|(i, &b)| white_cells.get(i).map_or(vec![b], |&w| vec![b, w]))
            .collect();
        assert_eq!(
            assert_board_consistent(&applied.state.black, &applied.state.white, &moves),
            Ok(())
        );
    }
}
