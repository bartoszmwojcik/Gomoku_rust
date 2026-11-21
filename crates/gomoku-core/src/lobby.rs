//! Lobby: the "looking for a game" status.

/// How long a status lasts, from the moment it was set; never renewed.
pub const SEEKING_TTL_MS: u64 = 5 * 60 * 1000;

/// Marking oneself as seeking is limited per user, to stop a mark/withdraw loop.
pub const LOBBY_MARK_LIMIT: u32 = 30;
pub const LOBBY_MARK_WINDOW_MS: u64 = 60 * 60 * 1000;

/// Picking an opponent from the list is limited per user, also when it fails.
pub const LOBBY_PICK_LIMIT: u32 = 60;
pub const LOBBY_PICK_WINDOW_MS: u64 = 60 * 60 * 1000;

/// The list of other seekers shows the oldest statuses first, up to this many.
pub const LOBBY_LIST_LIMIT: usize = 50;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_match_the_reference_implementation() {
        assert_eq!(SEEKING_TTL_MS, 300_000);
        assert_eq!((LOBBY_MARK_LIMIT, LOBBY_MARK_WINDOW_MS), (30, 3_600_000));
        assert_eq!((LOBBY_PICK_LIMIT, LOBBY_PICK_WINDOW_MS), (60, 3_600_000));
        assert_eq!(LOBBY_LIST_LIMIT, 50);
    }
}
