//! Domain logic for Gomoku: rules, clock, bots. No HTTP, no database.
//!
//! F0 only fixes the board dimensions that the rest of the workspace depends on.

/// Side length of the classic (free-style) board.
pub const CLASSIC_BOARD_SIZE: usize = 15;

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
