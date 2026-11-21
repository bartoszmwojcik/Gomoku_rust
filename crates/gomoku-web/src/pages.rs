//! Server-rendered pages (Askama) with htmx: home, sign up, log in, account, and the board spike.

use askama::Template;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum_extra::extract::CookieJar;
use gomoku_core::CLASSIC_BOARD_SIZE;
use gomoku_core::auth::safe_next_path;
use serde::Deserialize;

use crate::app::AppState;
use crate::i18n::{Lang, Translator};
use crate::session::{CurrentUser, current_user, require_page_session};

/// What every page needs: the texts, the signed-in user (if any) and the current path.
pub struct Ctx {
    pub tr: Translator,
    pub user: Option<CurrentUser>,
    pub path: &'static str,
    pub errors_json: String,
}

impl Ctx {
    fn new(tr: Translator, user: Option<CurrentUser>, path: &'static str) -> Self {
        let errors_json = tr.subtree_json("errors.");
        Self {
            tr,
            user,
            path,
            errors_json,
        }
    }
}

#[derive(Template)]
#[template(path = "home.html")]
struct HomePage {
    ctx: Ctx,
    signed_in_as: Option<String>,
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
}

#[derive(Template)]
#[template(path = "board.html")]
struct BoardPage {
    ctx: Ctx,
    rows: Vec<usize>,
    cols: Vec<usize>,
}

#[derive(Template)]
#[template(path = "clock.html")]
struct ClockFragment {
    minutes: i64,
    seconds: i64,
}

fn render<T: Template>(template: &T) -> Response {
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

pub async fn home(State(state): State<AppState>, Lang(locale): Lang, jar: CookieJar) -> Response {
    let tr = state.catalogs.translator(locale);
    let user = current_user(&state, &jar).await;
    let signed_in_as = user
        .as_ref()
        .map(|u| tr.t_with("home.signedIn", &[("name", &u.display_name)]));
    render(&HomePage {
        ctx: Ctx::new(tr, user, "/"),
        signed_in_as,
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

pub async fn account(
    State(state): State<AppState>,
    Lang(locale): Lang,
    jar: CookieJar,
) -> Response {
    let user = match require_page_session(&state, &jar, "/account").await {
        Ok(user) => user,
        Err(redirect) => return redirect.into_response(),
    };
    let page = AccountPage {
        display_name: user.display_name.clone(),
        email: user.email.clone(),
        verified: user.email_verified_at.is_some(),
        ctx: Ctx::new(state.catalogs.translator(locale), Some(user), "/account"),
    };
    render(&page)
}

pub async fn board(State(state): State<AppState>, Lang(locale): Lang, jar: CookieJar) -> Response {
    let axis: Vec<usize> = (0..CLASSIC_BOARD_SIZE).collect();
    let user = current_user(&state, &jar).await;
    render(&BoardPage {
        ctx: Ctx::new(state.catalogs.translator(locale), user, "/spike/board"),
        rows: axis.clone(),
        cols: axis,
    })
}

/// Demo clock fragment polled by htmx: counts down from 10:00 using the server's time.
pub async fn clock(State(state): State<AppState>) -> Response {
    let left = 600 - (state.clock)().unix_timestamp().rem_euclid(600);
    render(&ClockFragment {
        minutes: left / 60,
        seconds: left % 60,
    })
}
