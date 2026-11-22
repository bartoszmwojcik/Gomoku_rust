//! The algorithmic bot (levels 1 and 2), its strength profile, per-move budget and the arena.

pub mod arena;
pub mod budget;
pub mod choose_move;
pub mod level1;
pub mod level2;
pub mod opponent;
pub mod patterns;
pub mod profile;
pub mod seed;
pub mod transposition_table;
pub mod zobrist;

#[cfg(test)]
pub(crate) mod test_support {
    use super::choose_move::SearchBudget;
    use super::level1::choose_level1;
    use super::patterns::EngineBoard;
    use super::seed::Rng;
    use crate::board::cell_of;

    pub const BUDGET: SearchBudget = SearchBudget {
        soft_ms: 1e9,
        ceiling_ms: 1e9,
        max_depth: 4,
        tt_bytes: 65_536,
        max_nodes: Some(20_000),
    };

    pub fn board_of(black: &[(usize, usize)], white: &[(usize, usize)]) -> EngineBoard {
        let mut board = EngineBoard::new();
        for &(x, y) in black {
            board.place(cell_of(x, y), 0);
        }
        for &(x, y) in white {
            board.place(cell_of(x, y), 1);
        }
        board
    }

    /// Twenty level-1 moves from the centre with one shared generator.
    pub fn middle_game(seed: u32) -> EngineBoard {
        let mut rng = Rng::new(seed);
        let mut board = EngineBoard::new();
        for m in 0..20usize {
            let cell = if m == 0 {
                112
            } else if m == 1 {
                super::choose_move::CENTER_NEIGHBOURS
                    [(super::seed::RngSource::next_f64(&mut rng) * 8.0) as usize]
            } else {
                choose_level1(&board, m % 2, &mut rng).cell
            };
            board.place(cell, m % 2);
        }
        board
    }
}
