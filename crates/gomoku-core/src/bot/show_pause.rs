//! The pace of a show: the bot waits a deterministic pause (counted from the start of its turn) before it searches, and
//! searches with tighter limits, so a show reads like two players and never runs out of clock.

use super::budget::{BotTimeLimits, TimeLimit};
use super::seed::seed_of;

/// Divisor of the clock the pause may take at most.
pub const SHOW_PAUSE_CLOCK_DIVISOR: i64 = 4;

/// The pause before the bot's move number `move_number`: `min_ms..=max_ms`, the same for a game and move after a restart.
pub fn show_pause_ms(game_id: &str, move_number: u32, min_ms: i64, max_ms: i64) -> i64 {
    let span = u32::try_from(max_ms - min_ms + 1).unwrap_or(1).max(1);
    let salt = format!("show-pause:{game_id}");
    min_ms + i64::from(seed_of(&salt, move_number) % span)
}

/// A pause never takes more than a quarter of the bot's remaining clock.
pub fn clip_show_pause(pause_ms: i64, clock_ms: i64) -> i64 {
    pause_ms.min((clock_ms / SHOW_PAUSE_CLOCK_DIVISOR).max(0))
}

/// The time limits of a show: level 2 gets `l2_soft_ms` and a ceiling of twice that, at most the normal ceiling.
pub fn show_bot_limits(limits: BotTimeLimits, l2_soft_ms: i64) -> BotTimeLimits {
    let ceiling = limits.level2.ceiling_ms.min(2 * l2_soft_ms);
    BotTimeLimits {
        level1: limits.level1,
        level2: TimeLimit {
            soft_ms: l2_soft_ms.min(ceiling),
            ceiling_ms: ceiling,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bot::budget::DEFAULT_BOT_TIME_LIMITS;

    const ID: &str = "4d293981-7b3c-4f81-a1af-0a9e3bf3636b";

    #[test]
    fn the_pause_is_deterministic_and_inside_the_range() {
        let pauses: Vec<i64> = [1, 2, 3, 7]
            .iter()
            .map(|&m| show_pause_ms(ID, m, 1000, 2000))
            .collect();
        assert_eq!(pauses, [1627, 1485, 1343, 1776]);
        assert_eq!(
            show_pause_ms(ID, 7, 1000, 2000),
            show_pause_ms(ID, 7, 1000, 2000)
        );
        assert_eq!(seed_of(&format!("show-pause:{ID}"), 7), 958_971_789);
        let wide: Vec<i64> = (1..=50).map(|m| show_pause_ms(ID, m, 10, 11)).collect();
        assert!(wide.contains(&10) && wide.contains(&11));
        assert!(wide.iter().all(|p| (10..=11).contains(p)));
        assert_eq!(show_pause_ms(ID, 5, 0, 0), 0);
        assert_eq!(show_pause_ms(ID, 5, 700, 700), 700);
    }

    #[test]
    fn the_pause_is_clipped_to_a_quarter_of_the_clock() {
        for (pause, clock, expected) in [
            (2000, 600_000, 2000),
            (2000, 8000, 2000),
            (2000, 7999, 1999),
            (8000, 10_000, 2500),
            (1500, 3, 0),
            (2000, 0, 0),
            (2000, -500, 0),
        ] {
            assert_eq!(clip_show_pause(pause, clock), expected, "{pause}/{clock}");
        }
    }

    #[test]
    fn show_limits_tighten_level_two_only() {
        for (soft, expected) in [
            (1500, (1500, 3000)),
            (1000, (1000, 2000)),
            (700, (700, 1400)),
            (2000, (2000, 3000)),
            (5000, (3000, 3000)),
        ] {
            let limits = show_bot_limits(DEFAULT_BOT_TIME_LIMITS, soft);
            assert_eq!(
                (limits.level2.soft_ms, limits.level2.ceiling_ms),
                expected,
                "{soft}"
            );
            assert_eq!(limits.level1, DEFAULT_BOT_TIME_LIMITS.level1);
        }
    }
}
