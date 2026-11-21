# Plan: przepisanie Gomoku z Node.js na Rust

- Data: 2026-11-21
- Status: **szkic do zatwierdzenia przez właściciela** (Bartosz Wójcik) — żadnego kodu przed akceptacją planu
- Źródło prawdy dla wymagań: repozytorium `~/Development/ai_techleaders_gomoku` (Next.js), w szczególności
  `.ai/specs/product-brief.md` (reguły BR-*, decyzje D-*), `.ai/specs/implemented/*` (kontrakty API) i `docs/architektura.md`.
  To przepisanie **nie zmienia produktu** — odtwarza istniejące zachowanie. Nowe funkcje są poza zakresem.

## 1. Cel i motywacja

Zastąpić aplikację Next.js/TypeScript (ok. 40 tys. linii bez testów: app ≈ 30 tys., core ≈ 4,4 tys., db ≈ 2 tys., ui ≈ 3 tys.)
jednym serwerem w Rust. Oczekiwane zyski: mały i szybki proces (ważne na VPS Mikrus z ograniczoną pamięcią, gdzie
build Node.js już dziś kończy się OOM), bot liczony w prawdziwych wątkach zamiast osobnego procesu roboczego,
silne typowanie stanów gry, jeden statyczny plik binarny w obrazie Docker.

Kryterium sukcesu: **parytet funkcjonalny** z wersją Node.js (wszystkie epiki E01–E15 poza narzędziami deweloperskimi
specyficznymi dla Node), na tym samym schemacie danych albo z migracją danych (patrz pytania otwarte).

## 2. Zakres wejściowy (co trzeba odtworzyć)

| Obszar | Co jest dziś | Uwagi do portu |
| --- | --- | --- |
| Reguły gry | Gomoku swobodne 15×15, zegar 10 min, poddanie, rewanż, timeouty (lazy closing), kodowanie bitboardów, kody zaproszeń, nicki | Czysta logika — port 1:1, najłatwiejszy kawałek |
| Wariant „papier i ołówek” | Plansza 101×101, punkty za piątki/dziesiątki, rozliczanie (`paper-settler`) | Spec: `implemented/2026-11-02-paper-pencil-gomoku.md` |
| Bot | Poziomy 1–2, tablica transpozycji, Zobrist, wzorce, budżet czasu, arena, worker w osobnym procesie | Zysk wydajności z Rusta; arena służy jako test regresji siły gry |
| Konta | Rejestracja, logowanie, sesje w bazie (30 dni), reset i zmiana hasła, zmiana e-maila i nazwy, usunięcie konta z anonimizacją, potwierdzenie e-maila | bcrypt, CSRF (Origin + `X-Gomoku-Request`), limity prób |
| E-mail | Brevo przez HTTPS, transport plikowy w dev, dzienne limity | `reqwest` |
| Gry | Gość/konto, zaproszenie kodem i QR, lobby („szukam gry”), partie publiczne + widzowie, czat, historia, przegląd partii, rewanż | Czas rzeczywisty = **polling**, nie WebSocket |
| Agenci LLM | Serwer MCP `/api/mcp` (dołączanie do partii, ruchy, czat) | Tożsamość klienta, token, limity (D-29) |
| Pokaz (E14) | `/show`, bot vs bot / bot vs agent, widok pod projektor, tylko admin | |
| Admin | Panel `/backend` tylko do odczytu, liczniki zakończonych partii | |
| i18n | PL (źródło), EN, FR, DE po recenzji; test parzystości katalogów | Katalogi do przeniesienia |
| Infra | PostgreSQL, 19 migracji MikroORM, Docker + `compose.prod.yaml`, deploy na Mikrus, sprzątanie wygasłych rekordów (cron) | |
| API | ~38 tras `/api/*`, 18 stron | Kontrakty (kody błędów, koperta `{ok,data}`) zachować |

## 3. Decyzje architektoniczne (rekomendacje — do zatwierdzenia)

