//! The engine board: five-cell windows with incremental scores, forced-four counters, neighbour counts
//! and a Zobrist hash, all updated in O(windows of the cell) per stone.

use std::sync::OnceLock;

use crate::board::{BOARD_CELLS, Bitboard};
use crate::rules::BOARD_SIZE;

use super::zobrist::{zobrist_hi, zobrist_lo};

pub const CENTER: usize = 112;
pub const CANDIDATE_RADIUS: isize = 2;
pub const WINDOW_COUNT: usize = 572;
/// Weight of a window holding `k` stones of one colour and none of the other.
pub const WINDOW_WEIGHTS: [i64; 6] = [0, 1, 10, 100, 1000, 100_000];

const AXES: [(isize, isize); 4] = [(1, 0), (0, 1), (1, 1), (1, -1)];
const SIZE: isize = BOARD_SIZE as isize;

struct Geometry {
    windows_of: Vec<Vec<u16>>,
    neighbours: Vec<Vec<u8>>,
}

fn geometry() -> &'static Geometry {
    static GEOMETRY: OnceLock<Geometry> = OnceLock::new();
    GEOMETRY.get_or_init(|| {
        let mut windows_of = vec![Vec::new(); BOARD_CELLS];
        let mut next: u16 = 0;
        for y in 0..SIZE {
            for x in 0..SIZE {
                for (dx, dy) in AXES {
                    let (end_x, end_y) = (x + dx * 4, y + dy * 4);
                    if end_x >= SIZE || !(0..SIZE).contains(&end_y) {
                        continue;
                    }
                    for step in 0..5 {
                        windows_of[((y + dy * step) * SIZE + x + dx * step) as usize].push(next);
                    }
                    next += 1;
                }
            }
        }
        let mut neighbours = vec![Vec::new(); BOARD_CELLS];
        for y in 0..SIZE {
            for x in 0..SIZE {
                for ny in (y - CANDIDATE_RADIUS)..=(y + CANDIDATE_RADIUS) {
                    for nx in (x - CANDIDATE_RADIUS)..=(x + CANDIDATE_RADIUS) {
                        if (nx, ny) != (x, y) && (0..SIZE).contains(&nx) && (0..SIZE).contains(&ny)
                        {
                            neighbours[(y * SIZE + x) as usize].push((ny * SIZE + nx) as u8);
                        }
                    }
                }
            }
        }
        Geometry {
            windows_of,
            neighbours,
        }
    })
}

/// Number of windows a cell belongs to.
pub fn windows_of_cell(cell: usize) -> usize {
    geometry().windows_of[cell].len()
}

#[derive(Debug, Clone)]
pub struct EngineBoard {
    /// 0 empty, 1 black, 2 white.
    pub cells: [u8; BOARD_CELLS],
    counts: [[i8; WINDOW_COUNT]; 2],
    pub score: [i64; 2],
    pub fours: [i32; 2],
    near: [i16; BOARD_CELLS],
    pub stones: usize,
    pub hash_hi: i32,
    pub hash_lo: i32,
}

impl Default for EngineBoard {
    fn default() -> Self {
        Self {
            cells: [0; BOARD_CELLS],
            counts: [[0; WINDOW_COUNT]; 2],
            score: [0; 2],
            fours: [0; 2],
            near: [0; BOARD_CELLS],
            stones: 0,
            hash_hi: 0,
            hash_lo: 0,
        }
    }
}

