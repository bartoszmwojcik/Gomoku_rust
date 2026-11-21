//! Vocabulary of a game: statuses, seats, colours, how a game ended.

/// Lifecycle of a game. `Waiting` has no opponent yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GameStatus {
    Waiting,
    Active,
    Finished,
    Cancelled,
}

/// How a game came to be: `Rematch` follows a finished game, `Bot` is a game against the hosted bot,
/// `Show` is a private game without a human seat created by the administrator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GameOrigin {
    Invite,
    Lobby,
    Rematch,
    Bot,
    Show,
}

/// Who sits in a seat: an account holder, a guest with a one-game token, an LLM agent joined over MCP,
/// or the bot hosted by us.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ParticipantKind {
    Account,
    Guest,
    Agent,
    Bot,
}

/// Seat 1 is the creator, seat 2 the joining player.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Seat {
    One,
    Two,
}

impl Seat {
    /// The other seat.
    pub fn other(self) -> Seat {
        match self {
            Seat::One => Seat::Two,
            Seat::Two => Seat::One,
        }
    }

    /// Seat number as stored and served: 1 or 2.
    pub fn number(self) -> u8 {
        match self {
            Seat::One => 1,
            Seat::Two => 2,
        }
    }

    /// Zero-based index into per-seat arrays.
    pub fn index(self) -> usize {
        usize::from(self.number() - 1)
    }

    /// Inverse of [`Seat::number`]; `None` for anything but 1 and 2.
    pub fn from_number(number: u8) -> Option<Seat> {
        match number {
            1 => Some(Seat::One),
            2 => Some(Seat::Two),
            _ => None,
        }
    }
}

/// State of a rematch offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RematchState {
    Pending,
    Accepted,
    Declined,
}

/// Stone colour; black moves first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Color {
    Black,
    White,
}

/// Why a game finished: five (or more) in a row, a full board, the clock running out, a resignation,
/// or the score of the paper variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EndReason {
    FiveInRow,
    Draw,
    Timeout,
    Resignation,
    Points,
}

/// Outcome of a finished game; `winner` is `None` only for a draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GameResult {
    pub winner: Option<Seat>,
    pub reason: EndReason,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seats_map_to_numbers_indices_and_back() {
        assert_eq!(Seat::One.other(), Seat::Two);
        assert_eq!(Seat::Two.other(), Seat::One);
        assert_eq!((Seat::One.number(), Seat::Two.number()), (1, 2));
        assert_eq!((Seat::One.index(), Seat::Two.index()), (0, 1));
        assert_eq!(Seat::from_number(1), Some(Seat::One));
        assert_eq!(Seat::from_number(2), Some(Seat::Two));
        assert_eq!(Seat::from_number(0), None);
        assert_eq!(Seat::from_number(3), None);
    }
}
