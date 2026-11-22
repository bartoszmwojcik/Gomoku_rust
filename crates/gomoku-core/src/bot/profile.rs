//! Strength profile of the bot: what the available memory buys (table size and depth cap).

use super::choose_move::BotLevel;

pub const BOT_MIN_MEMORY_MB: i64 = 256;
pub const BOT_MEMORY_MB_MAX: i64 = 32_767;
pub const BOT_UNAVAILABLE_MESSAGE: &str =
    "Bot jest teraz niedostępny. Spróbuj za chwilę albo zaproś osobę.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tier {
    pub tt_mb: i64,
    pub depth_cap: i32,
}

/// Fallback for a level-2 seat stored without parameters.
pub const FALLBACK_TIER: Tier = Tier {
    tt_mb: 8,
    depth_cap: 6,
};

pub fn bot_tier_of(memory_mb: i64) -> Option<Tier> {
    match memory_mb {
        m if m >= 3072 => Some(Tier {
            tt_mb: 64,
            depth_cap: 12,
        }),
        m if m >= 1536 => Some(Tier {
            tt_mb: 32,
            depth_cap: 10,
        }),
        m if m >= 768 => Some(Tier {
            tt_mb: 16,
            depth_cap: 8,
        }),
        m if m >= 256 => Some(Tier {
            tt_mb: 8,
            depth_cap: 6,
        }),
        _ => None,
    }
}

/// `"Bot · poziom N"`, plus the table size for a level-2 bot with a known tier.
pub fn bot_display_name(level: BotLevel, memory_mb: Option<i64>) -> String {
    let base = format!("Bot · poziom {}", level.number());
    match (level, memory_mb.and_then(bot_tier_of)) {
        (BotLevel::Two, Some(tier)) => format!("{base} · {} MB", tier.tt_mb),
        _ => base,
    }
}

/// Table size shown to players: level 2 with a tier only.
pub fn bot_table_mb(level: BotLevel, memory_mb: Option<i64>) -> Option<i64> {
    match level {
        BotLevel::Two => memory_mb.and_then(bot_tier_of).map(|t| t.tt_mb),
        BotLevel::One => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_boundaries() {
        assert_eq!(bot_tier_of(255), None);
        assert_eq!(
            bot_tier_of(256),
            Some(Tier {
                tt_mb: 8,
                depth_cap: 6
            })
        );
        assert_eq!(bot_tier_of(767).map(|t| t.tt_mb), Some(8));
        assert_eq!(
            bot_tier_of(768),
            Some(Tier {
                tt_mb: 16,
                depth_cap: 8
            })
        );
        assert_eq!(bot_tier_of(1535).map(|t| t.tt_mb), Some(16));
        assert_eq!(
            bot_tier_of(1536),
            Some(Tier {
                tt_mb: 32,
                depth_cap: 10
            })
        );
        assert_eq!(bot_tier_of(3071).map(|t| t.tt_mb), Some(32));
        assert_eq!(
            bot_tier_of(3072),
            Some(Tier {
                tt_mb: 64,
                depth_cap: 12
            })
        );
    }

    #[test]
    fn names() {
        assert_eq!(bot_display_name(BotLevel::One, None), "Bot · poziom 1");
        assert_eq!(
            bot_display_name(BotLevel::One, Some(4096)),
            "Bot · poziom 1"
        );
        assert_eq!(
            bot_display_name(BotLevel::Two, Some(768)),
            "Bot · poziom 2 · 16 MB"
        );
        assert_eq!(
            bot_display_name(BotLevel::Two, Some(300)),
            "Bot · poziom 2 · 8 MB"
        );
        assert_eq!(bot_display_name(BotLevel::Two, None), "Bot · poziom 2");
        assert_eq!(bot_display_name(BotLevel::Two, Some(100)), "Bot · poziom 2");
    }

    #[test]
    fn table_mb() {
        assert_eq!(bot_table_mb(BotLevel::One, Some(4096)), None);
        assert_eq!(bot_table_mb(BotLevel::Two, Some(768)), Some(16));
        assert_eq!(bot_table_mb(BotLevel::Two, None), None);
    }
}
