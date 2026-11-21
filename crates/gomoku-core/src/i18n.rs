//! Interface languages: which are supported, how one is chosen, and the fallback chain of a missing text.
//!
//! Resolution order: the `gomoku_lang` cookie, then `Accept-Language`, then Polish. The language is chosen
//! only by an explicit user action (`?lang=`), never stored automatically from `Accept-Language`.

/// Name of the cookie that holds the chosen language code (nothing else).
pub const LANG_COOKIE: &str = "gomoku_lang";
/// One year.
pub const LANG_COOKIE_MAX_AGE_SECONDS: u64 = 31_536_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Locale {
    Pl,
    En,
    Fr,
    De,
}

pub const LOCALES: [Locale; 4] = [Locale::Pl, Locale::En, Locale::Fr, Locale::De];
pub const DEFAULT_LOCALE: Locale = Locale::Pl;

impl Locale {
    /// The two-letter code used in the cookie, `?lang=` and `<html lang>`.
    pub fn code(self) -> &'static str {
        match self {
            Locale::Pl => "pl",
            Locale::En => "en",
            Locale::Fr => "fr",
            Locale::De => "de",
        }
    }

    /// Strict: exact codes only (no regions, no case folding).
    pub fn from_code(code: &str) -> Option<Locale> {
        LOCALES.into_iter().find(|locale| locale.code() == code)
    }

    /// Where a missing text is looked up next: `fr` falls back to `en`, then `pl`; Polish is the source.
    pub fn fallback_chain(self) -> &'static [Locale] {
        match self {
            Locale::Pl => &[Locale::Pl],
            Locale::En => &[Locale::En, Locale::Pl],
            Locale::Fr => &[Locale::Fr, Locale::En, Locale::Pl],
            Locale::De => &[Locale::De, Locale::En, Locale::Pl],
        }
    }
}

const ACCEPT_LANGUAGE_MAX_LENGTH: usize = 256;
const ACCEPT_LANGUAGE_MAX_ENTRIES: usize = 20;

/// Languages of an `Accept-Language` header, best first (by `q`, stable for equal weights). Regions are
/// dropped (`fr-CA` is `fr`), `*` and unknown languages and `q=0` are ignored; oversized input yields nothing.
pub fn parse_accept_language(header: &str) -> Vec<Locale> {
    if header.len() > ACCEPT_LANGUAGE_MAX_LENGTH {
        return Vec::new();
    }
    let mut entries: Vec<(Locale, f64)> = Vec::new();
    for part in header.split(',').take(ACCEPT_LANGUAGE_MAX_ENTRIES) {
        let mut pieces = part.split(';');
        let tag = pieces.next().unwrap_or("").trim().to_ascii_lowercase();
        let primary = tag.split('-').next().unwrap_or("");
        let Some(locale) = Locale::from_code(primary) else {
            continue;
        };
        let quality = pieces
            .find_map(|param| param.trim().strip_prefix("q=").map(str::to_owned))
            .map_or(Some(1.0), |raw| raw.trim().parse::<f64>().ok());
        match quality {
            Some(q) if (0.0..=1.0).contains(&q) && q > 0.0 => entries.push((locale, q)),
            _ => {}
        }
    }
    entries.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut seen = Vec::new();
    for (locale, _) in entries {
        if !seen.contains(&locale) {
            seen.push(locale);
        }
    }
    seen
}

/// The language for a request: a valid cookie wins, then the first supported `Accept-Language` entry, then Polish.
pub fn resolve_locale(cookie: Option<&str>, accept_language: Option<&str>) -> Locale {
    cookie
        .and_then(Locale::from_code)
        .or_else(|| {
            accept_language.and_then(|header| parse_accept_language(header).first().copied())
        })
        .unwrap_or(DEFAULT_LOCALE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_round_trip_and_are_strict() {
        for locale in LOCALES {
            assert_eq!(Locale::from_code(locale.code()), Some(locale));
        }
        for bad in ["", "PL", "pl-PL", "xx", " fr"] {
            assert_eq!(Locale::from_code(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn fallback_chains_end_in_polish() {
        assert_eq!(Locale::Pl.fallback_chain(), &[Locale::Pl]);
        assert_eq!(Locale::En.fallback_chain(), &[Locale::En, Locale::Pl]);
        assert_eq!(
            Locale::Fr.fallback_chain(),
            &[Locale::Fr, Locale::En, Locale::Pl]
        );
        assert_eq!(
            Locale::De.fallback_chain(),
            &[Locale::De, Locale::En, Locale::Pl]
        );
    }

    #[test]
    fn accept_language_is_ordered_by_quality_and_drops_regions() {
        assert_eq!(
            parse_accept_language("fr-CA,en;q=0.8,pl;q=0.9"),
            vec![Locale::Fr, Locale::Pl, Locale::En]
        );
        assert_eq!(
            parse_accept_language("pl-PL, en-US;q=0.5"),
            vec![Locale::Pl, Locale::En]
        );
        assert_eq!(
            parse_accept_language("en;q=0.5, de;q=0.5"),
            vec![Locale::En, Locale::De]
        );
        assert_eq!(parse_accept_language("EN-us"), vec![Locale::En]);
        assert_eq!(parse_accept_language("fr, fr-CA;q=0.2"), vec![Locale::Fr]);
    }

    #[test]
    fn accept_language_ignores_what_it_cannot_use() {
        assert_eq!(
            parse_accept_language("*, xx, ja;q=0.9"),
            Vec::<Locale>::new()
        );
        assert_eq!(parse_accept_language("fr;q=0"), Vec::<Locale>::new());
        assert_eq!(parse_accept_language("fr;q=1.5"), Vec::<Locale>::new());
        assert_eq!(parse_accept_language("fr;q=abc"), Vec::<Locale>::new());
        assert_eq!(parse_accept_language(""), Vec::<Locale>::new());
        assert_eq!(
            parse_accept_language(&"fr,".repeat(100)),
            Vec::<Locale>::new()
        );
        assert_eq!(
            parse_accept_language("fr;x=1;q=0.4, en"),
            vec![Locale::En, Locale::Fr]
        );
    }

    #[test]
    fn accept_language_reads_at_most_20_entries() {
        let header = format!("{}fr", "xx,".repeat(20));
        assert_eq!(parse_accept_language(&header), Vec::<Locale>::new());
        let header = format!("{}fr", "xx,".repeat(19));
        assert_eq!(parse_accept_language(&header), vec![Locale::Fr]);
    }

    #[test]
    fn the_cookie_wins_then_the_header_then_polish() {
        assert_eq!(resolve_locale(Some("fr"), Some("en")), Locale::Fr);
        assert_eq!(resolve_locale(Some("xx"), Some("en-US")), Locale::En);
        assert_eq!(resolve_locale(None, Some("de-DE,en;q=0.5")), Locale::De);
        assert_eq!(resolve_locale(None, Some("ja")), Locale::Pl);
        assert_eq!(resolve_locale(None, None), Locale::Pl);
        assert_eq!(LANG_COOKIE, "gomoku_lang");
        assert_eq!(LANG_COOKIE_MAX_AGE_SECONDS, 31_536_000);
    }
}
