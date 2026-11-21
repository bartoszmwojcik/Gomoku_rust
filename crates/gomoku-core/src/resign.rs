//! Resignation of a running game.

use crate::types::{EndReason, GameResult, GameStatus, Seat};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResignRejection {
    /// The game is already over.
    Finished,
    /// `waiting` has no opponent and `cancelled` nothing to resign.
    NotActive,
}

/// Outcome of a resignation: only a running game can be resigned, the opponent of `seat` wins with
/// reason `Resignation`. A pure step; locking and writing are the service's job.
pub fn resolve_resignation(status: GameStatus, seat: Seat) -> Result<GameResult, ResignRejection> {
    match status {
        GameStatus::Finished => Err(ResignRejection::Finished),
        GameStatus::Active => Ok(GameResult {
            winner: Some(seat.other()),
            reason: EndReason::Resignation,
        }),
        GameStatus::Waiting | GameStatus::Cancelled => Err(ResignRejection::NotActive),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_opponent_wins_an_active_game() {
        for seat in [Seat::One, Seat::Two] {
            assert_eq!(
                resolve_resignation(GameStatus::Active, seat),
                Ok(GameResult {
                    winner: Some(seat.other()),
                    reason: EndReason::Resignation
                })
            );
        }
    }

    #[test]
    fn nothing_to_resign_unless_active() {
        assert_eq!(
            resolve_resignation(GameStatus::Finished, Seat::One),
            Err(ResignRejection::Finished)
        );
        for status in [GameStatus::Waiting, GameStatus::Cancelled] {
            assert_eq!(
                resolve_resignation(status, Seat::One),
                Err(ResignRejection::NotActive)
            );
        }
    }
}
