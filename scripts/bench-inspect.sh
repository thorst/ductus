#!/usr/bin/env bash
# What `ductus dev --inspect` costs the interfaces that aren't being debugged, and the start.
#
#   COUNT=20000 CONNECTIONS=4 RUNS=3 STARTS=5 bash scripts/bench-inspect.sh
#   BASE=~/target-base/release/ductus bash scripts/bench-inspect.sh   # an older binary's "off" too
#
# Under `ductus dev` (its own Postgres, kept between runs), two interfaces with a transform each:
# `other` (the shared pool, a file destination) takes COUNT messages from `ductus gen` as fast as
# its ACKs come back, while `dbg` is
#   off     without --inspect (BASE: the same with an older binary)
#   idle    under --inspect, no debugger attached
#   paused  under --inspect, a debugger attached (scripts/dev/cdp-hold.mjs) and `dbg` paused at a
#           `debugger;` statement, a message of its own waiting
# Prints `other`'s ingest rate (gen's ACKs per second) and the time until every message of the run
# was delivered, per run; then dev's start with and without --inspect: the engine's own `took=` and
# from the command to its `started` line (Postgres's start included). Not a pass/fail demo.
# The project lives under ~/.cache (WSL's /tmp is tmpfs: no disk numbers there).
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
BIN=$(realpath "${BIN:-${CARGO_TARGET_DIR:-target}/release/ductus}")
HOLD=$(realpath scripts/dev/cdp-hold.mjs)
COUNT=${COUNT:-20000}
CONNECTIONS=${CONNECTIONS:-4}
RUNS=${RUNS:-3}
STARTS=${STARTS:-5}
P=${BENCH_DIR:-$HOME/.cache/ductus-bench-inspect}
OTHER=6697 DBG=6698 INSPECT=9239
rm -rf "$P/interfaces"
mkdir -p "$P/interfaces/other" "$P/interfaces/dbg"
cat >"$P/interfaces/other/interface.yaml" <<EOF
name: other
script: route.ts
source: { mllp: { host: 127.0.0.1, port: $OTHER } }
ordering: { key: PID-3.1 }
destinations: { files: { file: { dir: out } } }
EOF
cat >"$P/interfaces/dbg/interface.yaml" <<EOF
name: dbg
script: route.ts
source: { mllp: { host: 127.0.0.1, port: $DBG } }
destinations: { files: { file: { dir: out } } }
EOF
printf 'import type { Message } from "@ductus/sdk";\n\nexport function transform(m: Message) {\n  m.set("MSH-3", "BENCH");\n  return m;\n}\n' >"$P/interfaces/other/route.ts"
printf 'import type { Message } from "@ductus/sdk";\n\nexport function transform(m: Message) {\n  debugger;\n  return m;\n}\n' >"$P/interfaces/dbg/route.ts"

ENGINE="" HOLDER=""
cleanup() {
  [ -n "$HOLDER" ] && kill "$HOLDER" 2>/dev/null || true
  [ -n "$ENGINE" ] && kill -TERM "$ENGINE" 2>/dev/null && wait "$ENGINE" 2>/dev/null || true
}
trap cleanup EXIT

# start BIN ARGS...: dev in the background; sets ENGINE, URL and STARTED (ms from the command to
# its `started` line) and TOOK (the engine's own).
start() {
  local bin=$1
  shift
  local t0
  t0=$(date +%s%N)
  # Gone first: the background job truncates it only once it runs, and the wait below would read
  # the last run's `started` line meanwhile.
  rm -f "$P/dev.log"
  (cd "$P" && exec "$bin" dev --api-listen off "$@") >"$P/dev.log" 2>&1 &
  ENGINE=$!
  until grep -q 'started interfaces=' "$P/dev.log" 2>/dev/null; do
    kill -0 "$ENGINE" 2>/dev/null || { cat "$P/dev.log"; exit 1; }
    sleep 0.01
  done
  STARTED=$(( ($(date +%s%N) - t0) / 1000000 ))
  TOOK=$(sed -n 's/.*started interfaces=.* took=\([0-9.]*m\?s\).*/\1/p' "$P/dev.log")
  URL=$(grep -o 'postgresql://[^ ]*' "$P/dev.log" | sed -n 1p)
}
stop() {
  kill -TERM "$ENGINE"
  wait "$ENGINE" 2>/dev/null || true
  ENGINE=""
}
q() { psql -tA "$URL" -c "$1"; }

# run LABEL BIN ARGS...: COUNT messages through `other`, RUNS times.
run() {
  local label=$1 bin=$2
  shift 2
  start "$bin" "$@"
  if [ "$label" = paused ]; then
    # As dev.log: the last pass's "attached" and "paused" would be read before it's truncated (a
    # second pass then sent before the debugger attached and waited for a pause forever).
    rm -f "$P/hold.log"
    node "$HOLD" "$INSPECT" >"$P/hold.log" &
    HOLDER=$!
    until grep -q attached "$P/hold.log" 2>/dev/null; do sleep 0.05; done
    "$BIN" gen --to 127.0.0.1:$DBG --count 1 >/dev/null
    until grep -q paused "$P/hold.log"; do sleep 0.05; done
  fi
  for r in $(seq 1 "$RUNS"); do
    local t0 gen ms
    t0=$(date +%s%N)
    gen=$("$BIN" gen --to 127.0.0.1:$OTHER --count "$COUNT" --patients 500 --connections "$CONNECTIONS" \
      --offset $((r * 1000000)) | tail -n 1)
    until [ "$(q "SELECT count(*) FROM messages WHERE interface = 'other' AND status = 0")" = 0 ]; do
      sleep 0.05
    done
    ms=$(( ($(date +%s%N) - t0) / 1000000 ))
    printf "%-8s %-12s %4s %14s %18.2f\n" "$label" "$(basename "$(dirname "$(dirname "$bin")")")" "$r" \
      "$(sed -E 's/.*\(([0-9]+)\/s\).*/\1/' <<<"$gen")" "$(bc -l <<<"$ms/1000")"
  done
  if [ -n "$HOLDER" ]; then kill "$HOLDER" 2>/dev/null || true; wait "$HOLDER" 2>/dev/null || true; HOLDER=""; fi
  stop
}

echo "== other: $COUNT messages, $CONNECTIONS connections, while dbg is ..."
printf "%-8s %-12s %4s %14s %18s\n" dbg binary run "ingest msg/s" "all delivered s"
start "$BIN" # the first start of a new project makes its database: not timed
stop
[ -n "${BASE:-}" ] && run off "$(realpath "$BASE")"
run off "$BIN"
run idle "$BIN" --inspect dbg --inspect-address 127.0.0.1:$INSPECT
run paused "$BIN" --inspect dbg --inspect-address 127.0.0.1:$INSPECT

echo "== dev's start, $STARTS each (ms): the engine's took=, and from the command to started"
for mode in without with; do
  for s in $(seq 1 "$STARTS"); do
    if [ $mode = with ]; then start "$BIN" --inspect dbg --inspect-address 127.0.0.1:$INSPECT; else start "$BIN"; fi
    printf "%-8s %s %10s %8s\n" "$mode" "$s" "$TOOK" "$STARTED"
    stop
  done
done
