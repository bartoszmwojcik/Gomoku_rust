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

/// The installation administrator is the account whose e-mail equals `ADMIN_EMAIL` (no role column). Both sides
/// are compared normalized; without a configured administrator nobody is one.
pub fn is_installation_admin(email: &str, admin_email: Option<&str>) -> bool {
    admin_email
        .is_some_and(|admin| !admin.is_empty() && normalize_email(email) == normalize_email(admin))
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
    fn the_administrator_is_found_by_normalized_email() {
        assert!(is_installation_admin(
            " Admin@Example.com ",
            Some("admin@example.com")
        ));
        assert!(is_installation_admin(
            "admin@example.com",
            Some("ADMIN@example.com ")
        ));
        assert!(!is_installation_admin(
            "other@example.com",
            Some("admin@example.com")
        ));
        assert!(!is_installation_admin("admin@example.com", None));
        assert!(!is_installation_admin("", Some("")));
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
