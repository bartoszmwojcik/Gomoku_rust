//! The 15x15 board: cell `c = y * 15 + x`, one bitboard per colour.

use std::collections::HashSet;

use thiserror::Error;

use crate::rules::BOARD_SIZE;

pub const BOARD_CELLS: usize = BOARD_SIZE * BOARD_SIZE;
/// `ceil(225 / 8)` bytes per colour.
pub const BITBOARD_BYTES: usize = BOARD_CELLS.div_ceil(8);

/// One colour's stones: bit `c` lives in byte `c >> 3` at position `c & 7`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Bitboard([u8; BITBOARD_BYTES]);

impl Bitboard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether `cell` holds a stone; cells off the board hold none.
    pub fn has(&self, cell: usize) -> bool {
        is_cell(cell) && self.0[cell >> 3] & (1 << (cell & 7)) != 0
    }

    /// A copy with the stone set; the receiver is never mutated. Cells off the board are ignored.
    pub fn with(&self, cell: usize) -> Self {
        let mut copy = self.clone();
        if is_cell(cell) {
            copy.0[cell >> 3] |= 1 << (cell & 7);
        }
        copy
    }

    pub fn count(&self) -> usize {
        self.0.iter().map(|byte| byte.count_ones() as usize).sum()
    }
}

pub fn is_cell(cell: usize) -> bool {
    cell < BOARD_CELLS
}

pub fn cell_of(x: usize, y: usize) -> usize {
    y * BOARD_SIZE + x
}

