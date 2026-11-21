# Zasady pracy agentów

Ten plik jest jedynym źródłem instrukcji dla agentów AI. `CLAUDE.md` tylko go importuje — nie dopisuj tam treści.

Repozytorium to przepisanie aplikacji Gomoku (Node.js) na Rust. Plan, fazy i decyzje właściciela:
`.ai/specs/2026-11-21-rust-rewrite-plan.md` (źródło prawdy). Wymagania produktu i kontrakty API:
`~/Development/ai_techleaders_gomoku/.ai/specs/` (`product-brief.md`, `implemented/*`) — to repo ich nie zmienia.
**Cała praca odbywa się w tym katalogu**; stare repo służy tylko do odczytu.

## Stack (wiążący — nie zmieniaj bez zgody użytkownika)

| Obszar | Technologia |
| --- | --- |
| Język, workspace | Rust (stable, `rust-toolchain.toml`), edycja 2024, Cargo workspace `crates/*` |
| HTTP | Axum + Tokio |
| Frontend | Askama (szablony) + htmx, mało JS; tokeny z `assets/game-tokens.css` |
| Baza | PostgreSQL + SQLx (`sqlx migrate`, bez ORM, nowy schemat od zera) |
| DI | brak kontenera — jawna kompozycja w `AppState` |
| Hasła | argon2 (od F2) |
| Testy | `cargo test`, `cargo llvm-cov`, Testcontainers (PostgreSQL), `agent-browser` dla przeglądarki |
| CI | GitHub Actions (`.github/workflows/ci.yml`) |

### Crates i kierunek zależności

`gomoku-web → gomoku-core, gomoku-db`; `gomoku-db → gomoku-core`; `gomoku-core` nie zależy od niczego z workspace,
HTTP ani bazy. Nowy crate dopisz do tej tabeli.

## Konwencje

- Kod, identyfikatory i komentarze po angielsku; teksty UI i dokumentacja po polsku (PL jest źródłem, EN/FR/DE to tłumaczenia).
- Brak `unwrap()`/`expect()` poza testami (lint `clippy::unwrap_used`, `expect_used`); brak `unsafe` (`forbid`).
- Granice (czas, baza, e-mail) wstrzykuj przez traity i struktury w `AppState`, żeby logikę dało się testować bez sieci.
- Zmiany schematu tylko przez nowe pliki w `crates/gomoku-db/migrations`; nie edytuj zastosowanych migracji.
- Konfiguracja przez zmienne środowiskowe (`.env.example`); sekretów nie commituj. Czytaj je przez `from_lookup`, nie wprost z `std::env` w logice.
- Strony i endpointy czytające bazę nie inicjalizują puli na poziomie modułu — pula jest leniwa i żyje w `AppState`.

## Testy i pokrycie

Progi (decyzja właściciela, `scripts/coverage.sh`): `gomoku-core` 100% linii i funkcji; `gomoku-db` i `gomoku-web`
≥ 85% linii na crate, bez `main.rs` (samo okablowanie procesu). Progów nie obniżaj, nie dodawaj wyłączeń ani atrybutów
wyłączających pokrycie bez zgody użytkownika. Testy jednostkowe obok kodu (`#[cfg(test)]`), HTTP w procesie w
`crates/*/tests`, baza na prawdziwym PostgreSQL przez Testcontainers (wymaga Dockera). Reguły gry testuj przypadkami
przeniesionymi ze starego repo (`packages/core/src/**/*.test.ts`) i testami własnościowymi.

## Komendy

`cargo fmt --all --check` · `cargo clippy --workspace --all-targets -- -D warnings` · `cargo test --workspace` ·
`scripts/coverage.sh` · `docker build -t gomoku-rust .` · `cargo run -p gomoku-web`

Bramka przed uznaniem zmiany za gotową: fmt → clippy → test → coverage → (UI) sprawdzenie w działającej aplikacji.
Jeśli czegoś nie dało się uruchomić (np. brak Dockera) — napisz to wprost.

## Przepływ pracy

1. Na start przeczytaj plan w `.ai/specs/` i `.ai/lessons.md`.
2. Złożone zadanie zaczynaj od planu, nie od kodu; gdy coś się rozjeżdża — wróć do planowania.
3. Mało, ale pełnych PR-ów: jeden PR na fazę/funkcję (kod, testy, dokumentacja razem). Deploy dopiero na końcu całości.
4. Po poprawce użytkownika zapisz lekcję w `.ai/lessons/YYYY-MM-DD-slug.md` i linię w `.ai/lessons.md`.
5. Pliki w `.ai/specs|docs|runs|lessons` nazywaj `YYYY-MM-DD-slug.md`. Przebiegi autonomiczne loguj w `.ai/runs/`.
6. Skille `om-*` leżą w `.claude/skills/`; nie edytuj ich — odstępstwa zapisuj w `.ai/skills/<skill>/SKILL.md`.

## Proces dostarczania (skille `om-*`)

Konfiguracja pipeline'u: `.ai/agentic.config.json` (bramka walidacyjna = fmt, clippy, `scripts/coverage.sh`; tracker GitHub; przeglądarka `agent-browser`).
Etapy, etykiety i bramka QA: `SDLC.md`; zasady review: `CODE_REVIEW.md`; chronione kontrakty: `BACKWARD_COMPATIBILITY.md`.
