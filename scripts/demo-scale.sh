#!/usr/bin/env bash
# v0.3 demo: 1,000 shared-pool interfaces plus one dedicated high-volume "Epic ADT" interface
# with a hot MRN, all in one engine. Measures startup, memory and threads, then verifies
# that every message arrived exactly once and in per-patient order.
set -euo pipefail

BIN=${BIN:-${CARGO_TARGET_DIR:-target}/release/ductus}
BIN=$(realpath "$BIN")
SHARED=${SHARED:-1000}          # shared-pool interfaces
EPIC_RATE=${EPIC_RATE:-120}     # msg/s into the dedicated interface (reference peak)
EPIC_SECONDS=${EPIC_SECONDS:-60}
HOT=${HOT:-0.3}                 # share of Epic traffic for one MRN
SAMPLED=${SAMPLED:-200}         # shared interfaces that also get traffic
PER_SAMPLED=${PER_SAMPLED:-20}
DB=${DUCTUS_DATABASE_URL:-postgres://todd:ductus@localhost/ductus_dev}
WORK=$(mktemp -d)
export DUCTUS_DATABASE_URL=$DB

echo "== building a project with $SHARED shared interfaces + 1 dedicated (in $WORK)"
mkdir -p "$WORK/interfaces/epic-adt"
cat >"$WORK/interfaces/epic-adt/interface.yaml" <<EOF
name: epic-adt
source: { mllp: { port: 6661 } }
script: transform.ts
ordering: { key: PID-3.1 }
isolation: dedicated
workers: 4
dedupe: {}
destination: { mllp: { host: 127.0.0.1, port: 6662 } }
EOF
cat >"$WORK/interfaces/epic-adt/transform.ts" <<'EOF'
import type { Message } from "@ductus/sdk";
export function transform(msg: Message) {
  msg.PID[5][1] = msg.PID[5][1].toUpperCase();
  return msg;
}
EOF
for i in $(seq 0 $((SHARED - 1))); do
  d="$WORK/interfaces/feed-$(printf %04d "$i")"
  mkdir -p "$d"
  cat >"$d/interface.yaml" <<EOF
name: feed-$(printf %04d "$i")
source: { mllp: { port: $((20000 + i)) } }
script: transform.ts
ordering: { key: PID-3.1 }
dedupe: {}
destination: { mllp: { host: 127.0.0.1, port: 6662 } }
EOF
  cat >"$d/transform.ts" <<EOF
import type { Message } from "@ductus/sdk";
let handled = 0;
export function transform(msg: Message) {
  handled++;
  msg.add("ZIF", ["feed-$i", String(handled)]);
  return msg;
}
EOF
done

psql -q "$DB" -c "TRUNCATE messages, deliveries, dedupe_keys" 2>/dev/null || true
"$BIN" sink --port 6662 >"$WORK/sink.log" 2>&1 &
SINK=$!

echo "== starting the engine"
start=$(date +%s.%N)
"$BIN" run --root "$WORK" >"$WORK/run.log" 2>&1 &
ENGINE=$!
until grep -q '"started"\|started' "$WORK/run.log"; do
  kill -0 $ENGINE 2>/dev/null || { cat "$WORK/run.log"; exit 1; }
  sleep 0.2
done
startup=$(echo "$(date +%s.%N) - $start" | bc)
sleep 3
rss_mb=$(( $(awk '/VmRSS/ {print $2}' /proc/$ENGINE/status) / 1024 ))
threads=$(awk '/Threads/ {print $2}' /proc/$ENGINE/status)
cpu_before=$(awk '{print $14 + $15}' /proc/$ENGINE/stat)
sleep 10
cpu_idle=$(( $(awk '{print $14 + $15}' /proc/$ENGINE/stat) - cpu_before ))
printf "   ready in %.1fs | memory %d MB | %d threads | idle CPU %d ticks in 10s\n" "$startup" "$rss_mb" "$threads" "$cpu_idle"

epic_count=$((EPIC_RATE * EPIC_SECONDS))
echo "== traffic: Epic ADT $EPIC_RATE msg/s for ${EPIC_SECONDS}s (${HOT} hot MRN), plus $SAMPLED shared interfaces x $PER_SAMPLED"
"$BIN" gen --to 127.0.0.1:6661 --count $epic_count --patients 2000 --rate "$EPIC_RATE" --hot "$HOT" \
  >"$WORK/gen-epic.log" 2>&1 &
EPIC=$!
for k in $(seq 0 $((SAMPLED - 1))); do
  i=$(( (k * 7919) % SHARED ))   # spread across the pool
  "$BIN" gen --to 127.0.0.1:$((20000 + i)) --count "$PER_SAMPLED" --patients 5 --offset $(( (k + 1) * 1000000 )) \
    >"$WORK/gen-$k.log" 2>&1 &
  (( k % 50 == 49 )) && wait $(jobs -p | grep -v "^$EPIC$" | grep -v "^$SINK$" | grep -v "^$ENGINE$") 2>/dev/null || true
done
wait $EPIC
echo "   epic generator: $(tail -1 "$WORK/gen-epic.log")"
sleep 1

for _ in $(seq 1 120); do "$BIN" status | grep pending >/dev/null || break; sleep 1; done
rss_after=$(( $(awk '/VmRSS/ {print $2}' /proc/$ENGINE/status) / 1024 ))
kill -INT $SINK; wait $SINK 2>/dev/null || true
kill -INT $ENGINE; wait $ENGINE 2>/dev/null || true

expected=$((epic_count + SAMPLED * PER_SAMPLED))
echo "== results"
echo "   memory after traffic: $rss_after MB"
"$BIN" status | awk '{s[$2]+=$3} END {for (k in s) printf "   %-12s %d\n", k, s[k]}'
echo "   sink $(tail -1 "$WORK/sink.log")"
echo "   expected unique: $expected"
grep -iE ' (error|panicked)' "$WORK/run.log" | head -5 || true
echo "logs: $WORK"

sink=$(tail -1 "$WORK/sink.log")
unique=$(sed -E 's/.*unique ([0-9]+).*/\1/' <<<"$sink")
out_of_order=$(sed -E 's/.*out of order ([0-9]+).*/\1/' <<<"$sink")
if [ "$unique" != "$expected" ] || [ "$out_of_order" != 0 ]; then
  echo "FAIL: expected $expected unique in order, got $unique unique, $out_of_order out of order"
  exit 1
fi
echo "PASS"
