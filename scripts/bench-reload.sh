#!/usr/bin/env bash
# A reload in folder mode at a thousand interfaces (0.7.5.3.1), against the full handover: what
# Todd's Q2 asked ("whatever is faster"). An engine in folder mode runs a project of COUNT MLLP
# interfaces on 4 pool threads, each with a transform that imports a shared table, its control
# socket on. Traffic at RATE msg/s to A (v-0000, the one changed) and O (v-0001, on another
# thread). Each phase sends for SECONDS_ seconds; its action starts after 3 s. Reports per phase
# each one's send latency (stored to sent: p50/p99/max, ms) and the senders' longest wait for an
# ACK, the command's time, and from the engine's log the plan's time (the folder read again), the
# time to applied and, for a handover, how long sending paused. Phases:
#   1. nothing (the baseline);
#   2. `ductus reload` with A's code changed (ROUNDS times): in place;
#   3. `ductus reload` with 50 interfaces' code changed (ROUNDS times): in place;
#   4. `ductus reload` with nothing changed (ROUNDS times): the cost of looking;
#   5. `ductus handover` with A's code changed (ROUNDS times): every interface started again.
# Then the engine's memory, and that every message sent arrived once.
#
#   bash scripts/bench-reload.sh            # COUNT=1001 ROUNDS=3 by default
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/dev/db.sh"

