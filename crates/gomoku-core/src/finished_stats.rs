//! Statistics of finished games from the persistent pair counters: totals, today and the last seven days, per pair of
//! participant kinds, in the Warsaw calendar.

pub const FINISHED_STATS_TIME_ZONE: &str = "Europe/Warsaw";

/// The pairs always listed, in display order (each alphabetical, as stored).
pub const KNOWN_PAIRS: [(&str, &str); 7] = [
    ("account", "account"),
    ("account", "guest"),
    ("account", "agent"),
    ("guest", "guest"),
    ("agent", "guest"),
    ("account", "bot"),
    ("bot", "guest"),
];

/// One row of the aggregation: a pair of kinds with its three counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairRow {
    pub kind_a: String,
    pub kind_b: String,
    pub total: i64,
    pub today: i64,
    pub last_7_days: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairCounter {
    /// `"<a>-<b>"`.
    pub pair: String,
    pub kinds: (String, String),
    pub total: i64,
    pub today: i64,
    pub last_7_days: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinishedGameStats {
    pub total: i64,
    pub today: i64,
    pub last_7_days: i64,
    pub pairs: Vec<PairCounter>,
}

/// The seven known pairs always (zeros without a row), then unknown pairs by name; totals sum over all pairs.
pub fn build_finished_game_stats(rows: &[PairRow]) -> FinishedGameStats {
    let counter = |a: &str, b: &str| {
        let found = rows.iter().find(|r| r.kind_a == a && r.kind_b == b);
        PairCounter {
            pair: format!("{a}-{b}"),
            kinds: (a.to_owned(), b.to_owned()),
            total: found.map_or(0, |r| r.total),
            today: found.map_or(0, |r| r.today),
            last_7_days: found.map_or(0, |r| r.last_7_days),
        }
    };
    let mut pairs: Vec<PairCounter> = KNOWN_PAIRS.iter().map(|(a, b)| counter(a, b)).collect();
    let mut unknown: Vec<PairCounter> = rows
        .iter()
        .filter(|r| !KNOWN_PAIRS.contains(&(r.kind_a.as_str(), r.kind_b.as_str())))
        .map(|r| counter(&r.kind_a, &r.kind_b))
        .collect();
    unknown.sort_by(|x, y| x.pair.cmp(&y.pair));
    pairs.extend(unknown);
    FinishedGameStats {
        total: pairs.iter().map(|p| p.total).sum(),
        today: pairs.iter().map(|p| p.today).sum(),
        last_7_days: pairs.iter().map(|p| p.last_7_days).sum(),
        pairs,
    }
}

/// How a pair is named on the panel (unknown kinds as they are).
pub fn pair_label(kinds: &(String, String)) -> String {
    let name = |kind: &str| match kind {
        "account" => "konto".to_owned(),
        "guest" => "gość".to_owned(),
        "agent" => "agent".to_owned(),
        "bot" => "bot".to_owned(),
        other => other.to_owned(),
    };
    format!("{} – {}", name(&kinds.0), name(&kinds.1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(a: &str, b: &str, total: i64, today: i64, last: i64) -> PairRow {
        PairRow {
            kind_a: a.into(),
            kind_b: b.into(),
            total,
            today,
            last_7_days: last,
        }
    }

    #[test]
    fn no_rows_list_the_known_pairs_in_order() {
        let stats = build_finished_game_stats(&[]);
        assert_eq!((stats.total, stats.today, stats.last_7_days), (0, 0, 0));
        let names: Vec<&str> = stats.pairs.iter().map(|p| p.pair.as_str()).collect();
        assert_eq!(
            names,
            [
                "account-account",
                "account-guest",
                "account-agent",
                "guest-guest",
                "agent-guest",
                "account-bot",
                "bot-guest"
            ]
        );
        assert!(KNOWN_PAIRS.iter().all(|(a, b)| a <= b));
    }

    #[test]
    fn rows_fill_their_pairs_and_the_totals() {
        let stats = build_finished_game_stats(&[
            row("guest", "guest", 3, 1, 2),
            row("account", "account", 4, 0, 4),
        ]);
        assert_eq!(stats.pairs[0].total, 4);
        assert_eq!(
            stats.pairs[0].kinds,
            ("account".to_owned(), "account".to_owned())
        );
        assert_eq!(
            (
                stats.pairs[3].total,
                stats.pairs[3].today,
                stats.pairs[3].last_7_days
            ),
            (3, 1, 2)
        );
        assert_eq!((stats.total, stats.today, stats.last_7_days), (7, 1, 6));
        let bots = build_finished_game_stats(&[
            row("account", "bot", 2, 1, 2),
            row("bot", "guest", 5, 0, 3),
        ]);
        assert_eq!((bots.total, bots.today, bots.last_7_days), (7, 1, 5));
    }

    #[test]
    fn unknown_pairs_follow_alphabetically_and_count() {
        let stats = build_finished_game_stats(&[
            row("robot", "robot", 1, 1, 1),
            row("llm", "account", 2, 0, 2),
        ]);
        let tail: Vec<&str> = stats.pairs[7..].iter().map(|p| p.pair.as_str()).collect();
        assert_eq!(tail, ["llm-account", "robot-robot"]);
        assert_eq!(stats.total, 3);
    }

    #[test]
    fn pairs_are_named_in_polish() {
        assert_eq!(
            pair_label(&("account".into(), "guest".into())),
            "konto – gość"
        );
        assert_eq!(
            pair_label(&("agent".into(), "guest".into())),
            "agent – gość"
        );
        assert_eq!(pair_label(&("bot".into(), "ufo".into())), "bot – ufo");
    }
}
