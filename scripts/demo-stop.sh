#!/usr/bin/env bash
# A clean stop (SIGTERM: systemd, Docker, Kubernetes; Ctrl-C) against a hard kill, mid-stream.
# Each phase streams ADT through the example relay to a sink, stops the engine part way, starts
# it again and waits until everything is delivered:
#   1. kill -9 (what SIGTERM did before the engine handled it): nothing lost, some sent twice
#      (reported, not required: it depends on where each worker was);
#   2. SIGTERM: nothing lost, nothing sent twice, exit 0;
#   3. SIGTERM while the receiver is down (the worker in a retry back-off): stops at once.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/dev/db.sh"

BIN=${BIN:-${CARGO_TARGET_DIR:-target}/release/ductus}
DB=$DUCTUS_DATABASE_URL
COUNT=${COUNT:-6000}
# As fast as ACKs come back over 8 connections: ingest outruns delivery, so workers are mid-batch (finished
# messages not saved yet) when the engine stops. That's when a hard kill re-sends.
RATE=${RATE:-0}
STOP_AFTER=${STOP_AFTER:-1}
LOGS=$(mktemp -d)
ROOT=$(cd "$(dirname "$0")/../examples/adt-relay" && pwd)
ENGINE="" SINK="" GEN=""
trap 'kill -9 $ENGINE $SINK $GEN 2>/dev/null || true' EXIT

fail=""
check() { # check <name> <condition...>: record a failure instead of stopping at the first
  local name=$1
  shift
  if ! eval "$@"; then fail+=" $name"; fi
}

start_engine() {
  "$BIN" run --root "$ROOT" >>"$LOGS/$1.log" 2>&1 &
  ENGINE=$!
  for _ in $(seq 1 50); do
    grep 'started' "$LOGS/$1.log" >/dev/null && return
    sleep 0.1
  done
  echo "engine didn't start:"
  tail -5 "$LOGS/$1.log"
  exit 1
}

pending() { psql -qAt "$DB" -c "SELECT count(*) FROM deliveries WHERE status = 0"; }

# Streams COUNT messages, stopping the engine with `$1` STOP_AFTER seconds in, then starts it
# again and waits for everything to arrive. Prints the sink's last line.
phase() {
  local signal=$1 name=$2
  psql -q "$DB" -c "TRUNCATE messages, deliveries, dedupe_keys" 2>/dev/null
  "$BIN" sink --port 6662 >"$LOGS/$name-sink.log" 2>&1 &
  SINK=$!
  start_engine "$name"
  "$BIN" gen --to 127.0.0.1:6661 --count "$COUNT" --patients 300 --rate "$RATE" --connections 8 \
    >"$LOGS/$name-gen.log" 2>&1 &
  GEN=$!
  sleep "$STOP_AFTER"
  local at
  at=$(date +%s%N)
  kill "-$signal" "$ENGINE"
  local code=0
  wait "$ENGINE" || code=$?
  local took=$((($(date +%s%N) - at) / 1000000))
  echo "   $signal: exit code $code after $took ms" >&2
  echo "$code $took" >"$LOGS/$name.stop"
  sleep 1
  start_engine "$name"
  wait "$GEN"
  for _ in $(seq 1 120); do
    [ "$(pending)" = 0 ] && break
    sleep 0.5
  done
  sleep 1
  kill -INT "$SINK"
  wait "$SINK" || true
  kill -TERM "$ENGINE"
  wait "$ENGINE" || true
  tail -1 "$LOGS/$name-sink.log"
}

echo "== 1. kill -9 ${STOP_AFTER}s into $COUNT messages sent as fast as they're taken"
hard=$(phase KILL hard)
echo "   receiver: ${hard#*| }"
echo "== 2. SIGTERM at the same point"
clean=$(phase TERM clean)
echo "   receiver: ${clean#*| }"
read -r code took <"$LOGS/clean.stop"
grep "stopped" "$LOGS/clean.log" >/dev/null && echo "   engine: $(grep -o 'stopped took=[^ ]*' "$LOGS/clean.log" | head -1)"
dupes() { sed -n 's/.*duplicates \([0-9]*\).*/\1/p' <<<"$1"; }
echo "   sent twice: $(dupes "$hard") after kill -9, $(dupes "$clean") after SIGTERM"

check hard-lost-messages 'grep "unique $COUNT |" <<<"$hard" >/dev/null'
check clean-lost-messages 'grep "unique $COUNT |" <<<"$clean" >/dev/null'
check clean-out-of-order 'grep "out of order 0 |" <<<"$clean" >/dev/null'
check clean-sent-twice '[ "$(dupes "$clean")" = 0 ]'
check clean-exit-code '[ "$code" = 0 ]'
check clean-log 'grep "stopping: taking no new messages" "$LOGS/clean.log" >/dev/null'

echo "== 3. SIGTERM while the receiver is down (the worker waits in a retry back-off)"
psql -q "$DB" -c "TRUNCATE messages, deliveries, dedupe_keys" 2>/dev/null
start_engine down
"$BIN" gen --to 127.0.0.1:6661 --count 20 --patients 5 >"$LOGS/down-gen.log" 2>&1
for _ in $(seq 1 50); do
  grep 'delivery failed; retrying' "$LOGS/down.log" >/dev/null && break
  sleep 0.2
done
sleep 3 # into a longer back-off
at=$(date +%s%N)
kill -TERM "$ENGINE"
code=0
wait "$ENGINE" || code=$?
took=$((($(date +%s%N) - at) / 1000000))
echo "   SIGTERM: exit code $code after $took ms; $(pending) deliveries still pending for the next start"
check down-exit-code '[ "$code" = 0 ]'
check down-quick '[ "$took" -lt 2000 ]'
check down-kept '[ "$(pending)" = 20 ]'

echo "logs: $LOGS"
if [ -z "$fail" ]; then echo PASS; else echo "FAIL:$fail"; exit 1; fi
