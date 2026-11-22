//! Per-move search budget from the bot's level, frozen memory tier and remaining clock.

use super::choose_move::{BotLevel, SearchBudget};
use super::profile::{BOT_MEMORY_MB_MAX, FALLBACK_TIER, bot_tier_of};

pub const BOT_LOW_CLOCK_MS: i64 = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeLimit {
    pub soft_ms: i64,
    pub ceiling_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BotTimeLimits {
    pub level1: TimeLimit,
    pub level2: TimeLimit,
}

pub const DEFAULT_BOT_TIME_LIMITS: BotTimeLimits = BotTimeLimits {
    level1: TimeLimit {
        soft_ms: 50,
        ceiling_ms: 250,
    },
    level2: TimeLimit {
        soft_ms: 1500,
        ceiling_ms: 3000,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BotStrength {
    pub level: BotLevel,
    pub memory_mb: Option<i64>,
    pub depth_cap: Option<i32>,
}

/// Whole megabytes, `0` for anything unusable, at most the column maximum.
pub fn clamp_bot_memory_mb(value: f64) -> i64 {
    if !value.is_finite() || value <= 0.0 {
        0
    } else {
        (value.floor() as i64).min(BOT_MEMORY_MB_MAX)
    }
}

/// The strength a game gets from the available memory, or `None` when it is too low to play.
pub fn bot_strength_for(level: BotLevel, available_mb: f64) -> Option<BotStrength> {
    let memory = clamp_bot_memory_mb(available_mb);
    let tier = bot_tier_of(memory)?;
    Some(match level {
        BotLevel::One => BotStrength {
            level,
            memory_mb: None,
            depth_cap: None,
        },
        BotLevel::Two => BotStrength {
            level,
            memory_mb: Some(memory),
            depth_cap: Some(tier.depth_cap),
        },
    })
}

pub struct BudgetInput {
    pub level: BotLevel,
    pub memory_mb: Option<i64>,
    pub depth_cap: Option<i32>,
    pub remaining_clock_ms: i64,
    pub limits: BotTimeLimits,
}

/// The level actually played (level 1 when the clock is nearly out) and its budget.
pub fn budget_for(input: &BudgetInput) -> (BotLevel, SearchBudget) {
    let played = if input.remaining_clock_ms < BOT_LOW_CLOCK_MS {
        BotLevel::One
    } else {
        input.level
    };
    let remaining = input.remaining_clock_ms.max(0);
    let limit = match played {
        BotLevel::One => input.limits.level1,
        BotLevel::Two => input.limits.level2,
    };
    let soft = limit.soft_ms.min(remaining / 30) as f64;
    let ceiling = limit.ceiling_ms.min(remaining / 10) as f64;
    if played == BotLevel::One {
        return (
            played,
            SearchBudget {
                soft_ms: soft,
                ceiling_ms: ceiling,
                max_depth: 1,
                tt_bytes: 0,
                max_nodes: None,
            },
        );
    }
    let tier = input
        .memory_mb
        .and_then(bot_tier_of)
        .unwrap_or(FALLBACK_TIER);
    (
        played,
        SearchBudget {
            soft_ms: soft,
            ceiling_ms: ceiling,
            max_depth: input.depth_cap.unwrap_or(tier.depth_cap),
            tt_bytes: tier.tt_mb * 1024 * 1024,
            max_nodes: None,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(level: BotLevel, mem: Option<i64>, cap: Option<i32>, remaining: i64) -> BudgetInput {
        BudgetInput {
            level,
            memory_mb: mem,
            depth_cap: cap,
            remaining_clock_ms: remaining,
            limits: DEFAULT_BOT_TIME_LIMITS,
        }
    }

    fn mib(n: i64) -> i64 {
        n * 1024 * 1024
    }

    #[test]
    fn clamp() {
        assert_eq!(clamp_bot_memory_mb(767.9), 767);
        assert_eq!(clamp_bot_memory_mb(1_000_000.0), 32_767);
        for bad in [f64::NAN, f64::INFINITY, -5.0, 0.0] {
            assert_eq!(clamp_bot_memory_mb(bad), 0);
        }
    }

    #[test]
    fn strength() {
        assert_eq!(bot_strength_for(BotLevel::Two, 255.9), None);
        let s = |mb, cap| BotStrength {
            level: BotLevel::Two,
            memory_mb: Some(mb),
            depth_cap: Some(cap),
        };
        assert_eq!(bot_strength_for(BotLevel::Two, 256.0), Some(s(256, 6)));
        assert_eq!(bot_strength_for(BotLevel::Two, 768.0), Some(s(768, 8)));
        assert_eq!(bot_strength_for(BotLevel::Two, 1536.0), Some(s(1536, 10)));
        assert_eq!(bot_strength_for(BotLevel::Two, 3072.0), Some(s(3072, 12)));
        assert_eq!(
            bot_strength_for(BotLevel::Two, 64000.0),
            Some(s(32_767, 12))
        );
        assert_eq!(
            bot_strength_for(BotLevel::One, 4096.0),
            Some(BotStrength {
                level: BotLevel::One,
                memory_mb: None,
                depth_cap: None
            })
        );
    }

    #[test]
    fn budgets() {
        let (l, b) = budget_for(&input(BotLevel::Two, Some(768), Some(8), 600_000));
        assert_eq!(
            (l, b.soft_ms, b.ceiling_ms, b.max_depth, b.tt_bytes),
            (BotLevel::Two, 1500.0, 3000.0, 8, mib(16))
        );
        assert_eq!(
            budget_for(&input(BotLevel::Two, Some(4000), Some(12), 600_000))
                .1
                .tt_bytes,
            mib(64)
        );
        let (_, b) = budget_for(&input(BotLevel::Two, Some(768), Some(8), 15_000));
        assert_eq!((b.soft_ms, b.ceiling_ms), (500.0, 1500.0));
        let (l, b) = budget_for(&input(BotLevel::One, None, None, 1200));
        assert_eq!(
            (l, b.soft_ms, b.ceiling_ms, b.max_depth, b.tt_bytes),
            (BotLevel::One, 40.0, 120.0, 1, 0)
        );
        assert_eq!(
            budget_for(&input(BotLevel::Two, Some(768), Some(8), 1000)).0,
            BotLevel::Two
        );
        let (l, b) = budget_for(&input(BotLevel::Two, Some(768), Some(8), 999));
        assert_eq!(
            (l, b.soft_ms, b.ceiling_ms, b.max_depth, b.tt_bytes),
            (BotLevel::One, 33.0, 99.0, 1, 0)
        );
        let (_, b) = budget_for(&input(BotLevel::Two, Some(768), Some(8), -50));
        assert_eq!((b.soft_ms, b.ceiling_ms), (0.0, 0.0));
    }

    #[test]
    fn custom_limits_and_fallbacks() {
        let limits = BotTimeLimits {
            level1: TimeLimit {
                soft_ms: 10,
                ceiling_ms: 20,
            },
            level2: TimeLimit {
                soft_ms: 700,
                ceiling_ms: 900,
            },
        };
        let custom = BudgetInput {
            limits,
            ..input(BotLevel::Two, Some(300), Some(6), 600_000)
        };
        let (_, b) = budget_for(&custom);
        assert_eq!(
            (b.soft_ms, b.ceiling_ms, b.max_depth, b.tt_bytes),
            (700.0, 900.0, 6, mib(8))
        );
        let (_, b) = budget_for(&input(BotLevel::Two, None, None, 600_000));
        assert_eq!((b.max_depth, b.tt_bytes), (6, mib(8)));
        let (_, b) = budget_for(&input(BotLevel::Two, Some(100), Some(7), 600_000));
        assert_eq!((b.max_depth, b.tt_bytes), (7, mib(8)));
    }
}
