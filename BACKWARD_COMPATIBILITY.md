# Chronione kontrakty

Repo odtwarza produkt z `~/Development/ai_techleaders_gomoku` (spec: `.ai/specs/product-brief.md`, kontrakty: `.ai/specs/implemented/*`).
Zmiana poniższych powierzchni wymaga jawnej zgody właściciela i notatki w PR („Odstępstwa”). Dopóki nie wdrożono wersji Rust na produkcję, powierzchnie
oznaczone „kontrakt z Node” są **wymaganiem parytetu**, a nie zobowiązaniem wobec użytkowników.

| Powierzchnia | Co jest naruszeniem | Wymagana ścieżka |
| --- | --- | --- |
| Trasy HTTP `/api/*` | zmiana ścieżki, metody, statusu, kodu błędu (`code`) lub kształtu koperty `{ok,data}` / `{ok:false,error}` względem kontraktu z Node | zgoda właściciela, aktualizacja specyfikacji i testów integracyjnych |
| Nagłówki bezpieczeństwa (`Origin`, `X-Gomoku-Request`, `Retry-After`, ciasteczko sesji `__Host-gomoku_session` w produkcji) | zmiana nazwy lub semantyki | jak wyżej; klienci MCP i przeglądarka zależą od nich |
| Narzędzia MCP (`/api/mcp`: nazwy narzędzi, parametry, kształt wyników, tożsamość klienta) | zmiana nazwy/parametru/odpowiedzi | wersjonowanie narzędzia lub okres przejściowy; klienci agentów są zewnętrzni |
| Reguły gry (15×15, zegar 10 min, wariant papier i ołówek 101×101, punktacja) | jakakolwiek zmiana wyniku partii dla tych samych ruchów | nie dopuszczalne bez zmiany `product-brief.md` przez właściciela |
| Kody zaproszeń, formaty linków (`/invite/<kod>`, QR) | zmiana alfabetu, długości, formatu | zgoda właściciela; stare linki muszą działać |
| Schemat bazy (`crates/gomoku-db/migrations`) | edycja zastosowanej migracji; usunięcie kolumny bez dwuetapowej migracji | tylko nowa migracja; migracje w górę i w dół sprawdzone na PostgreSQL (Testcontainers) |
| Zmienne środowiskowe (`.env.example`) | zmiana nazwy, znaczenia lub domyślnej wartości | notatka w README i `.env.example`; stara nazwa jako alias przez jedno wydanie |
| Katalogi tłumaczeń (PL/EN/FR/DE) | usunięcie klucza używanego przez ekran; rozjazd zestawu kluczy między językami | test parzystości kluczy musi przechodzić |
| Obraz Docker (`/api/health`, port, `BIND_ADDR`, `ASSETS_DIR`, użytkownik niebędący rootem) | zmiana sondy zdrowia lub portu | aktualizacja `compose.prod.yaml` i dokumentacji wdrożenia |
| Wiersze poleceń (`gomoku-db` CLI, gdy powstanie) | zmiana flag lub kodów wyjścia | zgoda właściciela, opis w README |

Reguła ogólna: nowa funkcja może rozszerzać powierzchnię (nowe pola opcjonalne, nowe trasy); nie wolno zawężać ani zmieniać istniejącej bez ścieżki z tabeli.
