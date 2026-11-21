#!/usr/bin/env bash
# Coverage gates (decision 4 in .ai/specs/2026-11-21-rust-rewrite-plan.md):
#   gomoku-core: 100% lines and functions (pure logic);
#   gomoku-db, gomoku-web: >= 85% lines per crate, main.rs (process wiring) excluded.
# gomoku-db tests need Docker (Testcontainers). Use `scripts/coverage.sh core web` to skip it.
set -euo pipefail
cd "$(dirname "$0")/.."

targets=("$@")
[ ${#targets[@]} -eq 0 ] && targets=(core db web)

for t in "${targets[@]}"; do
  case "$t" in
    core) cargo llvm-cov -p gomoku-core --fail-under-lines 100 --fail-under-functions 100 ;;
    db)   cargo llvm-cov -p gomoku-db --fail-under-lines 85 ;;
    web)  cargo llvm-cov -p gomoku-web --ignore-filename-regex 'main\.rs' --fail-under-lines 85 ;;
    *) echo "unknown target: $t" >&2; exit 2 ;;
  esac
done