impl EngineBoard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replays both bitboards in ascending cell order.
    pub fn from_bitboards(black: &Bitboard, white: &Bitboard) -> Self {
        let mut board = Self::new();
        for cell in 0..BOARD_CELLS {
            if black.has(cell) {
                board.place(cell, 0);
            } else if white.has(cell) {
                board.place(cell, 1);
            }
        }
        board
    }

    pub fn place(&mut self, cell: usize, side: usize) {
        self.cells[cell] = side as u8 + 1;
        self.stones += 1;
        self.update(cell, side, 1);
    }

    pub fn remove(&mut self, cell: usize) {
        let side = usize::from(self.cells[cell] - 1);
        self.cells[cell] = 0;
        self.stones -= 1;
        self.update(cell, side, -1);
    }

    fn update(&mut self, cell: usize, side: usize, delta: i8) {
        let geo = geometry();
        let other = 1 - side;
        for &w in &geo.windows_of[cell] {
            let w = usize::from(w);
            let other_count = self.counts[other][w];
            let before = self.counts[side][w];
            let after = before + delta;
            self.counts[side][w] = after;
            if other_count == 0 {
                self.score[side] +=
                    WINDOW_WEIGHTS[after as usize] - WINDOW_WEIGHTS[before as usize];
                self.fours[side] += i32::from(after == 4) - i32::from(before == 4);
            }
            if before == 0 || after == 0 {
                let sign: i64 = if after == 0 { 1 } else { -1 };
                self.score[other] += sign * WINDOW_WEIGHTS[other_count as usize];
                if other_count == 4 {
                    self.fours[other] += sign as i32;
                }
            }
        }
        for &n in &geo.neighbours[cell] {
            self.near[usize::from(n)] += i16::from(delta);
        }
        self.hash_hi ^= zobrist_hi()[cell * 2 + side];
        self.hash_lo ^= zobrist_lo()[cell * 2 + side];
    }

    /// Whether `side` playing `cell` completes five (or more).
    pub fn wins_at(&self, cell: usize, side: usize) -> bool {
        geometry().windows_of[cell].iter().any(|&w| {
            let w = usize::from(w);
            self.counts[side][w] == 4 && self.counts[1 - side][w] == 0
        })
    }

    /// Attack value of the cell for `side` plus `defense_factor` times what it takes from the opponent.
    pub fn cell_value(&self, cell: usize, side: usize, defense_factor: f64) -> f64 {
        let (mut attack, mut defense) = (0i64, 0i64);
        for &w in &geometry().windows_of[cell] {
            let w = usize::from(w);
            let (own, other) = (self.counts[side][w], self.counts[1 - side][w]);
            if other == 0 {
                attack += WINDOW_WEIGHTS[own as usize + 1];
            }
            if own == 0 {
                defense += WINDOW_WEIGHTS[other as usize + 1];
            }
        }
        attack as f64 + defense_factor * defense as f64
    }

    /// Empty cells within two of a stone, ascending.
    pub fn candidates(&self, out: &mut Vec<usize>) {
        out.clear();
        out.extend((0..BOARD_CELLS).filter(|&c| self.cells[c] == 0 && self.near[c] > 0));
    }

    /// Cells where `side` wins at once, ascending, at most `limit`; returns how many.
    pub fn winning_cells(&self, side: usize, out: &mut Vec<usize>, limit: usize) -> usize {
        out.clear();
        for c in 0..BOARD_CELLS {
            if out.len() >= limit {
                break;
            }
            if self.cells[c] == 0 && self.near[c] > 0 && self.wins_at(c, side) {
                out.push(c);
            }
        }
        out.len()
    }

    #[cfg(test)]
    fn recount(&self) -> ([i64; 2], [i32; 2]) {
        let mut score = [0i64; 2];
        let mut fours = [0i32; 2];
        for side in 0..2 {
            for w in 0..WINDOW_COUNT {
                if self.counts[1 - side][w] == 0 {
                    score[side] += WINDOW_WEIGHTS[self.counts[side][w] as usize];
                    fours[side] += i32::from(self.counts[side][w] == 4);
                }
            }
        }
        (score, fours)
    }
}

#[cfg(test)]
mod tests {
    use super::super::seed::Rng;
    use super::*;
    use crate::board::cell_of;
    use crate::rules::check_win;

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

    fn random_cells(seed: u32, count: usize) -> Vec<usize> {
        let mut rng = Rng::new(seed);
        let mut cells = Vec::new();
        while cells.len() < count {
            let c = (rng.next_u32() as usize) % BOARD_CELLS;
            if !cells.contains(&c) {
                cells.push(c);
            }
        }
        cells
    }

    #[test]
    fn window_geometry() {
        assert_eq!(
            geometry().windows_of.iter().map(Vec::len).sum::<usize>(),
            WINDOW_COUNT * 5
        );
        assert_eq!(windows_of_cell(112), 20);
        assert_eq!(windows_of_cell(0), 3);
        assert_eq!(WINDOW_COUNT, 2 * 15 * 11 + 2 * 11 * 11);
    }

