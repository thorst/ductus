#!/usr/bin/env bash
# The gates every change must pass before commit (CI runs the same):
#   format -> build (incl. the ductus binary) -> clippy -D warnings -> all tests.
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
export DUCTUS_TEST_DATABASE_URL=${DUCTUS_TEST_DATABASE_URL:-postgres://todd:ductus@localhost/ductus_dev}
if [ -z "${DUCTUS_TEST_SQLSERVER:-}" ]; then
  if { docker inspect ductus-mssql || sudo -n docker inspect ductus-mssql; } >/dev/null 2>&1; then
    DUCTUS_TEST_SQLSERVER=$(bash scripts/dev/sqlserver.sh)
    export DUCTUS_TEST_SQLSERVER
  else
    echo "(SQL Server tests skipped: set one up with scripts/dev/sqlserver.sh)"
  fi
fi

echo "== fmt";    cargo fmt --all
echo "== build";  cargo build --release 2>&1 | grep -E '^(warning|error)|-->' || true
echo "== clippy"; cargo clippy --release --all-targets -- -D warnings 2>&1 | grep -E '^(warning|error)|-->' || true
echo "== test"
if cargo test --release -q 2>&1 | tee /tmp/ductus-test.log | grep -E 'test result: ok\. [1-9]|FAILED|panicked'; then :; fi
if grep -qE 'FAILED|panicked|error\[' /tmp/ductus-test.log; then
  echo "TESTS FAILED (full log: /tmp/ductus-test.log)"
  exit 1
fi
cargo clippy --release --all-targets -q -- -D warnings >/dev/null 2>&1 || { echo "CLIPPY FAILED"; exit 1; }
echo "ALL CHECKS PASSED"
