//! A guest's nickname: normalization, shape rules and the default name.

use unicode_normalization::UnicodeNormalization;
use unicode_properties::{GeneralCategory, GeneralCategoryGroup, UnicodeGeneralCategory};

use crate::types::Seat;
use crate::validators::{is_reserved_display_name, normalize_display_name};

/// Longest nickname, counted in code points.
pub const NICKNAME_MAX_LENGTH: usize = 24;

const ZWNJ: char = '\u{200C}';
const ZWJ: char = '\u{200D}';

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NicknameError {
    TooLong { max: usize },
    Invalid,
    Reserved,
}

/// Canonical form of a nickname: NFC; control characters become spaces; format characters (bidi,
/// zero-width) are dropped except ZWNJ/ZWJ, which scripts other than Latin need; every space separator
/// collapses into one space; the result is trimmed. May be empty.
pub fn normalize_nickname(input: &str) -> String {
    let mut out = String::new();
    let mut in_spaces = false;
    for c in input.nfc() {
        let category = c.general_category();
        if category == GeneralCategory::Format && c != ZWNJ && c != ZWJ {
            continue;
        }
        let is_space =
            category == GeneralCategory::Control || category == GeneralCategory::SpaceSeparator;
        if is_space {
            if !in_spaces {
                out.push(' ');
            }
            in_spaces = true;
        } else {
            out.push(c);
            in_spaces = false;
        }
    }
    out.trim().to_string()
}

/// Whether a nickname would pass for a reserved display name: compared like an account name, with spaces
/// and joiners removed first, so that "Usunięty gracz" and "Ad min" count as well.
pub fn is_reserved_nickname(nickname: &str) -> bool {
    let core: String = nickname
        .chars()
        .filter(|&c| !c.is_whitespace() && c != ZWNJ && c != ZWJ)
        .collect();
    is_reserved_display_name(&normalize_display_name(&core))
}

fn is_allowed(c: char) -> bool {
    matches!(
        c.general_category_group(),
        GeneralCategoryGroup::Letter | GeneralCategoryGroup::Mark | GeneralCategoryGroup::Number
    ) || matches!(c, ZWNJ | ZWJ | ' ' | '_' | '-')
}

/// Validates a guest's nickname: optional text of 1-24 code points (letters, marks, digits, spaces, `_`,
/// `-`). Empty after normalization means "no nickname" and yields `Ok(None)`, so the default name applies.
pub fn validate_nickname(input: &str) -> Result<Option<String>, NicknameError> {
    let nickname = normalize_nickname(input);
    if nickname.is_empty() {
        return Ok(None);
    }
    if nickname.chars().count() > NICKNAME_MAX_LENGTH {
        return Err(NicknameError::TooLong {
            max: NICKNAME_MAX_LENGTH,
        });
    }
    if !nickname.chars().all(is_allowed) {
        return Err(NicknameError::Invalid);
    }
    if is_reserved_nickname(&nickname) {
        return Err(NicknameError::Reserved);
    }
    Ok(Some(nickname))
}

/// Name shown for a guest without a nickname: derived from the seat on every read.
pub fn default_nickname(seat: Seat) -> String {
    format!("Gracz {}", seat.number())
}

