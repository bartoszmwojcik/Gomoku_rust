//! The administrator's pages: the read-only panel `/backend` and the show (`/show`, `/show/{id}`). Every handler asks the
//! gate first; anybody else gets the 404 of a path that does not exist.

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum_extra::extract::CookieJar;
use gomoku_core::admin::{
    HrefChange, SortDir, SortKey, admin_accounts_href, parse_admin_accounts_query,
};
use gomoku_core::finished_stats::pair_label;
use gomoku_core::validators::is_installation_admin;
use gomoku_db::OffsetDateTime;
use serde::Deserialize;
use serde_json::json;

use crate::admin::{admin_user, denied_page};
use crate::app::AppState;
use crate::game_view::iso_ms;
use crate::i18n::Lang;
use crate::pages::{Ctx, no_referrer, render};
use crate::pages_games::script_json;
use crate::routes_show::{show_id, show_state};

pub struct Card {
    pub title: String,
    pub description: String,
    pub rows: Vec<(String, String)>,
}

pub struct AccountView {
    pub name: String,
    pub email: String,
    pub verified: &'static str,
    pub created_iso: String,
    pub created_text: String,
    pub finished: i64,
    pub role: &'static str,
}

#[derive(Template)]
#[template(path = "backend.html")]
struct BackendPage {
    data_error: bool,
    cards: Vec<Card>,
    query_q: String,
    sort_is_name: bool,
    rows: Vec<AccountView>,
    name_aria: &'static str,
    created_aria: &'static str,
    name_href: String,
    created_href: String,
    page: i64,
    page_count: i64,
    total: i64,
    prev_href: Option<String>,
    next_href: Option<String>,
    db_ok: bool,
}

#[derive(Deserialize, Default)]
pub struct BackendQuery {
    page: Option<String>,
    q: Option<String>,
    sort: Option<String>,
    dir: Option<String>,
}

/// Whether Warsaw is on summer time at `at` (UTC): from the last Sunday of March 01:00 UTC to the last Sunday of October
/// 01:00 UTC.
fn warsaw_summer(at: OffsetDateTime) -> bool {
    let year = at.year();
    let last_sunday = |month: time::Month| {
        let last = time::Date::from_calendar_date(year, month, 31).ok();
        last.map(|d| {
            let back = i64::from(d.weekday().number_days_from_sunday());
            d - time::Duration::days(back)
        })
    };
    let (Some(start), Some(end)) = (
        last_sunday(time::Month::March),
        last_sunday(time::Month::October),
    ) else {
        return false;
    };
    let hour = time::Duration::hours(1);
    let start = start.midnight().assume_utc() + hour;
    let end = end.midnight().assume_utc() + hour;
    at >= start && at < end
}

/// `23.11.2026, 14:05` in the Warsaw calendar.
fn warsaw_text(at: OffsetDateTime) -> String {
    let offset = if warsaw_summer(at) { 2 } else { 1 };
    let local = at + time::Duration::hours(offset);
    format!(
        "{:02}.{:02}.{}, {:02}:{:02}",
        local.day(),
        u8::from(local.month()),
        local.year(),
        local.hour(),
        local.minute()
    )
}

fn aria(active: bool, dir: SortDir) -> &'static str {
    match (active, dir) {
        (false, _) => "none",
        (true, SortDir::Asc) => "ascending",
        (true, SortDir::Desc) => "descending",
    }
}

fn card(title: &str, description: &str, rows: &[(&str, i64)]) -> Card {
    Card {
        title: title.to_owned(),
        description: description.to_owned(),
        rows: rows
            .iter()
            .map(|(l, v)| ((*l).to_owned(), v.to_string()))
            .collect(),
    }
}

