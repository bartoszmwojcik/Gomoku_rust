# Gomoku (Rust)

Przepisanie aplikacji Gomoku 1:1 online z Node.js (Next.js) na Rust. Plan i decyzje:
[`.ai/specs/2026-11-21-rust-rewrite-plan.md`](.ai/specs/2026-11-21-rust-rewrite-plan.md).
Wersja referencyjna (Node.js): `~/Development/ai_techleaders_gomoku`.

## Uruchomienie lokalne

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # jednorazowo; toolchain z rust-toolchain.toml
cargo install cargo-llvm-cov --locked                            # jednorazowo, do pokrycia
docker compose up -d                                             # PostgreSQL
cp .env.example .env
cargo run -p gomoku-db -- migrate                                # schemat bazy
cargo run -p gomoku-web                                          # http://localhost:3000
```

## Komendy

| Komenda | Co robi |
| --- | --- |
| `cargo fmt --all --check` | format |
| `cargo clippy --workspace --all-targets -- -D warnings` | lint |
| `cargo test --workspace` | testy (db wymaga Dockera) |
| `scripts/coverage.sh [core db web]` | bramki pokrycia |
| `cargo run -p gomoku-db -- migrate` | migracje (aplikacja sama ich nie uruchamia) |
| `docker build -t gomoku-rust .` | obraz produkcyjny (zawiera `gomoku-web` i `gomoku-db`) |

## Struktura

```
crates/gomoku-core  reguły gry, zegar, boty (bez HTTP i bazy)
crates/gomoku-db    konfiguracja, pula, migracje, konta, sesje, limity (SQLx; CLI `gomoku-db`)
crates/gomoku-web   Axum + Askama + htmx; katalogi tekstów w `crates/gomoku-web/i18n/{pl,en,fr}.json`
assets/             tokeny design systemu, CSS, JS
```