| # | Decyzja | Rekomendacja | Alternatywa / uzasadnienie |
| --- | --- | --- | --- |
| A1 | Serwer HTTP | **Axum** + Tokio | Standard ekosystemu, dobre middleware (Tower) pod CSRF, limity, sesje |
| A2 | Baza | **PostgreSQL + SQLx** (zapytania sprawdzane w czasie kompilacji, `sqlx migrate`) | SeaORM/Diesel dodają warstwę ORM bez potrzeby; MikroORM też był tylko cienką warstwą. Transakcje jawne (`tx`) — ta sama dyscyplina co dziś |
| A3 | DI | **Brak kontenera** — `AppState` ze strukturami serwisów (konstruktory + traity tam, gdzie potrzebny mock) | Awilix nie ma sensownego odpowiednika; idiomatycznie w Ruście to jawna kompozycja w `main` |
| A4 | Frontend | **Renderowanie po stronie serwera: Askama (szablony) + htmx**, mała ilość JS (plansza, zegar, polling) | Alternatywa: Leptos (pełny Rust/WASM) — większy zysk spójności, ale duże ryzyko i koszt przy 18 stronach, planszy 101×101 i design systemie. Alternatywa 2: zostawić React SPA i wymienić tylko backend (najmniejsze ryzyko, ale dwa stosy). **To główna decyzja do podjęcia** — patrz pytanie 1 |
| A5 | Style / design system | Tailwind CSS 4 (standalone CLI, bez Node w runtime) + **ponowne użycie `game-tokens.css`** i zasady z `packages/ui/docs/design-system.md` | Tokeny to czysty CSS — przenoszą się bez zmian; `check:tokens` do przeportowania jako skrypt |
| A6 | Czas rzeczywisty | Polling jak dziś (htmx `hx-trigger="every Ns"` / fetch), czas serwera do zegara | SSE można dodać później; poza zakresem parytetu |
| A7 | Hasła i sesje | `bcrypt` (zgodność z istniejącymi hashami), sesje w tabeli `sessions`, token w ciasteczku `httpOnly` (SHA-256 w bazie), `__Host-` w produkcji | Argon2 tylko jeśli właściciel zechce przeliczać hashe przy logowaniu |
| A8 | MCP | Oficjalny SDK Rusta (`rmcp`) lub własna cienka implementacja JSON-RPC nad Axum — **spike w fazie 7** | Trzeba sprawdzić aktualną wersję i wsparcie transportu streamable HTTP przed wyborem |
| A9 | Bot | `spawn_blocking`/wątek dedykowany z budżetem czasu; usunąć proces roboczy | Port algorytmu 1:1 najpierw, optymalizacje później; **arena (bot vs bot) jako test parytetu siły** |
| A10 | i18n | Katalogi PL/EN/FR/DE przeniesione do plików (JSON lub Fluent), ładowane przy starcie; test parzystości kluczy | PL nadal źródłem tekstów |
| A11 | Konfiguracja | Zmienne środowiskowe + `.env*` (`dotenvy`), ta sama lista zmiennych co `.env.example` | Zero zmian operacyjnych na VPS |
| A12 | Testy jednostkowe | `cargo test` + **`cargo llvm-cov` z progiem 100% per plik** (jak dziś) | Zasada „100% pokrycia” jest wiążąca w oryginale; w Ruście wymaga wyłączenia `main.rs` i generowanego kodu — **do ustalenia, bez cichych wyjątków** |
| A13 | Testy integracyjne | `testcontainers` (PostgreSQL) + testy HTTP w procesie (`axum` + `tower::ServiceExt`); scenariusze przeglądarkowe nadal przez `agent-browser` | Ten sam model co dziś |
| A14 | Struktura | Cargo workspace `crates/*` z jednokierunkowymi zależnościami: `web → core, db`; `db → core`; `core` bez zależności od HTTP/DB | Odpowiednik dzisiejszego `app → core, db, ui` |
| A15 | CI / obraz | GitHub Actions: `fmt`, `clippy -D warnings`, `test`, `llvm-cov`, `build`, integracja, audyt (`cargo audit`/`deny`); obraz Docker wieloetapowy budowany **w CI** i wypychany do rejestru | Na Mikrus nie budujemy Rusta (OOM) — tylko `docker pull` |

### Układ workspace

```
crates/
  gomoku-core/   reguły gry, zegar, boty, kody zaproszeń, typy domenowe (bez HTTP, bez DB)
  gomoku-db/     modele, zapytania SQLx, migracje, sprzątanie wygasłych rekordów (bin: db-cli)
  gomoku-web/    Axum: trasy, szablony, middleware (CSRF, sesja, limity), MCP, kompozycja AppState
  gomoku-i18n/   katalogi i ładowanie (opcjonalnie wydzielone z web)
assets/          game-tokens.css, ikony, katalogi tłumaczeń
tests/           integracyjne (testcontainers, agent-browser)
```

