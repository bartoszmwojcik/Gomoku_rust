//! Validation of account input: e-mail, display name, passwords, post-login redirect path.
//!
//! Errors are codes with scalar params (the web layer maps them to messages); several rules may fail at once.

use std::sync::LazyLock;

use regex::Regex;

use crate::validators::{is_reserved_display_name, normalize_display_name, normalize_email};

pub const EMAIL_MAX_LENGTH: usize = 254;
pub const DISPLAY_NAME_MIN_LENGTH: usize = 3;
pub const DISPLAY_NAME_MAX_LENGTH: usize = 24;
pub const PASSWORD_MIN_LENGTH: usize = 8;
/// The hash input is limited to 72 bytes (a limit inherited from bcrypt, kept so the contract is stable).
pub const PASSWORD_MAX_BYTES: usize = 72;
/// Upper bound that only guards against huge input; it is not a password rule.
pub const LOGIN_PASSWORD_MAX_LENGTH: usize = 1024;

/// Machine-readable reason a field was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldIssue {
    EmailRequired,
    EmailTooLong,
    EmailInvalid,
    DisplayNameRequired,
    DisplayNameTooShort,
    DisplayNameTooLong,
    DisplayNameInvalid,
    DisplayNameReserved,
    PasswordRequired,
    PasswordTooShort,
    PasswordTooLong,
    /// A login password over the size guard (not a password rule: logging in reveals none).
    LoginPasswordTooLong,
    CurrentPasswordRequired,
    /// The new password equals the current one.
    PasswordUnchanged,
    TokenRequired,
    TokenTooLong,
}

impl FieldIssue {
    /// Stable code served to clients (`fieldIssues[].code`).
    pub fn code(self) -> &'static str {
        match self {
            FieldIssue::EmailRequired => "EMAIL_REQUIRED",
            FieldIssue::EmailTooLong => "EMAIL_TOO_LONG",
            FieldIssue::EmailInvalid => "EMAIL_INVALID",
            FieldIssue::DisplayNameRequired => "DISPLAY_NAME_REQUIRED",
            FieldIssue::DisplayNameTooShort => "DISPLAY_NAME_TOO_SHORT",
            FieldIssue::DisplayNameTooLong => "DISPLAY_NAME_TOO_LONG",
            FieldIssue::DisplayNameInvalid => "DISPLAY_NAME_INVALID",
            FieldIssue::DisplayNameReserved => "DISPLAY_NAME_RESERVED",
            FieldIssue::PasswordRequired => "PASSWORD_REQUIRED",
            FieldIssue::PasswordTooShort => "PASSWORD_TOO_SHORT",
            FieldIssue::PasswordTooLong | FieldIssue::LoginPasswordTooLong => "PASSWORD_TOO_LONG",
            FieldIssue::CurrentPasswordRequired => "CURRENT_PASSWORD_REQUIRED",
            FieldIssue::PasswordUnchanged => "PASSWORD_UNCHANGED",
            FieldIssue::TokenRequired => "TOKEN_REQUIRED",
            FieldIssue::TokenTooLong => "TOKEN_TOO_LONG",
        }
    }
}

// The pattern is a constant, known-valid literal (the same shape as the reference implementation's e-mail check).
#[allow(clippy::expect_used)]
static EMAIL_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^[A-Za-z0-9_'+\-]+(?:\.[A-Za-z0-9_'+\-]+)*@(?:[A-Za-z0-9](?:[A-Za-z0-9\-]*[A-Za-z0-9])?\.)+[A-Za-z]{2,}$",
    )
    .expect("the e-mail pattern is a valid regex")
});

/// Normalizes (trim, lowercase) and validates an e-mail address.
pub fn validate_email(input: &str) -> Result<String, Vec<FieldIssue>> {
    let email = normalize_email(input);
    if email.chars().count() > EMAIL_MAX_LENGTH {
        return Err(vec![FieldIssue::EmailTooLong]);
    }
    if !EMAIL_PATTERN.is_match(&email) {
        return Err(vec![FieldIssue::EmailInvalid]);
    }
    Ok(email)
}

