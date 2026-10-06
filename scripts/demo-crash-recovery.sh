#!/usr/bin/env bash
# v0.1 demo: stream synthetic ADT through Ductus, hard-kill the engine mid-stream,
# restart it, and verify nothing was lost and per-patient order held.
set -euo pipefail

BIN=${BIN:-${CARGO_TARGET_DIR:-target}/release/ductus}
COUNT=${COUNT:-20000}
PATIENTS=${PATIENTS:-500}
KILL_AFTER=${KILL_AFTER:-5}
LOGS=$(mktemp -d)
ROOT=$(cd "$(dirname "$0")/../examples/adt-relay" && pwd)

psql -q "${DUCTUS_DATABASE_URL:-postgres://todd:ductus@localhost/ductus_dev}" -c "TRUNCATE messages, dedupe_keys" 2>/dev/null || true

echo "== starting sink, engine, generator ($COUNT messages, $PATIENTS patients)"
"$BIN" sink --port 6662 >"$LOGS/sink.log" 2>&1 &
SINK=$!
"$BIN" run --root "$ROOT" >"$LOGS/run1.log" 2>&1 &
ENGINE=$!
sleep 2
"$BIN" gen --to 127.0.0.1:6661 --count "$COUNT" --patients "$PATIENTS" >"$LOGS/gen.log" 2>&1 &
GEN=$!

sleep "$KILL_AFTER"
echo "== kill -9 engine (pid $ENGINE) mid-stream"
kill -9 "$ENGINE"
"$BIN" status
sleep 3

echo "== restarting engine"
"$BIN" run --root "$ROOT" >"$LOGS/run2.log" 2>&1 &
ENGINE=$!

wait "$GEN"
echo "== generator: $(tail -1 "$LOGS/gen.log")"

echo "== waiting for the queue to drain"
for _ in $(seq 1 120); do
  pending=$("$BIN" status | awk '$2 == "pending" {print $3}')
  [ -z "$pending" ] && break
  sleep 1
done
sleep 2

kill -INT "$SINK"; wait "$SINK" || true
kill -INT "$ENGINE"; wait "$ENGINE" || true

echo "== engine status"
"$BIN" status
echo "== sink $(tail -1 "$LOGS/sink.log")"
echo "logs: $LOGS"
