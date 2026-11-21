//! Server-rendered pages (Askama): home, sign up, log in, account and the account-related screens.

use askama::Template;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum_extra::extract::CookieJar;
use gomoku_core::auth::safe_next_path;
use gomoku_core::validators::is_installation_admin;
use serde::Deserialize;

use crate::app::AppState;
use crate::i18n::{Lang, Translator};
use crate::routes_email::display_name_available_at;
use crate::session::{CurrentUser, current_user, require_page_session};

/// What every page needs: the texts, the signed-in user (if any) and the current path.
#[derive(Clone)]
pub struct Ctx {
    /// Texts for the scripts that render screens in the browser (full keys), when the page has such a script.
    pub bag: String,
    pub tr: Translator,
    pub user: Option<CurrentUser>,
    pub path: &'static str,
    pub errors_json: String,
    /// The reminder to confirm the e-mail address (home and account, for an unconfirmed signed-in player).
    pub banner: bool,
    pub user_id: String,
}

impl Ctx {
    pub(crate) fn new(tr: Translator, user: Option<CurrentUser>, path: &'static str) -> Self {
        let errors_json = tr.subtree_json("errors.");
        let user_id = user.as_ref().map(|u| u.id.to_string()).unwrap_or_default();
        Self {
            bag: String::new(),
            tr,
            user,
            path,
            errors_json,
            banner: false,
            user_id,
        }
    }

    /// Adds the texts a page script needs.
    pub(crate) fn with_bag(mut self, prefixes: &[&str]) -> Self {
        self.bag = self.tr.bag_json(prefixes);
        self
    }

    /// Shows the e-mail confirmation reminder when the signed-in player has not confirmed the address.
    pub(crate) fn with_banner(mut self) -> Self {
        self.banner = self
            .user
            .as_ref()
            .is_some_and(|u| u.email_verified_at.is_none());
        self
    }
}

#[derive(Template)]
#[template(path = "home.html")]
struct HomePage {
    ctx: Ctx,
    signed_in_as: Option<String>,
    /// The game that holds the player's slot: its id and the text for its phase.
    current: Option<(String, String)>,
    cancelled_notice: bool,
}

#[derive(Template)]
#[template(path = "login.html")]
struct LoginPage {
    ctx: Ctx,
    next: String,
}

#[derive(Template)]
#[template(path = "register.html")]
struct RegisterPage {
    ctx: Ctx,
    next: String,
}

#[derive(Template)]
#[template(path = "account.html")]
struct AccountPage {
    ctx: Ctx,
    display_name: String,
    email: String,
    verified: bool,
    /// When the name may be changed again, if it may not be now (UTC, for display).
    name_cooldown: Option<String>,
    /// The administrator's address never changes through the app.
    email_locked: bool,
}

#[derive(Template)]
#[template(path = "verify_email.html")]
struct VerifyEmailPage {
    ctx: Ctx,
    token: String,
    next: String,
    can_resend: bool,
}

#[derive(Template)]
#[template(path = "confirm_email_change.html")]
struct ConfirmEmailChangePage {
    ctx: Ctx,
    token: String,
}

#[derive(Template)]
#[template(path = "forgot_password.html")]
struct ForgotPasswordPage {
    ctx: Ctx,
}

#[derive(Template)]
#[template(path = "reset_password.html")]
struct ResetPasswordPage {
    ctx: Ctx,
    token: String,
}

