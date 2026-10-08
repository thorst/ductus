#!/usr/bin/env bash
# v0.5 demo: retention. Each interface keeps its messages for 1, 3 or 7 weeks; old messages go
# by dropping whole partitions, never by deleting rows from the busy tables.
#
#   adt  (MLLP, retention 7w): four days of history, 3 to 120 days old, plus live traffic.
#   lab  (MLLP, no retention: this project's ductus.yaml says 3w): history 28 and 35 days old.
#
# 1. Startup drops what's expired: adt's 120-day-old partition, lab's 35-day-old one. Lab's
#    28-day-old partition stays: it holds quarantined messages (someone must look at them).
# 2. adt is shortened to 3w while ADT keeps flowing. That deletes messages, so the engine waits
#    for `ductus retention confirm adt`; then it deletes adt's finished messages past 21 days
#    (never quarantined ones) and moves the rest into the 3w tier, in the background.
# 3. lab is lengthened to 7w: no confirmation, it starts at once and keeps everything; its
#    quarantined messages move to the 7w tier, so their old partition can finally go.
set -euo pipefail

BIN=$(realpath "${BIN:-${CARGO_TARGET_DIR:-target}/release/ductus}")
DB=${DUCTUS_DATABASE_URL:-postgres://todd:ductus@localhost/ductus_dev}
HISTORY=2000   # adt messages per day of history
LIVE=2000      # adt messages sent while the change runs
P=$(mktemp -d)
export DUCTUS_DATABASE_URL=$DB DUCTUS_API_URL=http://127.0.0.1:7791 PGTZ=UTC  # dates as the engine sees them
q() { psql -tA "$DB" -c "$1"; }
q "TRUNCATE messages, deliveries, dedupe_keys" >/dev/null
q "DELETE FROM retention WHERE interface IN ('adt', 'lab')" >/dev/null

mkdir -p "$P/interfaces/"{adt,lab}
printf 'retention: 3w\napi: { listen: 127.0.0.1:7791 }\n' >"$P/ductus.yaml"
cat >"$P/interfaces/adt/interface.yaml" <<'EOF'
name: adt
source: { mllp: { port: 6693 } }
ordering: { key: PID-3.1 }
destination: { file: { dir: out } }
retention: 7w
EOF
cat >"$P/interfaces/lab/interface.yaml" <<'EOF'
name: lab
source: { mllp: { port: 6694 } }
destination: { file: { dir: out } }
EOF

cd "$P"
ENGINE=""
trap 'kill -INT $ENGINE 2>/dev/null || true' EXIT
start_engine() {
  "$BIN" run >>"$P/run.log" 2>&1 &
  ENGINE=$!
  until [ "$(grep -c "started" "$P/run.log")" -ge "$1" ]; do kill -0 $ENGINE || { cat "$P/run.log"; exit 1; }; sleep 0.2; done
}
stop_engine() { kill -INT $ENGINE; wait $ENGINE 2>/dev/null || true; }
# wait_for <seconds> '<condition>': re-evaluated each time; on timeout, the engine log and FAIL.
wait_for() {
  local until=$(( $(date +%s) + $1 ))
  until eval "$2"; do
    if [ "$(date +%s)" -ge $until ] || ! kill -0 $ENGINE 2>/dev/null; then
      echo "FAIL: timed out waiting for: $2"; tail -30 "$P/run.log"; exit 1
    fi
    sleep 0.2
  done
}
tier_counts() {
  q "SELECT string_agg(tier || 'd: ' || n, ', ' ORDER BY tier) FROM (
       SELECT tier, count(*) n FROM messages WHERE interface = '$1' GROUP BY tier) t"
}
exists() { [ "$(q "SELECT to_regclass('$1') IS NOT NULL")" = t ]; }
leaf() { q "SELECT format('messages_t%s_%s', $1, to_char(date_trunc(ductus_tier_unit($1), current_date - $2), 'YYYYMMDD'))"; }
# History: `n` messages received `days` ago into `tier`, with status `status`, and their deliveries.
history() {
  local interface=$1 tier=$2 days=$3 n=$4 status=$5
  q "SELECT ductus_create_tier_partition('messages', $tier, current_date - $days)" >/dev/null
  q "SELECT ductus_ensure_delivery_partitions($tier)" >/dev/null
  q "WITH m AS (
       INSERT INTO messages (received_at, interface, partition_key, payload, status, tier)
       SELECT current_date - $days + interval '10 hours' + g * interval '1 second', '$interface', 0,
              convert_to('MSH|history|' || g, 'UTF8'), $status, $tier
       FROM generate_series(1, $n) g RETURNING id, received_at)
     INSERT INTO deliveries (message_id, received_at, destination_id, partition_key, status, tier)
     SELECT id, received_at, (SELECT id FROM destinations WHERE interface = '$interface' AND name = 'default'), 0, $status, $tier FROM m" >/dev/null
}

