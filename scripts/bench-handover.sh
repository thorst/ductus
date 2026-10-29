#!/usr/bin/env bash
# The handover at a thousand interfaces: an engine with 1,001 MLLP interfaces (traffic on one, 4
# kept connections at 200 msg/s), handed over three times (`ductus handover`, what `systemctl
# reload` runs). Reports per handover how long the new engine took to be ready (it receives
# beside the old one meanwhile), the old one's stop, and how long sending paused; and what the
# sender saw over all three (refused, dropped, longest wait). Then that every message arrived once.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/dev/db.sh"

BIN=${BIN:-${CARGO_TARGET_DIR:-target}/release/ductus}
BIN=$(realpath "$BIN")
COUNT=${COUNT:-1001}
ROUNDS=${ROUNDS:-3}
WORK=$(mktemp -d)
PREFIX=bench-handover-$$
SINK=19999
BASE=20000

echo "== a project with $COUNT MLLP interfaces (in $WORK)"
for i in $(seq 0 $((COUNT - 1))); do
  name=$(printf '%s-%04d' "$PREFIX" "$i")
  mkdir -p "$WORK/interfaces/$name"
  printf 'name: %s\nsource: { mllp: { host: 127.0.0.1, port: %d } }\nordering: { key: PID-3.1, partitions: 8 }\ndestination: { mllp: { host: 127.0.0.1, port: %d } }\n' \
    "$name" $((BASE + i)) "$SINK" >"$WORK/interfaces/$name/interface.yaml"
done

cleanup() {
  pkill -INT -f "ductus sink --port $SINK" || true
  pkill -f "ductus run --root $WORK" || true
}
trap cleanup EXIT

"$BIN" sink --port $SINK >"$WORK/sink.log" 2>&1 &
export DUCTUS_CONTROL=$WORK/control RUST_LOG=${RUST_LOG:-info,ductus_runtime=debug}
"$BIN" run --root "$WORK" --threads 4 --api-listen off >"$WORK/engine.log" 2>&1 &
for _ in $(seq 1 300); do
  grep "started interfaces=" "$WORK/engine.log" >/dev/null && break
  sleep 0.1
done
echo "   started: $(grep -o 'took=[^ ]*' "$WORK/engine.log" | tail -1)"

TOTAL=$((ROUNDS * 4 * 200 + 1200)) # 200 msg/s for each round's 4 s, and a little before and after
"$BIN" gen --to 127.0.0.1:$BASE --count $TOTAL --patients 200 --connections 4 --rate 200 >"$WORK/gen.log" 2>&1 &
SENDER=$!
sleep 3
for round in $(seq 1 "$ROUNDS"); do
  started=$(date +%s.%N)
  out=$("$BIN" handover --control "$WORK/control")
  took=$(echo "$(date +%s.%N) - $started" | bc)
  ready=$(grep -o 'ready after [0-9.]* m*s' <<<"$out" | sed 's/ready after //')
  stop=$(grep -o 'stopped in [0-9.]* m*s' <<<"$out" | sed 's/stopped in //')
  paused=$(grep -o 'sending paused [0-9.]* m*s' <<<"$out" | sed 's/sending paused //')
  printf '   handover %d: ready after %s, old engine stopped in %s, sending paused %s (the command: %.2f s)\n' \
    "$round" "$ready" "$stop" "$paused" "$took"
  sleep 4
done
# What the second engine costs while both run: one engine's memory, once settled.
pid=$(pgrep -f "ductus run --root $WORK" | head -1)
echo "   one engine's memory: $(ps -o rss= -p "$pid" | awk '{printf "%.0f MB", $1 / 1024}') (two run while they overlap)"
wait $SENDER
echo "   sender: $(tail -1 "$WORK/gen.log" | sed 's/.*), //')"
echo "   the last takeover's phases (debug):"
awk '/taking over from the running engine/ { n = NR } { line[NR] = $0 } END { for (i = n; i <= NR; i++) print line[i] }' "$WORK/engine.log" |
  grep -E 'start: |took over' | sed -E 's/^[^ ]+ +[A-Z]+ [^ ]+ //' | sed 's/^/     /'
for _ in $(seq 1 120); do
  left=$(PGTZ=UTC psql "$DUCTUS_DATABASE_URL" -Atc "SELECT count(*) FROM deliveries d JOIN destinations t ON t.id = d.destination_id WHERE t.interface LIKE '$PREFIX-%' AND d.status = 0")
  [ "$left" = 0 ] && break
  sleep 0.5
done
pkill -INT -f "ductus sink --port $SINK"
sleep 0.5
echo "   receiver: $(tail -1 "$WORK/sink.log")"
refused=$(sed -n 's/.*refused \([0-9]*\),.*/\1/p' "$WORK/gen.log" | tail -1)
if [ "$refused" = 0 ] && grep "unique $TOTAL | duplicates 0 | out of order 0" "$WORK/sink.log" >/dev/null; then
  echo PASS
else
  echo "FAIL (logs: $WORK)"
  exit 1
fi
