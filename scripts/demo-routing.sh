#!/usr/bin/env bash
# v0.5 demo: routing between interfaces. One ADT feed fans out to two internal interfaces,
# and one of those routes on to a third:
#
#   adt (MLLP in) ─┬─► lab (source: internal, 2 workers) ──► lab system (MLLP, DOWN at first)
#                  └─► billing (internal, JSON) ─┬─► charges (files)
#                                                └─► audit (internal, JSON) ──► files
#
# A route stores the message in its target interface in the same transaction that marks it
# sent, so each hop is exactly once, even across a crash. The audit route is rate limited
# (100/s) so the engine can be killed (kill -9) in the middle of routing. After a restart:
# every interface has each message exactly once (one per origin), the lab system gets all of
# them in order per patient, and each audit record points back through billing to its ADT.
# `ductus validate` refuses a loop; `ductus impact` follows the routes downstream.
set -euo pipefail

BIN=$(realpath "${BIN:-${CARGO_TARGET_DIR:-target}/release/ductus}")
DB=${DUCTUS_DATABASE_URL:-postgres://todd:ductus@localhost/ductus_dev}
COUNT=2000   # 100 patients x 20 messages; MRNs 100000..100099, the 50 odd ones are billable
BILLABLE=1000
P=$(mktemp -d)
export DUCTUS_DATABASE_URL=$DB
psql -q "$DB" -c "TRUNCATE messages, deliveries, dedupe_keys"

mkdir -p "$P/interfaces/"{adt,lab,billing,audit}
cat >"$P/interfaces/adt/interface.yaml" <<'EOF'
name: adt
about: { owner: Integration team, from: { application: Epic, vendor: Epic Systems } }
source: { mllp: { port: 6691 } }
script: transform.ts
ordering: { key: PID-3.1 }
destinations:
  lab: { interface: lab }
  billing: { interface: billing, transform: toBilling, output: json }
EOF
cat >"$P/interfaces/adt/transform.ts" <<'EOF'
import { kill, type Message, type Meta } from "@ductus/sdk";

// To the lab: the message as received.
export function transform(msg: Message) {
  return msg;
}

// To billing: JSON, billable patients only.
export function toBilling(msg: Message, meta: Meta) {
  const mrn = msg.PID[3][1];
  if (Number(mrn) % 2 === 0) return kill("not billable");
  return { adtMessage: meta.id, mrn, event: msg.MSH[9][2] };
}
EOF
cat >"$P/interfaces/lab/interface.yaml" <<'EOF'
name: lab
about: { owner: Lab team, to: { application: Sunquest Lab } }
source: internal
ordering: { key: PID-3.1 }
isolation: dedicated
workers: 2
destination: { mllp: { host: 127.0.0.1, port: 6692 } }
EOF
cat >"$P/interfaces/billing/interface.yaml" <<'EOF'
name: billing
about: { to: { application: Billing } }
source: internal
format: json
script: transform.ts
destinations:
  charges: { file: { dir: out/charges } }
  audit: { interface: audit, transform: toAudit, rate_limit: { per_minute: 6000, burst: 10 } }
EOF
cat >"$P/interfaces/billing/transform.ts" <<'EOF'
import type { Meta } from "@ductus/sdk";

type Charge = { adtMessage: number; mrn: string; event: string };

export function transform(charge: Charge) {
  return charge;
}

// meta.origin: the adt message this one was routed from.
export function toAudit(charge: Charge, meta: Meta) {
  return { billingMessage: meta.id, routedFrom: meta.origin, mrn: charge.mrn };
}
EOF
cat >"$P/interfaces/audit/interface.yaml" <<'EOF'
name: audit
about: { to: { application: Audit store } }
source: internal
format: json
destination: { file: { dir: out } }
EOF

cd "$P"
echo "== validate"
"$BIN" validate >/dev/null || { "$BIN" validate; echo "FAIL: validation"; exit 1; }
# A loop is refused: audit routing back to adt.
cp interfaces/audit/interface.yaml "$P/audit.yaml"
sed -i 's/^destination: .*/destination: { interface: billing }/' interfaces/audit/interface.yaml
loop=$("$BIN" validate 2>&1 || true)
cp "$P/audit.yaml" interfaces/audit/interface.yaml
grep "✗ routes loop: audit → billing → audit" <<<"$loop" | sed 's/^/   refused: /' || true
echo "== impact --vendor 'Epic Systems'"
impact=$("$BIN" impact --vendor "Epic Systems")
sed 's/^/   /' <<<"$impact"

q() { psql -tA "$DB" -c "$1"; }
count() { q "SELECT count(*) FROM messages WHERE interface = '$1' ${2:+AND $2}"; }
start_engine() {
  "$BIN" run >>"$P/run.log" 2>&1 &
  ENGINE=$!
  until [ "$(grep -c "started" "$P/run.log")" -ge "$1" ]; do kill -0 $ENGINE || { cat "$P/run.log"; exit 1; }; sleep 0.2; done
}
SINK=""
trap 'kill -INT $SINK $ENGINE 2>/dev/null || true' EXIT
start_engine 1
echo "engine log: $P/run.log"

echo "== $COUNT ADT messages; the lab system is down"
start=$(date +%s%N)
"$BIN" gen --to 127.0.0.1:6691 --count $COUNT --patients 100 --connections 4 | tail -1 | sed 's/^/   /'
for _ in $(seq 1 120); do
  [ "$(count lab)" = $COUNT ] && [ "$(count billing)" = $BILLABLE ] && break
  sleep 0.1
done
elapsed=$(( ($(date +%s%N) - start) / 1000000 ))
echo "   received and routed: lab has $(count lab), billing $(count billing) after ${elapsed} ms"
for _ in $(seq 1 100); do [ "$(count audit)" -ge 200 ] && break; sleep 0.1; done
before_kill=$(count audit)

echo "== kill -9 in the middle of routing to audit ($before_kill of $BILLABLE there); restart with the lab system up"
kill -9 $ENGINE; wait $ENGINE 2>/dev/null || true
"$BIN" sink --port 6692 >"$P/sink.log" 2>&1 &
SINK=$!
start_engine 2
for _ in $(seq 1 300); do
  [ "$(q "SELECT count(*) FROM messages WHERE status = 0")" = 0 ] && [ "$(count audit)" = $BILLABLE ] && break
  sleep 0.1
done
sleep 0.5
kill -INT $SINK; wait $SINK 2>/dev/null || true

echo "== results"
psql -q "$DB" -c "SELECT interface, count(*) AS messages, count(DISTINCT origin_id) AS distinct_origins,
    count(*) FILTER (WHERE status = 0) AS pending FROM messages GROUP BY 1 ORDER BY 1"
echo "   lab system $(tail -1 "$P/sink.log")"
audit_file=$(find "$P/interfaces/audit/out" -type f | sort | sed -n 1p)
echo "   an audit record: $(cat "$audit_file")"
chain=$(q "SELECT count(*) FROM messages a
  JOIN messages b ON b.id = a.origin_id AND b.interface = 'billing'
  JOIN messages m ON m.id = b.origin_id AND m.interface = 'adt'
  WHERE a.interface = 'audit'
    AND convert_from(a.payload, 'UTF8')::json->>'billingMessage' = b.id::text
    AND convert_from(a.payload, 'UTF8')::json->>'routedFrom' = m.id::text")

fail=""
[ "$before_kill" -lt $BILLABLE ] || fail+=" not-killed-mid-route($before_kill)"
for i in lab:$COUNT billing:$BILLABLE audit:$BILLABLE; do
  name=${i%%:*} want=${i##*:}
  [ "$(count "$name")" = "$want" ] || fail+=" $name=$(count "$name")"
  [ "$(q "SELECT count(DISTINCT origin_id) FROM messages WHERE interface = '$name'")" = "$want" ] || fail+=" $name-origins"
done
[ "$(q "SELECT count(*) FROM messages WHERE status <> 1 AND interface <> 'adt'")" = 0 ] || fail+=" not-all-sent"
[ "$(count adt "status = 1")" = $COUNT ] || fail+=" adt-overall"
grep -q "final: received $COUNT | unique $COUNT | duplicates 0 | out of order 0" "$P/sink.log" || fail+=" lab-sink"
[ "$(find "$P/interfaces/billing/out/charges" -type f | wc -l)" = $BILLABLE ] || fail+=" charges"
[ "$(find "$P/interfaces/audit/out" -type f | wc -l)" = $BILLABLE ] || fail+=" audit-files"
[ "$chain" = $BILLABLE ] || fail+=" origin-chain=$chain"
grep "routes loop: audit → billing → audit" <<<"$loop" >/dev/null || fail+=" loop-not-refused"
grep -A1 "^ *audit" <<<"$impact" | grep "source  routed from adt → billing (about.from: Epic" >/dev/null || fail+=" impact-downstream"
if [ -z "$fail" ]; then echo PASS; else echo "FAIL:$fail"; tail -20 "$P/run.log"; exit 1; fi
