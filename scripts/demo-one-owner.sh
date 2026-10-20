#!/usr/bin/env bash
# v0.6.2 demo: one engine per interface across every engine on one database, and the lanes guard.
#   1. A second engine started with the same project (a DR server booted with production's
#      config, a test VM pointed at production) is refused: exit 3, the other engine named, the
#      refusal in the audit log. Before 0.6.2 both ran: duplicates, and per-patient order broken.
#   2. An engine with other interfaces shares the database; each lists the other (GET /engine).
#   3. The engine loses the connection that holds its interfaces (a database restart or failover,
#      here its session killed): sending pauses, receiving goes on; it takes the lock back and
#      delivers what came in meanwhile.
#   4. kill -9: the lock goes with the process, so the refused engine starts at once.
#   5. `ordering.partitions` lowered with messages waiting in the lanes it drops: `ductus validate`
#      says so and the start is refused; set back, drained, then changed: fine.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/dev/db.sh"

BIN=$(realpath "${BIN:-${CARGO_TARGET_DIR:-target}/release/ductus}")
DB=$DUCTUS_DATABASE_URL
export DUCTUS_DATABASE_URL=$DB
API=7796
export DUCTUS_API_URL=http://127.0.0.1:$API
P=$(mktemp -d)
O=$(mktemp -d)
q() { psql -tA "$DB" -c "$1"; }
ENGINE="" OTHER="" SINK=""
fail=""

wait_for() { # wait_for <seconds> '<condition, evaluated each time>'
  local until=$((SECONDS + $1))
  while [ $SECONDS -lt $until ]; do
    if eval "$2"; then return 0; fi
    sleep 0.1
  done
  return 1
}

# The project: one ADT feed ordered by MRN over 64 lanes, to an MLLP receiver.
mkdir -p "$P/interfaces/one-owner-adt" "$O/interfaces/one-owner-billing"
printf 'api: { listen: 127.0.0.1:%s }\n' "$API" >"$P/ductus.yaml"
adt() { # adt <partitions>
  printf 'name: one-owner-adt\nsource: { mllp: { port: 6731 } }\nordering: { key: PID-3.1, partitions: %s }\ndestination: { mllp: { host: 127.0.0.1, port: 6732, timeout_ms: 500 } }\n' "$1" \
    >"$P/interfaces/one-owner-adt/interface.yaml"
}
adt 64
# Another server's interfaces, on the same database.
printf 'name: one-owner-billing\nsource: { mllp: { port: 6733 } }\ndestination: { file: { dir: out } }\n' \
  >"$O/interfaces/one-owner-billing/interface.yaml"

# Earlier runs' messages (one killed before its cleanup): start clean.
forget_waiting() {
  q "UPDATE deliveries SET status = 2, last_error = 'demo-one-owner ended' WHERE status = 0 AND destination_id IN (SELECT id FROM destinations WHERE interface IN ('one-owner-adt', 'one-owner-billing'))" >/dev/null
  q "UPDATE messages SET status = 2, last_error = 'demo-one-owner ended' WHERE status = 0 AND interface IN ('one-owner-adt', 'one-owner-billing')" >/dev/null
}
forget_waiting
q "DELETE FROM interface_applied WHERE interface = 'one-owner-adt'" >/dev/null
trap 'kill -9 $ENGINE $OTHER 2>/dev/null || true; kill -INT $SINK 2>/dev/null || true; wait 2>/dev/null || true; forget_waiting || true; "$BIN" token revoke "demo-one-owner-$$" >/dev/null 2>&1 || true' EXIT

start() { # start <log> [root]: start an engine in the background, set ENGINE, wait until it runs
  local log=$1 root=${2:-$P}
  "$BIN" run --root "$root" >"$log" 2>&1 &
  ENGINE=$!
  wait_for 30 'grep "started interfaces=" "$log" >/dev/null' || { cat "$log"; echo "FAIL: engine didn't start"; exit 1; }
}
sent() { q "SELECT count(*) FROM messages WHERE interface = 'one-owner-adt' AND status = 1"; }
waiting() { q "SELECT count(*) FROM messages WHERE interface = 'one-owner-adt' AND status = 0"; }
received_since() { q "SELECT count(*) FROM messages WHERE interface = 'one-owner-adt' AND received_at > '$1'"; }

"$BIN" sink --port 6732 >"$P/sink.log" 2>&1 &
SINK=$!

echo "== 1. the same project twice on one database"
start "$P/first.log"
FIRST=$ENGINE
first_pid=$FIRST
code=0
refused=$("$BIN" run --root "$P" --api-listen off 2>&1) || code=$?
sed 's/^/   | /' <<<"$refused"
[ "$code" = 3 ] || fail+=" exit-$code"
grep "pid $first_pid" <<<"$refused" >/dev/null || fail+=" first-not-named"
logged=$(q "SELECT count(*) FROM audit_log WHERE action = 'engine.refused' AND detail->>'reason' = 'owned' AND detail->'interfaces' ? 'one-owner-adt' AND at > now() - interval '1 minute'")
echo "   refused (exit $code), naming pid $first_pid; audit log: $logged engine.refused row"
[ "$logged" -ge 1 ] || fail+=" not-audited"

