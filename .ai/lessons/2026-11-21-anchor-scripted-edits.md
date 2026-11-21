# Edycje skryptem muszą mieć jednoznaczną kotwicę i sprawdzony diff

- **Problem:** w F2 skrypt Pythona zastąpił „od pierwszego wystąpienia `let unknown = app`” i wyciął kilka testów (wystąpienie było w innym
  teście niż zamierzony).
- **Kontekst:** `crates/gomoku-web/tests/auth.rs`; testy trzeba było odtworzyć z pamięci rozmowy.
- **Reguła:** przy edycji zakresu tekstu skryptem kotwicz na tekście, który występuje **dokładnie raz** (asercja `count == 1`), a po edycji
  sprawdź `git diff --stat` i liczbę testów. Do małych zmian używaj narzędzia Edit (wymaga unikalnego dopasowania).
