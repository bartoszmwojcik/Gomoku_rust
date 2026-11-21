//! "You are back in the game": when the notice is due after a player returns to the tab.

/// Away for longer than this and the player is told they are back; shorter absences pass silently.
pub const RETURN_NOTICE_THRESHOLD_MS: i64 = 30 * 1000;

/// The notice is only worth showing while it is news: a read finishing later than this after the return is
/// too late.
pub const RETURN_NOTICE_WINDOW_MS: i64 = 10 * 1000;

/// Whether the notice is due: the tab was hidden at `hidden_at_ms` and is visible again at `returned_at_ms`
/// (both on the same clock). Strictly longer than the threshold; a clock set back never shows it.
pub fn should_show_return_notice(
    hidden_at_ms: Option<i64>,
    returned_at_ms: i64,
    threshold_ms: i64,
) -> bool {
    hidden_at_ms.is_some_and(|hidden| returned_at_ms - hidden > threshold_ms)
}

/// Whether a successful read finished soon enough after the return for the notice to still be accurate: at
/// most `window_ms` later, boundary included. A clock set back counts as fresh.
pub fn is_return_notice_fresh(returned_at_ms: i64, read_at_ms: i64, window_ms: i64) -> bool {
    read_at_ms - returned_at_ms <= window_ms
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shows_the_notice_only_after_a_long_enough_absence() {
        let t = RETURN_NOTICE_THRESHOLD_MS;
        assert!(!should_show_return_notice(None, 100_000, t));
        assert!(!should_show_return_notice(Some(0), t, t));
        assert!(should_show_return_notice(Some(0), t + 1, t));
        assert!(!should_show_return_notice(Some(100_000), 0, t));
        assert!(should_show_return_notice(Some(0), 5_000, 1_000));
    }

    #[test]
    fn a_late_read_is_no_longer_news() {
        let w = RETURN_NOTICE_WINDOW_MS;
        assert!(is_return_notice_fresh(1_000, 1_000 + w, w));
        assert!(!is_return_notice_fresh(1_000, 1_000 + w + 1, w));
        assert!(is_return_notice_fresh(1_000, 500, w));
    }
}
