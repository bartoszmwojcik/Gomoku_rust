# Zasady przeglądu kodu

Plik czytany automatycznie przez `om-code-review` i `om-auto-review-pr`. Uzupełnia bramkę walidacyjną z `.ai/agentic.config.json`.
Stack i konwencje: `AGENTS.md`; plan i decyzje właściciela: `.ai/specs/2026-11-21-rust-rewrite-plan.md`.

## Priorytety

1. **Poprawność** — reguły gry, zegar, timeouty i rozliczenie wyniku muszą zgadzać się z wersją referencyjną (`~/Development/ai_techleaders_gomoku`). Przypadki testowe przenoszone 1:1; rozbieżność to bug, nie „ulepszenie”.
2. **Bezpieczeństwo** — sesje, CSRF (`Origin` + `X-Gomoku-Request`), limity prób, brak ujawniania istnienia kont, sekrety tylko w środowisku, szczegóły błędów (DSN, SQL) tylko w logu, nigdy w odpowiedzi.
3. **Kontrakty** — kody błędów, statusy i koperta `{ok,data}` / `{ok:false,error:{code,message}}` z `BACKWARD_COMPATIBILITY.md`.
4. **Testowalność i pokrycie** — progi z `scripts/coverage.sh` (core 100%, db/web ≥ 85%); bez nowych wyłączeń pokrycia.

## Kontrole specyficzne dla repozytorium

- Brak `unwrap()`/`expect()`/`panic!` poza testami; brak `unsafe`. Błędy mają typ (`thiserror`), nie `String`, poza granicą traitów testowych.
- Zapytania SQL tylko przez SQLx w `gomoku-db`; zapis w transakcji używa wyłącznie obiektu transakcji, nie puli. Zmiany schematu tylko nową migracją; zastosowanych migracji nie edytujemy.
- `gomoku-core` nie zależy od Axum, SQLx ani Tokio (czysta logika); kierunek zależności `web → core, db`, `db → core`.
- Granice (czas, baza, e-mail, losowość) wstrzykiwane przez trait/closure w `AppState`; w logice brak `SystemTime::now()` i `rand` wprost.
- Handler nie ufa wejściu: walidacja kształtu i długości przed użyciem; limity rozmiaru ciała żądania.
- Odpowiedzi nie zdradzają, czy konto istnieje (logowanie, rejestracja, reset, zmiana e-maila).
- Kod blokujący CPU (bot) nie działa na wątku async — `spawn_blocking`/dedykowany wątek z budżetem czasu.
- UI: wartości tylko z tokenów (`assets/game-tokens.css`), teksty z katalogu PL (+ EN/FR w tym samym PR), brak literałów w szablonach ekranów graczy, elementy testowe z `data-testid`.
- Nowa strona lub endpoint ma scenariusz integracyjny; nowy kod domenowy ma testy własnościowe tam, gdzie to ma sens (reguły, zegar, punktacja).
- Docker/CI: obraz buduje się w CI, nie na VPS; akcje w workflow przypięte do SHA z tagiem w komentarzu.

## Waga uwag

| Poziom | Przykłady | Skutek |
| --- | --- | --- |
| blocker | wyciek sekretu/szczegółów błędu, błędny wynik partii, brak autoryzacji, utrata danych, migracja edytowana po zastosowaniu | request changes |
| major | zepsuty kontrakt API, brak testów nowego zachowania, `unwrap` w kodzie produkcyjnym, blokowanie wątku async | request changes |
| minor | nazewnictwo, duplikacja, drobna czytelność | zgłoś, nie blokuje |
| nit | styl poza `rustfmt` | opcjonalnie |
