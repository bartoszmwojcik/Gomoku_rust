//! Rematch rules: pure functions, the time comes in as epoch milliseconds.

use crate::types::{RematchState, Seat};

/// What a client is told about an offer: `Expired` is a pending offer whose time is up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RematchStatus {
    Pending,
    Accepted,
    Declined,
    Expired,
}

/// The offer as stored in the finished game's row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoredRematch {
    pub state: Option<RematchState>,
    pub expires_at_ms: Option<i64>,
}

/// Colours are swapped: whoever played black plays white, no draw.
pub fn rematch_black_seat(previous_black_seat: Seat) -> Seat {
    previous_black_seat.other()
}

/// Whether the offer can still be accepted or declined: pending and not yet expired.
pub fn is_rematch_open(rematch: &StoredRematch, now_ms: i64) -> bool {
    rematch.state == Some(RematchState::Pending)
        && rematch
            .expires_at_ms
            .is_some_and(|expires| expires > now_ms)
}

/// The offer as the UI sees it, or `None` when nobody offered. Expiry is lazy: a pending offer past its
/// time reads as `Expired` (nothing runs in the background).
pub fn rematch_status(rematch: &StoredRematch, now_ms: i64) -> Option<RematchStatus> {
    match rematch.state? {
        RematchState::Pending if is_rematch_open(rematch, now_ms) => Some(RematchStatus::Pending),
        RematchState::Pending => Some(RematchStatus::Expired),
        RematchState::Accepted => Some(RematchStatus::Accepted),
        RematchState::Declined => Some(RematchStatus::Declined),
    }
}

/// What the API tells a client about the offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RematchView {
    pub state: RematchStatus,
    /// Seat of the player who offered.
    pub offered_by_seat: Seat,
    pub expires_at_ms: i64,
    /// The rematch game once accepted (`None` before, and after the cleanup deleted it).
    pub game_id: Option<String>,
}

/// The offer columns of a game row; the database keeps them together (all set or all empty).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredRematchRow {
    pub state: Option<RematchState>,
    pub offered_by_seat: Option<Seat>,
    pub expires_at_ms: Option<i64>,
    pub game_id: Option<String>,
}

/// The offer for the API, or `None` when there is none or the row is incomplete.
pub fn rematch_view(row: &StoredRematchRow, now_ms: i64) -> Option<RematchView> {
    let stored = StoredRematch {
        state: row.state,
        expires_at_ms: row.expires_at_ms,
    };
    let state = rematch_status(&stored, now_ms)?;
    Some(RematchView {
        state,
        offered_by_seat: row.offered_by_seat?,
        expires_at_ms: row.expires_at_ms?,
        game_id: row.game_id.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_000_000;

    fn stored(state: Option<RematchState>, expires: Option<i64>) -> StoredRematch {
        StoredRematch {
            state,
            expires_at_ms: expires,
        }
    }

    #[test]
    fn swaps_colours() {
        assert_eq!(rematch_black_seat(Seat::One), Seat::Two);
        assert_eq!(rematch_black_seat(Seat::Two), Seat::One);
    }

    #[test]
    fn an_offer_is_open_only_while_pending_and_not_expired() {
        let pending = |expires| stored(Some(RematchState::Pending), expires);
        assert!(is_rematch_open(&pending(Some(NOW + 1)), NOW));
        assert!(!is_rematch_open(&pending(Some(NOW)), NOW));
        assert!(!is_rematch_open(&pending(Some(NOW - 1)), NOW));
        assert!(!is_rematch_open(&pending(None), NOW));
        assert!(!is_rematch_open(
            &stored(Some(RematchState::Accepted), Some(NOW + 1)),
            NOW
        ));
        assert!(!is_rematch_open(&stored(None, None), NOW));
    }

    #[test]
    fn expiry_is_lazy() {
        assert_eq!(rematch_status(&stored(None, None), NOW), None);
        assert_eq!(
            rematch_status(&stored(Some(RematchState::Pending), Some(NOW + 1)), NOW),
            Some(RematchStatus::Pending)
        );
        assert_eq!(
            rematch_status(&stored(Some(RematchState::Pending), Some(NOW)), NOW),
            Some(RematchStatus::Expired)
        );
        assert_eq!(
            rematch_status(&stored(Some(RematchState::Accepted), Some(NOW - 5)), NOW),
            Some(RematchStatus::Accepted)
        );
        assert_eq!(
            rematch_status(&stored(Some(RematchState::Declined), None), NOW),
            Some(RematchStatus::Declined)
        );
    }

    #[test]
    fn builds_the_view_for_the_api() {
        let row = StoredRematchRow {
            state: Some(RematchState::Accepted),
            offered_by_seat: Some(Seat::Two),
            expires_at_ms: Some(NOW + 10),
            game_id: Some("g1".into()),
        };
        assert_eq!(
            rematch_view(&row, NOW),
            Some(RematchView {
                state: RematchStatus::Accepted,
                offered_by_seat: Seat::Two,
                expires_at_ms: NOW + 10,
                game_id: Some("g1".into()),
            })
        );
    }

    #[test]
    fn no_view_without_an_offer_or_with_an_incomplete_row() {
        let empty = StoredRematchRow {
            state: None,
            offered_by_seat: None,
            expires_at_ms: None,
            game_id: None,
        };
        assert_eq!(rematch_view(&empty, NOW), None);
        let no_seat = StoredRematchRow {
            state: Some(RematchState::Declined),
            offered_by_seat: None,
            expires_at_ms: Some(NOW),
            game_id: None,
        };
        assert_eq!(rematch_view(&no_seat, NOW), None);
        let no_expiry = StoredRematchRow {
            state: Some(RematchState::Declined),
            offered_by_seat: Some(Seat::One),
            expires_at_ms: None,
            game_id: None,
        };
        assert_eq!(rematch_view(&no_expiry, NOW), None);
    }
}
