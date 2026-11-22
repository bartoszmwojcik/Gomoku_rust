//! The guest cookie: a token that identifies a player without an account, only for the games it was created for.

use axum_extra::extract::CookieJar;
use axum_extra::extract::cookie::{Cookie, SameSite};
use gomoku_core::constants::SPECTATOR_ACCESS_MS;
use gomoku_core::game::{GUEST_COOKIE_MAX_AGE_SECONDS, guest_cookie_name, spectator_cookie_name};
use time::Duration;

use crate::security::hash_opaque_token;

fn cookie(secure: bool, token: String, max_age_seconds: i64) -> Cookie<'static> {
    let mut cookie = Cookie::new(guest_cookie_name(secure), token);
    cookie.set_http_only(true);
    cookie.set_secure(secure);
    cookie.set_same_site(SameSite::Lax);
    cookie.set_path("/");
    cookie.set_max_age(Duration::seconds(max_age_seconds));
    cookie
}

/// The guest cookie with a given lifetime.
pub fn guest_cookie(secure: bool, token: String, max_age_seconds: i64) -> Cookie<'static> {
    cookie(secure, token, max_age_seconds)
}

/// The guest cookie with the default lifetime of two hours.
pub fn guest_cookie_default(secure: bool, token: String) -> Cookie<'static> {
    cookie(secure, token, GUEST_COOKIE_MAX_AGE_SECONDS)
}

/// The raw guest token from the cookie jar, if any (an empty cookie is none).
pub fn guest_token(jar: &CookieJar, secure: bool) -> Option<String> {
    jar.get(guest_cookie_name(secure))
        .map(|c| c.value().to_owned())
        .filter(|v| !v.is_empty())
}

/// The hash under which the token is stored.
pub fn guest_token_hash(jar: &CookieJar, secure: bool) -> Option<String> {
    guest_token(jar, secure).map(|token| hash_opaque_token(&token))
}

/// Re-sets the cookie the browser already carries with a fresh lifetime (no cookie: nothing).
pub fn refreshed(jar: CookieJar, secure: bool, max_age_seconds: i64) -> CookieJar {
    match guest_token(&jar, secure) {
        Some(token) => jar.add(cookie(secure, token, max_age_seconds)),
        None => jar,
    }
}

fn spectator_cookie_of(secure: bool, token: String) -> Cookie<'static> {
    let mut cookie = Cookie::new(spectator_cookie_name(secure), token);
    cookie.set_http_only(true);
    cookie.set_secure(secure);
    cookie.set_same_site(SameSite::Lax);
    cookie.set_path("/");
    cookie.set_max_age(Duration::seconds((SPECTATOR_ACCESS_MS / 1000) as i64));
    cookie
}

/// The spectator cookie: the access of a visitor to one public game, for an hour from the last read.
pub fn spectator_cookie(secure: bool, token: String) -> Cookie<'static> {
    spectator_cookie_of(secure, token)
}

/// The raw spectator token from the cookie jar, if any.
pub fn spectator_token(jar: &CookieJar, secure: bool) -> Option<String> {
    jar.get(spectator_cookie_name(secure))
        .map(|c| c.value().to_owned())
        .filter(|v| !v.is_empty())
}

/// Re-sets the spectator cookie the browser carries with a fresh hour (none: nothing).
pub fn spectator_refreshed(jar: CookieJar, secure: bool) -> CookieJar {
    match spectator_token(&jar, secure) {
        Some(token) => jar.add(spectator_cookie_of(secure, token)),
        None => jar,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookies_carry_the_attributes_and_the_right_name() {
        let secure = guest_cookie(true, "t".into(), 100).to_string();
        assert!(secure.starts_with("__Host-gomoku_guest=t"), "{secure}");
        for attribute in [
            "HttpOnly",
            "Secure",
            "SameSite=Lax",
            "Path=/",
            "Max-Age=100",
        ] {
            assert!(secure.contains(attribute), "{attribute} in {secure}");
        }
        let plain = guest_cookie_default(false, "t".into()).to_string();
        assert!(
            plain.starts_with("gomoku_guest=t")
                && !plain.contains("Secure")
                && plain.contains("Max-Age=7200"),
            "{plain}"
        );
    }

    #[test]
    fn reads_hashes_and_refreshes_the_token() {
        let jar = CookieJar::new();
        assert_eq!(guest_token(&jar, false), None);
        assert_eq!(guest_token_hash(&jar, false), None);
        assert!(
            refreshed(jar.clone(), false, 5)
                .get("gomoku_guest")
                .is_none()
        );
        let jar = jar.add(Cookie::new("gomoku_guest", "abc"));
        assert_eq!(guest_token(&jar, false).as_deref(), Some("abc"));
        assert_eq!(
            guest_token_hash(&jar, false),
            Some(hash_opaque_token("abc"))
        );
        let refreshed_jar = refreshed(jar.clone(), false, 5);
        assert_eq!(
            refreshed_jar.get("gomoku_guest").unwrap().max_age(),
            Some(Duration::seconds(5))
        );
        assert_eq!(
            guest_token(
                &CookieJar::new().add(Cookie::new("gomoku_guest", "")),
                false
            ),
            None
        );
        assert_eq!(
            guest_token(&jar, true),
            None,
            "the prefixed name is a different cookie"
        );
    }

    #[test]
    fn the_spectator_cookie_is_its_own() {
        let secure = spectator_cookie(true, "t".into()).to_string();
        assert!(secure.starts_with("__Host-gomoku_spectator=t"), "{secure}");
        for attribute in [
            "HttpOnly",
            "Secure",
            "SameSite=Lax",
            "Path=/",
            "Max-Age=3600",
        ] {
            assert!(secure.contains(attribute), "{attribute} in {secure}");
        }
        let jar = CookieJar::new();
        assert_eq!(spectator_token(&jar, false), None);
        assert!(
            spectator_refreshed(jar.clone(), false)
                .get("gomoku_spectator")
                .is_none()
        );
        let jar = jar.add(Cookie::new("gomoku_spectator", "abc"));
        assert_eq!(spectator_token(&jar, false).as_deref(), Some("abc"));
        let again = spectator_refreshed(jar, false);
        assert_eq!(
            again.get("gomoku_spectator").map(Cookie::value),
            Some("abc")
        );
        assert_eq!(
            again.get("gomoku_spectator").and_then(Cookie::max_age),
            Some(Duration::seconds(3600))
        );
    }
}
