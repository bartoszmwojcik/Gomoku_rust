//! Invitation codes: 8 symbols of a 31-symbol alphabet, shown as `ABCD-EFGH`.

use url::Url;

use crate::constants::{INVITATION_CODE_ALPHABET, INVITATION_CODE_LENGTH};

/// Generates a code in storage form: 8 symbols of the alphabet, no separator. `random_int(max)` must
/// return a uniform integer in `[0, max)`; production passes a CSPRNG-backed source.
pub fn generate_invitation_code(mut random_int: impl FnMut(usize) -> usize) -> String {
    let alphabet: Vec<char> = INVITATION_CODE_ALPHABET.chars().collect();
    (0..INVITATION_CODE_LENGTH)
        .map(|_| alphabet[random_int(alphabet.len())])
        .collect()
}

/// Display form `ABCD-EFGH` of a stored code.
pub fn format_invitation_code(code: &str) -> String {
    let half = INVITATION_CODE_LENGTH / 2;
    let head: String = code.chars().take(half).collect();
    let tail: String = code.chars().skip(half).collect();
    format!("{head}-{tail}")
}

fn is_separator(c: char) -> bool {
    c == '-' || c.is_whitespace() || c == '\u{feff}'
}

/// Converts user input (any case, with or without a dash or spaces) to storage form. `None` for anything
/// that cannot be a code, so callers answer with one uniform "invalid invitation" for a bad shape and an
/// unknown code alike.
pub fn normalize_invitation_code(input: &str) -> Option<String> {
    let code: String = input
        .chars()
        .filter(|&c| !is_separator(c))
        .collect::<String>()
        .to_uppercase();
    if code.chars().count() != INVITATION_CODE_LENGTH {
        return None;
    }
    code.chars()
        .all(|c| INVITATION_CODE_ALPHABET.contains(c))
        .then_some(code)
}

/// Link to the invitation page; the code is shown grouped, the page normalizes it back.
pub fn invitation_url(origin: &str, code: &str) -> String {
    format!("{origin}/invite/{}", format_invitation_code(code))
}

/// The code behind what an agent hands to `join`: exactly one of a code in any form or a link to the
/// invitation page whose last path segment is the code (`https://host/invite/ABCD-EFGH`; query and
/// fragment are ignored). The host is ignored too and nothing is fetched. Anything else, several codes
/// in one text included, is `None`.
pub fn extract_invitation_code(input: &str) -> Option<String> {
    let text = input.trim();
    if let Some(code) = normalize_invitation_code(text) {
        return Some(code);
    }
    let url = Url::parse(text).ok()?;
    if url.scheme() != "https" && url.scheme() != "http" {
        return None;
    }
    let segment = url.path().split('/').rfind(|part| !part.is_empty())?;
    normalize_invitation_code(segment)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draws_every_symbol_from_the_injected_source_over_the_whole_alphabet() {
        let mut calls = Vec::new();
        let mut next = 0;
        let code = generate_invitation_code(|max| {
            calls.push(max);
            next += 1;
            next - 1
        });
        assert_eq!(code, &INVITATION_CODE_ALPHABET[..INVITATION_CODE_LENGTH]);
        assert_eq!(calls, vec![31; INVITATION_CODE_LENGTH]);
    }

    #[test]
    fn can_reach_the_last_symbol() {
        assert_eq!(
            generate_invitation_code(|max| max - 1),
            "Z".repeat(INVITATION_CODE_LENGTH)
        );
    }

    #[test]
    fn groups_the_code() {
        assert_eq!(format_invitation_code("ABCDEFGH"), "ABCD-EFGH");
    }

    #[test]
    fn normalizes_any_case_dash_and_spaces() {
        assert_eq!(
            normalize_invitation_code("abcd-efgh").as_deref(),
            Some("ABCDEFGH")
        );
        assert_eq!(
            normalize_invitation_code(" ab cd efgh ").as_deref(),
            Some("ABCDEFGH")
        );
        assert_eq!(
            normalize_invitation_code("ABCDEFGH").as_deref(),
            Some("ABCDEFGH")
        );
    }

    #[test]
    fn rejects_a_wrong_length_and_symbols_outside_the_alphabet() {
        for bad in [
            "ABCDEFG",
            "ABCDEFGHJ",
            "",
            "ABCDEFG0",
            "ABCDEFGL",
            "ABCD*FGH",
        ] {
            assert_eq!(normalize_invitation_code(bad), None, "{bad}");
        }
    }

    #[test]
    fn builds_the_invitation_url() {
        assert_eq!(
            invitation_url("https://gomoku.test", "ABCDEFGH"),
            "https://gomoku.test/invite/ABCD-EFGH"
        );
    }

    #[test]
    fn finds_the_code_in_a_code_or_a_link() {
        for input in [
            "ABCD-EFGH",
            "abcd-efgh",
            "ABCDEFGH",
            "  abcd efgh  ",
            "https://gomoku.example/invite/ABCD-EFGH",
            "http://localhost:3000/invite/abcd-efgh/",
            "https://other.host/invite/ABCD-EFGH?utm=1&x=ABCD-EFGH#ABCD-EFGH",
            "https://u:p@host/a/b/ABCDEFGH",
        ] {
            assert_eq!(
                extract_invitation_code(input).as_deref(),
                Some("ABCDEFGH"),
                "{input}"
            );
        }
    }

    #[test]
    fn rejects_everything_else() {
        let long = "x".repeat(500);
        for input in [
            "",
            "   ",
            "ABCD-EFG",
            "ABCD-EFGH ABCD-EFGH",
            "kod: ABCD-EFGH",
            "ABCD-EFGH oraz https://x/invite/ABCD-EFGH",
            "https://x/invite/",
            "https://x",
            "https://x/invite/ABCD-EFG0",
            "ftp://x/invite/ABCD-EFGH",
            "javascript:ABCD-EFGH",
            "not a url at all",
            long.as_str(),
        ] {
            assert_eq!(extract_invitation_code(input), None, "{input:?}");
        }
    }
}
