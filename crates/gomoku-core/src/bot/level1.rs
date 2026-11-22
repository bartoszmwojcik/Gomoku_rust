//! Level 1: one-ply pattern heuristic, no search.

use super::choose_move::{MoveChoice, Reason, StoppedBy};
use super::patterns::EngineBoard;
use super::seed::RngSource;

const DEFENSE_FACTOR: f64 = 0.9;
const TOLERANCE: f64 = 0.1;

/// Wins when it can, blocks a winning cell, else picks among the cells close to the best value.
/// Needs at least one stone and one empty cell.
pub fn choose_level1(board: &EngineBoard, side: usize, rng: &mut dyn RngSource) -> MoveChoice {
    let mut cands = Vec::new();
    board.candidates(&mut cands);
    let values: Vec<f64> = cands
        .iter()
        .map(|&c| board.cell_value(c, side, DEFENSE_FACTOR))
        .collect();
    let best = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let mut block = None;
    for (i, &cell) in cands.iter().enumerate() {
        if board.wins_at(cell, side) {
            return pick(cell, values[i], Reason::Win);
        }
        if block.is_none() && board.wins_at(cell, 1 - side) {
            block = Some(i);
        }
    }
    if let Some(i) = block {
        return pick(cands[i], values[i], Reason::Block);
    }
    let threshold = best * (1.0 - TOLERANCE);
    let pool: Vec<usize> = (0..cands.len())
        .filter(|&i| values[i] >= threshold)
        .collect();
    let picked = pool[(rng.next_f64() * pool.len() as f64) as usize];
    pick(cands[picked], values[picked], Reason::Heuristic)
}

fn pick(cell: usize, score: f64, reason: Reason) -> MoveChoice {
    MoveChoice {
        cell,
        depth: 0,
        nodes: 0,
        score,
        reason,
        stopped_by: StoppedBy::Completed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::cell_of;

    fn board_of(black: &[(usize, usize)], white: &[(usize, usize)]) -> EngineBoard {
        let mut board = EngineBoard::new();
        for &(x, y) in black {
            board.place(cell_of(x, y), 0);
        }
        for &(x, y) in white {
            board.place(cell_of(x, y), 1);
        }
        board
    }

    #[test]
    fn heuristic_vectors() {
        let board = board_of(&[(7, 7)], &[(8, 8)]);
        let low = choose_level1(&board, 0, &mut || 0.0);
        assert_eq!((low.cell, low.reason), (98, Reason::Heuristic));
        assert!((low.score - 91.7).abs() < 1e-9);
        let high = choose_level1(&board, 0, &mut || 0.999);
        assert_eq!(high.cell, 142);
        assert!((high.score - 90.7).abs() < 1e-9);
    }

    #[test]
    fn wins_at_once_even_when_the_opponent_also_threatens() {
        let board = board_of(
            &[(3, 7), (4, 7), (5, 7), (6, 7), (0, 14)],
            &[(3, 3), (4, 3), (5, 3), (6, 3)],
        );
        let choice = choose_level1(&board, 0, &mut || 0.5);
        assert_eq!(choice.reason, Reason::Win);
        assert!([cell_of(2, 7), cell_of(7, 7)].contains(&choice.cell));
    }

    #[test]
    fn blocks_a_four() {
        let board = board_of(
            &[(2, 3), (10, 10), (12, 12), (0, 14)],
            &[(3, 3), (4, 3), (5, 3), (6, 3)],
        );
        let choice = choose_level1(&board, 0, &mut || 0.5);
        assert_eq!((choice.cell, choice.reason), (cell_of(7, 3), Reason::Block));
    }

    #[test]
    fn extends_an_open_three() {
        let board = board_of(&[(5, 7), (6, 7), (7, 7)], &[(0, 0), (14, 0), (0, 14)]);
        let choice = choose_level1(&board, 0, &mut || 0.5);
        assert!([cell_of(4, 7), cell_of(8, 7)].contains(&choice.cell));
    }
}
