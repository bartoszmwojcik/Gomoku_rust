//! The session cookie and the user behind it.

use axum::http::HeaderMap;
use axum::response::Redirect;
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use gomoku_core::auth::safe_next_path;
use gomoku_db::{OffsetDateTime, SessionUser, Uuid};
use time::Duration;

use crate::app::AppState;
use crate::security::hash_opaque_token;

/// Session lifetime: 30 days from creation, never extended.
pub const SESSION_TTL: Duration = Duration::days(30);
/// The cookie outlives the session by one hour, so the browser does not drop it during a game that still
/// counts; the server alone decides whether a session is valid.
pub const SESSION_COOKIE_MAX_AGE: Duration = Duration::seconds(30 * 24 * 3600 + 3600);

/// The user behind a valid session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrentUser {
    pub session_id: Uuid,
    pub id: Uuid,
    pub display_name: String,
    pub email: String,
    pub email_verified_at: Option<OffsetDateTime>,
}

impl From<SessionUser> for CurrentUser {
    fn from(session: SessionUser) -> Self {
        Self {
            session_id: session.session_id,
            id: session.user_id,
            display_name: session.display_name,
            email: session.email,
            email_verified_at: session.email_verified_at,
        }
    }
}

/// `__Host-` prefix when the cookie is `Secure` (no `Domain`, `Path=/`, so a sibling subdomain cannot plant it);
/// the prefix requires HTTPS, hence the plain name otherwise.
pub fn session_cookie_name(secure: bool) -> &'static str {
    if secure {
        "__Host-gomoku_session"
    } else {
        "gomoku_session"
    }
}

fn base_cookie(secure: bool, value: String) -> Cookie<'static> {
    let mut cookie = Cookie::new(session_cookie_name(secure), value);
    cookie.set_http_only(true);
    cookie.set_secure(secure);
    cookie.set_same_site(SameSite::Lax);
    cookie.set_path("/");
    cookie
}

pub fn session_cookie(secure: bool, token: String) -> Cookie<'static> {
    let mut cookie = base_cookie(secure, token);
    cookie.set_max_age(SESSION_COOKIE_MAX_AGE);
    cookie
}

pub fn expired_session_cookie(secure: bool) -> Cookie<'static> {
    let mut cookie = base_cookie(secure, String::new());
    cookie.set_max_age(Duration::ZERO);
    cookie
}

/// The raw session token from the cookie jar, if any.
pub fn session_token(jar: &CookieJar, secure: bool) -> Option<String> {
    jar.get(session_cookie_name(secure))
        .map(|cookie| cookie.value().to_owned())
        .filter(|v| !v.is_empty())
}

/// The user behind the session cookie, or `None` when there is no valid session. Any failure (an unavailable
/// database) also yields `None`: fail closed, so pages never show someone else's identity and never crash
/// because of it.
pub async fn current_user(state: &AppState, jar: &CookieJar) -> Option<CurrentUser> {
    let token = session_token(jar, state.config.session_cookie_secure)?;
    match state
        .db
        .find_valid_session(&hash_opaque_token(&token), (state.clock)())
        .await
    {
        Ok(found) => found.map(CurrentUser::from),
        Err(error) => {
            tracing::error!(error = ?error, "session lookup failed");
            None
        }
    }
}

/// Guard for pages: the current user, or a redirect to `/login?next=<path>`. The path goes through
/// `safe_next_path`, so a wrong argument can never become an open redirect.
pub async fn require_page_session(
    state: &AppState,
    jar: &CookieJar,
    path: &str,
) -> Result<CurrentUser, Redirect> {
    match current_user(state, jar).await {
        Some(user) => Ok(user),
        None => {
            let next = safe_next_path(Some(path));
            Err(Redirect::to(&format!(
                "/login?next={}",
                encode_component(&next)
            )))
        }
    }
}

/// Percent-encodes everything but unreserved characters (as `encodeURIComponent` does).
pub fn encode_component(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// A header value as text, if it is valid UTF-8.
pub fn header_text<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name)?.to_str().ok()
}