pub(crate) fn render<T: Template>(template: &T) -> Response {
    match template.render() {
        Ok(html) => Html(html).into_response(),
        Err(err) => {
            tracing::error!(%err, "template rendering failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[derive(Deserialize, Default)]
pub struct NextQuery {
    next: Option<String>,
}

#[derive(Deserialize, Default)]
pub struct HomeQuery {
    notice: Option<String>,
}

pub async fn home(
    State(state): State<AppState>,
    Lang(locale): Lang,
    jar: CookieJar,
    Query(query): Query<HomeQuery>,
) -> Response {
    let tr = state.catalogs.translator(locale);
    let user = current_user(&state, &jar).await;
    let signed_in_as = user
        .as_ref()
        .map(|u| tr.t_with("home.signedIn", &[("name", &u.display_name)]));
    let now = (state.clock)();
    // The slot is judged after the lazy closings, so a dead game never shows up here.
    let current = match &user {
        Some(user) => state.db.current_game(user.id, now).await.ok().flatten(),
        None => match crate::guest::guest_token_hash(&jar, state.config.session_cookie_secure) {
            Some(hash) => state
                .db
                .current_game_of_guest(&hash, now)
                .await
                .ok()
                .flatten(),
            None => None,
        },
    };
    let current = current.map(|game| {
        let key = format!(
            "home.page.{}.{}",
            if user.is_some() {
                "phase"
            } else {
                "guestPhase"
            },
            game.status.as_str()
        );
        (game.game_id.to_string(), tr.t(&key))
    });
    let cancelled_notice = query.notice.as_deref() == Some("cancelled");
    render(&HomePage {
        ctx: Ctx::new(tr, user, "/").with_banner(),
        signed_in_as,
        current,
        cancelled_notice,
    })
}

/// Sends a signed-in visitor of `/login` or `/register` to `next`.
async fn redirect_if_signed_in(state: &AppState, jar: &CookieJar, next: &str) -> Option<Response> {
    current_user(state, jar)
        .await
        .map(|_| Redirect::to(next).into_response())
}

pub async fn login(
    State(state): State<AppState>,
    Lang(locale): Lang,
    jar: CookieJar,
    Query(query): Query<NextQuery>,
) -> Response {
    let next = safe_next_path(query.next.as_deref());
    if let Some(redirect) = redirect_if_signed_in(&state, &jar, &next).await {
        return redirect;
    }
    render(&LoginPage {
        ctx: Ctx::new(state.catalogs.translator(locale), None, "/login"),
        next,
    })
}

pub async fn register(
    State(state): State<AppState>,
    Lang(locale): Lang,
    jar: CookieJar,
    Query(query): Query<NextQuery>,
) -> Response {
    let next = safe_next_path(query.next.as_deref());
    if let Some(redirect) = redirect_if_signed_in(&state, &jar, &next).await {
        return redirect;
    }
    render(&RegisterPage {
        ctx: Ctx::new(state.catalogs.translator(locale), None, "/register"),
        next,
    })
}

fn format_utc(at: gomoku_db::OffsetDateTime) -> String {
    let at = at.to_offset(time::UtcOffset::UTC);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02} UTC",
        at.year(),
        u8::from(at.month()),
        at.day(),
        at.hour(),
        at.minute()
    )
}

pub async fn account(
    State(state): State<AppState>,
    Lang(locale): Lang,
    jar: CookieJar,
) -> Response {
    let user = match require_page_session(&state, &jar, "/account").await {
        Ok(user) => user,
        Err(redirect) => return redirect.into_response(),
    };
    let changed_at = match state.db.find_user_by_id(user.id).await {
        Ok(stored) => stored.and_then(|u| u.display_name_changed_at),
        Err(error) => {
            tracing::error!(error = ?error, "account lookup failed");
            None
        }
    };
    let name_cooldown = display_name_available_at(changed_at, (state.clock)()).map(format_utc);
    let email_locked = is_installation_admin(&user.email, state.config.admin_email.as_deref());
    let page = AccountPage {
        display_name: user.display_name.clone(),
        email: user.email.clone(),
        verified: user.email_verified_at.is_some(),
        name_cooldown,
        email_locked,
        ctx: Ctx::new(state.catalogs.translator(locale), Some(user), "/account").with_banner(),
    };
    render(&page)
}

#[derive(Deserialize, Default)]
pub struct VerifyQuery {
    token: Option<String>,
    next: Option<String>,
}

/// The link from the confirmation e-mail. A GET changes nothing (mail scanners and link previews open addresses):
/// the page shows a button, and only its click confirms. The token is in the address, so the page must not leak it
/// through `Referer`.
pub async fn verify_email(
    State(state): State<AppState>,
    Lang(locale): Lang,
    jar: CookieJar,
    Query(query): Query<VerifyQuery>,
) -> Response {
    let user = current_user(&state, &jar).await;
    let can_resend = user.as_ref().is_some_and(|u| u.email_verified_at.is_none());
    let page = VerifyEmailPage {
        ctx: Ctx::new(state.catalogs.translator(locale), user, "/verify-email"),
        token: query.token.unwrap_or_default(),
        next: safe_next_path(query.next.as_deref()),
        can_resend,
    };
    no_referrer(render(&page))
}

pub async fn confirm_email_change(
    State(state): State<AppState>,
    Lang(locale): Lang,
    jar: CookieJar,
    Query(query): Query<TokenQuery>,
) -> Response {
    let user = current_user(&state, &jar).await;
    let page = ConfirmEmailChangePage {
        ctx: Ctx::new(
            state.catalogs.translator(locale),
            user,
            "/confirm-email-change",
        ),
        token: query.token.unwrap_or_default(),
    };
    no_referrer(render(&page))
}

pub(crate) fn no_referrer(mut response: Response) -> Response {
    response.headers_mut().insert(
        axum::http::header::REFERRER_POLICY,
        axum::http::HeaderValue::from_static("no-referrer"),
    );
    response
}

pub async fn forgot_password(
    State(state): State<AppState>,
    Lang(locale): Lang,
    jar: CookieJar,
) -> Response {
    let user = current_user(&state, &jar).await;
    render(&ForgotPasswordPage {
        ctx: Ctx::new(state.catalogs.translator(locale), user, "/forgot-password"),
    })
}

#[derive(Deserialize, Default)]
pub struct TokenQuery {
    token: Option<String>,
}

/// The link from the e-mail. The token is in the address, so the page must not leak it through `Referer`.
pub async fn reset_password(
    State(state): State<AppState>,
    Lang(locale): Lang,
    jar: CookieJar,
    Query(query): Query<TokenQuery>,
) -> Response {
    let user = current_user(&state, &jar).await;
    let page = ResetPasswordPage {
        ctx: Ctx::new(state.catalogs.translator(locale), user, "/reset-password"),
        token: query.token.unwrap_or_default(),
    };
    no_referrer(render(&page))
}
