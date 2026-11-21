//! Interface texts: flat JSON catalogs per language, merged along the fallback chain, plus the `?lang=` switch.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{FromRequestParts, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderValue, Method, StatusCode, Uri, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum_extra::extract::CookieJar;
use axum_extra::extract::cookie::{Cookie, SameSite};
use gomoku_core::i18n::{LANG_COOKIE, LANG_COOKIE_MAX_AGE_SECONDS, Locale, resolve_locale};
use thiserror::Error;
use time::Duration;

use crate::app::AppState;

const PL: &str = include_str!("../i18n/pl.json");
const EN: &str = include_str!("../i18n/en.json");
const FR: &str = include_str!("../i18n/fr.json");

#[derive(Debug, Error)]
pub enum CatalogError {
    #[error("invalid catalog {0}: {1}")]
    Invalid(&'static str, serde_json::Error),
}

type Flat = BTreeMap<String, String>;

/// All catalogs. A language is looked up along its fallback chain (`fr` → `en` → `pl`), so a missing key never
/// shows up empty.
#[derive(Debug)]
pub struct Catalogs {
    by_locale: BTreeMap<&'static str, Flat>,
}

impl Catalogs {
    pub fn embedded() -> Result<Arc<Self>, CatalogError> {
        Self::from_sources(&[("pl", PL), ("en", EN), ("fr", FR)])
    }

    /// Builds catalogs from `(language code, JSON text)` pairs; every text is a flat object of strings.
    pub fn from_sources(sources: &[(&'static str, &str)]) -> Result<Arc<Self>, CatalogError> {
        let mut by_locale = BTreeMap::new();
        for (code, text) in sources {
            let flat: Flat =
                serde_json::from_str(text).map_err(|e| CatalogError::Invalid(code, e))?;
            by_locale.insert(*code, flat);
        }
        Ok(Arc::new(Self { by_locale }))
    }

    fn lookup(&self, locale: Locale, key: &str) -> Option<&str> {
        locale
            .fallback_chain()
            .iter()
            .find_map(|candidate| self.by_locale.get(candidate.code())?.get(key))
            .map(String::as_str)
    }

    /// Keys of one language (used by the parity test).
    pub fn keys(&self, code: &str) -> Vec<&str> {
        self.by_locale
            .get(code)
            .map(|flat| flat.keys().map(String::as_str).collect())
            .unwrap_or_default()
    }

    pub fn translator(self: &Arc<Self>, locale: Locale) -> Translator {
        Translator {
            catalogs: Arc::clone(self),
            locale,
        }
    }
}

/// Texts in one language.
#[derive(Debug, Clone)]
pub struct Translator {
    catalogs: Arc<Catalogs>,
    locale: Locale,
}

impl Translator {
    pub fn locale(&self) -> Locale {
        self.locale
    }

    /// The `<html lang>` code.
    pub fn lang(&self) -> &'static str {
        self.locale.code()
    }

    /// The text for `key`; an unknown key yields the key itself and is logged, never an empty string.
    pub fn t(&self, key: &str) -> String {
        match self.catalogs.lookup(self.locale, key) {
            Some(text) => text.to_owned(),
            None => {
                tracing::warn!(key, lang = self.locale.code(), "missing translation key");
                key.to_owned()
            }
        }
    }

    /// The text for `key` with `{name}` placeholders filled from `args`.
    pub fn t_with(&self, key: &str, args: &[(&str, &str)]) -> String {
        args.iter().fold(self.t(key), |text, (name, value)| {
            text.replace(&format!("{{{name}}}"), value)
        })
    }

    /// The text of a counted phrase: `key.one|few|many|other` by the language's plural rule (missing category: `other`),
    /// with `{count}` filled.
    pub fn t_plural(&self, key: &str, count: u64) -> String {
        let category = self.locale.plural(count).suffix();
        let specific = format!("{key}.{category}");
        let text = if self.catalogs.lookup(self.locale, &specific).is_some() {
            self.t(&specific)
        } else {
            self.t(&format!("{key}.other"))
        };
        text.replace("{count}", &count.to_string())
    }

    /// All texts under any of `prefixes`, with their full keys, as one JSON object (the chain merged), for the scripts
    /// that render screens in the browser.
    pub fn bag_json(&self, prefixes: &[&str]) -> String {
        let mut merged: BTreeMap<&str, &str> = BTreeMap::new();
        for locale in self.locale.fallback_chain().iter().rev() {
            if let Some(flat) = self.catalogs.by_locale.get(locale.code()) {
                for (key, text) in flat
                    .iter()
                    .filter(|(key, _)| prefixes.iter().any(|p| key.starts_with(p)))
                {
                    merged.insert(key, text);
                }
            }
        }
        serde_json::to_string(&merged)
            .unwrap_or_else(|_| "{}".to_owned())
            .replace('<', "\\u003c")
    }

    /// All texts under `prefix` (the prefix removed) as one JSON object, merged along the fallback chain; for the
    /// form script, which translates server error codes in the browser.
    pub fn subtree_json(&self, prefix: &str) -> String {
        let mut merged: BTreeMap<&str, &str> = BTreeMap::new();
        for locale in self.locale.fallback_chain().iter().rev() {
            if let Some(flat) = self.catalogs.by_locale.get(locale.code()) {
                for (key, text) in flat.iter().filter(|(key, _)| key.starts_with(prefix)) {
                    merged.insert(&key[prefix.len()..], text);
                }
            }
        }
        serde_json::to_string(&merged)
            .unwrap_or_else(|_| "{}".to_owned())
            .replace('<', "\\u003c")
    }
}

/// The language of a request: the cookie, then `Accept-Language`, then Polish.
pub struct Lang(pub Locale);

impl<S: Send + Sync> FromRequestParts<S> for Lang {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let jar = CookieJar::from_headers(&parts.headers);
        let cookie = jar.get(LANG_COOKIE).map(|c| c.value().to_owned());
        let accept = parts
            .headers
            .get(header::ACCEPT_LANGUAGE)
            .and_then(|v| v.to_str().ok());
        Ok(Lang(resolve_locale(cookie.as_deref(), accept)))
    }
}

/// The query of `uri` without the `lang` parameter, and the value of `lang` if present.
fn split_lang(uri: &Uri) -> (Option<String>, Option<String>) {
    let Some(query) = uri.query() else {
        return (None, None);
    };
    let mut lang = None;
    let mut rest = Vec::new();
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        match pair.split_once('=').unwrap_or((pair, "")) {
            ("lang", value) => {
                if lang.is_none() {
                    lang = Some(value.to_owned());
                }
            }
            _ => rest.push(pair),
        }
    }
    (lang, (!rest.is_empty()).then(|| rest.join("&")))
}

