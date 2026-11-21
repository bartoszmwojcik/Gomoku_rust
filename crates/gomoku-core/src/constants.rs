//! Durations and limits of the game rules, in milliseconds unless stated otherwise.

const MINUTE: u64 = 60 * 1000;
const HOUR: u64 = 60 * MINUTE;

/// Invitation validity: a friend may answer after a few hours.
pub const INVITATION_TTL_MS: u64 = 24 * HOUR;
/// A show waiting for its agent holds its invitation only this long.
pub const SHOW_WAITING_TTL_MS: u64 = 30 * MINUTE;
/// A guest token keeps working this long after the game finished.
pub const GUEST_TOKEN_GRACE_MS: u64 = HOUR;
/// Time both players get to open the game page after joining; after it a game where someone never
/// showed up is cancelled lazily.
pub const READY_DEADLINE_MS: u64 = 15 * MINUTE;

/// Games one user may create per hour; guards against a create/cancel loop.
pub const CREATE_GAME_LIMIT: u32 = 10;
pub const CREATE_GAME_WINDOW_MS: u64 = HOUR;

/// Each player's clock budget.
pub const INITIAL_CLOCK_MS: i64 = 10 * MINUTE as i64;

/// Invitation alphabet: digits and letters without the look-alikes `0 O 1 I L`.
/// 8 digits + 23 letters = 31 symbols, so a code carries 31^8 (~39.6 bits) of entropy.
pub const INVITATION_CODE_ALPHABET: &str = "23456789ABCDEFGHJKMNPQRSTUVWXYZ";
pub const INVITATION_CODE_LENGTH: usize = 8;

/// Failed invitation-code lookups share one pool: 20 per client address in 15 minutes; when the address
/// cannot be trusted a global pool of 300 applies instead.
pub const INVITATION_LOOKUP_LIMIT: u32 = 20;
pub const INVITATION_LOOKUP_GLOBAL_LIMIT: u32 = 300;
pub const INVITATION_LOOKUP_WINDOW_MS: u64 = 15 * MINUTE;

/// A rematch offer stays open this long.
pub const REMATCH_TTL_MS: u64 = 5 * MINUTE;
/// Rematch offers one player may make per hour.
pub const REMATCH_OFFER_LIMIT: u32 = 10;
pub const REMATCH_OFFER_WINDOW_MS: u64 = HOUR;

/// Spectators of a public game: at most 20 per game; present while seen in the last 15 s; access slides
/// one hour from the last read; presence is written at most every 10 s.
pub const SPECTATORS_MAX: usize = 20;
pub const SPECTATOR_PRESENCE_MS: u64 = 15 * 1000;
pub const SPECTATOR_ACCESS_MS: u64 = HOUR;
pub const SPECTATOR_TOUCH_INTERVAL_MS: u64 = 10 * 1000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_match_the_reference_implementation() {
        assert_eq!(INITIAL_CLOCK_MS, 600_000);
        assert_eq!(INVITATION_TTL_MS, 86_400_000);
        assert_eq!(SHOW_WAITING_TTL_MS, 1_800_000);
        assert_eq!(GUEST_TOKEN_GRACE_MS, 3_600_000);
        assert_eq!(READY_DEADLINE_MS, 900_000);
        assert_eq!((CREATE_GAME_LIMIT, CREATE_GAME_WINDOW_MS), (10, 3_600_000));
        assert_eq!(
            (
                INVITATION_LOOKUP_LIMIT,
                INVITATION_LOOKUP_GLOBAL_LIMIT,
                INVITATION_LOOKUP_WINDOW_MS
            ),
            (20, 300, 900_000)
        );
        assert_eq!(
            (REMATCH_TTL_MS, REMATCH_OFFER_LIMIT, REMATCH_OFFER_WINDOW_MS),
            (300_000, 10, 3_600_000)
        );
        assert_eq!(
            (
                SPECTATORS_MAX,
                SPECTATOR_PRESENCE_MS,
                SPECTATOR_ACCESS_MS,
                SPECTATOR_TOUCH_INTERVAL_MS
            ),
            (20, 15_000, 3_600_000, 10_000)
        );
    }

    #[test]
    fn invitation_alphabet_has_31_distinct_symbols_without_look_alikes() {
        assert_eq!(INVITATION_CODE_ALPHABET.chars().count(), 31);
        let distinct: std::collections::HashSet<char> = INVITATION_CODE_ALPHABET.chars().collect();
        assert_eq!(distinct.len(), 31);
        for banned in ['0', 'O', '1', 'I', 'L'] {
            assert!(!INVITATION_CODE_ALPHABET.contains(banned));
        }
        assert_eq!(INVITATION_CODE_LENGTH, 8);
    }
}