    #[test]
    fn incremental_state_matches_a_recount_and_undo_restores() {
        for seed in 1..=3 {
            let cells = random_cells(seed, 120);
            let mut board = EngineBoard::new();
            for (i, &c) in cells.iter().enumerate() {
                board.place(c, i % 2);
                let (score, fours) = board.recount();
                assert_eq!(board.score, score);
                assert_eq!(board.fours, fours);
            }
            for &c in cells.iter().rev() {
                board.remove(c);
            }
            let fresh = EngineBoard::new();
            assert_eq!(board.cells, fresh.cells);
            assert_eq!(board.counts, fresh.counts);
            assert_eq!(board.near, fresh.near);
            assert_eq!(
                (board.score, board.fours, board.stones),
                ([0; 2], [0; 2], 0)
            );
            assert_eq!((board.hash_hi, board.hash_lo), (0, 0));
        }
    }

    #[test]
    fn hash_is_order_independent() {
        let mut a = EngineBoard::new();
        a.place(5, 0);
        a.place(9, 1);
        let mut b = EngineBoard::new();
        b.place(9, 1);
        b.place(5, 0);
        assert_eq!((a.hash_hi, a.hash_lo), (b.hash_hi, b.hash_lo));
        assert_ne!((a.hash_hi, a.hash_lo), (0, 0));
    }

    #[test]
    fn from_bitboards_replays_stones() {
        let black = Bitboard::new().with(3).with(112);
        let white = Bitboard::new().with(4);
        let board = EngineBoard::from_bitboards(&black, &white);
        assert_eq!(board.stones, 3);
        assert_eq!(
            (board.cells[3], board.cells[4], board.cells[112]),
            (1, 2, 1)
        );
    }

    #[test]
    fn wins_at_agrees_with_check_win() {
        for seed in 11..=14 {
            let cells = random_cells(seed, 90);
            let mut board = EngineBoard::new();
            for (i, &c) in cells.iter().enumerate() {
                board.place(c, i % 2);
            }
            for side in 0..2 {
                for c in (0..BOARD_CELLS).filter(|&c| board.cells[c] == 0) {
                    let own = |n: usize| n == c || board.cells[n] == side as u8 + 1;
                    assert_eq!(
                        board.wins_at(c, side),
                        check_win(own, c).is_some(),
                        "seed {seed} cell {c}"
                    );
                }
            }
        }
    }

    #[test]
    fn overline_and_blocked_four() {
        let overline = board_of(&[(3, 7), (4, 7), (5, 7), (7, 7), (8, 7)], &[]);
        assert!(overline.wins_at(cell_of(6, 7), 0));
        let blocked = board_of(&[(3, 7), (4, 7), (5, 7), (6, 7)], &[(2, 7), (7, 7)]);
        assert_eq!(blocked.fours[0], 0);
        assert!(!blocked.wins_at(cell_of(7, 7), 0));
    }

    #[test]
    fn fours_counter() {
        let mut board = board_of(&[(3, 7), (4, 7), (5, 7), (6, 7)], &[]);
        assert_eq!(board.fours[0], 2);
        board.place(cell_of(7, 7), 1);
        assert_eq!(board.fours[0], 1);
        board.place(cell_of(2, 7), 1);
        assert_eq!(board.fours[0], 0);
    }

    #[test]
    fn cell_value_vector() {
        let board = board_of(&[], &[(1, 0)]);
        assert_eq!(board.cell_value(0, 0, 0.5), 8.0);
    }

    #[test]
    fn candidate_cells() {
        let mut out = Vec::new();
        EngineBoard::new().candidates(&mut out);
        assert!(out.is_empty());
        board_of(&[(0, 0)], &[]).candidates(&mut out);
        assert_eq!(out, vec![1, 2, 15, 16, 17, 30, 31, 32]);
    }

    #[test]
    fn winning_cells_and_limit() {
        let board = board_of(&[(3, 7), (4, 7), (5, 7), (6, 7)], &[]);
        let mut out = Vec::new();
        assert_eq!(board.winning_cells(0, &mut out, 225), 2);
        assert_eq!(out, vec![cell_of(2, 7), cell_of(7, 7)]);
        assert_eq!(board.winning_cells(0, &mut out, 1), 1);
        assert_eq!(board.winning_cells(1, &mut out, 225), 0);
    }
}
