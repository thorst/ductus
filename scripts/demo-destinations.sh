#!/usr/bin/env bash
# v0.5 demo: one ADT feed, three destinations, each with its own queue and status.
#   lab      MLLP, gets every message as HL7 (the lab system is DOWN at first)
#   billing  HTTP through the `billing` API in resources.yaml, gets JSON from its own function
#            `toBilling` (which skips patients that aren't billable), keeps a copy of what it
#            sent, and uses meta.id as the key
#   archive  files, every message as received
# While the lab is down, billing and archive must get everything anyway. Then the engine is
# killed (kill -9) with the lab's 200 messages still waiting, restarted, the lab comes up, and
# it must get all 200, in order per patient, once each; billing and archive nothing twice.
# `ductus impact` must find the interface by the billing vendor and the lab's host.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/dev/db.sh"

BIN=$(realpath "${BIN:-${CARGO_TARGET_DIR:-target}/release/ductus}")
DB=$DUCTUS_DATABASE_URL
COUNT=200 # 20 patients x 10 messages; MRNs 100000..100019, the 10 odd ones are billable
P=$(mktemp -d)
export DUCTUS_DATABASE_URL=$DB
psql -q "$DB" -c "TRUNCATE messages, deliveries, dedupe_keys"

mkdir -p "$P/interfaces/adt" "$P/interfaces/billing-api"
cat >"$P/interfaces/adt/interface.yaml" <<'EOF'
name: adt
about:
  owner: Integration team
  from: { application: Epic, vendor: Epic Systems, hosts: [127.0.0.0/8] }
source: { mllp: { port: 6681 } }
script: transform.ts
ordering: { key: PID-3.1 }
destinations:
  lab: { mllp: { host: 127.0.0.1, port: 6682 }, application: Sunquest Lab }
  billing:
    http: { api: billing, path: /charges }
    transform: toBilling
    output: json
    save_output: true
  archive: { file: { dir: out/archive }, application: Archive }
EOF
cat >"$P/resources.yaml" <<'EOF'
apis:
  billing: { url: 'http://127.0.0.1:8091', headers: { X-Billing-Key: k1 }, vendor: Experian }
EOF
cat >"$P/interfaces/adt/transform.ts" <<'EOF'
import { kill, type Message, type Meta } from "@ductus/sdk";

// lab and archive: the message as received.
export function transform(msg: Message) {
  return msg;
}

// billing: its own JSON, only for billable patients. meta.id is the same on every retry and
// restart, so the billing system can use it to ignore a repeat.
export function toBilling(msg: Message, meta: Meta) {
  const mrn = msg.PID[3][1];
  if (Number(mrn) % 2 === 0) return kill("not billable");
  return { messageId: meta.id, mrn, event: msg.MSH[9][2], name: msg.PID[5][1] };
}
EOF
# The "billing system": another interface in the same engine, storing what it's sent as files.
cat >"$P/interfaces/billing-api/interface.yaml" <<'EOF'
name: billing-api
about: { from: { application: Ductus }, to: { application: Billing } }
source: { http: { port: 8091, path: /charges } }
format: json
destination: { file: { dir: out/charges } }
EOF

cd "$P"
echo "== validate"; "$BIN" validate >/dev/null || { "$BIN" validate; echo "FAIL: validation"; exit 1; }
echo "== impact"
"$BIN" impact --vendor experian | sed 's/^/   /'
impact_vendor=$("$BIN" impact --vendor experian)
impact_host=$("$BIN" impact --host 127.0.0.1)
q() { psql -tA "$DB" -c "$1"; }
# Deliveries with their destination's name.
D="deliveries d JOIN destinations t ON t.id = d.destination_id"
files() { find "$P/interfaces/$1/out/$2" -type f 2>/dev/null | wc -l; }
start_engine() {
  "$BIN" run >>"$P/run.log" 2>&1 &
  ENGINE=$!
  until [ "$(grep -c "started" "$P/run.log")" -ge "$1" ]; do kill -0 $ENGINE || { cat "$P/run.log"; exit 1; }; sleep 0.2; done
}
SINK=""
trap 'kill -INT $SINK $ENGINE 2>/dev/null || true' EXIT
start_engine 1
echo "engine log: $P/run.log"

