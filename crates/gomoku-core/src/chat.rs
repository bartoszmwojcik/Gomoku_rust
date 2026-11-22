//! Game chat: what a message may contain and what state the chat of a game is in. Pure rules; no I/O.

use std::sync::LazyLock;

use regex::Regex;
use unicode_normalization::UnicodeNormalization;
use unicode_properties::{GeneralCategory, UnicodeGeneralCategory};

use crate::constants::{CHAT_MAX_COMBINING_RUN, CHAT_RAW_MAX_UNITS};
use crate::types::GameStatus;

/// Why a message was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatRejection {
    NotText,
    Empty,
    TooLong,
    ControlChar,
    InvisibleChar,
    CombiningRun,
    Url,
}

impl ChatRejection {
    /// The stable code used in answers and catalog keys.
    pub fn as_str(self) -> &'static str {
        match self {
            ChatRejection::NotText => "not_text",
            ChatRejection::Empty => "empty",
            ChatRejection::TooLong => "too_long",
            ChatRejection::ControlChar => "control_char",
            ChatRejection::InvisibleChar => "invisible_char",
            ChatRejection::CombiningRun => "combining_run",
            ChatRejection::Url => "url",
        }
    }

    /// The Polish sentence (server fallback; clients use the catalog).
    pub fn message(self) -> &'static str {
        match self {
            ChatRejection::NotText => "Wiadomość musi być tekstem.",
            ChatRejection::Empty => "Wpisz wiadomość.",
            ChatRejection::TooLong => "Wiadomość jest za długa.",
            ChatRejection::ControlChar => {
                "Wiadomość nie może zawierać znaków sterujących ani nowych linii."
            }
            ChatRejection::InvisibleChar => "Wiadomość nie może zawierać niewidocznych znaków.",
            ChatRejection::CombiningRun => "Wiadomość zawiera zbyt wiele znaków łączących.",
            ChatRejection::Url => "Wiadomość nie może zawierać adresów internetowych.",
        }
    }
}

const ZWJ: char = '\u{200D}';
const INVISIBLE_POINTS: [char; 6] = [
    '\u{034F}', '\u{115F}', '\u{1160}', '\u{3164}', '\u{FFA0}', '\u{2800}',
];
const TLDS: [&str; 30] = [
    "com", "net", "org", "info", "biz", "xyz", "io", "gg", "ly", "cc", "tv", "ws", "ai", "app",
    "dev", "link", "site", "online", "shop", "club", "pl", "eu", "de", "uk", "ru", "cz", "sk",
    "fr", "es", "nl",
];

#[allow(clippy::expect_used)]
static PICTOGRAPHIC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\p{Extended_Pictographic}$").expect("the pattern is a valid regex")
});
#[allow(clippy::expect_used)]
static SCHEME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)[a-z][a-z0-9+.-]*://").expect("the pattern is a valid regex")
});
#[allow(clippy::expect_used)]
static DANGEROUS_SCHEME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(^|[^\p{L}\p{N}])(mailto|javascript|data|file|vbscript):\S")
        .expect("the pattern is a valid regex")
});
#[allow(clippy::expect_used)]
static WWW: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(^|[^\p{L}\p{N}])www\.").expect("the pattern is a valid regex")
});
#[allow(clippy::expect_used)]
static IPV4: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(^|[^\d.])(\d{1,3}\.){3}\d{1,3}").expect("the pattern is a valid regex")
});

fn is_pictographic(c: char) -> bool {
    let mut buffer = [0u8; 4];
    PICTOGRAPHIC.is_match(c.encode_utf8(&mut buffer))
}

fn is_mark(category: GeneralCategory) -> bool {
    matches!(
        category,
        GeneralCategory::NonspacingMark
            | GeneralCategory::SpacingMark
            | GeneralCategory::EnclosingMark
    )
}

fn is_invisible(c: char, category: GeneralCategory) -> bool {
    matches!(
        category,
        GeneralCategory::Format | GeneralCategory::PrivateUse | GeneralCategory::Unassigned
    ) || INVISIBLE_POINTS.contains(&c)
        || ('\u{FE00}'..='\u{FE0E}').contains(&c)
        || ('\u{E0100}'..='\u{E01EF}').contains(&c)
        || (category == GeneralCategory::SpaceSeparator && c != ' ')
}