echo "== 2. another project on the same database"
"$BIN" run --root "$O" --api-listen off >"$O/run.log" 2>&1 &
OTHER=$!
wait_for 30 'grep "started interfaces=" "$O/run.log" >/dev/null' || { cat "$O/run.log"; fail+=" other-didnt-start"; }
"$BIN" token revoke "demo-one-owner-$$" >/dev/null 2>&1 || true
TOKEN=$("$BIN" token create "demo-one-owner-$$" --role operator 2>/dev/null)
others=$(curl -s -H "Authorization: Bearer $TOKEN" "$DUCTUS_API_URL/api/v1/engine")
listed=$(python3 -c "import json, sys; d = json.loads(sys.argv[1]); print(','.join(i for e in d['others'] for i in e['interfaces']))" "$others")
echo "   GET /engine on the first: others run [$listed]"
[ "$listed" = one-owner-billing ] || fail+=" others($listed)"

echo "== 3. the connection that holds the interfaces is lost"
"$BIN" gen --to 127.0.0.1:6731 --count 100 --patients 20 >/dev/null
wait_for 20 '[ "$(sent)" -ge 100 ]' || fail+=" first-batch"
cut=$(q "SELECT now()")
# What a database restart or a failover does to it: the session holding the lock ends.
q "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE application_name = 'ductus engine lock' AND pid IN (SELECT pid FROM pg_locks WHERE locktype = 'advisory' AND ((classid::bigint << 32) | objid::bigint) = (SELECT lock_key FROM engines WHERE pid = $first_pid AND stopped_at IS NULL ORDER BY started_at DESC LIMIT 1))" >/dev/null
wait_for 10 'grep "lost the connection that holds this engine" "$P/first.log" >/dev/null' || fail+=" loss-not-noticed"
"$BIN" gen --to 127.0.0.1:6731 --count 100 --patients 20 --offset 1000 >/dev/null
echo "   while cut off: $(received_since "$cut") received (ACKed), $(sent) sent in all"
wait_for 20 'grep "lock is back" "$P/first.log" >/dev/null' || fail+=" lock-not-back"
wait_for 20 '[ "$(sent)" -ge 200 ]' || fail+=" not-delivered-after($(sent))"
echo "   the lock is back: $(sent) sent, $(waiting) waiting"
grep -E "lost the connection|lock is back" "$P/first.log" | sed -E 's/^[^ ]+ +[A-Z]+ [^ ]+ /   log: /' | cut -c1-150

echo "== 4. kill -9, then the refused engine again"
kill -9 "$FIRST"
wait "$FIRST" 2>/dev/null || true
t0=$(date +%s.%N)
start "$P/second.log"
echo "   started $(echo "$(date +%s.%N) - $t0" | bc | cut -c1-4) s after the kill (no timeout to wait out)"

echo "== 5. lowering ordering.partitions with messages waiting"
kill -INT "$SINK"; wait "$SINK" 2>/dev/null || true; SINK=""
"$BIN" gen --to 127.0.0.1:6731 --count 200 --patients 200 --offset 5000 >/dev/null
wait_for 10 '[ "$(waiting)" -ge 200 ]' || fail+=" no-backlog"
kill -INT "$ENGINE"; wait "$ENGINE" 2>/dev/null || true
lanes=$(q "SELECT count(DISTINCT d.partition_key) FILTER (WHERE d.partition_key >= 32) FROM deliveries d JOIN destinations t ON t.id = d.destination_id WHERE t.interface = 'one-owner-adt' AND d.status = 0")
echo "   $(waiting) waiting, in $lanes lanes at 32 or above"
adt 32
check=$("$BIN" validate --root "$P" 2>&1) && fail+=" validate-passed"
grep '^✗ one-owner-adt → default' <<<"$check" | sed 's/^/   validate: /'
code=0
refused=$("$BIN" run --root "$P" 2>&1) || code=$?
sed 's/^/   | /' <<<"$refused"
[ "$code" = 3 ] || fail+=" lanes-exit-$code"
grep "now has 32 lanes (it had 64)" <<<"$refused" >/dev/null || fail+=" lanes-message"
echo "   set back to 64, drained, then 32"
adt 64
"$BIN" sink --port 6732 >"$P/sink2.log" 2>&1 &
SINK=$!
start "$P/third.log"
"$BIN" drain --wait --token "$TOKEN" >"$P/drain.log" 2>&1 || { cat "$P/drain.log"; fail+=" drain"; }
[ "$(waiting)" = 0 ] || fail+=" not-drained($(waiting))"
kill -INT "$ENGINE"; wait "$ENGINE" 2>/dev/null || true
adt 32
"$BIN" validate --root "$P" >/dev/null || fail+=" validate-after-drain"
start "$P/fourth.log"
echo "   32 lanes: started ($(sent) sent, $(waiting) waiting)"

if [ -z "$fail" ]; then echo PASS; else echo "FAIL:$fail"; exit 1; fi
