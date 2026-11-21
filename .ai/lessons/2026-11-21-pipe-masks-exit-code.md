# Potok `| tail` ukrywa kod wyjścia bramki

- **Problem:** w F1 `cargo clippy … | tail -2 && git commit` zatwierdził i wypchnął kod mimo czerwonego clippy, bo kodem wyjścia całego
  polecenia był kod `tail`, nie `cargo`.
- **Kontekst:** PR #3 (F1), poprawka poszła drugim commitem; CI w PR było zielone dopiero po niej.
- **Reguła:** bramkę (fmt, clippy, testy, pokrycie) uruchamiaj bez potoku albo sprawdzaj `${PIPESTATUS[0]}` / `set -o pipefail`;
  commit dopiero po jawnym zielonym wyniku. Do wglądu w wynik używaj `| tee`/`grep` po zapisaniu kodu wyjścia.
- **Powiązane:** `scripts/coverage.sh` (używa `set -euo pipefail`).
