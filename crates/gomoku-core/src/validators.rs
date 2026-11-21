//! Normalization shared by account and game name validation.

/// Names an account (or a guest nickname) may not take: they would pass for the system or an agent.
/// Compared after [`normalize_display_name`].
pub const RESERVED_DISPLAY_NAMES: [&str; 8] = [
    "usunietygracz",
    "admin",
    "administrator",
    "moderator",
    "system",
    "gomoku",
    "support",
    "agent",
];

/// Canonical form of an e-mail address: trimmed and lower-cased.
pub fn normalize_email(email: &str) -> String {
    email.trim().to_lowercase()
}

/// Uniqueness key of a display name: lower-case, Polish letters mapped to Latin ones, `_` and `-` removed.
pub fn normalize_display_name(display_name: &str) -> String {
    display_name
        .to_lowercase()
        .chars()
        .filter(|&c| c != '_' && c != '-')
        .map(|c| match c {
            'ą' => 'a',
            'ć' => 'c',
            'ę' => 'e',
            'ł' => 'l',
            'ń' => 'n',
            'ó' => 'o',
            'ś' => 's',
            'ź' | 'ż' => 'z',
            other => other,
        })
        .collect()
}

/// Whether an already normalized display name is reserved.
pub fn is_reserved_display_name(normalized: &str) -> bool {
    RESERVED_DISPLAY_NAMES.contains(&normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_is_trimmed_and_lowercased() {
        assert_eq!(normalize_email("  Ala@Example.COM "), "ala@example.com");
    }

    #[test]
    fn display_name_maps_polish_letters_and_drops_separators() {
        assert_eq!(
            normalize_display_name("Zażółć_Gęślą-Jaźń"),
            "zazolcgeslajazn"
        );
        assert_eq!(normalize_display_name("Usunięty gracz"), "usuniety gracz");
        assert_eq!(normalize_display_name("ĄĆĘŁŃÓŚŹŻ"), "acelnoszz");
    }

    #[test]
    fn recognises_reserved_names() {
        assert!(is_reserved_display_name("admin"));
        assert!(is_reserved_display_name(&normalize_display_name(
            "Usuniety_Gracz"
        )));
        assert!(!is_reserved_display_name("ala"));
    }
}
