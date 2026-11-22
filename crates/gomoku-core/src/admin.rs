//! The read-only administrator panel: the query of the account list and its links, and the shape of a show.

use crate::validators::normalize_display_name;

pub const ADMIN_PAGE_SIZE: i64 = 25;
pub const SEARCH_MAX_LENGTH: usize = 24;
const MAX_PAGE: i64 = 1_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Created,
    Name,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDir {
    Asc,
    Desc,
}

impl SortKey {
    pub fn as_str(self) -> &'static str {
        match self {
            SortKey::Created => "created",
            SortKey::Name => "name",
        }
    }

    fn default_dir(self) -> SortDir {
        match self {
            SortKey::Created => SortDir::Desc,
            SortKey::Name => SortDir::Asc,
        }
    }
}

impl SortDir {
    pub fn as_str(self) -> &'static str {
        match self {
            SortDir::Asc => "asc",
            SortDir::Desc => "desc",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountsQuery {
    pub page: i64,
    pub q: String,
    pub sort: SortKey,
    pub dir: SortDir,
}

/// Anything unknown or out of range falls back to the default; nothing is an error.
pub fn parse_admin_accounts_query(
    page: Option<&str>,
    q: Option<&str>,
    sort: Option<&str>,
    dir: Option<&str>,
) -> AccountsQuery {
    let page = page
        .and_then(|p| p.trim().parse::<i64>().ok())
        .filter(|p| (1..=MAX_PAGE).contains(p))
        .unwrap_or(1);
    let q: String = q
        .unwrap_or("")
        .trim()
        .chars()
        .take(SEARCH_MAX_LENGTH)
        .collect();
    let sort = if sort == Some("name") {
        SortKey::Name
    } else {
        SortKey::Created
    };
    let dir = match dir {
        Some("asc") => SortDir::Asc,
        Some("desc") => SortDir::Desc,
        _ => sort.default_dir(),
    };
    AccountsQuery {
        page,
        q: q.trim().to_owned(),
        sort,
        dir,
    }
}

/// The key of the prefix search (never an e-mail), `None` for an empty search.
pub fn search_key(q: &str) -> Option<String> {
    let key = normalize_display_name(q.trim());
    (!key.is_empty()).then_some(key)
}

/// `like` pattern of a prefix search: the key with `\`, `%` and `_` escaped.
pub fn like_prefix(key: &str) -> String {
    let mut out = String::with_capacity(key.len() + 1);
    for c in key.chars() {
        if matches!(c, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}

/// Changes of a link of the panel; `None` keeps the current value.
#[derive(Debug, Default, Clone)]
pub struct HrefChange {
    pub page: Option<i64>,
    pub sort: Option<SortKey>,
    pub dir: Option<SortDir>,
}

/// The link of the panel with the query after `change`, defaults omitted.
pub fn admin_accounts_href(query: &AccountsQuery, change: &HrefChange) -> String {
    let sort = change.sort.unwrap_or(query.sort);
    let dir = change.dir.unwrap_or(query.dir);
    let page = change.page.unwrap_or(query.page);
    let mut parts: Vec<String> = Vec::new();
    if !query.q.is_empty() {
        parts.push(format!("q={}", encode(&query.q)));
    }
    if sort != SortKey::Created {
        parts.push(format!("sort={}", sort.as_str()));
    }
    if dir != sort.default_dir() {
        parts.push(format!("dir={}", dir.as_str()));
    }
    if page > 1 {
        parts.push(format!("page={page}"));
    }
    if parts.is_empty() {
        "/backend".to_owned()
    } else {
        format!("/backend?{}", parts.join("&"))
    }
}

fn encode(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The shape of a show: two bots against each other, or a bot waiting for an agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShowLayout {
    BotBot,
    BotAgent,
}

impl ShowLayout {
    pub fn as_str(self) -> &'static str {
        match self {
            ShowLayout::BotBot => "bot-bot",
            ShowLayout::BotAgent => "bot-agent",
        }
    }

    pub fn parse(text: &str) -> Option<ShowLayout> {
        match text {
            "bot-bot" => Some(ShowLayout::BotBot),
            "bot-agent" => Some(ShowLayout::BotAgent),
            _ => None,
        }
    }

    /// By the number of bot seats of a stored show.
    pub fn of_bots(bots: i64) -> ShowLayout {
        if bots == 2 {
            ShowLayout::BotBot
        } else {
            ShowLayout::BotAgent
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_query_falls_back_to_defaults() {
        let q = parse_admin_accounts_query(Some("abc"), None, Some("bogus"), Some("x"));
        assert_eq!(
            (q.page, q.sort, q.dir, q.q.as_str()),
            (1, SortKey::Created, SortDir::Desc, "")
        );
        let q = parse_admin_accounts_query(None, None, Some("name"), None);
        assert_eq!((q.sort, q.dir), (SortKey::Name, SortDir::Asc));
        let q = parse_admin_accounts_query(Some("0"), None, None, Some("asc"));
        assert_eq!((q.page, q.dir), (1, SortDir::Asc));
        assert_eq!(
            parse_admin_accounts_query(Some("1000001"), None, None, None).page,
            1
        );
        assert_eq!(
            parse_admin_accounts_query(Some(" 7 "), None, None, None).page,
            7
        );
        let long = "x".repeat(30);
        assert_eq!(
            parse_admin_accounts_query(None, Some(&long), None, None)
                .q
                .len(),
            24
        );
        assert_eq!(
            parse_admin_accounts_query(None, Some("  ab  "), None, None).q,
            "ab"
        );
        assert_eq!(
            parse_admin_accounts_query(None, Some("ab "), None, Some("desc")).dir,
            SortDir::Desc
        );
    }

    #[test]
    fn links_omit_defaults() {
        let plain = parse_admin_accounts_query(None, None, None, None);
        assert_eq!(
            admin_accounts_href(&plain, &HrefChange::default()),
            "/backend"
        );
        let q = parse_admin_accounts_query(Some("3"), Some("ab"), Some("name"), Some("desc"));
        assert_eq!(
            admin_accounts_href(&q, &HrefChange::default()),
            "/backend?q=ab&sort=name&dir=desc&page=3"
        );
        let change = HrefChange {
            page: Some(1),
            sort: Some(SortKey::Created),
            dir: Some(SortDir::Desc),
        };
        assert_eq!(admin_accounts_href(&q, &change), "/backend?q=ab");
        assert_eq!(
            (SortKey::Created.as_str(), SortKey::Name.as_str()),
            ("created", "name")
        );
        assert_eq!(
            (SortDir::Asc.as_str(), SortDir::Desc.as_str()),
            ("asc", "desc")
        );
        let oldest_first = parse_admin_accounts_query(None, None, None, Some("asc"));
        assert_eq!(
            admin_accounts_href(&oldest_first, &HrefChange::default()),
            "/backend?dir=asc"
        );
        let space = parse_admin_accounts_query(None, Some("a b&c"), None, None);
        assert_eq!(
            admin_accounts_href(&space, &HrefChange::default()),
            "/backend?q=a%20b%26c"
        );
    }

    #[test]
    fn the_search_is_a_key_with_escaped_wildcards() {
        assert_eq!(search_key("  "), None);
        assert_eq!(search_key("Ąla_"), Some("ala".to_owned()));
        assert_eq!(like_prefix("a%b_c\\"), "a\\%b\\_c\\\\%");
        assert_eq!(like_prefix("ala"), "ala%");
    }

    #[test]
    fn layouts_round_trip() {
        for layout in [ShowLayout::BotBot, ShowLayout::BotAgent] {
            assert_eq!(ShowLayout::parse(layout.as_str()), Some(layout));
        }
        assert_eq!(ShowLayout::parse("x"), None);
        assert_eq!(ShowLayout::of_bots(2), ShowLayout::BotBot);
        assert_eq!(ShowLayout::of_bots(1), ShowLayout::BotAgent);
        assert_eq!(ShowLayout::of_bots(0), ShowLayout::BotAgent);
    }
}
