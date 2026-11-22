//! Domain logic for Gomoku: rules, clock, invitation codes. No HTTP, no database.
//!
//! Ported from the Node.js reference (`packages/core/src/games`); the tests carry over its cases.

pub mod apply_move;
pub mod auth;
pub mod board;
pub mod chat;
pub mod clock;
pub mod constants;
pub mod game;
pub mod history;
pub mod i18n;
pub mod invitation_code;
pub mod lobby;
pub mod nickname;
pub mod rematch;
pub mod resign;
pub mod return_notice;
pub mod review;
pub mod rules;
pub mod types;
pub mod validators;

/// Side length of the classic (free-style) board.
pub const CLASSIC_BOARD_SIZE: usize = rules::BOARD_SIZE;

/// Side length of the "paper and pencil" variant board.
pub const PAPER_BOARD_SIZE: usize = 101;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn board_sizes_match_the_product_brief() {
        assert_eq!(CLASSIC_BOARD_SIZE, 15);
        assert_eq!(PAPER_BOARD_SIZE, 101);
    }
}