## 4. Strategia migracji

**Zasada: odtwarzanie z testami parytetu, bez „big bang” w jednym PR.** Jeden PR na fazę (kod + testy + dokumentacja),
zgodnie z regułą „mało, ale pełnych PR-ów”.

- Testy jednostkowe z `ai_techleaders_gomoku` (zwłaszcza `core`) są **specyfikacją wykonywalną**: przy każdej funkcji
  domenowej przenosimy przypadki testowe 1:1 (te same wejścia i wyniki), a dla reguł gry dodajemy testy własnościowe (`proptest`)
  i porównanie wyniku z implementacją TS na wspólnym zbiorze partii (golden files — wygenerowane raz z Node, zapisane w repo).
- Bot: ta sama arena i seedy; Rust musi grać **nie słabiej** niż TS (mecz kontrolny) i mieści się w budżecie czasu.
- API: kontrakty (kody błędów, kształt koperty, statusy, nagłówki CSRF) przepisujemy z `.ai/specs/implemented/*`
  i pilnujemy testami integracyjnymi, żeby frontend i klienci MCP nie zauważyli różnicy.
- Schemat danych: **te same tabele** (patrz pytanie 2). Baseline migracji = zrzut schematu po 19 migracjach MikroORM;
  dalsze zmiany tylko przez `sqlx migrate`.

## 5. Fazy

Szacunki są orientacyjne (jedna osoba + agent, w dniach roboczych). Kolejność wynika z zależności: najpierw to, co
czyste i testowalne, na końcu to, co integracyjne.

| Faza | Zakres | Wynik / kryterium zakończenia | Szac. |
| --- | --- | --- | --- |
| **F0. Fundament** | Instalacja toolchaina (rustup), workspace, `AGENTS.md` dla nowego repo, CI (fmt/clippy/test/llvm-cov), `/api/health`, konfiguracja env, SQLx + baseline migracji, Testcontainers, obraz Docker | `cargo test` zielone, health 200/503, CI zielone, obraz startuje w compose | 2–3 |
| **F1. Rdzeń domeny** | `gomoku-core`: plansza, ruch, wykrywanie wygranej, zegar, poddanie, rewanż, kody zaproszeń, nicki, kodek bitboardów, historia, przegląd partii | Wszystkie testy z `packages/core/src/games` przeniesione i zielone; testy własnościowe; golden files zgodne | 4–5 |
| **F2. Konta i sesje** | Rejestracja, logowanie/wylogowanie, sesje, CSRF, limity prób, reset i zmiana hasła, potwierdzenie i zmiana e-maila, zmiana nazwy, usunięcie konta, e-mail (plik + Brevo), sprzątanie rekordów | Kontrakty z `implemented/2026-10-24-*` i `2026-11-02-account-*` spełnione; strony `/register /login /account /forgot-password /reset-password /verify-email /confirm-email-change` | 7–9 |
| **F3. Gra 1:1** | Tworzenie partii (gość/konto), zaproszenia i kody, dołączanie, ruchy, zegar i timeouty, poddanie, anulowanie, rewanż, historia, przegląd, lobby i „szukam gry”, wznowienie połączenia, widok mobilny | Pełna partia dwóch ludzi w przeglądarce (scenariusz integracyjny), zgodność z `game-*`, `lobby-*`, `guest-*` | 10–12 |
| **F4. Czat, widzowie, QR** | Czat (limity, wyłączanie, sprzątanie), partie publiczne, widzowie, zamknięcie publiczności, kod QR | Specyfikacje `game-chat`, `spectate-live-game`, `public-game-*`, `invite-qr` | 5–6 |
| **F5. Papier i ołówek** | Wariant 101×101: stan, ruchy, rozliczanie punktów | Specyfikacja `paper-pencil-gomoku`; testy własnościowe liczenia punktów | 4–5 |
| **F6. Bot** | Poziomy 1–2, Zobrist, tablica transpozycji, wzorce, budżet, arena, brama zasobów (`compute-gate`), partia z botem | Arena: Rust ≥ TS; testy z `core/src/bot`; partia z botem w UI | 5–7 |
| **F7. Agenci MCP** | Spike wyboru biblioteki MCP, endpoint, tożsamość klienta, dołączanie kodem, ruchy, czat, limity | Specyfikacje `agent-*`, `mcp-agent-identity`; klient MCP gra pełną partię | 4–5 |
| **F8. Admin i pokaz** | Panel `/backend` (tylko odczyt), liczniki par, `/show` (bot vs bot, bot vs agent, widok pod projektor), `requireAdmin` | Specyfikacje `admin-panel-read-only`, `show-mode`; strażnik tras adminowych | 4–5 |
| **F9. i18n i dopracowanie UI** | Katalogi PL/EN/FR(/DE), test parzystości, przegląd design systemu, motyw jasny/ciemny, dostępność | Brak literałów PL w ekranach graczy, parytet katalogów | 3–4 |
| **F10. Dane i wdrożenie** | Weryfikacja na kopii bazy produkcyjnej, plan przełączenia, obraz z CI, deploy na Mikrus, rollback, dokumentacja operacyjna | Produkcja działa na Rust, Node wyłączony | 3–4 |

