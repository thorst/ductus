#!/usr/bin/env bash
# v0.5 demo: hotspots. One ADT feed ordered by MRN on 4 workers, three destinations, three
# kinds of trouble, and `ductus hotspots` must name each one:
#   lab      the lab system answers 503 to one patient's messages (MRN 100007) and takes the
#            rest: that message blocks its worker's lanes, the other workers keep sending.
#            Hotspots must name the message, its key, lane and worker, and say it's the message
#            (other messages were sent since), not the receiver.
#   billing  nobody listens: every worker retries its first message. Hotspots must say the
#            receiver looks down.
#   archive  paused while a hot patient (MRN 100000, 70% of the traffic) arrives: one lane and
#            one worker hold most of the backlog. Hotspots must name the lane and the key; once
#            resumed, the archive drains and drops out of the report.
set -euo pipefail

BIN=$(realpath "${BIN:-${CARGO_TARGET_DIR:-target}/release/ductus}")
DB=${DUCTUS_DATABASE_URL:-postgres://todd:ductus@localhost/ductus_dev}
COUNT=400
P=$(mktemp -d)
export DUCTUS_DATABASE_URL=$DB
export DUCTUS_API_URL=http://127.0.0.1:7793
psql -q "$DB" -c "TRUNCATE messages, deliveries, dedupe_keys"

mkdir -p "$P/interfaces/adt" "$P/interfaces/lab-api"
printf 'api: { listen: 127.0.0.1:7793 }\n' >"$P/ductus.yaml"
cat >"$P/interfaces/adt/interface.yaml" <<'EOF'
name: adt
source: { mllp: { port: 6701 } }
ordering: { key: PID-3.1 }
isolation: dedicated
workers: 4
destinations:
  lab: { http: { url: "http://127.0.0.1:8095/lab" } }
  billing: { http: { url: "http://127.0.0.1:8096/charges" } }
  archive: { file: { dir: out/archive } }
EOF
# The "lab system": rejects one patient's messages with 503 (a temporary error), takes the rest.
cat >"$P/interfaces/lab-api/interface.yaml" <<'EOF'
name: lab-api
source: { http: { port: 8095, path: /lab, mode: respond } }
format: text
script: transform.ts
EOF
cat >"$P/interfaces/lab-api/transform.ts" <<'EOF'
import { response, type HttpRequest } from "@ductus/sdk";
export function transform(req: HttpRequest) {
  if (String(req.body).includes("|100007^")) return response({ status: 503, body: "lab busy for this patient" });
  return response({ status: 200, body: "ok" });
}
EOF

cd "$P"
"$BIN" validate >/dev/null || { "$BIN" validate; echo "FAIL: validation"; exit 1; }
q() { psql -tA "$DB" -c "$1"; }
D="deliveries d JOIN destinations t ON t.id = d.destination_id"
"$BIN" run >"$P/run.log" 2>&1 &
ENGINE=$!
trap 'kill -INT $ENGINE 2>/dev/null || true' EXIT
until grep "started interfaces=" "$P/run.log" >/dev/null; do kill -0 $ENGINE || { cat "$P/run.log"; exit 1; }; sleep 0.2; done
echo "engine log: $P/run.log"
export DUCTUS_API_TOKEN=$("$BIN" token create "demo-$$" --role operator 2>/dev/null)

echo "== pause the archive, then $COUNT ADT messages (20 patients, MRN 100000 is 70% of them)"
"$BIN" pause adt archive --reason "demo: let a hot patient pile up" | sed 's/^/   /'
"$BIN" gen --to 127.0.0.1:6701 --count $COUNT --patients 20 --hot 0.7 | tail -1 | sed 's/^/   /'

report=""
for _ in $(seq 1 120); do
  report=$("$BIN" hotspots adt)
  grep "^adt → lab: .*look at this message" <<<"$report" >/dev/null \
    && grep "^adt → billing: .*every worker is retrying" <<<"$report" >/dev/null && break
  sleep 0.25
done
echo "== ductus hotspots adt"
sed 's/^/   /' <<<"$report"

# What the report should say, from the database.
blocked_id=$(q "SELECT min(d.message_id) FROM $D JOIN messages m ON m.id = d.message_id AND m.received_at = d.received_at
  WHERE t.interface = 'adt' AND t.name = 'lab' AND d.status = 0 AND m.ordering_key = '100007'")
blocked_lane=$(q "SELECT partition_key FROM messages WHERE id = $blocked_id")
hot_lane=$(q "SELECT partition_key FROM messages WHERE interface = 'adt' AND ordering_key = '100000' LIMIT 1")
lab_sent=$(q "SELECT count(*) FROM $D WHERE t.interface = 'adt' AND t.name = 'lab' AND d.status = 1")
echo "   (database: lab's blocked message $blocked_id in lane $blocked_lane = worker $((blocked_lane % 4)); lab sent $lab_sent; hot lane $hot_lane)"

json=$(curl -s -H "Authorization: Bearer $DUCTUS_API_TOKEN" "$DUCTUS_API_URL/api/v1/hotspots?interface=adt")
missing=$("$BIN" hotspots nosuch 2>&1 || true)

echo "== resume the archive"
"$BIN" resume adt archive | sed 's/^/   /'
for _ in $(seq 1 120); do
  [ "$(find "$P/interfaces/adt/out/archive" -type f | wc -l)" = $COUNT ] && break
  sleep 0.25
done
sleep 0.5
after=$("$BIN" hotspots adt)
echo "== ductus hotspots adt (after)"
sed 's/^/   /' <<<"$after"

fail=""
lab=$(grep "^adt → lab:" <<<"$report" || true)
billing=$(grep "^adt → billing:" <<<"$report" || true)
archive=$(grep "^adt → archive:" <<<"$report" || true)
grep "worker $((blocked_lane % 4)) blocked by message $blocked_id (key 100007, lane $blocked_lane, " <<<"$lab" >/dev/null || fail+=" lab-blocked"
grep "HTTP 503" <<<"$lab" >/dev/null || fail+=" lab-error"
grep "other messages were sent since it arrived" <<<"$lab" >/dev/null || fail+=" lab-not-the-receiver"
[ "$lab_sent" -gt 0 ] || fail+=" lab-sent=$lab_sent"
grep "every worker is retrying its first message: the receiver, or something the code calls, looks down" <<<"$billing" >/dev/null || fail+=" billing-down"
grep "$COUNT waiting in .* lanes (paused)" <<<"$archive" >/dev/null || fail+=" archive-paused"
grep "lane $hot_lane has [0-9]*% of the backlog (.*key 100000 at its head)" <<<"$archive" >/dev/null || fail+=" hot-lane"
grep "worker $((hot_lane % 4)) has [0-9]*% of the backlog" <<<"$archive" >/dev/null || fail+=" uneven-worker"
grep '"kind":"blocked"' <<<"$json" >/dev/null && grep '"kind":"all_blocked"' <<<"$json" >/dev/null || fail+=" api-json"
grep "no interface named \`nosuch\`" <<<"$missing" >/dev/null || fail+=" unknown-interface"
grep "^adt → archive" <<<"$after" >/dev/null && fail+=" archive-still-listed"
grep "^adt → lab: .*blocked by message $blocked_id" <<<"$after" >/dev/null || fail+=" lab-still-blocked"
if [ -z "$fail" ]; then echo PASS; else echo "FAIL:$fail"; tail -20 "$P/run.log"; exit 1; fi
