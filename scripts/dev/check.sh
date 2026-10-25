#!/usr/bin/env bash
# The gates every change must pass before commit (CI runs the same):
#   web UI (format, types, lint, tests, build) -> format -> build (incl. the ductus binary, with
#   the UI embedded) -> the API document the UI is typed from is current -> clippy -D warnings
#   -> all tests.
#
# Needs Node 24 for the UI (in WSL: a per-user install in ~/.local/node is picked up).
#
# Run from WSL/Linux:   bash scripts/dev/check.sh
# From Windows:         wsl -d Ubuntu -- bash /mnt/d/Projects/integration_engine/ductus/scripts/dev/check.sh
#
# Needs Postgres for the store tests (skipped if DUCTUS_TEST_DATABASE_URL is unset).
# SQL Server tests run when the local container exists (create it once: scripts/dev/sqlserver.sh).
set -euo pipefail
cd "$(dirname "$0")/../.."
[ -f ~/.cargo/env ] && source ~/.cargo/env
# Build outside /mnt (Windows drives are slow from WSL).
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$HOME/target}
source scripts/dev/db.sh
export DUCTUS_TEST_DATABASE_URL=${DUCTUS_TEST_DATABASE_URL:-$DUCTUS_DATABASE_URL}
if [ -z "${DUCTUS_TEST_SQLSERVER:-}" ]; then
  if { docker inspect ductus-mssql || sudo -n docker inspect ductus-mssql; } >/dev/null 2>&1; then
    DUCTUS_TEST_SQLSERVER=$(bash scripts/dev/sqlserver.sh)
    DUCTUS_TEST_SQLSERVER_TLS=$(bash scripts/dev/sqlserver.sh dir)
    export DUCTUS_TEST_SQLSERVER DUCTUS_TEST_SQLSERVER_TLS
  else
    echo "(SQL Server tests skipped: set one up with scripts/dev/sqlserver.sh)"
  fi
fi

[ -x "$HOME/.local/node/bin/node" ] && export PATH="$HOME/.local/node/bin:$PATH"
command -v npm >/dev/null || { echo "Node 24 is needed for the web UI (nodejs.org)"; exit 1; }
echo "== ui"
(
  cd ui
  [ -d node_modules ] || npm ci --no-audit --no-fund >/dev/null
  npm run -s format >/dev/null
  npm run -s check >/tmp/ductus-ui-check.log 2>&1 || { cat /tmp/ductus-ui-check.log; echo "UI CHECKS FAILED"; exit 1; }
  sed 's/\x1b\[[0-9;]*m//g' /tmp/ductus-ui-check.log | grep -E '^ *Tests +[0-9]+ passed' || true
  npm run -s build 2>&1 | grep -E 'error|UI size|over the' || true
  [ -f dist/index.html ] || { echo "UI BUILD FAILED"; exit 1; }
)
export DUCTUS_REQUIRE_UI=1
echo "== fmt";    cargo fmt --all
echo "== build";  cargo build --release 2>&1 | grep -E '^(warning|error)|-->' || true
# The UI's API types are generated from the API document; a changed API means new types.
if ! "$CARGO_TARGET_DIR/release/ductus" api-spec | cmp -s - ui/openapi.json; then
  "$CARGO_TARGET_DIR/release/ductus" api-spec >ui/openapi.json
  (cd ui && npm run -s api-types >/dev/null)
  echo "The API changed: updated ui/openapi.json and ui/src/api/schema.d.ts. Run check.sh again."
  exit 1
fi
echo "== clippy"; cargo clippy --release --all-targets -- -D warnings 2>&1 | grep -E '^(warning|error)|-->' || true
echo "== test"
if cargo test --release -q 2>&1 | tee /tmp/ductus-test.log | grep -E 'test result: ok\. [1-9]|FAILED|panicked'; then :; fi
if grep -qE 'FAILED|panicked|error\[' /tmp/ductus-test.log; then
  echo "TESTS FAILED (full log: /tmp/ductus-test.log)"
  exit 1
fi
cargo clippy --release --all-targets -q -- -D warnings >/dev/null 2>&1 || { echo "CLIPPY FAILED"; exit 1; }
echo "ALL CHECKS PASSED"

# Build outputs only grow: cargo never deletes old artifacts (dependency bumps, Rust updates, a
# worktree built into the same folder). Said here, with what to do over 30 GB.
size_mb=$(du -sm "$CARGO_TARGET_DIR" 2>/dev/null | cut -f1)
echo "build outputs: $CARGO_TARGET_DIR $(awk "BEGIN { printf \"%.1f\", ${size_mb:-0} / 1024 }") GB"
if [ "${size_mb:-0}" -gt $((30 * 1024)) ]; then
  echo "  over 30 GB: run \`cargo clean\` (the next check.sh builds everything again); in WSL, then compact its disk (CLAUDE.md, \"WSL's disk lives on D:\")"
fi