/// `GET /backend`: one read-only page.
pub async fn backend(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(raw): Query<BackendQuery>,
) -> Response {
    if admin_user(&state, &jar).await.is_none() {
        return denied_page();
    }
    let query = parse_admin_accounts_query(
        raw.page.as_deref(),
        raw.q.as_deref(),
        raw.sort.as_deref(),
        raw.dir.as_deref(),
    );
    let now = (state.clock)();
    let totals = state.db.admin_account_totals().await;
    let counters = state.db.admin_game_counters().await;
    let stats = state.db.admin_finished_stats(now).await;
    let accounts = state.db.admin_account_page(&query).await;
    let db_ok = state.db.admin_account_totals().await.is_ok();
    let (Ok(totals), Ok(counters), Ok(stats), Ok(accounts)) = (totals, counters, stats, accounts)
    else {
        // Only the fact is logged: an error text may carry an address.
        tracing::error!(event = "admin.data_failed");
        return no_store(render(&BackendPage {
            data_error: true,
            cards: Vec::new(),
            query_q: String::new(),
            sort_is_name: false,
            rows: Vec::new(),
            name_aria: "none",
            created_aria: "none",
            name_href: "/backend".into(),
            created_href: "/backend".into(),
            page: 1,
            page_count: 1,
            total: 0,
            prev_href: None,
            next_href: None,
            db_ok,
        }));
    };
    let cards = vec![
        card(
            "Konta",
            "Wszystkie założone konta",
            &[
                ("Wszystkie", totals.total),
                ("E-mail potwierdzony", totals.verified),
                ("E-mail niepotwierdzony", totals.total - totals.verified),
            ],
        ),
        card(
            "Partie w bazie teraz",
            "Stan chwilowy: sprzątanie usuwa część zakończonych partii po ok. godzinie",
            &[
                ("oczekujące", counters.waiting),
                ("trwające", counters.active),
                ("zakończone", counters.finished),
                ("anulowane", counters.cancelled),
            ],
        ),
        card(
            "Zakończone partie",
            "Licznik trwały, doba w strefie Europe/Warsaw",
            &[
                ("Dziś", stats.today),
                ("Ostatnie 7 dni", stats.last_7_days),
                ("Razem", stats.total),
            ],
        ),
        Card {
            title: "Zakończone wg pary typów".into(),
            description: "Licznik trwały (E06-S03); „agent” to dołączenie przez MCP".into(),
            rows: stats
                .pairs
                .iter()
                .map(|p| (pair_label(&p.kinds), p.total.to_string()))
                .collect(),
        },
        card(
            "Zakończone w bazie wg początku",
            "Tylko partie, które jeszcze są w bazie",
            &[
                ("z zaproszenia", counters.invite),
                ("z lobby", counters.lobby),
                ("rewanż", counters.rematch),
                ("z botem", counters.bot),
            ],
        ),
        card(
            "Zakończone w bazie wg zakończenia",
            "Tylko partie, które jeszcze są w bazie",
            &[
                ("pięć w linii", counters.five_in_row),
                ("remis", counters.draw),
                ("czas", counters.timeout),
                ("poddanie", counters.resignation),
                ("punkty", counters.points),
            ],
        ),
    ];
    let shown = gomoku_core::admin::AccountsQuery {
        page: accounts.page,
        ..query.clone()
    };
    let toggle = |key: SortKey| {
        let dir = if query.sort == key {
            match query.dir {
                SortDir::Asc => SortDir::Desc,
                SortDir::Desc => SortDir::Asc,
            }
        } else if key == SortKey::Name {
            SortDir::Asc
        } else {
            SortDir::Desc
        };
        admin_accounts_href(
            &shown,
            &HrefChange {
                page: Some(1),
                sort: Some(key),
                dir: Some(dir),
            },
        )
    };
    let rows = accounts
        .rows
        .iter()
        .map(|r| AccountView {
            name: r.display_name.clone(),
            email: r.email.clone(),
            verified: if r.verified { "Tak" } else { "Nie" },
            created_iso: iso_ms(r.created_at),
            created_text: warsaw_text(r.created_at),
            finished: r.finished_games,
            role: if is_installation_admin(&r.email, state.config.admin_email.as_deref()) {
                "Administrator"
            } else {
                "Gracz"
            },
        })
        .collect();
    let page_link = |page: i64| {
        admin_accounts_href(
            &shown,
            &HrefChange {
                page: Some(page),
                ..HrefChange::default()
            },
        )
    };
    no_store(render(&BackendPage {
        data_error: false,
        cards,
        query_q: query.q.clone(),
        sort_is_name: query.sort == SortKey::Name,
        rows,
        name_aria: aria(query.sort == SortKey::Name, query.dir),
        created_aria: aria(query.sort == SortKey::Created, query.dir),
        name_href: toggle(SortKey::Name),
        created_href: toggle(SortKey::Created),
        page: accounts.page,
        page_count: accounts.page_count,
        total: accounts.total,
        prev_href: (accounts.page > 1).then(|| page_link(accounts.page - 1)),
        next_href: (accounts.page < accounts.page_count).then(|| page_link(accounts.page + 1)),
        db_ok,
    }))
}

fn no_store(mut response: Response) -> Response {
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("private, no-store"),
    );
    response
}

#[derive(Template)]
#[template(path = "show_console.html")]
struct ConsolePage {
    ctx: Ctx,
    initial: String,
}

#[derive(Template)]
#[template(path = "show_room.html")]
struct RoomPage {
    ctx: Ctx,
    game_id: String,
    initial: String,
}

const SHOW_BAG: [&str; 5] = ["show.", "common.", "nav.", "errors.api.", "game.shared."];

/// `GET /show`: the console; its first state is rendered here.
pub async fn show_console(
    State(state): State<AppState>,
    Lang(locale): Lang,
    jar: CookieJar,
) -> Response {
    let Some(user) = admin_user(&state, &jar).await else {
        return denied_page();
    };
    let initial = match crate::routes_show::overview_data(&state).await {
        Ok(data) => data,
        Err(error) => return error.into_response(),
    };
    let ctx = Ctx::new(state.catalogs.translator(locale), Some(user), "/show").with_bag(&SHOW_BAG);
    no_store(no_referrer(render(&ConsolePage {
        ctx,
        initial: script_json(&initial),
    })))
}

/// `GET /show/{id}`: the show; the 404 page for anything that is not a show.
pub async fn show_room(
    State(state): State<AppState>,
    Lang(locale): Lang,
    Path(id): Path<String>,
    jar: CookieJar,
) -> Response {
    let Some(user) = admin_user(&state, &jar).await else {
        return denied_page();
    };
    let Ok(game_id) = show_id(&id) else {
        return axum::http::StatusCode::NOT_FOUND.into_response();
    };
    let now = (state.clock)();
    let Ok(read) = state.db.show_read(game_id, 0, now).await else {
        return axum::http::StatusCode::NOT_FOUND.into_response();
    };
    let Ok(view) = show_state(&state, &read, 0, now) else {
        return axum::http::StatusCode::NOT_FOUND.into_response();
    };
    if read.bot_on_turn.is_some() {
        crate::bot::schedule_bot_turn(&state, game_id);
    }
    let initial = json!({ "gameId": game_id, "state": view });
    let ctx = Ctx::new(state.catalogs.translator(locale), Some(user), "/show").with_bag(&SHOW_BAG);
    no_store(no_referrer(render(&RoomPage {
        ctx,
        game_id: game_id.to_string(),
        initial: script_json(&initial),
    })))
}
