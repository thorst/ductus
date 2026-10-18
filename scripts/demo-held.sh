#!/usr/bin/env bash
# v0.5 demo: the held area (Todd's story: a receiver had a data breach and was cut off for three
# months; its ADT stayed queued until a security audit cleared it).
#
#   adt     (MLLP, retention 1w): destinations `lab` (fine) and `breached` (paused on purpose since
#           the breach). History 20, 30 and 40 days old: lab has it all, breached none of it; a few
#           quarantined messages and some finished ones too.
#   orders  (MLLP, retention 1w, `quarantined: purge`): quarantined messages 30 days old.
#
# 1. Startup's first retention run: the expired partitions hold messages breached still needs, so
#    those move (with all their deliveries) to the held area, and the partitions drop. The disk
#    goes back to the normal window at once instead of growing with the outage. adt's quarantined
#    messages move too (kept until resolved); orders' go with their partition (purge).
# 2. `ductus resume adt breached`: the audit cleared it. The held messages are still in breached's
#    queue, in order, and are delivered.
# 3. Next run: held months that are over and fully delivered are dropped whole; the month with
#    adt's unresolved quarantined messages stays.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/dev/db.sh"

BIN=$(realpath "${BIN:-${CARGO_TARGET_DIR:-target}/release/ductus}")
DB=$DUCTUS_DATABASE_URL
HISTORY=${HISTORY:-500}   # adt messages per day of history
P=$(mktemp -d)
export DUCTUS_DATABASE_URL=$DB DUCTUS_API_URL=http://127.0.0.1:7792 PGTZ=UTC  # dates as the engine sees them
q() { psql -tA "$DB" -c "$1"; }
q "TRUNCATE messages, deliveries, dedupe_keys" >/dev/null
q "DELETE FROM retention WHERE interface IN ('adt', 'orders')" >/dev/null

mkdir -p "$P/interfaces/"{adt,orders}
printf 'api: { listen: 127.0.0.1:7792 }\n' >"$P/ductus.yaml"
cat >"$P/interfaces/adt/interface.yaml" <<'EOF'
name: adt
source: { mllp: { port: 6697 } }
ordering: { key: PID-3.1 }
destinations:
  lab: { file: { dir: out/lab } }
  breached: { file: { dir: out/breached } }
retention: 1w
EOF
cat >"$P/interfaces/orders/interface.yaml" <<'EOF'
name: orders
source: { mllp: { port: 6698 } }
destination: { file: { dir: out } }
retention: 1w
quarantined: purge
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
exists() { [ "$(q "SELECT to_regclass('$1') IS NOT NULL")" = t ]; }
leaf() { q "SELECT 'messages_t7_' || to_char(current_date - $1, 'YYYYMMDD')"; }
dest() { q "SELECT id FROM destinations WHERE interface = '$1' AND name = '$2'"; }
# History: `n` messages of `interface` received `days` ago (1w tier) with message status `status`;
# each delivery gets `status`, except `breached`'s, which gets `breached`.
history() {
  local interface=$1 days=$2 n=$3 status=$4 breached=${5:-$4}
  q "SELECT ductus_create_tier_partition('messages', 7, current_date - $days)" >/dev/null
  q "SELECT ductus_ensure_delivery_partitions(7)" >/dev/null
  q "WITH m AS (
       INSERT INTO messages (received_at, interface, partition_key, payload, status, tier)
       SELECT current_date - $days + interval '10 hours' + g * interval '30000 seconds' / $n, '$interface', 0,
              convert_to('MSH|history|' || g, 'UTF8'), $status, 7
       FROM generate_series(1, $n) g RETURNING id, received_at)
     INSERT INTO deliveries (message_id, received_at, destination_id, partition_key, status, tier)
     SELECT m.id, m.received_at, t.id, 0, CASE WHEN t.name = 'breached' THEN $breached ELSE $status END, 7
     FROM m, destinations t WHERE t.interface = '$interface'" >/dev/null
}
held() { q "SELECT count(*) FROM messages WHERE interface = '$1' AND tier = 0 ${2:+AND $2}"; }

echo "== first start registers the interfaces; breached is paused (the breach)"
start_engine 1
# A token of this name left by an earlier run (process ids repeat after a reboot) would refuse the new one.
"$BIN" token revoke "demo-$$" >/dev/null 2>&1 || true
TOKEN=$("$BIN" token create "demo-$$" --role operator 2>/dev/null)
export DUCTUS_API_TOKEN=$TOKEN
"$BIN" pause adt breached --reason "data breach: cut off until the security audit clears it" | sed 's/^/   /'
stop_engine
for days in 20 30 40; do history adt $days $HISTORY 0 0; done  # pending: lab has them (1), breached doesn't
q "UPDATE deliveries SET status = 1 WHERE destination_id = $(dest adt lab)" >/dev/null
history adt 30 200 1                 # finished long ago
history adt 30 5 3                   # quarantined
history orders 30 4 3                # quarantined, but orders purges them
partitions="$(leaf 20) $(leaf 30) $(leaf 40)"
size_before=$(q "SELECT pg_size_pretty(sum(pg_total_relation_size(c.oid))) FROM pg_class c WHERE c.relname ~ '^(messages|deliveries)_t7_[0-9]{8}$'")
echo "   history: $((3 * HISTORY)) adt messages waiting for breached (20-40 days old, retention 1w), 5 quarantined, 200 finished; orders: 4 quarantined"

