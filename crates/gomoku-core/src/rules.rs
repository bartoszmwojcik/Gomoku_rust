//! Freestyle gomoku rules (no renju restrictions).

use crate::types::{Color, Seat};

pub const BOARD_SIZE: usize = 15;
pub const WIN_LENGTH: usize = 5;

/// The four line directions as `(dx, dy)`: horizontal, vertical, both diagonals.
const AXES: [(isize, isize); 4] = [(1, 0), (0, 1), (1, 1), (1, -1)];

/// Colour that makes the next move after `move_count` stones: black moves first.
pub fn turn_color(move_count: usize) -> Color {
    if move_count.is_multiple_of(2) {
        Color::Black
    } else {
        Color::White
    }
}

/// Seat that makes move number `move_count + 1`, given the seat playing black.
pub fn seat_on_turn(move_count: usize, black_seat: Seat) -> Seat {
    match turn_color(move_count) {
        Color::Black => black_seat,
        Color::White => black_seat.other(),
    }
}

/// Win detection around the stone just placed on `cell`: for each axis the run is
/// `1 + forward + backward`, each side looking at most `WIN_LENGTH - 1` cells. Walks coordinates, so a
/// line never wraps from the end of one row to the start of the next. Six or more in a row win too.
///
/// Returns the cells of the winning line (ascending, 5 to 9 of them) or `None`.
pub fn check_win(is_own: impl Fn(usize) -> bool, cell: usize) -> Option<Vec<usize>> {
    let x = (cell % BOARD_SIZE) as isize;
    let y = (cell / BOARD_SIZE) as isize;
    for (dx, dy) in AXES {
        let before = reach(&is_own, x, y, -dx, -dy);
        let after = reach(&is_own, x, y, dx, dy);
        if before.len() + 1 + after.len() >= WIN_LENGTH {
            let mut line: Vec<usize> = before.into_iter().chain([cell]).chain(after).collect();
            line.sort_unstable();
            return Some(line);
        }
    }
    None
}

/// Consecutive own cells from `(x, y)` in one direction, excluding the start, at most `WIN_LENGTH - 1`.
fn reach(is_own: &impl Fn(usize) -> bool, x: isize, y: isize, dx: isize, dy: isize) -> Vec<usize> {
    let size = BOARD_SIZE as isize;
    let mut cells = Vec::new();
    for step in 1..WIN_LENGTH as isize {
        let (cx, cy) = (x + dx * step, y + dy * step);
        if cx < 0 || cy < 0 || cx >= size || cy >= size {
            break;
        }
        let next = (cy * size + cx) as usize;
        if !is_own(next) {
            break;
        }
        cells.push(next);
    }
    cells
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::collections::HashSet;

    use super::*;
    use crate::board::{BOARD_CELLS, cell_of};

    fn own(cells: &[usize]) -> impl Fn(usize) -> bool {
        let set: HashSet<usize> = cells.iter().copied().collect();
        move |cell| set.contains(&cell)
    }

    fn line(x: isize, y: isize, dx: isize, dy: isize, length: isize) -> Vec<usize> {
        (0..length)
            .map(|i| cell_of((x + dx * i) as usize, (y + dy * i) as usize))
            .collect()
    }

    fn sorted(mut cells: Vec<usize>) -> Vec<usize> {
        cells.sort_unstable();
        cells
    }

    #[test]
    fn freestyle_constants() {
        assert_eq!((BOARD_SIZE, WIN_LENGTH), (15, 5));
    }

    #[test]
    fn black_gets_the_even_counts_and_seats_follow() {
        assert_eq!(turn_color(0), Color::Black);
        assert_eq!(turn_color(1), Color::White);
        assert_eq!(turn_color(224), Color::Black);
        assert_eq!(seat_on_turn(0, Seat::One), Seat::One);
        assert_eq!(seat_on_turn(1, Seat::One), Seat::Two);
        assert_eq!(seat_on_turn(0, Seat::Two), Seat::Two);
        assert_eq!(seat_on_turn(1, Seat::Two), Seat::One);
    }

    #[test]
    fn finds_five_on_every_axis_from_any_of_its_stones() {
        for (x, y, dx, dy) in [(3, 4, 1, 0), (3, 4, 0, 1), (3, 4, 1, 1), (3, 9, 1, -1)] {
            let cells = line(x, y, dx, dy, 5);
            for &cell in &cells {
                assert_eq!(check_win(own(&cells), cell), Some(sorted(cells.clone())));
            }
        }
    }

    #[test]
    fn four_is_not_a_win() {
        let cells = line(3, 4, 1, 0, 4);
        assert_eq!(check_win(own(&cells), cells[3]), None);
    }

    #[test]
    fn joins_two_groups() {
        let mut cells = line(2, 7, 1, 0, 2);
        cells.extend(line(5, 7, 1, 0, 2));
        cells.push(cell_of(4, 7));
        assert_eq!(
            check_win(own(&cells), cell_of(4, 7)),
            Some(line(2, 7, 1, 0, 5))
        );
    }

    #[test]
    fn six_or_more_win_and_return_the_whole_run() {
        let six = line(1, 1, 1, 1, 6);
        assert_eq!(check_win(own(&six), six[2]), Some(six.clone()));
        let nine = line(0, 6, 1, 0, 9);
        assert_eq!(check_win(own(&nine), nine[4]), Some(nine.clone()));
    }

    #[test]
    fn a_gap_breaks_the_run() {
        let mut cells = line(0, 0, 1, 0, 2);
        cells.extend(line(3, 0, 1, 0, 3));
        assert_eq!(check_win(own(&cells), cell_of(3, 0)), None);
    }

    #[test]
    fn finds_lines_on_edges_and_corners() {
        let bottom = line(10, 14, 1, 0, 5);
        assert_eq!(check_win(own(&bottom), bottom[4]), Some(bottom.clone()));
        let right = line(14, 0, 0, 1, 5);
        assert_eq!(check_win(own(&right), right[0]), Some(right.clone()));
        let corner = line(0, 0, 1, 1, 5);
        assert_eq!(check_win(own(&corner), corner[0]), Some(corner.clone()));
        let corner2 = line(14, 0, -1, 1, 5);
        assert_eq!(
            check_win(own(&corner2), corner2[0]),
            Some(sorted(corner2.clone()))
        );
    }

    #[test]
    fn never_wraps_from_one_row_to_the_next() {
        // cells 12..=16 are consecutive indices but straddle the row break (x = 12, 13, 14 | 0, 1)
        let wrapped = [12, 13, 14, 15, 16];
        assert_eq!(check_win(own(&wrapped), 14), None);
        assert_eq!(check_win(own(&wrapped), 15), None);
    }

    #[test]
    fn reads_at_most_32_cells_whatever_the_board_holds() {
        let reads = Cell::new(0);
        let everything = |cell: usize| {
            reads.set(reads.get() + 1);
            cell < BOARD_CELLS
        };
        check_win(everything, cell_of(7, 7));
        assert!(reads.get() <= 32);

        reads.set(0);
        let only_center = |cell: usize| {
            reads.set(reads.get() + 1);
            cell == cell_of(7, 7)
        };
        check_win(only_center, cell_of(7, 7));
        assert!(reads.get() <= 32);
    }
}