/// The normalized text: NFKC, no forbidden characters, ASCII spaces trimmed. Length is not checked here.
fn clean(raw: &str) -> Result<String, ChatRejection> {
    let points: Vec<char> = raw.nfkc().collect();
    let mut run = 0usize;
    let mut out = String::with_capacity(raw.len());
    for (i, &c) in points.iter().enumerate() {
        let category = c.general_category();
        if matches!(
            category,
            GeneralCategory::Control
                | GeneralCategory::Surrogate
                | GeneralCategory::LineSeparator
                | GeneralCategory::ParagraphSeparator
        ) {
            return Err(ChatRejection::ControlChar);
        }
        let joins_emoji = c == ZWJ
            && i > 0
            && points.get(i + 1).is_some_and(|&next| is_pictographic(next))
            && is_pictographic(points[i - 1]);
        if !joins_emoji && is_invisible(c, category) {
            return Err(ChatRejection::InvisibleChar);
        }
        if is_mark(category) {
            run += 1;
            if run > CHAT_MAX_COMBINING_RUN {
                return Err(ChatRejection::CombiningRun);
            }
        } else {
            run = 0;
        }
        out.push(c);
    }
    Ok(out.trim_matches(' ').to_owned())
}

fn is_alnum(c: char) -> bool {
    c.is_alphanumeric()
}

/// A domain-like token: `label.tld` where the whole run after the dot is a known TLD written in one case.
fn has_domain(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    for (dot, &c) in chars.iter().enumerate() {
        if c != '.' {
            continue;
        }
        let before: Vec<char> = chars[..dot]
            .iter()
            .rev()
            .take_while(|&&x| is_alnum(x) || x == '-')
            .copied()
            .collect();
        if !before.iter().any(|&x| is_alnum(x)) {
            continue;
        }
        let tld: String = chars[dot + 1..]
            .iter()
            .take_while(|&&x| is_alnum(x))
            .collect();
        if TLDS
            .iter()
            .any(|known| tld == *known || tld == known.to_uppercase())
        {
            return true;
        }
    }
    false
}

fn has_ipv4(text: &str) -> bool {
    IPV4.find_iter(text)
        .any(|m| !text[m.end()..].starts_with(|c: char| c.is_ascii_digit()))
}

fn looks_like_url(text: &str) -> bool {
    let text = text.replace('\u{3002}', ".");
    SCHEME.is_match(&text)
        || DANGEROUS_SCHEME.is_match(&text)
        || WWW.is_match(&text)
        || has_ipv4(&text)
        || has_domain(&text)
}

/// Validates and normalizes a message. `max_length` is counted in code points after normalization.
pub fn normalize_chat_message(raw: &str, max_length: usize) -> Result<String, ChatRejection> {
    if raw.encode_utf16().count() > CHAT_RAW_MAX_UNITS {
        return Err(ChatRejection::TooLong);
    }
    let text = clean(raw)?;
    if text.is_empty() {
        return Err(ChatRejection::Empty);
    }
    if text.chars().count() > max_length {
        return Err(ChatRejection::TooLong);
    }
    if looks_like_url(&text) {
        return Err(ChatRejection::Url);
    }
    Ok(text)
}

/// The length the composer counts: code points of the NFKC form with ASCII spaces trimmed.
pub fn chat_message_length(text: &str) -> usize {
    text.nfkc()
        .collect::<String>()
        .trim_matches(' ')
        .chars()
        .count()
}

/// How a chat looks to one asker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatState {
    Open,
    DisabledByYou,
    DisabledByOpponent,
    Unavailable,
    Closed,
}

impl ChatState {
    pub fn as_str(self) -> &'static str {
        match self {
            ChatState::Open => "open",
            ChatState::DisabledByYou => "disabled_by_you",
            ChatState::DisabledByOpponent => "disabled_by_opponent",
            ChatState::Unavailable => "unavailable",
            ChatState::Closed => "closed",
        }
    }

    /// Whether the asker may see the conversation.
    pub fn readable(self) -> bool {
        matches!(self, ChatState::Open | ChatState::DisabledByOpponent)
    }
}

/// The chat state of a player. `mine` / `theirs` are the two switches; in a public game the other switch does not
/// block the asker (they can still write to the spectators).
pub fn derive_chat_state(
    status: GameStatus,
    window_open: bool,
    mine: bool,
    theirs: Option<bool>,
    is_public: bool,
) -> ChatState {
    match status {
        GameStatus::Waiting | GameStatus::Cancelled => ChatState::Unavailable,
        GameStatus::Finished if !window_open => ChatState::Closed,
        _ if !mine => ChatState::DisabledByYou,
        _ if theirs == Some(false) && !is_public => ChatState::DisabledByOpponent,
        _ => ChatState::Open,
    }
}

