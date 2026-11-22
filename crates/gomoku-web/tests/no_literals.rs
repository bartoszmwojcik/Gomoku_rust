//! The player screens carry no Polish literals: every text comes from the catalogs (PL is the source, EN/FR/DE
//! translate). The administrator's panel (`backend.html`) is Polish by decision and is the only exception.

use std::fs;
use std::path::Path;

const POLISH: &str = "ąćęłńóśźżĄĆĘŁŃÓŚŹŻ";

fn files(dir: &str, extension: &str) -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join(dir);
    let mut out = Vec::new();
    for entry in fs::read_dir(root).expect("directory") {
        let path = entry.expect("entry").path();
        if path.extension().and_then(|e| e.to_str()) == Some(extension) {
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            out.push((name, fs::read_to_string(&path).expect("file")));
        }
    }
    out
}

/// The code of a script without `//` comments (a `//` inside a URL string is followed by no Polish text anyway).
fn code_of(text: &str) -> String {
    text.lines()
        .map(|line| line.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn scripts_and_templates_hold_no_polish_text() {
    let mut offenders = Vec::new();
    for (name, text) in files("../../assets", "js") {
        if code_of(&text).chars().any(|c| POLISH.contains(c)) {
            offenders.push(format!("assets/{name}"));
        }
    }
    for (name, text) in files("templates", "html") {
        if name != "backend.html" && text.chars().any(|c| POLISH.contains(c)) {
            offenders.push(format!("templates/{name}"));
        }
    }
    assert!(offenders.is_empty(), "Polish literals in: {offenders:?}");
}

/// Static keys asked for with `t("…")`, `gt("…")` or `gtp("…")`.
fn keys_in(text: &str) -> Vec<String> {
    let mut keys = Vec::new();
    for opener in ["t(\"", "gt(\"", "gtp(\"", "gt(`", "gtp(`"] {
        let quote = opener.chars().last().unwrap_or('"');
        let mut rest = text;
        while let Some(at) = rest.find(opener) {
            // `t(` must not be the tail of another identifier (`format(`, `get(`).
            let before = rest[..at].chars().last();
            rest = &rest[at + opener.len()..];
            if before.is_some_and(|c| c.is_alphanumeric() || c == '_') && opener.starts_with("t(") {
                continue;
            }
            if let Some(end) = rest.find(quote) {
                let key = &rest[..end];
                if !key.contains("${") && key.contains('.') && !key.contains(' ') {
                    keys.push(key.to_owned());
                }
            }
        }
    }
    keys
}

#[test]
fn every_static_key_the_screens_ask_for_exists_in_the_catalog() {
    let catalog: std::collections::BTreeMap<String, String> = serde_json::from_str(
        &fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("i18n/pl.json"))
            .expect("pl"),
    )
    .expect("json");
    let known = |key: &str| {
        catalog.contains_key(key)
            || catalog.contains_key(&format!("{key}.other"))
            || catalog.keys().any(|k| k.starts_with(&format!("{key}.")))
    };
    let mut missing = Vec::new();
    let mut seen = 0;
    let mut sources = files("../../assets", "js");
    sources.extend(files("templates", "html"));
    for (name, text) in sources {
        if name == "backend.html" {
            continue;
        }
        for key in keys_in(&text) {
            seen += 1;
            if !known(&key) {
                missing.push(format!("{name}: {key}"));
            }
        }
    }
    assert!(seen > 300, "the scan found only {seen} keys");
    assert!(
        missing.is_empty(),
        "keys missing from pl.json: {missing:#?}"
    );
}
