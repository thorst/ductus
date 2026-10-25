#!/usr/bin/env bash
# What verified TLS to SQL Server costs (0.6.4.3), against the test SQL Server
# (scripts/dev/sqlserver.sh: a certificate from a CA of its own):
#   1. Opening a connection as db()'s pool does (PRELOGIN, TLS, login, a first query), one after
#      another: `trust_server_certificate` (nothing checked) against verified (`ca:`), and TDS 8
#      strict (verified). The modes alternate each round.
#   2. Doctor's certificate check of it (what the engine's own check does every 6 hours per
#      verified SQL Server): through PRELOGIN, and strict.
#
#   ROUNDS=3 COUNT=100 bash scripts/bench-mssqltls.sh
#
# Not a pass/fail demo: it prints what it measured. Needs Docker and the tls-bench example (built
# if missing).
set -euo pipefail
DEV=$(realpath "$(dirname "${BASH_SOURCE[0]}")/dev")
BIN=$(realpath "${BIN:-${CARGO_TARGET_DIR:-target}/release/ductus}")
TB=${TLS_BENCH:-$(dirname "$BIN")/examples/tls-bench}
if [ ! -x "$TB" ]; then
  (cd "$DEV/../.." && cargo build --release -q -p ductus-runtime --example tls-bench)
fi
ROUNDS=${ROUNDS:-3}
COUNT=${COUNT:-100}
TARGET=$(bash "$DEV/sqlserver.sh")
CA=$(bash "$DEV/sqlserver.sh" dir)/ca.pem

echo "== opening a connection (PRELOGIN, TLS, login, SELECT 1), $COUNT one after another, ms"
for round in $(seq 1 "$ROUNDS"); do
  echo "round $round"
  echo "   trust_server_certificate  $("$TB" mssql-connect "$TARGET" "$COUNT")"
  echo "   verified (ca:)            $("$TB" mssql-connect "$TARGET" "$COUNT" --ca "$CA")"
  echo "   strict, verified          $("$TB" mssql-connect "$TARGET" "$COUNT" --ca "$CA" --strict)"
done

echo "== doctor's certificate check, $((COUNT / 2)) one after another, ms"
for round in $(seq 1 "$ROUNDS"); do
  echo "round $round"
  echo "   through PRELOGIN          $("$TB" mssql-check "$TARGET" $((COUNT / 2)) --ca "$CA")"
  echo "   strict                    $("$TB" mssql-check "$TARGET" $((COUNT / 2)) --ca "$CA" --strict)"
done
