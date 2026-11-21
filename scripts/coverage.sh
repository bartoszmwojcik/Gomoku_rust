#!/usr/bin/env bash
# Coverage gates (decision 4 in .ai/specs/2026-11-21-rust-rewrite-plan.md), measured over the WHOLE workspace run, so
# integration tests on a real PostgreSQL count towards the crate they exercise:
#   gomoku-core: 100% lines and functions (pure logic);
#   gomoku-db, gomoku-web: >= 85% lines per crate; process entry points (main.rs, src/bin/) are excluded.
# Needs Docker (Testcontainers) and python3.
set -euo pipefail
cd "$(dirname "$0")/.."

mkdir -p target
cargo llvm-cov clean --workspace >/dev/null
cargo llvm-cov --workspace --ignore-filename-regex '(main\.rs|src/bin/)' --json --summary-only --output-path target/coverage.json
python3 scripts/check_coverage.py target/coverage.json