BIN=$(realpath "${BIN:-${CARGO_TARGET_DIR:-target}/release/ductus}")
COUNT=${COUNT:-1001}
ROUNDS=${ROUNDS:-3}
RATE=${RATE:-100}
SECONDS_=${SECONDS_:-8}
WORK=$(mktemp -d)
P=$WORK/project
DB=bench_reload_$$
URL=${DUCTUS_DATABASE_URL%/*}/$DB
SINK=19997
BASE=23000

echo "== a project with $COUNT interfaces (in $P)"
mkdir -p "$P/lib" "$P/environments"
printf 'variables: {}\nconsole: warn\n' >"$P/environments/prod.yaml"
printf 'api: { listen: "off" }\n' >"$P/ductus.yaml"
for l in $(seq 0 9); do
  {
    printf 'export const CODES_%d: Record<string, string> = {\n' "$l"
    for k in $(seq 1 40); do printf '  "C%03d": "Code %d of table %d",\n' "$k" "$k" "$l"; done
    printf '};\nexport function code%d(c: string): string {\n  return CODES_%d[c] ?? c;\n}\n' "$l" "$l"
  } >"$P/lib/codes$l.ts"
done
for i in $(seq 0 $((COUNT - 1))); do
  n=$(printf 'v-%04d' "$i")
  d="$P/interfaces/$n"
  mkdir -p "$d"
  printf 'name: %s\nsource: { mllp: { host: 127.0.0.1, port: %d } }\ndestinations: { receiver: { mllp: { host: 127.0.0.1, port: %d }, script: transform.ts } }\n' \
    "$n" $((BASE + i)) "$SINK" >"$d/interface.yaml"
  printf 'export const ROUND = 0;\nexport function clean(s: string): string {\n  return s.trim().toUpperCase();\n}\n' >"$d/map.ts"
  cat >"$d/transform.ts" <<EOF
import type { Message } from "@ductus/sdk";
import { code$((i % 10)) } from "../../lib/codes$((i % 10)).ts";
import { clean, ROUND } from "./map.ts";

export function transform(msg: Message) {
  msg.PID[5][1] = clean(msg.PID[5][1]);
  msg.PV1[3][2] = code$((i % 10))(msg.PV1[3][2]) + (ROUND > 0 ? "" : "");
  return msg;
}
EOF
done
g() { git -C "$P" -c user.name=bench -c user.email=bench@example.org "$@"; }
"$BIN" types --root "$P" >/dev/null
g init -q -b main
g add .
g commit -qm "the project"

ADMIN=$DUCTUS_DATABASE_URL
PGTZ=UTC psql "$ADMIN" -qc "CREATE DATABASE $DB"
export DUCTUS_DATABASE_URL=$URL
running() { psql -tA "$URL" -c "SELECT pid FROM engines WHERE stopped_at IS NULL ORDER BY started_at DESC LIMIT 1"; }
cleanup() {
  set +e
  pkill -INT -f "ductus run --root $P"
  pkill -INT -f "ductus sink --port $SINK"
  sleep 2
  PGTZ=UTC psql "$ADMIN" -qc "DROP DATABASE IF EXISTS $DB WITH (FORCE)"
  [ "${KEEP:-0}" = 1 ] && echo "kept: $WORK" || rm -rf "$WORK"
}
trap cleanup EXIT

"$BIN" sink --port $SINK >"$WORK/sink.log" 2>&1 &
export RUST_LOG=${RUST_LOG:-info,ductus_runtime=debug}
export DUCTUS_CONTROL=$WORK/control
LOG=$WORK/engine.log
"$BIN" run --root "$P" --env prod --threads 4 >"$LOG" 2>&1 &
ENGINE=$!
until grep "deployed:" "$LOG" >/dev/null 2>&1; do
  kill -0 $ENGINE 2>/dev/null || { echo "FAIL: the engine ended:"; tail -5 "$LOG"; exit 1; }
  sleep 0.2
done
memory() { awk '/^VmRSS/ { printf "%.0f MB", $2 / 1024 }' "/proc/$(running)/status"; }
echo "   engine: $(memory) once started"

# `ductus reload`; fails unless it went the way $1 says (in_place, nothing).
reload() {
  "$BIN" reload --control "$DUCTUS_CONTROL" -m bench --json >"$WORK/reload.json" 2>"$WORK/reload.err" ||
    { cat "$WORK/reload.json" "$WORK/reload.err"; exit 1; }
  grep -q "\"applied\": \"$1\"" "$WORK/reload.json" || { echo "not $1:"; cat "$WORK/reload.json"; exit 1; }
}
handover() {
  "$BIN" handover --control "$DUCTUS_CONTROL" -m bench >"$WORK/handover.out" 2>&1 || { cat "$WORK/handover.out"; exit 1; }
}
# A change to the code of interfaces $1 … $2 (their map.ts), committed.
round_no=0
change() {
  round_no=$((round_no + 1))
  for i in $(seq "$1" "$2"); do
    sed -i -E "s/ROUND = [0-9]+/ROUND = $round_no/" "$P/interfaces/$(printf 'v-%04d' "$i")/map.ts"
  done
  g commit -qam "round $round_no"
}

declare -A PORT=([A]=$BASE [O]=$((BASE + 1)))
declare -A NAME=([A]=v-0000 [O]=v-0001)
PHASE=0
SENT=0
ms() { awk '{ v = $1; if (v ~ /µs$/) { sub(/µs$/, "", v); v /= 1000 } else if (v ~ /ms$/) sub(/ms$/, "", v); else { sub(/s$/, "", v); v *= 1000 } printf "%.1f ms\n", v }'; }
# One phase: traffic to A and O for SECONDS_ s; `$@` run after 3 s, timed.
phase() {
  local label=$1
  shift
  PHASE=$((PHASE + 1))
  local from lines n
  from=$(psql -tA "$URL" -c "SELECT now()")
  lines=$(wc -l <"$LOG")
  local pids=()
  for k in A O; do
    n=$((RATE * SECONDS_))
    "$BIN" gen --to "127.0.0.1:${PORT[$k]}" --count $n --connections 2 --rate "$RATE" --offset $((PHASE * 1000000 + ${#pids[@]} * 100000)) \
      >"$WORK/gen-$k.log" 2>&1 &
    pids+=($!)
    SENT=$((SENT + n))
  done
  local took=""
  if [ $# -gt 0 ]; then
    sleep 3
    local began
    began=$(date +%s.%N)
    "$@"
    took=$(awk "BEGIN { printf \"%.2f\", $(date +%s.%N) - $began }")
  fi
  for p in "${pids[@]}"; do wait "$p"; done
  for _ in $(seq 1 120); do
    [ "$(psql -tA "$URL" -c "SELECT count(*) FROM deliveries WHERE status = 0 AND received_at >= '$from'")" = 0 ] && break
    sleep 0.5
  done
  echo "-- $label${took:+ (the command: $took s)}"
  for k in A O; do
    printf '   %s %s ' "$k" "${NAME[$k]}"
    psql -tA -F ' ' "$URL" -c "SELECT count(*), round(percentile_cont(0.5) WITHIN GROUP (ORDER BY ms)::numeric, 1),
        round(percentile_cont(0.99) WITHIN GROUP (ORDER BY ms)::numeric, 1), round(max(ms)::numeric, 1)
      FROM (SELECT extract(epoch FROM d.updated_at - d.received_at) * 1000 AS ms FROM deliveries d
            JOIN destinations t ON t.id = d.destination_id
            WHERE t.interface = '${NAME[$k]}' AND d.received_at >= '$from' AND d.status = 1) x" |
      awk '{ printf "sent %5d  latency ms p50 %6s  p99 %7s  max %7s\n", $1, $2, $3, $4 }'
    grep -o 'longest wait [0-9]* ms' "$WORK/gen-$k.log" | sed 's/^/      ACK: /' || true
  done
  if [ $# -gt 0 ]; then
    tail -n +$((lines + 1)) "$LOG" >"$WORK/phase.log"
    grep -o 'reload: what the folder changes worked out.*' "$WORK/phase.log" | grep -o 'took=[^ ]*' | sed 's/took=//' | ms | sed 's/^/   plan (the folder read): /' || true
    grep -o 'applied in place: .*' "$WORK/phase.log" | grep -o 'interfaces=[0-9]* took=[^ ]*' | sed 's/^/   applied: /' || true
    grep -o 'sending paused [0-9.]* m\?s' "$WORK/phase.log" | sed 's/^/   handover: /' | tail -1 || true
    # When O's slowest message (an interface nothing changed) arrived, beside when the reload's
    # steps were logged: whether a stall lines up with one of them.
    if [ "${TIMES:-0}" = 1 ]; then
      psql -tA "$URL" -c "SELECT 'O slowest: received ' || to_char(d.received_at AT TIME ZONE 'UTC', 'HH24:MI:SS.MS') || ', '
          || round((extract(epoch FROM d.updated_at - d.received_at) * 1000)::numeric) || ' ms'
        FROM deliveries d JOIN destinations t ON t.id = d.destination_id
        WHERE t.interface = '${NAME[O]}' AND d.received_at >= '$from' AND d.status = 1
        ORDER BY d.updated_at - d.received_at DESC LIMIT 1" | sed 's/^/   /'
      grep -E 'reload asked|what the folder changes worked out|its new code runs|applied in place|deployed:|handed over to' "$WORK/phase.log" |
        sed -E 's/^([0-9T:.-]+)Z? +[A-Z]+ +([^:]+): ([^ ]+ [^ ]+ [^ ]+ [^ ]+ [^ ]+).*/   \1 \2: \3/' | cut -c1-120 || true
    fi
  fi
}

phase "nothing (baseline)"
for r in $(seq 1 "$ROUNDS"); do change 0 0; phase "ductus reload: A's code #$r" reload in_place; done
FIFTY=$((COUNT < 50 ? COUNT : 50))
for r in $(seq 1 "$ROUNDS"); do change 0 $((FIFTY - 1)); phase "ductus reload: $FIFTY interfaces' code #$r" reload in_place; done
for r in $(seq 1 "$ROUNDS"); do phase "ductus reload: nothing changed #$r" reload nothing; done
for r in $(seq 1 "$ROUNDS"); do
  change 0 0
  phase "ductus handover: A's code #$r" handover
  sleep 3 # the new engine logs its start to the same file
done
echo "   engine: $(memory) at the end"

got=$(psql -tA "$URL" -c "SELECT count(*) FROM deliveries WHERE status = 1")
echo "== $got of $SENT sent"
[ "$got" = "$SENT" ] && echo "PASS" || { echo "FAIL: $got of $SENT"; exit 1; }