/// Turns `?lang=xx` on a page into the language cookie and a redirect to the same address without the
/// parameter (the rest of the query is kept). An unsupported value is only dropped. Pages only: `/api` and
/// `/assets` are left alone.
pub async fn lang_redirect(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path();
    let is_page = request.method() == Method::GET
        && !path.starts_with("/api")
        && !path.starts_with("/assets");
    let (lang, rest) = split_lang(request.uri());
    if !is_page || lang.is_none() {
        return next.run(request).await;
    }
    let target = match rest {
        Some(query) => format!("{path}?{query}"),
        None => path.to_owned(),
    };
    let mut response =
        (StatusCode::TEMPORARY_REDIRECT, [(header::LOCATION, target)]).into_response();
    if let Some(locale) = lang.as_deref().and_then(Locale::from_code) {
        let mut cookie = Cookie::new(LANG_COOKIE, locale.code());
        cookie.set_http_only(true);
        cookie.set_secure(state.config.session_cookie_secure);
        cookie.set_same_site(SameSite::Lax);
        cookie.set_path("/");
        cookie.set_max_age(Duration::seconds(LANG_COOKIE_MAX_AGE_SECONDS as i64));
        if let Ok(value) = HeaderValue::from_str(&cookie.to_string()) {
            response.headers_mut().append(header::SET_COOKIE, value);
        }
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalogs() -> Arc<Catalogs> {
        Catalogs::from_sources(&[
            ("pl", r#"{"a":"A-pl","b":"B-pl","c {x}":"unused","errors.X":"X-pl","errors.Y":"Y-pl","hello":"Cześć {name}!"}"#),
            ("en", r#"{"a":"A-en","errors.X":"X-en"}"#),
            ("fr", r#"{"errors.Y":"Y-fr"}"#),
        ])
        .unwrap()
    }

    #[test]
    fn looks_up_along_the_fallback_chain() {
        let catalogs = catalogs();
        assert_eq!(catalogs.translator(Locale::Pl).t("a"), "A-pl");
        assert_eq!(catalogs.translator(Locale::En).t("a"), "A-en");
        assert_eq!(catalogs.translator(Locale::En).t("b"), "B-pl");
        assert_eq!(catalogs.translator(Locale::Fr).t("a"), "A-en");
        assert_eq!(catalogs.translator(Locale::Fr).t("b"), "B-pl");
        // German has no catalog yet: it falls back to English, then Polish.
        assert_eq!(catalogs.translator(Locale::De).t("a"), "A-en");
    }

    #[test]
    fn an_unknown_key_is_the_key_itself() {
        assert_eq!(catalogs().translator(Locale::Pl).t("nope"), "nope");
    }

    #[test]
    fn fills_placeholders() {
        let t = catalogs().translator(Locale::Pl);
        assert_eq!(t.t_with("hello", &[("name", "Ala")]), "Cześć Ala!");
        assert_eq!((t.lang(), t.locale()), ("pl", Locale::Pl));
    }

    #[test]
    fn subtree_is_merged_along_the_chain_with_the_prefix_removed() {
        let json = catalogs().translator(Locale::Fr).subtree_json("errors.");
        assert_eq!(json, r#"{"X":"X-en","Y":"Y-fr"}"#);
        assert_eq!(
            catalogs().translator(Locale::Pl).subtree_json("none."),
            "{}"
        );
    }

    #[test]
    fn counted_phrases_pick_the_plural_form_and_the_bag_keeps_full_keys() {
        let catalogs = Catalogs::from_sources(&[
            ("pl", r#"{"n.one":"{count} minuta","n.few":"{count} minuty","n.many":"{count} minut","n.other":"{count} min","x.a":"A","y.b":"B"}"#),
            ("en", r#"{"n.one":"{count} minute","n.other":"{count} minutes"}"#),
        ])
        .unwrap();
        let pl = catalogs.translator(Locale::Pl);
        assert_eq!(
            (
                pl.t_plural("n", 1),
                pl.t_plural("n", 3),
                pl.t_plural("n", 10)
            ),
            ("1 minuta".into(), "3 minuty".into(), "10 minut".into())
        );
        let en = catalogs.translator(Locale::En);
        assert_eq!(
            (en.t_plural("n", 1), en.t_plural("n", 5)),
            ("1 minute".into(), "5 minutes".into())
        );
        // A category the language lacks falls back to `other`.
        let fr = catalogs.translator(Locale::Fr);
        assert_eq!(fr.t_plural("n", 2), "2 minutes");
        let bag = pl.bag_json(&["x.", "n."]);
        assert!(
            bag.contains(r#""x.a":"A""#)
                && bag.contains(r#""n.few":"{count} minuty""#)
                && !bag.contains("y.b")
        );
    }

    #[test]
    fn rejects_a_catalog_that_is_not_flat_strings() {
        assert!(Catalogs::from_sources(&[("pl", r#"{"a":{"b":"c"}}"#)]).is_err());
        assert!(Catalogs::from_sources(&[("pl", "nope")]).is_err());
    }

    #[test]
    fn the_embedded_catalogs_cover_the_same_keys() {
        let catalogs = Catalogs::embedded().unwrap();
        let pl = catalogs.keys("pl");
        assert!(!pl.is_empty());
        assert_eq!(catalogs.keys("en"), pl, "EN catalog differs from PL");
        assert_eq!(catalogs.keys("fr"), pl, "FR catalog differs from PL");
        assert!(catalogs.keys("de").is_empty());
    }

    #[test]
    fn splits_the_lang_parameter_off_a_query() {
        let split = |uri: &str| split_lang(&uri.parse().unwrap());
        assert_eq!(split("/x"), (None, None));
        assert_eq!(split("/x?lang=fr"), (Some("fr".into()), None));
        assert_eq!(
            split("/x?a=1&lang=fr&b=2"),
            (Some("fr".into()), Some("a=1&b=2".into()))
        );
        assert_eq!(split("/x?a=1"), (None, Some("a=1".into())));
        assert_eq!(split("/x?lang=fr&lang=en"), (Some("fr".into()), None));
        assert_eq!(
            split("/x?&flag&lang="),
            (Some(String::new()), Some("flag".into()))
        );
    }
}