fn is_display_name_char(c: char) -> bool {
    // Explicit character set (no Unicode classes) so Cyrillic/Greek homoglyphs are rejected.
    c.is_ascii_alphanumeric() || c == '_' || c == '-' || "ąćęłńóśźżĄĆĘŁŃÓŚŹŻ".contains(c)
}

/// Trims and validates a player's display name; every violated rule is reported, in a fixed order.
pub fn validate_display_name(input: &str) -> Result<String, Vec<FieldIssue>> {
    let name = input.trim();
    let length = name.chars().count();
    let mut issues = Vec::new();
    if length < DISPLAY_NAME_MIN_LENGTH {
        issues.push(FieldIssue::DisplayNameTooShort);
    }
    if length > DISPLAY_NAME_MAX_LENGTH {
        issues.push(FieldIssue::DisplayNameTooLong);
    }
    if !name.chars().all(is_display_name_char) {
        issues.push(FieldIssue::DisplayNameInvalid);
    }
    if is_reserved_display_name(&normalize_display_name(name)) {
        issues.push(FieldIssue::DisplayNameReserved);
    }
    if issues.is_empty() {
        Ok(name.to_string())
    } else {
        Err(issues)
    }
}

/// Rules for a password being set (registration, change, reset): at least 8 characters and at most 72
/// UTF-8 bytes. Never trimmed.
pub fn validate_new_password(input: &str) -> Result<(), Vec<FieldIssue>> {
    let mut issues = Vec::new();
    if input.chars().count() < PASSWORD_MIN_LENGTH {
        issues.push(FieldIssue::PasswordTooShort);
    }
    if input.len() > PASSWORD_MAX_BYTES {
        issues.push(FieldIssue::PasswordTooLong);
    }
    if issues.is_empty() {
        Ok(())
    } else {
        Err(issues)
    }
}

/// Login input shape: deliberately none of the registration password rules, so logging in reveals none.
pub fn validate_login_password(input: &str) -> Result<(), Vec<FieldIssue>> {
    if input.is_empty() {
        return Err(vec![FieldIssue::PasswordRequired]);
    }
    if input.chars().count() > LOGIN_PASSWORD_MAX_LENGTH {
        return Err(vec![FieldIssue::LoginPasswordTooLong]);
    }
    Ok(())
}

/// The current password of a change request: no registration rules, only presence and size.
pub fn validate_current_password(input: &str) -> Result<(), Vec<FieldIssue>> {
    if input.is_empty() {
        return Err(vec![FieldIssue::CurrentPasswordRequired]);
    }
    validate_login_password(input)
}

/// Rules for a change request: the new password follows the registration rules and must differ from the current one.
pub fn validate_password_change(current: &str, new: &str) -> Result<(), PasswordChangeIssues> {
    let current_issues = validate_current_password(current).err().unwrap_or_default();
    let mut new_issues = validate_new_password(new).err().unwrap_or_default();
    if new_issues.is_empty() && new == current {
        new_issues.push(FieldIssue::PasswordUnchanged);
    }
    if current_issues.is_empty() && new_issues.is_empty() {
        Ok(())
    } else {
        Err(PasswordChangeIssues {
            current: current_issues,
            new: new_issues,
        })
    }
}

/// Issues per field of a rejected password change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasswordChangeIssues {
    pub current: Vec<FieldIssue>,
    pub new: Vec<FieldIssue>,
}

/// The token is 43 base64url characters; the bound only guards against huge input.
pub const RESET_TOKEN_MAX_LENGTH: usize = 512;

/// A one-time token from a link: only presence and size are checked, so an unknown or malformed token is just
/// "invalid" (no oracle for the format).
pub fn validate_link_token(input: &str) -> Result<(), Vec<FieldIssue>> {
    if input.is_empty() {
        return Err(vec![FieldIssue::TokenRequired]);
    }
    if input.chars().count() > RESET_TOKEN_MAX_LENGTH {
        return Err(vec![FieldIssue::TokenTooLong]);
    }
    Ok(())
}