/// `(x, y)` of a cell.
pub fn coords_of(cell: usize) -> (usize, usize) {
    (cell % BOARD_SIZE, cell / BOARD_SIZE)
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum BoardError {
    #[error("stone count differs from the number of moves")]
    CountMismatch,
    #[error("black and white stones do not alternate")]
    NotAlternating,
    #[error("move log repeats a cell or leaves the board")]
    BadMoveLog,
    #[error("move log disagrees with the bitboards")]
    LogDisagrees,
    #[error("a move log holds at most 225 cells of the board")]
    TooManyMoves,
}

/// Invariants of a board: disjoint bitboards, black has as many stones as white or one more, and
/// `moves` is exactly the set of stones with the colour taken from the parity of the move number.
/// A violation is a defect, never a client error.
pub fn assert_board_consistent(
    black: &Bitboard,
    white: &Bitboard,
    moves: &[usize],
) -> Result<(), BoardError> {
    let (black_count, white_count) = (black.count(), white.count());
    if black_count + white_count != moves.len() {
        return Err(BoardError::CountMismatch);
    }
    if black_count as isize - white_count as isize != (moves.len() % 2) as isize {
        return Err(BoardError::NotAlternating);
    }
    let mut seen = HashSet::new();
    for (index, &cell) in moves.iter().enumerate() {
        if !is_cell(cell) || !seen.insert(cell) {
            return Err(BoardError::BadMoveLog);
        }
        let (own, other) = if index % 2 == 0 {
            (black, white)
        } else {
            (white, black)
        };
        if !own.has(cell) || other.has(cell) {
            return Err(BoardError::LogDisagrees);
        }
    }
    Ok(())
}

/// Move log for storage: one byte per move.
pub fn encode_moves(cells: &[usize]) -> Result<Vec<u8>, BoardError> {
    if cells.len() > BOARD_CELLS || !cells.iter().all(|&c| is_cell(c)) {
        return Err(BoardError::TooManyMoves);
    }
    Ok(cells.iter().map(|&c| c as u8).collect())
}

/// Inverse of [`encode_moves`]; rejects a log longer than the board or a byte that is not a cell.
pub fn decode_moves(bytes: &[u8]) -> Result<Vec<usize>, BoardError> {
    let cells: Vec<usize> = bytes.iter().map(|&b| usize::from(b)).collect();
    if cells.len() > BOARD_CELLS || !cells.iter().all(|&c| is_cell(c)) {
        return Err(BoardError::TooManyMoves);
    }
    Ok(cells)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn board_of(cells: &[usize]) -> Bitboard {
        cells
            .iter()
            .fold(Bitboard::new(), |board, &cell| board.with(cell))
    }

    #[test]
    fn sizes() {
        assert_eq!((BOARD_CELLS, BITBOARD_BYTES), (225, 29));
    }

    #[test]
    fn cells_and_coordinates_round_trip() {
        assert!(is_cell(0) && is_cell(224));
        assert!(!is_cell(225));
        assert_eq!(cell_of(14, 14), 224);
        assert_eq!(coords_of(cell_of(3, 9)), (3, 9));
        assert_eq!(coords_of(224), (14, 14));
    }

    #[test]
    fn sets_stones_without_mutating_the_original() {
        let empty = Bitboard::new();
        let one = empty.with(7).with(224);
        assert!(!empty.has(7));
        assert!(one.has(7) && one.has(224));
        assert!(!one.has(8));
        assert_eq!(one.count(), 2);
        assert_eq!(one.with(7).count(), 2);
    }

    #[test]
    fn cells_off_the_board_hold_nothing_and_cannot_be_set() {
        let board = Bitboard::new();
        assert!(!board.has(225));
        assert!(!board.has(usize::MAX));
        assert_eq!(board.with(225), board);
    }

    #[test]
    fn accepts_a_consistent_board() {
        let (black, white) = (board_of(&[10, 12]), board_of(&[11]));
        assert_eq!(
            assert_board_consistent(&black, &white, &[10, 11, 12]),
            Ok(())
        );
        assert_eq!(
            assert_board_consistent(&Bitboard::new(), &Bitboard::new(), &[]),
            Ok(())
        );
    }

    #[test]
    fn rejects_each_violated_invariant() {
        let black = board_of(&[10, 12]);
        let white = board_of(&[11]);
        assert_eq!(
            assert_board_consistent(&black, &white, &[10, 11]),
            Err(BoardError::CountMismatch)
        );
        // white ahead of black
        assert_eq!(
            assert_board_consistent(&board_of(&[10]), &board_of(&[11, 12]), &[10, 11, 12]),
            Err(BoardError::NotAlternating)
        );
        // black two ahead
        assert_eq!(
            assert_board_consistent(
                &board_of(&[10, 12, 14]),
                &board_of(&[11]),
                &[10, 11, 12, 14]
            ),
            Err(BoardError::NotAlternating)
        );
        // repeated cell
        assert_eq!(
            assert_board_consistent(&board_of(&[10, 12]), &board_of(&[11]), &[10, 11, 10]),
            Err(BoardError::BadMoveLog)
        );
        // off the board
        assert_eq!(
            assert_board_consistent(&board_of(&[10]), &Bitboard::new(), &[300]),
            Err(BoardError::BadMoveLog)
        );
        // colours swapped relative to parity
        assert_eq!(
            assert_board_consistent(&black, &white, &[11, 10, 12]),
            Err(BoardError::LogDisagrees)
        );
        // a cell held by both colours
        assert_eq!(
            assert_board_consistent(&board_of(&[10, 11]), &board_of(&[11]), &[10, 11, 11]),
            Err(BoardError::LogDisagrees)
        );
    }

    #[test]
    fn move_log_round_trips_through_bytes() {
        let bytes = encode_moves(&[0, 112, 224]).unwrap();
        assert_eq!(bytes, vec![0, 112, 224]);
        assert_eq!(decode_moves(&bytes).unwrap(), vec![0, 112, 224]);
        assert_eq!(decode_moves(&[]).unwrap(), Vec::<usize>::new());
    }

    #[test]
    fn move_log_rejects_what_is_not_a_cell_or_too_long() {
        assert_eq!(encode_moves(&[225]), Err(BoardError::TooManyMoves));
        assert_eq!(encode_moves(&vec![0; 226]), Err(BoardError::TooManyMoves));
        assert_eq!(decode_moves(&[225]), Err(BoardError::TooManyMoves));
        assert_eq!(decode_moves(&vec![0; 226]), Err(BoardError::TooManyMoves));
    }
}
