//! Vocabulary of a game: statuses, seats, colours, how a game ended.

/// Lifecycle of a game. `Waiting` has no opponent yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GameStatus {
    Waiting,
    Active,
    Finished,
    Cancelled,
}

impl GameStatus {
    /// The text stored in the database and served to clients.
    pub fn as_str(self) -> &'static str {
        match self {
            GameStatus::Waiting => "waiting",
            GameStatus::Active => "active",
            GameStatus::Finished => "finished",
            GameStatus::Cancelled => "cancelled",
        }
    }

    /// Inverse of [`GameStatus::as_str`].
    pub fn parse(text: &str) -> Option<GameStatus> {
        match text {
            "waiting" => Some(GameStatus::Waiting),
            "active" => Some(GameStatus::Active),
            "finished" => Some(GameStatus::Finished),
            "cancelled" => Some(GameStatus::Cancelled),
            _ => None,
        }
    }
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

impl GameOrigin {
    /// The text stored in the database and served to clients.
    pub fn as_str(self) -> &'static str {
        match self {
            GameOrigin::Invite => "invite",
            GameOrigin::Lobby => "lobby",
            GameOrigin::Rematch => "rematch",
            GameOrigin::Bot => "bot",
            GameOrigin::Show => "show",
        }
    }

    /// Inverse of [`GameOrigin::as_str`].
    pub fn parse(text: &str) -> Option<GameOrigin> {
        match text {
            "invite" => Some(GameOrigin::Invite),
            "lobby" => Some(GameOrigin::Lobby),
            "rematch" => Some(GameOrigin::Rematch),
            "bot" => Some(GameOrigin::Bot),
            "show" => Some(GameOrigin::Show),
            _ => None,
        }
    }
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

impl ParticipantKind {
    /// The text stored in the database and served to clients.
    pub fn as_str(self) -> &'static str {
        match self {
            ParticipantKind::Account => "account",
            ParticipantKind::Guest => "guest",
            ParticipantKind::Agent => "agent",
            ParticipantKind::Bot => "bot",
        }
    }

    /// Inverse of [`ParticipantKind::as_str`].
    pub fn parse(text: &str) -> Option<ParticipantKind> {
        match text {
            "account" => Some(ParticipantKind::Account),
            "guest" => Some(ParticipantKind::Guest),
            "agent" => Some(ParticipantKind::Agent),
            "bot" => Some(ParticipantKind::Bot),
            _ => None,
        }
    }
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

impl RematchState {
    /// The text stored in the database and served to clients.
    pub fn as_str(self) -> &'static str {
        match self {
            RematchState::Pending => "pending",
            RematchState::Accepted => "accepted",
            RematchState::Declined => "declined",
        }
    }

    /// Inverse of [`RematchState::as_str`].
    pub fn parse(text: &str) -> Option<RematchState> {
        match text {
            "pending" => Some(RematchState::Pending),
            "accepted" => Some(RematchState::Accepted),
            "declined" => Some(RematchState::Declined),
            _ => None,
        }
    }
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

impl EndReason {
    /// The text stored in the database and served to clients.
    pub fn as_str(self) -> &'static str {
        match self {
            EndReason::FiveInRow => "five_in_row",
            EndReason::Draw => "draw",
            EndReason::Timeout => "timeout",
            EndReason::Resignation => "resignation",
            EndReason::Points => "points",
        }
    }

    /// Inverse of [`EndReason::as_str`].
    pub fn parse(text: &str) -> Option<EndReason> {
        match text {
            "five_in_row" => Some(EndReason::FiveInRow),
            "draw" => Some(EndReason::Draw),
            "timeout" => Some(EndReason::Timeout),
            "resignation" => Some(EndReason::Resignation),
            "points" => Some(EndReason::Points),
            _ => None,
        }
    }
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
    fn enums_round_trip_through_their_stored_text() {
        for status in [
            GameStatus::Waiting,
            GameStatus::Active,
            GameStatus::Finished,
            GameStatus::Cancelled,
        ] {
            assert_eq!(GameStatus::parse(status.as_str()), Some(status));
        }
        for origin in [
            GameOrigin::Invite,
            GameOrigin::Lobby,
            GameOrigin::Rematch,
            GameOrigin::Bot,
            GameOrigin::Show,
        ] {
            assert_eq!(GameOrigin::parse(origin.as_str()), Some(origin));
        }
        for kind in [
            ParticipantKind::Account,
            ParticipantKind::Guest,
            ParticipantKind::Agent,
            ParticipantKind::Bot,
        ] {
            assert_eq!(ParticipantKind::parse(kind.as_str()), Some(kind));
        }
        for state in [
            RematchState::Pending,
            RematchState::Accepted,
            RematchState::Declined,
        ] {
            assert_eq!(RematchState::parse(state.as_str()), Some(state));
        }
        for reason in [
            EndReason::FiveInRow,
            EndReason::Draw,
            EndReason::Timeout,
            EndReason::Resignation,
            EndReason::Points,
        ] {
            assert_eq!(EndReason::parse(reason.as_str()), Some(reason));
        }
        assert_eq!(GameStatus::parse("nope"), None);
        assert_eq!(GameOrigin::parse(""), None);
        assert_eq!(ParticipantKind::parse("Guest"), None);
        assert_eq!(RematchState::parse("x"), None);
        assert_eq!(EndReason::parse("x"), None);
    }

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
