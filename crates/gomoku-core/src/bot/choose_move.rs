//! Move selection dispatch: opening book, level 1 heuristic, level 2 search.

use thiserror::Error;

use crate::board::{BOARD_CELLS, Bitboard};

use super::level1::choose_level1;
use super::level2::search_level2;
use super::patterns::{CENTER, EngineBoard};
use super::seed::RngSource;

/// Replies to a lone centre stone; the order matters for the random pick.
pub const CENTER_NEIGHBOURS: [usize; 8] = [96, 97, 98, 111, 113, 126, 127, 128];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BotLevel {
    One,
    Two,
}

impl BotLevel {
    pub fn from_number(level: i64) -> Option<Self> {
        match level {
            1 => Some(Self::One),
            2 => Some(Self::Two),
            _ => None,
        }
    }

    pub fn number(self) -> i16 {
        match self {
            Self::One => 1,
            Self::Two => 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    Opening,
    Win,
    Block,
    Heuristic,
    Search,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoppedBy {
    Completed,
    Soft,
    Ceiling,
    Nodes,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MoveChoice {
    pub cell: usize,
    pub depth: i32,
    pub nodes: u64,
    pub score: f64,
    pub reason: Reason,
    pub stopped_by: StoppedBy,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SearchBudget {
    pub soft_ms: f64,
    pub ceiling_ms: f64,
    pub max_depth: i32,
    pub tt_bytes: i64,
    pub max_nodes: Option<u64>,
}

pub struct SearchInput<'a> {
    pub budget: SearchBudget,
    pub rng: &'a mut dyn RngSource,
    pub now: &'a dyn Fn() -> f64,
    pub should_stop: Option<&'a dyn Fn() -> bool>,
    pub on_iteration: Option<&'a mut dyn FnMut(&MoveChoice)>,
}

#[derive(Debug, Clone)]
pub struct BotPosition {
    pub black: Bitboard,
    pub white: Bitboard,
    pub move_count: usize,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ChooseMoveError {
    #[error("The board is full")]
    BoardFull,
}

fn opening(cell: usize) -> MoveChoice {
    MoveChoice {
        cell,
        depth: 0,
        nodes: 0,
        score: 0.0,
        reason: Reason::Opening,
        stopped_by: StoppedBy::Completed,
    }
}

/// Picks the move of the colour on turn (`move_count % 2`; 0 black).
pub fn choose_move(
    position: &BotPosition,
    level: BotLevel,
    input: SearchInput<'_>,
) -> Result<MoveChoice, ChooseMoveError> {
    let board = EngineBoard::from_bitboards(&position.black, &position.white);
    if board.stones >= BOARD_CELLS {
        return Err(ChooseMoveError::BoardFull);
    }
    let side = position.move_count % 2;
    if board.stones == 0 {
        return Ok(opening(CENTER));
    }
    if board.stones == 1 && board.cells[CENTER] != 0 {
        let pick = (input.rng.next_f64() * 8.0) as usize;
        return Ok(opening(CENTER_NEIGHBOURS[pick]));
    }
    Ok(match level {
        BotLevel::One => choose_level1(&board, side, input.rng),
        BotLevel::Two => search_level2(&board, side, input),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::cell_of;
    use crate::bot::test_support::BUDGET;

    fn position(black: &[(usize, usize)], white: &[(usize, usize)]) -> BotPosition {
        let mut b = Bitboard::new();
        let mut w = Bitboard::new();
        for &(x, y) in black {
            b = b.with(cell_of(x, y));
        }
        for &(x, y) in white {
            w = w.with(cell_of(x, y));
        }
        BotPosition {
            black: b,
            white: w,
            move_count: black.len() + white.len(),
        }
    }

    fn run(pos: &BotPosition, level: BotLevel, rng: f64) -> Result<MoveChoice, ChooseMoveError> {
        let mut source = move || rng;
        choose_move(
            pos,
            level,
            SearchInput {
                budget: BUDGET,
                rng: &mut source,
                now: &|| 0.0,
                should_stop: None,
                on_iteration: None,
            },
        )
    }

    #[test]
    fn levels_convert() {
        assert_eq!(BotLevel::from_number(1), Some(BotLevel::One));
        assert_eq!(BotLevel::from_number(2), Some(BotLevel::Two));
        assert_eq!(BotLevel::from_number(3), None);
        assert_eq!((BotLevel::One.number(), BotLevel::Two.number()), (1, 2));
    }

    #[test]
    fn empty_board_opens_in_the_centre() {
        for level in [BotLevel::One, BotLevel::Two] {
            let choice = run(&position(&[], &[]), level, 0.3).unwrap();
            assert_eq!((choice.cell, choice.reason), (112, Reason::Opening));
        }
    }

    #[test]
    fn lone_centre_stone_gets_a_neighbour() {
        let pos = position(&[(7, 7)], &[]);
        assert_eq!(run(&pos, BotLevel::Two, 0.0).unwrap().cell, cell_of(6, 6));
        let last = run(&pos, BotLevel::One, 0.999).unwrap();
        assert_eq!((last.cell, last.reason), (cell_of(8, 8), Reason::Opening));
    }

    #[test]
    fn lone_off_centre_stone_is_heuristic() {
        let choice = run(&position(&[(0, 0)], &[]), BotLevel::One, 0.5).unwrap();
        assert_eq!(choice.reason, Reason::Heuristic);
        assert_ne!(choice.cell, 0);
    }

    #[test]
    fn full_board_is_an_error() {
        let mut pos = position(&[], &[]);
        for c in 0..BOARD_CELLS {
            if c % 2 == 0 {
                pos.black = pos.black.with(c);
            } else {
                pos.white = pos.white.with(c);
            }
        }
        pos.move_count = BOARD_CELLS;
        let err = run(&pos, BotLevel::One, 0.5).unwrap_err();
        assert_eq!(err.to_string(), "The board is full");
    }

    #[test]
    fn colour_comes_from_parity() {
        let pos = position(
            &[(3, 7), (4, 7), (5, 7), (6, 7), (0, 14)],
            &[(3, 3), (4, 3), (5, 3), (6, 3)],
        );
        for level in [BotLevel::One, BotLevel::Two] {
            let choice = run(&pos, level, 0.5).unwrap();
            assert_eq!(choice.reason, Reason::Win);
            assert!([cell_of(2, 3), cell_of(7, 3)].contains(&choice.cell));
        }
    }
}