Orientacyjnie: **50–65 dni roboczych**; największa niepewność to F3 (zakres), F7 (biblioteka MCP) i F6 (parytet siły bota).
Deploy robimy **na końcu epiku/całości** (reguła właściciela: nie per PR), więc przez F1–F9 produkcja zostaje na Node.

## 6. Ryzyka

| Ryzyko | Skutek | Mitygacja |
| --- | --- | --- |
| Wybór frontendu (A4) okaże się zły po F2–F3 | Przeróbka wielu ekranów | Spike w F0: jedna strona (`/login`) + plansza w wybranej technice, przed F2 |
| Rozjazd zachowania (reguły, zegar, timeouty) | Błędne wyniki partii | Golden files z Node, testy własnościowe, te same przypadki testowe |
| Bot słabszy lub wolniejszy | Gorsza rozgrywka, pokaz | Arena jako bramka w F6, ten sam seed, budżet czasu |
| 100% pokrycia w Ruście bywa kosztowne | Spowolnienie lub pokusa wyjątków | Jawna lista wyłączeń uzgodniona w F0 (`main.rs`, kod generowany), architektura z wstrzykiwaniem zależności; bez cichych wyjątków |
| MCP w Ruście młodsze niż TS SDK | Opóźnienie F7 | Spike; awaryjnie własna implementacja JSON-RPC (protokół jest mały) |
| Dane produkcyjne i hashe haseł | Utrata kont | Ten sam schemat, bcrypt zgodny; próba migracji na kopii bazy w F10 |
| Czas jednej osoby | Wydłużenie | Fazy dostarczalne niezależnie; Node działa do końca |
| Brak Rusta na maszynie | — | F0 zaczyna się od `rustup` (obecnie `cargo` nie jest zainstalowany) |

## 7. Pytania otwarte (do właściciela)

1. **Frontend (A4):** Askama + htmx (rekomendacja), Leptos, czy zostawić React SPA i przepisać tylko backend?
2. **Dane:** czy na produkcji są dane do zachowania (konta, historia partii)? Jeśli tak — Rust przejmuje istniejący schemat
   i hashe bcrypt (rekomendacja). Jeśli nie — możemy uprościć schemat od zera.
3. **Zakres parytetu:** czy przenosimy wszystko (łącznie z pokazem E14, MCP, wariantem papier-ołówek i DE), czy tniemy
   do MVP gracza (F0–F4) i resztę dokładamy później?
4. **Pokrycie 100%:** utrzymujemy próg per plik w Ruście (z uzgodnionymi wyłączeniami), czy łagodzimy do progu globalnego?
5. **Nazwa i miejsce:** repozytorium `Gomoku_rust` jako osobne, nowe repo z własnym remote na GitHubie? Co z historią starego repo (archiwum po wdrożeniu)?
6. **Skille `om-*`:** po F0 uruchomić `om-setup-agent-pipeline` w tym repo (tracker, bramka walidacji, `SDLC.md`) i przeportować `AGENTS.md`/`CODE_REVIEW.md` ze starego repo?

## 8. Następny krok po zatwierdzeniu

F0: instalacja toolchaina, szkielet workspace, `AGENTS.md`, CI i `/api/health`; w F0 także spike frontendu z pytania 1.