echo "== the engine starts; its first retention run comes 5 s later"
start_engine 2
wait_for 600 '! exists $(leaf 20) && ! exists $(leaf 30) && ! exists $(leaf 40)'
grep -o 'expired partition: messages still needed moved.*' run.log | sed 's/^/   log: /'
grep -o 'expired partitions dropped.*' run.log | sed 's/^/   log: /'
wait_for 30 '"$BIN" retention | grep "held area" >/dev/null'
echo "   \$ ductus retention"
"$BIN" retention | sed 's/^/     /'
curl -s -H "Authorization: Bearer $TOKEN" "$DUCTUS_API_URL/api/v1/metrics" | grep '^ductus_held' | sed 's/^/   metrics: /'
held_pending=$(held adt "status = 0") held_quarantined=$(held adt "status = 3")
orders_left=$(q "SELECT count(*) FROM messages WHERE interface = 'orders'")
finished_left=$(q "SELECT count(*) FROM messages WHERE interface = 'adt' AND status = 1")
split=$(q "SELECT count(*) FROM deliveries d JOIN messages m ON m.id = d.message_id AND m.received_at = d.received_at WHERE d.tier <> m.tier")
echo "   1w partitions before: $size_before; held: $held_pending pending, $held_quarantined quarantined; orders left: $orders_left; finished history left: $finished_left"

echo "== the audit cleared it: \$ ductus resume adt breached"
start=$(date +%s%N)
"$BIN" resume adt breached | sed 's/^/   /'
wait_for 900 '[ "$(held adt "status = 0")" = 0 ]'
elapsed=$(( ($(date +%s%N) - start) / 1000000 ))
delivered=$(find interfaces/adt/out/breached -type f | wc -l)
order=$(q "SELECT bool_and(ok) FROM (SELECT updated_at >= lag(updated_at, 1, updated_at) OVER (ORDER BY message_id) AS ok
            FROM deliveries WHERE destination_id = $(dest adt breached) AND tier = 0 AND status = 1) t")
echo "   $delivered held messages delivered to breached in ${elapsed} ms (in message order: $order)"

echo "== next retention run (a restart runs one at once): finished past months drop whole"
stop_engine
start_engine 3
wait_for 30 'curl -s -H "Authorization: Bearer $TOKEN" "$DUCTUS_API_URL/api/v1/retention" | grep "\"last_drop_at\":\"" >/dev/null'
grep -o 'held area: months.*' run.log | sed 's/^/   log: /' || true
idle_months=$(q "SELECT count(*) FROM pg_inherits i JOIN pg_class c ON c.oid = i.inhrelid
                 WHERE i.inhparent = 'messages_held'::regclass AND right(c.relname, 6) < to_char(now(), 'YYYYMM')
                   AND NOT EXISTS (SELECT 1 FROM messages m WHERE m.tier = 0 AND m.status IN (0, 3)
                                   AND to_char(m.received_at, 'YYYYMM') = right(c.relname, 6))")
"$BIN" retention | sed -n '/held area/,$p' | sed 's/^/   /'
stop_engine
"$BIN" token revoke "demo-$$" >/dev/null

fail=""
for p in $partitions; do exists "$p" && fail+=" $p-not-dropped"; done
[ "$held_pending" = $((3 * HISTORY)) ] || fail+=" held-pending($held_pending)"
[ "$held_quarantined" = 5 ] || fail+=" held-quarantined($held_quarantined)"
[ "$orders_left" = 0 ] || fail+=" orders-not-purged($orders_left)"
[ "$finished_left" = 0 ] || fail+=" finished-kept($finished_left)"
[ "$split" = 0 ] || fail+=" deliveries-split($split)"
[ "$delivered" = $((3 * HISTORY)) ] || fail+=" delivered($delivered)"
[ "$order" = t ] || fail+=" out-of-order"
[ "$idle_months" = 0 ] || fail+=" finished-months-kept($idle_months)"
[ "$(held adt "status = 3")" = 5 ] || fail+=" quarantined-lost"
if [ -z "$fail" ]; then echo PASS; else echo "FAIL:$fail"; tail -30 "$P/run.log"; exit 1; fi