echo "== $COUNT ADT messages, lab system down"
start=$(date +%s%N)
"$BIN" gen --to 127.0.0.1:6681 --count $COUNT --patients 20 | tail -1 | sed 's/^/   /'
for _ in $(seq 1 120); do
  [ "$(files adt archive)" = $COUNT ] && [ "$(files billing-api charges)" = 100 ] && break
  sleep 0.25
done
elapsed=$(( ($(date +%s%N) - start) / 1000000 ))
sleep 0.5 # sent statuses are flushed when a worker goes idle
by_destination() {
  psql -q "$DB" -c "SELECT t.name AS destination, CASE status WHEN 0 THEN 'pending' WHEN 1 THEN 'sent' WHEN 2 THEN 'filtered' WHEN 3 THEN 'quarantined' END AS status,
    count(*), min(coalesce(last_error, '')) AS reason FROM $D WHERE t.interface = 'adt' GROUP BY 1, 2 ORDER BY 1, 2"
}
echo "   archive and billing done in ${elapsed} ms while the lab was down:"
by_destination
lab_waiting=$(q "SELECT count(*) FROM $D WHERE t.interface = 'adt' AND t.name = 'lab' AND status = 0")
adt_pending=$(q "SELECT count(*) FROM messages WHERE interface = 'adt' AND status = 0")

echo "== kill -9 the engine; start the lab system; restart the engine"
kill -9 $ENGINE; wait $ENGINE 2>/dev/null || true
"$BIN" sink --port 6682 >"$P/sink.log" 2>&1 &
SINK=$!
start_engine 2
for _ in $(seq 1 120); do
  [ "$(q "SELECT count(*) FROM messages WHERE interface = 'adt' AND status = 0")" = 0 ] && break
  sleep 0.25
done
sleep 0.5
kill -INT $SINK; wait $SINK 2>/dev/null || true

echo "== results"
by_destination
psql -q "$DB" -c "SELECT interface, CASE status WHEN 0 THEN 'pending' WHEN 1 THEN 'sent' END AS overall, count(*) FROM messages GROUP BY 1, 2 ORDER BY 1, 2"
echo "   lab $(tail -1 "$P/sink.log")"
echo "   a saved billing output: $(q "SELECT convert_from(output, 'UTF8') FROM $D WHERE t.name = 'billing' AND status = 1 ORDER BY message_id LIMIT 1")"
"$BIN" status | sed -n '/by destination/,$p' | grep -E "^adt|by destination" || true

fail=""
[ "$lab_waiting" = $COUNT ] || fail+=" lab-waiting=$lab_waiting"
[ "$adt_pending" = $COUNT ] || fail+=" overall-pending-before=$adt_pending"
grep -q "final: received $COUNT | unique $COUNT | duplicates 0 | out of order 0" "$P/sink.log" || fail+=" lab-sink"
[ "$(files adt archive)" = $COUNT ] || fail+=" archive=$(files adt archive)"
[ "$(files billing-api charges)" = 100 ] || fail+=" charges=$(files billing-api charges)"
[ "$(q "SELECT count(*) FROM messages WHERE interface = 'billing-api'")" = 100 ] || fail+=" billing-twice"
billed=$(q "SELECT count(*) FROM $D WHERE t.name = 'billing' AND status = 1
  AND convert_from(output, 'UTF8')::json->>'messageId' = message_id::text")
[ "$billed" = 100 ] || fail+=" billing-saved-with-meta-id=$billed"
[ "$(q "SELECT count(*) FROM $D WHERE t.name = 'billing' AND status = 2 AND last_error = 'killed: not billable'")" = 100 ] || fail+=" not-billable"
[ "$(q "SELECT count(*) FROM messages WHERE interface = 'adt' AND status = 1")" = $COUNT ] || fail+=" overall-sent"
grep "destination billing.*(Experian)" <<<"$impact_vendor" >/dev/null || fail+=" impact-vendor"
grep "127.0.0.1:8091 HTTP" <<<"$impact_vendor" >/dev/null || fail+=" impact-endpoint"
grep "destination lab *host: 127.0.0.1" <<<"$impact_host" >/dev/null || fail+=" impact-host"
grep "sender host (about.from.hosts): 127.0.0.0/8" <<<"$impact_host" >/dev/null || fail+=" impact-sender"
if [ -z "$fail" ]; then echo PASS; else echo "FAIL:$fail"; tail -20 "$P/run.log"; exit 1; fi