/// The seat a name stands for when it is exactly a default guest nickname, otherwise `None`. Lets the UI
/// translate the server-composed text for a guest seat without a new field in the API envelope.
pub fn default_nickname_seat(name: &str) -> Option<Seat> {
    [Seat::One, Seat::Two]
        .into_iter()
        .find(|&seat| name == default_nickname(seat))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_composes_to_nfc() {
        assert_eq!(normalize_nickname("e\u{301}"), "\u{e9}");
    }

    #[test]
    fn turns_controls_into_spaces_and_collapses_them() {
        assert_eq!(normalize_nickname("a\tb\nc"), "a b c");
        assert_eq!(normalize_nickname("  a  b \u{3000}"), "a b");
        assert_eq!(normalize_nickname("a\u{a0}\u{a0}b"), "a b");
    }

    #[test]
    fn drops_bidi_and_zero_width_but_keeps_the_joiners() {
        assert_eq!(normalize_nickname("a\u{202e}b\u{200b}c\u{feff}d"), "abcd");
        assert_eq!(
            normalize_nickname("a\u{200c}b\u{200d}c"),
            "a\u{200c}b\u{200d}c"
        );
    }

    #[test]
    fn spaces_only_normalize_to_empty() {
        assert_eq!(normalize_nickname("     "), "");
    }

    #[test]
    fn accepts_letters_digits_spaces_underscore_and_hyphen() {
        assert_eq!(
            validate_nickname("Ala_ma-kota 42"),
            Ok(Some("Ala_ma-kota 42".into()))
        );
        assert_eq!(
            validate_nickname("Zażółć gęślą"),
            Ok(Some("Zażółć gęślą".into()))
        );
    }

    #[test]
    fn returns_the_normalized_value() {
        assert_eq!(
            validate_nickname("  Ala  Ola\t"),
            Ok(Some("Ala Ola".into()))
        );
    }

    #[test]
    fn an_empty_result_means_no_nickname() {
        for value in ["", "   ", " \t\u{202e}\u{200b}"] {
            assert_eq!(validate_nickname(value), Ok(None), "{value:?}");
        }
    }

    #[test]
    fn accepts_exactly_24_characters_and_rejects_25() {
        assert!(validate_nickname(&"a".repeat(NICKNAME_MAX_LENGTH)).is_ok());
        assert_eq!(
            validate_nickname(&"a".repeat(NICKNAME_MAX_LENGTH + 1)),
            Err(NicknameError::TooLong { max: 24 })
        );
    }

    #[test]
    fn counts_code_points_not_bytes() {
        let wide = "\u{20BB7}"; // CJK Extension B, outside the BMP
        assert!(validate_nickname(&wide.repeat(24)).is_ok());
        assert!(validate_nickname(&wide.repeat(25)).is_err());
        assert!(validate_nickname(&"\u{1D7D8}".repeat(24)).is_ok()); // astral digit
    }

    #[test]
    fn accepts_combining_marks_and_joiners_of_non_latin_scripts() {
        assert!(validate_nickname("a\u{308}\u{301}").is_ok());
        assert_eq!(
            validate_nickname("\u{915}\u{94d}\u{200d}\u{937}"),
            Ok(Some("\u{915}\u{94d}\u{200d}\u{937}".into()))
        );
        assert!(validate_nickname("\u{10400}").is_ok()); // Deseret capital letter
    }

    #[test]
    fn rejects_markup_and_symbols() {
        for value in ["<b>x</b>", "a;b", "a@b", "ok\u{1F600}", "a.b", "a/b"] {
            assert_eq!(
                validate_nickname(value),
                Err(NicknameError::Invalid),
                "{value}"
            );
        }
    }

    #[test]
    fn rejects_reserved_names_however_they_are_written() {
        for value in [
            "Admin",
            "admin",
            "ADMIN",
            "Agent",
            "a_d-m_i-n",
            "Ad min",
            "Usunięty gracz",
            "Usuniety_Gracz",
            "Gomoku",
            "Sys\u{200c}tem",
        ] {
            assert_eq!(
                validate_nickname(value),
                Err(NicknameError::Reserved),
                "{value}"
            );
        }
    }

    #[test]
    fn a_dotted_variant_is_invalid_not_reserved() {
        assert_eq!(validate_nickname("a.d.m.i.n"), Err(NicknameError::Invalid));
        assert!(!is_reserved_nickname("a.d.m.i.n"));
    }

    #[test]
    fn accepts_names_that_merely_contain_a_reserved_word() {
        assert!(validate_nickname("Adminka").is_ok());
        assert!(validate_nickname("Agent 007").is_ok());
        assert!(!is_reserved_nickname("Ala"));
    }

    #[test]
    fn default_nickname_is_derived_from_the_seat() {
        assert_eq!(default_nickname(Seat::One), "Gracz 1");
        assert_eq!(default_nickname(Seat::Two), "Gracz 2");
    }

    #[test]
    fn recognises_exactly_the_default_guest_names() {
        assert_eq!(default_nickname_seat("Gracz 1"), Some(Seat::One));
        assert_eq!(default_nickname_seat("Gracz 2"), Some(Seat::Two));
        for other in ["Gracz 3", "gracz 1", "Gracz 1 ", "Gracz", ""] {
            assert_eq!(default_nickname_seat(other), None, "{other:?}");
        }
    }
}
