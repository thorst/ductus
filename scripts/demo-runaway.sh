#!/usr/bin/env bash
# v0.5 demo: runaway scripts don't take their neighbours down. Four interfaces share ONE pool
# thread (one V8 isolate):
#   steady  passes messages through
#   spiky   loops forever on 1 message in 50 (a pathological message): those are quarantined,
#           the rest flow, the interface keeps running
#   broken  loops forever after an `await` on every message: 3 quarantined, then the interface
#           is stopped and its messages wait (pending)
#   hog     fills the heap on every message: 1 quarantined, interface stopped
# steady must deliver everything, as fast as it would alone.
set -euo pipefail

BIN=$(realpath "${BIN:-${CARGO_TARGET_DIR:-target}/release/ductus}")
DB=${DUCTUS_DATABASE_URL:-postgres://todd:ductus@localhost/ductus_dev}
COUNT=${COUNT:-200}
P=$(mktemp -d)
export DUCTUS_DATABASE_URL=$DB
psql -q "$DB" -c "TRUNCATE messages, deliveries, dedupe_keys"

echo "isolates: { heap_mb: 128 }" >"$P/ductus.yaml"
add() { # name port timeout transform-body
  mkdir -p "$P/interfaces/$1"
  cat >"$P/interfaces/$1/interface.yaml" <<EOF
name: $1
source: { mllp: { port: $2 } }
script: transform.ts
timeout: $3
destination: { mllp: { host: 127.0.0.1, port: 6662 } }
EOF
  printf 'import type { Message } from "@ductus/sdk";\n%s\n' "$4" >"$P/interfaces/$1/transform.ts"
}
add steady 6681 5s 'export function transform(msg: Message) { return msg; }'
add spiky 6682 300ms 'export function transform(msg: Message) {
  if (Number(msg.MSH[10]) % 50 === 7) while (true) {}
  return msg;
}'
add broken 6683 300ms 'export async function transform(msg: Message) {
  await Promise.resolve();
  while (true) {}
}'
add hog 6684 60s 'export function transform(msg: Message) {
  const keep: string[] = [];
  while (true) keep.push("x".repeat(1000) + keep.length);
}'

cd "$P"
"$BIN" sink --port 6662 >"$P/sink.log" 2>&1 &
SINK=$!
"$BIN" run --threads 1 >"$P/run.log" 2>&1 &
echo "engine log: $P/run.log"
ENGINE=$!
trap 'kill -INT $SINK $ENGINE 2>/dev/null || true' EXIT
until grep -q "started" "$P/run.log"; do kill -0 $ENGINE || { cat "$P/run.log"; exit 1; }; sleep 0.2; done

echo "== $COUNT messages to each interface"
start=$(date +%s%N)
gens=()
for port in 6681 6682 6683 6684; do
  (echo "   port $port: $("$BIN" gen --to 127.0.0.1:$port --count "$COUNT" --patients 20 | tail -1)") & gens+=($!)
done
wait "${gens[@]}"
q() { psql -tA "$DB" -c "$1"; }
for _ in $(seq 1 120); do
  [ "$(q "SELECT count(*) FROM messages WHERE interface IN ('steady','spiky') AND status = 0")" = 0 ] && break
  sleep 0.25
done
elapsed=$(( ($(date +%s%N) - start) / 1000000 ))
sleep 1 # let broken and hog settle into their final state
kill -INT $SINK $ENGINE; wait $ENGINE 2>/dev/null || true

echo "== results (steady and spiky done in ${elapsed} ms)"
psql -q "$DB" -c "SELECT interface, CASE status WHEN 0 THEN 'pending' WHEN 1 THEN 'sent' WHEN 3 THEN 'quarantined' END AS status,
  count(*), min(left(coalesce(last_error, ''), 70)) AS error FROM messages GROUP BY 1, 2 ORDER BY 1, 2"
grep -E "runaway|interface stopped" "$P/run.log" | sed -E 's/^.*(ERROR|WARN) //' | sort | uniq -c | head -8 || true

count() { q "SELECT count(*) FROM messages WHERE interface = '$1' AND status = $2"; }
spikes=$(( (COUNT + 42) / 50 ))
fail=""
[ "$(count steady 1)" = "$COUNT" ] || fail+=" steady-sent=$(count steady 1)"
[ "$(count spiky 3)" = "$spikes" ] || fail+=" spiky-quarantined=$(count spiky 3)"
[ "$(count spiky 1)" = $((COUNT - spikes)) ] || fail+=" spiky-sent=$(count spiky 1)"
[ "$(count broken 3)" = 3 ] || fail+=" broken-quarantined=$(count broken 3)"
[ "$(count broken 0)" = $((COUNT - 3)) ] || fail+=" broken-pending=$(count broken 0)"
[ "$(count hog 3)" = 1 ] || fail+=" hog-quarantined=$(count hog 3)"
[ "$(count hog 0)" = $((COUNT - 1)) ] || fail+=" hog-pending=$(count hog 0)"
grep -q 'interface stopped.*interface=broken' "$P/run.log" || fail+=" broken-not-stopped"
grep -q 'interface stopped.*interface=hog' "$P/run.log" || fail+=" hog-not-stopped"
if grep -q 'interface stopped.*interface=spiky' "$P/run.log"; then fail+=" spiky-stopped"; fi
if [ -z "$fail" ]; then echo PASS; else echo "FAIL:$fail"; tail -20 "$P/run.log"; exit 1; fi
