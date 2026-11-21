//! Review of a finished game: stepping through the move log.

/// A step is the number of moves shown: 0 is the empty board, `total` the final position.
pub fn clamp_review_step(step: i64, total: usize) -> usize {
    usize::try_from(step.max(0)).map_or(0, |step| step.min(total))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamps_between_the_empty_board_and_the_final_position() {
        assert_eq!(clamp_review_step(-5, 30), 0);
        assert_eq!(clamp_review_step(0, 30), 0);
        assert_eq!(clamp_review_step(12, 30), 12);
        assert_eq!(clamp_review_step(30, 30), 30);
        assert_eq!(clamp_review_step(99, 30), 30);
        assert_eq!(clamp_review_step(5, 0), 0);
        assert_eq!(clamp_review_step(i64::MAX, usize::MAX), i64::MAX as usize);
    }
}