/// Validates a post-auth return path against open redirects: only same-origin relative paths starting with
/// a single `/`; anything else (missing, empty, `//host`, `/\host`, schemes, control characters) is `/`.
pub fn safe_next_path(value: Option<&str>) -> String {
    const FALLBACK: &str = "/";
    match value {
        Some(path)
            if path.starts_with('/')
                && !path.starts_with("//")
                && !path.chars().any(|c| c <= ' ' || c == '\u{7f}' || c == '\\') =>
        {
            path.to_string()
        }
        _ => FALLBACK.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_is_normalized_and_checked() {
        assert_eq!(
            validate_email("  Ala@Example.COM "),
            Ok("ala@example.com".into())
        );
        assert_eq!(
            validate_email("a.b+c@sub.example.pl"),
            Ok("a.b+c@sub.example.pl".into())
        );
        for bad in [
            "",
            "ala",
            "ala@",
            "@x.pl",
            "a@b",
            "a b@x.pl",
            "a..b@x.pl",
            ".a@x.pl",
            "a@-x.pl",
            "a@x.p",
        ] {
            assert_eq!(
                validate_email(bad),
                Err(vec![FieldIssue::EmailInvalid]),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn email_length_is_limited_after_trimming() {
        let local = "a".repeat(EMAIL_MAX_LENGTH - "@x.pl".len());
        assert!(validate_email(&format!("{local}@x.pl")).is_ok());
        assert_eq!(
            validate_email(&format!("{local}a@x.pl")),
            Err(vec![FieldIssue::EmailTooLong])
        );
    }

    #[test]
    fn display_name_accepts_letters_digits_polish_letters_underscore_and_hyphen() {
        assert_eq!(
            validate_display_name("  Zażółć_Gęślą-1 "),
            Ok("Zażółć_Gęślą-1".into())
        );
        assert_eq!(validate_display_name("abc"), Ok("abc".into()));
        assert!(validate_display_name(&"a".repeat(24)).is_ok());
    }

    #[test]
    fn display_name_reports_every_violated_rule() {
        assert_eq!(
            validate_display_name("ab"),
            Err(vec![FieldIssue::DisplayNameTooShort])
        );
        assert_eq!(
            validate_display_name(&"a".repeat(25)),
            Err(vec![FieldIssue::DisplayNameTooLong])
        );
        assert_eq!(
            validate_display_name("a b c"),
            Err(vec![FieldIssue::DisplayNameInvalid])
        );
        // Cyrillic "а" looks like Latin "a" but is rejected.
        assert_eq!(
            validate_display_name("\u{430}dmin1"),
            Err(vec![FieldIssue::DisplayNameInvalid])
        );
        assert_eq!(
            validate_display_name("Admin"),
            Err(vec![FieldIssue::DisplayNameReserved])
        );
        assert_eq!(
            validate_display_name("a_d-m_i-n"),
            Err(vec![FieldIssue::DisplayNameReserved])
        );
        assert_eq!(
            validate_display_name("a!"),
            Err(vec![
                FieldIssue::DisplayNameTooShort,
                FieldIssue::DisplayNameInvalid
            ])
        );
    }

    #[test]
    fn new_password_needs_8_characters_and_at_most_72_bytes() {
        assert_eq!(validate_new_password("12345678"), Ok(()));
        assert_eq!(
            validate_new_password("1234567"),
            Err(vec![FieldIssue::PasswordTooShort])
        );
        assert_eq!(validate_new_password(&"a".repeat(72)), Ok(()));
        assert_eq!(
            validate_new_password(&"a".repeat(73)),
            Err(vec![FieldIssue::PasswordTooLong])
        );
        // 36 two-byte characters are 72 bytes; 37 are too many bytes though only 37 characters.
        assert_eq!(validate_new_password(&"ą".repeat(36)), Ok(()));
        assert_eq!(
            validate_new_password(&"ą".repeat(37)),
            Err(vec![FieldIssue::PasswordTooLong])
        );
        assert_eq!(validate_new_password("  spaces kept  "), Ok(()));
    }

    #[test]
    fn login_password_is_only_checked_for_presence_and_size() {
        assert_eq!(validate_login_password("x"), Ok(()));
        assert_eq!(
            validate_login_password(""),
            Err(vec![FieldIssue::PasswordRequired])
        );
        assert_eq!(validate_login_password(&"a".repeat(1024)), Ok(()));
        assert_eq!(
            validate_login_password(&"a".repeat(1025)),
            Err(vec![FieldIssue::LoginPasswordTooLong])
        );
    }

    #[test]
    fn current_password_needs_presence_and_size_only() {
        assert_eq!(validate_current_password("x"), Ok(()));
        assert_eq!(
            validate_current_password(""),
            Err(vec![FieldIssue::CurrentPasswordRequired])
        );
        assert_eq!(
            validate_current_password(&"a".repeat(1025)),
            Err(vec![FieldIssue::LoginPasswordTooLong])
        );
    }

    #[test]
    fn a_password_change_needs_a_valid_and_different_new_password() {
        assert_eq!(
            validate_password_change("old password", "new password"),
            Ok(())
        );
        assert_eq!(
            validate_password_change("", "short"),
            Err(PasswordChangeIssues {
                current: vec![FieldIssue::CurrentPasswordRequired],
                new: vec![FieldIssue::PasswordTooShort]
            })
        );
        assert_eq!(
            validate_password_change("same password", "same password"),
            Err(PasswordChangeIssues {
                current: vec![],
                new: vec![FieldIssue::PasswordUnchanged]
            })
        );
        // An unchanged password that is also too short reports the shortness only.
        assert_eq!(
            validate_password_change("short", "short").unwrap_err().new,
            vec![FieldIssue::PasswordTooShort]
        );
    }

    #[test]
    fn link_tokens_need_presence_and_size_only() {
        assert_eq!(validate_link_token("abc"), Ok(()));
        assert_eq!(validate_link_token(&"a".repeat(512)), Ok(()));
        assert_eq!(
            validate_link_token(""),
            Err(vec![FieldIssue::TokenRequired])
        );
        assert_eq!(
            validate_link_token(&"a".repeat(513)),
            Err(vec![FieldIssue::TokenTooLong])
        );
    }

    #[test]
    fn issue_codes_are_stable() {
        let all = [
            (FieldIssue::EmailRequired, "EMAIL_REQUIRED"),
            (FieldIssue::EmailTooLong, "EMAIL_TOO_LONG"),
            (FieldIssue::EmailInvalid, "EMAIL_INVALID"),
            (FieldIssue::DisplayNameRequired, "DISPLAY_NAME_REQUIRED"),
            (FieldIssue::DisplayNameTooShort, "DISPLAY_NAME_TOO_SHORT"),
            (FieldIssue::DisplayNameTooLong, "DISPLAY_NAME_TOO_LONG"),
            (FieldIssue::DisplayNameInvalid, "DISPLAY_NAME_INVALID"),
            (FieldIssue::DisplayNameReserved, "DISPLAY_NAME_RESERVED"),
            (FieldIssue::PasswordRequired, "PASSWORD_REQUIRED"),
            (FieldIssue::PasswordTooShort, "PASSWORD_TOO_SHORT"),
            (FieldIssue::PasswordTooLong, "PASSWORD_TOO_LONG"),
            (FieldIssue::LoginPasswordTooLong, "PASSWORD_TOO_LONG"),
            (
                FieldIssue::CurrentPasswordRequired,
                "CURRENT_PASSWORD_REQUIRED",
            ),
            (FieldIssue::PasswordUnchanged, "PASSWORD_UNCHANGED"),
            (FieldIssue::TokenRequired, "TOKEN_REQUIRED"),
            (FieldIssue::TokenTooLong, "TOKEN_TOO_LONG"),
        ];
        for (issue, code) in all {
            assert_eq!(issue.code(), code);
        }
    }

    #[test]
    fn safe_next_path_blocks_open_redirects() {
        assert_eq!(safe_next_path(Some("/account")), "/account");
        assert_eq!(safe_next_path(Some("/lobby?x=1#y")), "/lobby?x=1#y");
        for bad in [
            None,
            Some(""),
            Some("account"),
            Some("//evil.com"),
            Some("/\\evil.com"),
            Some("/\t/evil.com"),
            Some("https://evil.com"),
            Some("/a b"),
            Some("/\u{7f}"),
        ] {
            assert_eq!(safe_next_path(bad), "/", "{bad:?}");
        }
    }
}