/// The chat state of a spectator: players' switches never touch it.
pub fn derive_spectator_chat_state(status: GameStatus, window_open: bool) -> ChatState {
    match status {
        GameStatus::Waiting | GameStatus::Cancelled => ChatState::Unavailable,
        GameStatus::Finished if !window_open => ChatState::Closed,
        _ => ChatState::Open,
    }
}

/// How long a sender still has to wait; `None` when they never wrote.
pub fn retry_after_ms(since_last_ms: Option<f64>, min_interval_ms: u64) -> u64 {
    match since_last_ms {
        None => 0,
        Some(since) => {
            let wait = (min_interval_ms as f64 - since).ceil();
            if wait <= 0.0 { 0 } else { wait as u64 }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn norm(text: &str) -> Result<String, ChatRejection> {
        normalize_chat_message(text, 200)
    }

    #[test]
    fn accepts_plain_text_and_normalizes() {
        assert_eq!(norm("  hej  ").as_deref(), Ok("hej"));
        assert_eq!(norm("™").as_deref(), Ok("TM"));
        assert_eq!(norm("Cześć.Co tam").as_deref(), Ok("Cześć.Co tam"));
        assert_eq!(norm("Dobra.Gramy").as_deref(), Ok("Dobra.Gramy"));
        assert_eq!(norm("1.5").as_deref(), Ok("1.5"));
        assert_eq!(norm("ok…").as_deref(), Ok("ok..."));
        assert_eq!(norm("Jaka data: dziś").as_deref(), Ok("Jaka data: dziś"));
        assert_eq!(
            norm("<script>alert(1)</script>").as_deref(),
            Ok("<script>alert(1)</script>")
        );
        assert!(norm("שלום עולם").is_ok());
        assert!(norm("مرحبا").is_ok());
    }

    #[test]
    fn length_is_counted_in_code_points_after_normalization() {
        assert_eq!(normalize_chat_message("", 200), Err(ChatRejection::Empty));
        assert_eq!(
            normalize_chat_message("   ", 200),
            Err(ChatRejection::Empty)
        );
        assert!(normalize_chat_message(&"a".repeat(200), 200).is_ok());
        assert_eq!(
            normalize_chat_message(&"a".repeat(201), 200),
            Err(ChatRejection::TooLong)
        );
        // U+FDFA expands to 18 code points.
        assert_eq!(
            normalize_chat_message(&"\u{FDFA}".repeat(12), 200),
            Err(ChatRejection::TooLong)
        );
        assert_eq!(
            normalize_chat_message(&"a".repeat(1001), 2000),
            Err(ChatRejection::TooLong)
        );
        assert_eq!(chat_message_length("  ™ "), 2);
    }

    #[test]
    fn emoji_joiners_are_allowed_only_between_pictographs() {
        assert!(norm("👨\u{200D}👩\u{200D}👧").is_ok());
        assert_eq!(norm("\u{200D}👨"), Err(ChatRejection::InvisibleChar));
        assert_eq!(norm("👨\u{200D}"), Err(ChatRejection::InvisibleChar));
        assert_eq!(norm("a\u{200D}b"), Err(ChatRejection::InvisibleChar));
        assert_eq!(norm("a\u{200D}👨"), Err(ChatRejection::InvisibleChar));
    }

    #[test]
    fn control_and_invisible_characters_are_refused() {
        for text in ["\u{0}", "a\nb", "a\u{2028}b", "a\u{2029}b"] {
            assert_eq!(norm(text), Err(ChatRejection::ControlChar), "{text:?}");
        }
        for text in [
            "\u{202E}",
            "\u{2066}",
            "\u{200B}",
            "\u{FEFF}",
            "\u{2800}",
            "\u{E000}",
            "\u{0378}",
            "\u{FE00}",
            "\u{E0100}",
            "a\u{3000}b".replace('\u{3000}', "\u{1680}").as_str(),
        ] {
            assert_eq!(
                norm(&format!("a{text}b")),
                Err(ChatRejection::InvisibleChar),
                "{text:?}"
            );
        }
        assert!(norm("a\u{FE0F}").is_ok());
        assert_eq!(norm("a\u{3000}b").as_deref(), Ok("a b"));
    }

    #[test]
    fn combining_runs_are_limited() {
        assert!(norm(&format!("x{}", "\u{0301}".repeat(4))).is_ok());
        assert_eq!(
            norm(&format!("x{}", "\u{0301}".repeat(5))),
            Err(ChatRejection::CombiningRun)
        );
        assert!(
            norm(&format!(
                "x{}y{}",
                "\u{0301}".repeat(4),
                "\u{0301}".repeat(4)
            ))
            .is_ok()
        );
    }

    #[test]
    fn web_addresses_are_refused() {
        for text in [
            "http://x",
            "HTTPS://X",
            "ftp://a",
            "www.x.pl",
            "see WWW.X",
            "1.2.3.4",
            "ip 10.0.0.1 ok",
            "ok.com",
            "EXAMPLE.COM",
            "a。com",
            "mailto:a",
            "javascript:alert(1)",
            "x.pl",
            "a-b.io",
            "ok.com-x",
            "(x.net)",
        ] {
            assert_eq!(norm(text), Err(ChatRejection::Url), "{text}");
        }
        for text in [
            "ok.Com",
            "x.company",
            "-.com",
            "1.2.3.4567",
            "a.",
            ".com",
            "mailto: a",
            "3.14.15",
        ] {
            assert!(norm(text).is_ok(), "{text}");
        }
        assert_eq!(norm("1.2.3.4.5"), Err(ChatRejection::Url));
    }

    #[test]
    fn rejections_have_codes_and_sentences() {
        for (rejection, code) in [
            (ChatRejection::NotText, "not_text"),
            (ChatRejection::Empty, "empty"),
            (ChatRejection::TooLong, "too_long"),
            (ChatRejection::ControlChar, "control_char"),
            (ChatRejection::InvisibleChar, "invisible_char"),
            (ChatRejection::CombiningRun, "combining_run"),
            (ChatRejection::Url, "url"),
        ] {
            assert_eq!(rejection.as_str(), code);
            assert!(rejection.message().ends_with('.'));
        }
    }

    #[test]
    fn chat_state_follows_the_game_and_the_switches() {
        use GameStatus::{Active, Cancelled, Finished, Waiting};
        let d = |status, window, mine, theirs, public| {
            derive_chat_state(status, window, mine, theirs, public)
        };
        assert_eq!(d(Waiting, false, true, None, false), ChatState::Unavailable);
        assert_eq!(
            d(Cancelled, false, true, Some(true), false),
            ChatState::Unavailable
        );
        assert_eq!(
            d(Finished, false, true, Some(true), false),
            ChatState::Closed
        );
        assert_eq!(d(Finished, true, true, Some(true), false), ChatState::Open);
        assert_eq!(
            d(Active, false, false, Some(true), false),
            ChatState::DisabledByYou
        );
        assert_eq!(
            d(Active, false, true, Some(false), false),
            ChatState::DisabledByOpponent
        );
        assert_eq!(d(Active, false, true, Some(false), true), ChatState::Open);
        assert_eq!(
            d(Active, false, false, Some(false), true),
            ChatState::DisabledByYou
        );
        assert_eq!(d(Active, false, true, None, false), ChatState::Open);
        assert_eq!(
            derive_spectator_chat_state(Waiting, false),
            ChatState::Unavailable
        );
        assert_eq!(
            derive_spectator_chat_state(Cancelled, true),
            ChatState::Unavailable
        );
        assert_eq!(
            derive_spectator_chat_state(Finished, false),
            ChatState::Closed
        );
        assert_eq!(derive_spectator_chat_state(Finished, true), ChatState::Open);
        assert_eq!(derive_spectator_chat_state(Active, false), ChatState::Open);
    }

    #[test]
    fn states_have_names_and_readability() {
        let all = [
            (ChatState::Open, "open", true),
            (ChatState::DisabledByYou, "disabled_by_you", false),
            (ChatState::DisabledByOpponent, "disabled_by_opponent", true),
            (ChatState::Unavailable, "unavailable", false),
            (ChatState::Closed, "closed", false),
        ];
        for (state, name, readable) in all {
            assert_eq!((state.as_str(), state.readable()), (name, readable));
        }
    }

    #[test]
    fn the_wait_is_what_is_left_of_the_interval() {
        assert_eq!(retry_after_ms(None, 2000), 0);
        assert_eq!(retry_after_ms(Some(500.2), 2000), 1500);
        assert_eq!(retry_after_ms(Some(2000.0), 2000), 0);
        assert_eq!(retry_after_ms(Some(5000.0), 2000), 0);
    }
}