echo "== first start registers the interfaces; then history is written straight to the tables"
start_engine 1
stop_engine
for days in 120 35 14 3; do history adt 49 $days $HISTORY 1; done
history adt 49 35 5 3     # quarantined, 35 days old
history lab 21 35 1000 1
history lab 21 28 3 3     # quarantined, 28 days old
echo "   adt: $(tier_counts adt)   lab: $(tier_counts lab)"
adt120=$(leaf 49 120) lab35=$(leaf 21 35) lab28=$(leaf 21 28)
TOKEN=$("$BIN" token create "demo-$$" --role operator 2>/dev/null)
export DUCTUS_API_TOKEN=$TOKEN

echo "== adt shortened to 3w; the engine starts while ADT keeps coming"
sed -i 's/retention: 7w/retention: 3w/' interfaces/adt/interface.yaml
start_engine 2
"$BIN" gen --to 127.0.0.1:6693 --count $LIVE --patients 100 --connections 4 >"$P/gen.log" &
GEN=$!
sleep 6 # the first drop of expired partitions runs 5 s after startup
grep -o 'shorter retention not applied.*' run.log | sed 's/^/   log: /'
echo "   \$ ductus retention"
"$BIN" retention | sed 's/^/     /'
deleted_before=$(q "SELECT count(*) FROM messages WHERE interface = 'adt' AND received_at < current_date - 21 AND status = 1")
exists "$adt120" && dropped_adt120=no || dropped_adt120=yes
exists "$lab35" && dropped_lab35=no || dropped_lab35=yes
exists "$lab28" && kept_lab28=yes || kept_lab28=no
echo "   expired partitions dropped: $adt120 $dropped_adt120, $lab35 $dropped_lab35; $lab28 kept: $kept_lab28"

echo "== \$ ductus retention confirm adt"
start=$(date +%s%N)
"$BIN" retention confirm adt | sed 's/^/   /'
wait_for 120 '"$BIN" retention | grep -E "^adt +3w *$" >/dev/null'
elapsed=$(( ($(date +%s%N) - start) / 1000000 ))
wait $GEN
tail -1 "$P/gen.log" | sed 's/^/   gen: /'
wait_for 60 '[ "$(q "SELECT count(*) FROM messages WHERE interface = '"'adt'"' AND tier = 21 AND status = 1 AND received_at >= current_date")" = $LIVE ]'
grep -o 'retention change done.*' run.log | sed 's/^/   log: /'
echo "   adt after ${elapsed} ms: $(tier_counts adt)"

echo "== lab lengthened to 7w (no confirmation needed)"
stop_engine
echo "retention: 7w" >>interfaces/lab/interface.yaml
start_engine 3
wait_for 60 '"$BIN" retention | grep -E "^lab +7w *$" >/dev/null'
for _ in $(seq 1 50); do exists "$lab28" || break; sleep 0.2; done
echo "   lab: $(tier_counts lab); $lab28 $(exists "$lab28" && echo still there || echo dropped)"
"$BIN" retention | sed 's/^/   /'
stop_engine
"$BIN" token revoke "demo-$$" >/dev/null

fail=""
[ "$dropped_adt120" = yes ] || fail+=" adt-120-days-not-dropped"
[ "$dropped_lab35" = yes ] || fail+=" lab-35-days-not-dropped"
[ "$kept_lab28" = yes ] || fail+=" quarantined-partition-dropped"
[ "$deleted_before" = $HISTORY ] || fail+=" deleted-before-confirmation($deleted_before)"
[ "$(tier_counts adt)" = "21d: $((2 * HISTORY + LIVE)), 49d: 5" ] || fail+=" adt-tiers($(tier_counts adt))"
[ "$(q "SELECT count(*) FROM messages WHERE interface = 'adt' AND status = 3")" = 5 ] || fail+=" adt-quarantined"
[ "$(q "SELECT count(*) FROM deliveries d JOIN messages m ON m.id = d.message_id AND m.received_at = d.received_at WHERE m.interface = 'adt' AND d.tier <> m.tier")" = 0 ] || fail+=" deliveries-split"
[ "$(tier_counts lab)" = "49d: 3" ] || fail+=" lab-tiers($(tier_counts lab))"
exists "$lab28" && fail+=" lab-28-not-dropped"
if [ -z "$fail" ]; then echo PASS; else echo "FAIL:$fail"; tail -30 "$P/run.log"; exit 1; fi
