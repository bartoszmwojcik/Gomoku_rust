//! Server-rendered pages (Askama) with htmx. `/login` and `/spike/*` are the F0 spike that
//! validates the Askama + htmx choice before the real screens are built.

use askama::Template;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use gomoku_core::CLASSIC_BOARD_SIZE;

use crate::app::AppState;

#[derive(Template)]
#[template(path = "home.html")]
struct HomePage;

#[derive(Template)]
#[template(path = "login.html")]
struct LoginPage;

#[derive(Template)]
#[template(path = "board.html")]
struct BoardPage {
    rows: Vec<usize>,
    cols: Vec<usize>,
}

#[derive(Template)]
#[template(path = "clock.html")]
struct ClockFragment {
    minutes: u64,
    seconds: u64,
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

pub async fn home() -> Response {
    render(&HomePage)
}

pub async fn login() -> Response {
    render(&LoginPage)
}

pub async fn board() -> Response {
    let axis: Vec<usize> = (0..CLASSIC_BOARD_SIZE).collect();
    render(&BoardPage {
        rows: axis.clone(),
        cols: axis,
    })
}

/// Demo clock fragment polled by htmx: counts down from 10:00 using the server's time.
pub async fn clock(State(state): State<AppState>) -> Response {
    let elapsed = (state.now)() % 600;
    let left = 600 - elapsed;
    render(&ClockFragment {
        minutes: left / 60,
        seconds: left % 60,
    })
}
