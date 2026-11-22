# Plan: przepisanie Gomoku z Node.js na Rust

- Data: 2026-11-21
- Status: **zatwierdzony przez właściciela (Bartosz Wójcik) 2026-11-21** — decyzje w sekcji 7; kolejny krok to F0
- Źródło prawdy dla wymagań: repozytorium `~/Development/ai_techleaders_gomoku` (Next.js), w szczególności
  `.ai/specs/product-brief.md` (reguły BR-*, decyzje D-*), `.ai/specs/implemented/*` (kontrakty API) i `docs/architektura.md`.
  To przepisanie **nie zmienia produktu** — odtwarza istniejące zachowanie. Nowe funkcje są poza zakresem.

## 1. Cel i motywacja

Zastąpić aplikację Next.js/TypeScript (ok. 40 tys. linii bez testów: app ≈ 30 tys., core ≈ 4,4 tys., db ≈ 2 tys., ui ≈ 3 tys.)
jednym serwerem w Rust. Oczekiwane zyski: mały i szybki proces (ważne na VPS Mikrus z ograniczoną pamięcią, gdzie
build Node.js już dziś kończy się OOM), bot liczony w prawdziwych wątkach zamiast osobnego procesu roboczego,
silne typowanie stanów gry, jeden statyczny plik binarny w obrazie Docker.

Kryterium sukcesu: **parytet funkcjonalny** z wersją Node.js (wszystkie epiki E01–E15 poza narzędziami deweloperskimi
specyficznymi dla Node), na nowym schemacie danych (bez migracji danych, patrz sekcja 7).

## 2. Zakres wejściowy (co trzeba odtworzyć)

| Obszar | Co jest dziś | Uwagi do portu |
| --- | --- | --- |
| Reguły gry | Gomoku swobodne 15×15, zegar 10 min, poddanie, rewanż, timeouty (lazy closing), kodowanie bitboardów, kody zaproszeń, nicki | Czysta logika — port 1:1, najłatwiejszy kawałek |
| Wariant „papier i ołówek” | Plansza 101×101, punkty za piątki/dziesiątki, rozliczanie (`paper-settler`) | Spec: `implemented/2026-11-02-paper-pencil-gomoku.md` |
| Bot | Poziomy 1–2, tablica transpozycji, Zobrist, wzorce, budżet czasu, arena, worker w osobnym procesie | Zysk wydajności z Rusta; arena służy jako test regresji siły gry |
| Konta | Rejestracja, logowanie, sesje w bazie (30 dni), reset i zmiana hasła, zmiana e-maila i nazwy, usunięcie konta z anonimizacją, potwierdzenie e-maila | argon2, CSRF (Origin + `X-Gomoku-Request`), limity prób |
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
| A4 | Frontend (**zatwierdzone**) | **Renderowanie po stronie serwera: Askama (szablony) + htmx**, mała ilość JS (plansza, zegar, polling) | Alternatywa: Leptos (pełny Rust/WASM) — większy zysk spójności, ale duże ryzyko i koszt przy 18 stronach, planszy 101×101 i design systemie. Alternatywa 2: zostawić React SPA i wymienić tylko backend (najmniejsze ryzyko, ale dwa stosy). Rozstrzygnięte w sekcji 7 (decyzja 1) |
| A5 | Style / design system | Tailwind CSS 4 (standalone CLI, bez Node w runtime) + **ponowne użycie `game-tokens.css`** i zasady z `packages/ui/docs/design-system.md` | Tokeny to czysty CSS — przenoszą się bez zmian; `check:tokens` do przeportowania jako skrypt |
| A6 | Czas rzeczywisty | Polling jak dziś (htmx `hx-trigger="every Ns"` / fetch), czas serwera do zegara | SSE można dodać później; poza zakresem parytetu |
| A7 | Hasła i sesje | `argon2` (brak hashy do zachowania, patrz sekcja 7), sesje w tabeli `sessions`, token w ciasteczku `httpOnly` (SHA-256 w bazie), `__Host-` w produkcji | Argon2 tylko jeśli właściciel zechce przeliczać hashe przy logowaniu |
| A8 | MCP | Oficjalny SDK Rusta (`rmcp`) lub własna cienka implementacja JSON-RPC nad Axum — **spike w fazie 7** | Trzeba sprawdzić aktualną wersję i wsparcie transportu streamable HTTP przed wyborem |
| A9 | Bot | `spawn_blocking`/wątek dedykowany z budżetem czasu; usunąć proces roboczy | Port algorytmu 1:1 najpierw, optymalizacje później; **arena (bot vs bot) jako test parytetu siły** |
| A10 | i18n | Katalogi PL/EN/FR/DE przeniesione do plików (JSON lub Fluent), ładowane przy starcie; test parzystości kluczy | PL nadal źródłem tekstów |
| A11 | Konfiguracja | Zmienne środowiskowe + `.env*` (`dotenvy`), ta sama lista zmiennych co `.env.example` | Zero zmian operacyjnych na VPS |
| A12 | Testy jednostkowe | `cargo test` + `cargo llvm-cov` z **progiem sensownym, nie sztywnym 100%** — patrz sekcja 7 (decyzja 4) | Właściciel odstąpił od „100% per plik” z oryginału |
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
- Schemat danych: **nowy, uproszczony** (decyzja 2), wyłącznie przez `sqlx migrate`; stare encje to materiał referencyjny.

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
| **F10. Wdrożenie** | Plan przełączenia (bez migracji danych), obraz z CI, deploy na Mikrus, rollback, dokumentacja operacyjna | Produkcja działa na Rust, Node wyłączony | 3–4 |

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
| Nowy schemat zamiast przejętego | Rozjazd z kontraktami API | Schemat projektujemy ze specyfikacji; testy integracyjne na kontraktach z `implemented/*` |
| Czas jednej osoby | Wydłużenie | Fazy dostarczalne niezależnie; Node działa do końca |
| Brak Rusta na maszynie | — | F0 zaczyna się od `rustup` (obecnie `cargo` nie jest zainstalowany) |

## 7. Decyzje właściciela (2026-11-21)

1. **Frontend:** Askama + htmx (A4 zatwierdzone). Spike w F0 (strona `/login` + plansza) potwierdza wybór przed F2.
2. **Dane:** na produkcji nie ma danych do zachowania, a schemat można przejąć lub nie. **Przyjmujemy start od zera:** nowy,
   uproszczony schemat w `sqlx migrate` (bez baseline'u z MikroORM), projektowany pod Rusta. Historia 19 migracji nie jest
   przenoszona; encje odtwarzamy ze specyfikacji i ze starych `packages/db/src/entities`. Zmiana nazw/typów jest dozwolona,
   o ile kontrakty API i zachowanie zostają. F10 upraszcza się: brak migracji danych, jest tylko przełączenie.
   Hasła nowe: **argon2** zamiast bcrypt (nie ma hashy do zachowania).
3. **Zakres:** pełny parytet (F0–F10), łącznie z pokazem, MCP, papierem i ołówkiem oraz DE.
4. **Pokrycie testami — „takie, które ma sens”:**
   - `gomoku-core` (reguły, zegar, bot, kody): **100% linii i gałęzi per plik**. To czysta logika, tu pokrycie ma wartość.
   - `gomoku-db` i `gomoku-web`: **próg globalny ≥ 85%** na crate (linie), z testami integracyjnymi na prawdziwym
     PostgreSQL liczonymi do pokrycia; bez testów wyłącznie dla pokrycia (gettery, glue, `main.rs`).
   - Wyłączenia (`main.rs`, kod generowany) jawnie wypisane w konfiguracji `llvm-cov` i uzasadnione w `AGENTS.md`.
   - Zakaz wyłączeń „po cichu” w kodzie (np. `#[coverage(off)]`) bez zgody właściciela. Progi podnosimy, nie obniżamy.
5. **Repozytorium:** osobne repo `Gomoku_rust` z własnym remote na GitHubie (prywatne, dopóki właściciel nie zdecyduje inaczej).
   Stare repo zostaje do końca F10, potem archiwum.
6. **Skille `om-*`:** uruchamiamy `om-setup-agent-pipeline` w tym repo **na końcu F0**, gdy istnieją komendy bramki
   walidacyjnej (`cargo fmt/clippy/test/llvm-cov`) i CI; razem z nim przenosimy `AGENTS.md`, `CODE_REVIEW.md`, `SDLC.md`.

## 8. Następny krok po zatwierdzeniu

F0: instalacja toolchaina, szkielet workspace, `AGENTS.md`, CI i `/api/health`, spike frontendu (decyzja 1), na końcu `om-setup-agent-pipeline` (decyzja 6).

## 9. Stan realizacji

### F0 (2026-11-21) — gotowe do przeglądu w PR `feat/f0-foundation`

- Zrobione: toolchain (`rustup`), workspace (`gomoku-core`, `gomoku-db`, `gomoku-web`), konfiguracja przez `from_lookup`,
  `/api/health` (200/503, szczegół błędu tylko w logu, `acquire_timeout` 3 s), SQLx + pierwsza migracja (`app_meta`),
  testy na PostgreSQL w Testcontainers, `scripts/coverage.sh` (progi z decyzji 4), `Dockerfile` (obraz 98 MB, healthcheck),
  `compose.yaml`, CI (`lint`, `test`+pokrycie, `docker`), `AGENTS.md`.
- **Spike frontendu (decyzja 1) potwierdzony:** `/login` i `/spike/board` (Askama + htmx) działają w prawdziwej przeglądarce
  (`agent-browser`): plansza 15×15, stawianie kamieni, zegar odświeżany przez htmx co 1 s z czasu serwera. Decyzja A4 zostaje.
- Wynik pokrycia: core 100%, db ≈ 86%, web ≈ 94% (linie).
- Nie zrobione w F0: `om-setup-agent-pipeline` (po zielonym CI na PR), `cargo audit` (osobny job informacyjny), Tailwind CLI
  (na razie ręczny `assets/app.css` z tokenami; decyzja przy pierwszym prawdziwym ekranie w F2).
- Strony `/spike/*` usunięte w F3.

### F1 (2026-11-21) — w PR `feat/f1-domain-core`

- Zrobione w `gomoku-core`: typy (`Seat`, `Color`, `EndReason`, …), reguły i `check_win`, plansza (bitboard 225 bitów, kodek dziennika ruchów),
  `apply_move`, zegar, poddanie, rewanż, kody zaproszeń (generowanie, normalizacja, wyciąganie z linku), nick gościa, kursor historii,
  krok przeglądu partii, stałe limitów i lobby, powiadomienie o powrocie, normalizacja e-maila i nazwy (`validators`).
- Testy: 83 jednostkowe (przypadki przeniesione z `packages/core/src/games/*.test.ts`), testy własnościowe `proptest` i 2000 partii
  seedowanych (`tests/win_equivalence.rs`), **parytet z Node** na plikach golden (`tests/golden/node.json`: 300 partii, 23 nicki, 11 kodów;
  generator `tests/golden/generate.ts` uruchamiany w starym repo). Pokrycie `gomoku-core`: 100% linii i funkcji.
- Odstępstwa od portu 1:1 (świadome): czas jako `i64` epoch ms zamiast `Date`; brak kodeka bitboard↔tekst `bit(225)` (nowy schemat,
  decyzja 2 — format zapisu zdecyduje F3); komunikaty błędów PL zostają w katalogu i18n (core zwraca kody błędów).
- Przeniesione do późniejszych faz (zależą od schematu, bota lub MCP): `board-view`, `finished-stats` (F8), `agent-*` (F7), `paper/*` (F5), `bot/*` (F6).

### F2a (2026-11-21) — w PR `feat/f2a-accounts`

- Zrobione: nowy schemat `users`/`sessions`/`rate_limit_buckets` (migracja 0002), rejestracja, logowanie, wylogowanie, sesje (cookie `httpOnly`,
  w produkcji `__Host-`, w bazie tylko SHA-256 tokenu), argon2id z atrapowym hashem przeciw wyliczaniu kont, CSRF (`Origin` + `X-Gomoku-Request`),
  limity prób (SQL, okno stałe), adres klienta za proxy (IPv6 /64), koperta API `{ok,data|error}` z kodami `fieldIssues`, limit ciała 64 KB,
  strony `/register`, `/login`, `/account` i nawigacja; fundament i18n (PL/EN/FR, `?lang=` → ciasteczko, łańcuch zapasowy, test parzystości kluczy),
  CLI `gomoku-db migrate`.
- Świadome odstępstwa: w produkcji brak `APP_ORIGIN` lub `TRUST_PROXY_HOPS` przerywa start procesu (w Node trasy odpowiadały 500, health działał);
  hasła argon2id (bez hashy do przeniesienia); wyjątek „sesja wygasła, ale partia trwa” (G5) dojdzie z grami w F3.
- Do F2b–F2d: zmiana i reset hasła, potwierdzenie e-maila i e-mail (plik/Brevo), zmiana nazwy i e-maila, usunięcie konta, sprzątanie wygasłych rekordów,
  administrator z `ADMIN_EMAIL`.
- Nie zrobione: automatyczne scenariusze przeglądarkowe w `tests/integration` (harness `agent-browser`); ścieżkę sprawdziłem ręcznie w przeglądarce
  (rejestracja z błędem pola, rejestracja, wylogowanie, logowanie ze złym i dobrym hasłem, przełączenie na FR).

### F2b (2026-11-21) — w PR `feat/f2b-passwords`

- Zrobione: zmiana hasła (`POST /api/auth/password`: limit 10 błędnych prób / 15 min na konto, reszta sesji kończona, unieważnienie linków resetu,
  warunkowy `UPDATE` przeciw wyścigowi), reset hasła (`/password-reset/request` zawsze `200`, praca po odpowiedzi w `TaskTracker`; limity: 10/h
  na adres klienta, 3/h na e-mail, dzienna pula `email-daily:reset`; `/password-reset/confirm` jednorazowy token 1 h, tylko potwierdzony e-mail,
  kończy wszystkie sesje), tabela `password_reset_tokens` (migracja 0003), strony `/forgot-password`, `/reset-password` (`Referrer-Policy:
  no-referrer`) i formularz zmiany hasła na `/account`, teksty PL/EN/FR.
- Warstwa poczty (potrzebna resetowi, użyje jej F2c): port `EmailSender`, transporty `file` / `brevo` (reqwest + rustls, bez przekierowań, kody błędów
  bez klucza i adresu) / brak, fabryka z degradacją, szablon e-maila (PL), `Secret` w konfiguracji.
- Znalezione testami: `tokio::fs::File` bez `flush()` gubi dane przy zamknięciu (transport plikowy).
- Odstępstwa: link w e-mailu bez `APP_ORIGIN` wskazuje `http://localhost:3000` (jak w Node), więc lokalnie ustaw `APP_ORIGIN`.
- Zostaje na F2c: potwierdzenie e-maila (pula 70% dziennego limitu), zmiana e-maila i nazwy; F2d: usunięcie konta, sprzątanie, administrator.

### F2c (2026-11-21) — w PR `feat/f2c-email-and-name`

- Zrobione: potwierdzenie e-maila (wysyłka po rejestracji bez wpływu na `201`; ponowna wysyłka 3/h na konto i 10/h na adres; pula dzienna 70% z minimum 1;
  potwierdzenie po kliknięciu przycisku, GET niczego nie zmienia; `next` przez `safe_next_path`), zmiana e-maila (hasło we wspólnej puli prób ze zmianą hasła,
  zawsze `202`, link na nowy adres, kończy wszystkie sesje, powiadomienie na stary adres tylko gdy był potwierdzony), zmiana nazwy gracza (raz na dobę,
  warunkowy `UPDATE`, stara nazwa zwalnia się), baner przypomnienia (zamknięcie pamiętane 7 dni w `localStorage`), strony `/verify-email`,
  `/confirm-email-change`, formularze na `/account`, tabele `email_verification_tokens` i `email_change_tokens` (migracja 0004), teksty PL/EN/FR.
- Zmiana i reset hasła unieważniają teraz także linki zmiany e-maila (jak `invalidateResetTokens` w Node).
- `ADMIN_EMAIL` (opcjonalny do F8): nikt nie rejestruje się na tym adresie, adres administratora nie zmienia się i nie można na niego zmienić cudzego.
- Odstępstwo: limity prób liczy czas bazy (`now()`), nie zegar aplikacji; testy rozdzielają adresy klientów (`TRUST_PROXY_HOPS=1` + `X-Forwarded-For`).
- Zostaje na F2d: usunięcie konta z anonimizacją (wymaga gier z F3 do pełnej reguły „partia w toku”), sprzątanie wygasłych rekordów, konto administratora (CLI).

### F2d (2026-11-21) — w PR `feat/f2d-account-lifecycle` (kończy F2)

- Zrobione: usunięcie konta (`POST /api/account/delete`: hasło + jawne potwierdzenie, limit 10 błędnych prób / 15 min na konto — błędne hasło zostaje
  policzone, reszta wyników oddaje próbę, sukces zeruje licznik; administrator chroniony `403 ADMIN_ACCOUNT_PROTECTED` przed sprawdzeniem hasła; jedna
  transakcja z blokadą wiersza i warunkiem na zweryfikowany hash; ciasteczko kasowane po sukcesie i gdy konto już zniknęło), sekcja na `/account`,
  `gomoku-db cleanup` (reguły: sesje po 1 h, liczniki po 48 h, linki po 24 h, niepotwierdzone konta po 14 dniach, z pominięciem konta `ADMIN_EMAIL`;
  paczki po 1000 z `FOR UPDATE SKIP LOCKED`, budżet 15 s na regułę, błąd jednej reguły nie zatrzymuje reszty, test strażnik: każda tabela z kolumną
  wygaśnięcia ma regułę), `gomoku-admin create` (tworzy lub przejmuje konto administratora: hasło tylko z terminala lub jednej linii stdin, `--name`, `--yes`).
- Świadomie odłożone do F3 (wymaga gier): reguła „partia w toku blokuje usunięcie”, anonimizacja partii zakończonych („Usunięty gracz”), usuwanie
  oczekujących partii i czatu przy usunięciu konta, reguły sprzątania gier/czatu/gości, warunek „bez żadnej partii” dla niepotwierdzonych kont.
- Odstępstwo: `ADMIN_EMAIL` pozostaje opcjonalny (w Node wymagany w produkcji); zrobimy go wymaganym razem z panelem administratora (F8).
- **F2 zamknięte:** konta, sesje, hasła, e-maile, nazwa, usunięcie, sprzątanie, administrator, i18n (fundament).

### F3 — Gra 1:1 (zrobione, jeden PR)

- Rdzeń (`gomoku-core::game`): losowanie stron, tokeny gościa, wygasanie zaproszeń, rewanż, widok planszy, odtwarzanie partii.
- Baza (migracja 0005): `games` (ruchy jako `bytea`), `game_players`, `invitations`, `lobby_seekers`, `game_pair_counters`; przejścia leniwe (wygaśnięcie, brak otwarcia, timeouty) i warunkowe UPDATE-y z blokadami wierszy; reguła G5; sprzątanie rozszerzone do 13 reguł; usunięcie konta rozlicza partie.
- Web: `/api/games*`, `/api/invitations/{code}/accept`, `/api/lobby*`, `/api/history`; strony `/play /join /invite/{code} /games/{id} /games/{id}/review /lobby /history`; skrypty `game-room.js`, `lobby.js`, `history.js`, `review.js`.
- Weryfikacja: testy HTTP/DB (w tym wyścigi), `pages_games.rs`, ręcznie w przeglądarce (dwie sesje: zaproszenie, partia do pięciu, rewanż, poddanie, lobby, historia, przegląd, gość). Pokrycie: core 100%, db 96%, web 96% (`scripts/coverage.sh`).
- Nie sprawdzono: dotyk (wybór i potwierdzenie ruchu) i widok mobilny w prawdziwej przeglądarce; scenariusz integracyjny agent-browser nie jest jeszcze zautomatyzowany.

### F4 — Czat, partie publiczne, widzowie, QR (zrobione, jeden PR)

- Kontrakt: `.ai/docs/2026-11-22-f4-contract.md` (wyciągnięty z implementacji w Node).
- Rdzeń: `gomoku-core::chat` (normalizacja wiadomości NFKC, odrzucanie adresów, stan czatu, odstęp), cookie widza.
- Baza (migracja 0006): `games.is_public` / `audience_closed_at`, `game_players.chat_enabled`, `game_spectators`, `chat_messages`; czat na zegarze bazy
  z blokadą doradczą na grę (numeracja `seq` bez dziur); widzowie: limit 20, obecność 15 s, dostęp 1 h; wyłącznik publiczności; kod QR (SVG w aplikacji).
  Sprzątanie: 15 reguł (`chatMessages`, `gameSpectators` przed regułami gier); usunięcie konta kasuje rozmowy jego partii.
- Web: `/api/games/{id}/chat*`, `/spectate*`, `/audience/close`, `/qr`, `/api/invitations/{code}/spectate`; strony: pokój widza, ekran „publiczność zamknięta”, przegląd po kodzie
  (`/games/{id}/review?code=`), widok `watch` zaproszenia; skrypty `chat.js`, `spectator-room.js`.
- Odstępstwa: czat widza nie przedłuża dostępu (tak jak w Node: tylko odczyt stanu); klient nie powtarza po stronie przeglądarki wszystkich reguł normalizacji
  (adresy, znaki niewidoczne sprawdza serwer, klient liczy długość i pusty tekst); nowy klucz `chat.spectatorLabel` zamiast zaszytego „Widz ·”.
- Weryfikacja: 25 testów HTTP/DB (w tym wyścigi), ręcznie w przeglądarce (publiczna partia, czat gracz–gracz–widz, przełącznik, wyłącznik publiczności, QR).
  Pokrycie: core 100%, db 96%, web 96%.
- Nie sprawdzono: dotyk/mobile, czytnik ekranu (live regions czatu), zachowanie przy bardzo dużej liczbie wiadomości w przeglądarce.

### F5 — Papier i ołówek (zrobione, jeden PR)

- Kontrakt: `.ai/docs/2026-11-23-f5-contract.md`. Rdzeń: `gomoku-core::paper` (arkusz 101×101, reguła „najwyżej 3 pola od kamienia”, punkty za piątki
  przeliczane z kamieni, ruch automatu, leniwa oś czasu `resolve_paper_clock`, ustawienia gry) — 100% pokrycia, w tym testy własnościowe punktów.
- Baza (migracja 0007, tylko w przód): `games.variant/time_control_ms`, `paper_game_states`, `paper_moves`, ustawienia w `lobby_seekers`; rozliczanie
  czasu i ruch automatu pod blokadą wiersza gry; sprzątanie ma jeden wspólny wzór terminu partii (także dla wariantu papierowego).
- Web: `variant`/`timeControlMs` przy tworzeniu partii i w lobby, widok `paper` (kamienie, punkty, piątki, automat), błędy `FIRST_MOVE_CENTER`/`MOVE_TOO_FAR`/
  `NOT_YOUR_TURN (auto_turn)`; granice ruchu klasycznego sprawdzane w serwisie po ustaleniu partii (kontrakt klasyczny bez zmian).
  UI: `paper-board.js` (okno na arkusz, powiększanie, przesuwanie, piątki jako linie), wybór wariantu i czasu (`settings.js`), historia, przegląd (notatka), widz.
- Odstępstwa: losowość automatu bez wstrzykiwania (CSPRNG systemu; testy sprawdzają własności, nie konkretną komórkę); kolumna `last_move_at` pominięta
  (w Node tylko zapis); logika widoku planszy tylko w JS (decyzja 11.3-2 kontraktu).
- Nie zrobione / nie sprawdzono: szczypanie (pinch) i zapamiętanie powiększenia w `localStorage`, tryb wysokiego kontrastu, ostrzeżenie o nieznanym wariancie,
  pliki „golden” z Node dla punktacji i automatu, dotyk/mobile w prawdziwej przeglądarce (układ telefonu nieprzejrzany wizualnie).
- Weryfikacja: 13 testów HTTP/DB (reguły, punkty, koniec na punkty, automat, poddanie, rewanż, lobby, historia, widz, sprzątanie), ręcznie w przeglądarce
  (partia 1 min i 5 min: kamienie, punkty 1:1, poddanie, historia, przegląd). Pokrycie: core 100%, db 96%, web 96%.
- Środowisko: testy zostawiały kontenery PostgreSQL — `scripts/coverage.sh` sprząta teraz te, które sam utworzył.

### F6 — Bot (zrobione, jeden PR)

- Kontrakt: `.ai/docs/2026-11-22-f6-contract.md`. Rdzeń: `gomoku-core::bot` (PRNG mulberry32 + ziarno FNV-1a, Zobrist, plansza wzorców z przyrostowymi
  wynikami, tablica transpozycji, poziom 1, poziom 2: negamax + alfa-beta + pogłębianie iteracyjne, dyspozytor, profil pamięci, budżet na ruch,
  walidacja pól `opponent`/`botLevel`, arena R17 + binarka `bot_arena`).
- **Parytet z Node co do ruchu:** złota partia (seed 1, 86 ruchów) i pełna arena 100 partii dają wyniki identyczne z TS (poziom 2: czarne 50/50, białe
  47/50, razem 0,97, remisy w partiach 8 i 92, porażki w 18 i 28; te same długości partii). Arena w Ruście: 110 s (release) wobec 225 s w TS.
- Baza (migracja 0008): `game_players.bot_level/bot_memory_mb/bot_depth_cap`, rodzaj `bot`, pochodzenie `bot` (bez `show` do fazy pokazu), ograniczenia
  (bot zawsze na miejscu 2, bez konta, parametry tylko dla poziomu 2). Partia z botem jest od razu aktywna i prywatna, bot ma `seen_at` (zegar rusza
  przy pierwszym odczycie człowieka). Bot gra tą samą ścieżką `play_move` co człowiek; sprzątanie traktuje partie gość–bot jak „tylko goście”.
- Web: `BOT_*` w konfiguracji; admisja (`503 BOT_UNAVAILABLE` + `Retry-After: 60`, jeden tekst dla każdej przyczyny), bramka obliczeń (jedno wyszukiwanie
  naraz, FIFO), silnik w osobnym wątku z dużym stosem, licznik awarii (po 3 poziom 1 do końca), usługa tury; planowanie po ruchu człowieka i przy każdym
  odczycie, gdy na ruchu jest bot (odzyskiwanie leniwe). Brak czatu, rewanżu i flagi publicznej; historia i przegląd nazywają bota siłą.
- UI: wybór przeciwnika i poziomu (`settings.js`), etykiety przycisków, „Bot myśli…”, odpytywanie co 750 ms, karta „Zagraj ponownie z botem”.
  Znaleziony przy okazji błąd F5: `last` poza zasięgiem w `game-room.js` psuł ekran po ruchu przeciwnika w partii klasycznej.
- Odstępstwa: pułap czasu egzekwowany współpracą silnika (co 1024 węzły), bez zabijania wątku; sonda pamięci czyta cgroup v2 i `/proc/meminfo`
  (mniejsza z wartości); stos wątku wyszukiwania 16 MiB; limity czasu bez zmian (Rust jest szybszy na węzeł, więc głębokość w produkcji będzie większa).
- Nie zrobione / nie sprawdzono: wydajność na docelowym hoście i sonda pamięci w kontenerze produkcyjnym, tryb pokazu (F8), dotyk/mobile.
