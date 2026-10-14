#!/usr/bin/env bash
# v0.5 demo: the audit log, in the order a compliance officer would ask the questions.
#   1. Who may read the log: a token with the `audit` permission can, one with only `payloads`
#      (or an operator) cannot.
#   2. A quarantine is dismissed, by an operator, with a reason; without a reason it is refused.
#      The log has the payload read, the dismissals (one single, one bulk), the reasons, the address.
#   3. A payload is never shown unlogged: with the audit table refusing writes, reading one is a
#      503 and no payload leaves the engine; with the table back, it is a row and the payload.
#   4. `ductus audit export` (straight from the database) matches the API.
#   5. A killed engine shows: its start has no stop, and the next start says `previous: unclean`;
#      after a clean stop the next start says `stopped`.
#   6. `audit.keep` below a year is refused.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/dev/db.sh"

BIN=$(realpath "${BIN:-${CARGO_TARGET_DIR:-target}/release/ductus}")
DB=$DUCTUS_DATABASE_URL
P=$(mktemp -d)
export DUCTUS_DATABASE_URL=$DB
export DUCTUS_API_URL=http://127.0.0.1:7795
psql -q "$DB" -c "TRUNCATE messages, deliveries, dedupe_keys"
q() { psql -tA "$DB" -c "$1"; }
fail=""

mkdir -p "$P/interfaces/orders"
printf 'api: { listen: 127.0.0.1:7795 }\n' >"$P/ductus.yaml"
cat >"$P/interfaces/orders/interface.yaml" <<'EOF'
name: orders
source: { mllp: { port: 6724 } }
ordering: { key: PID-3.1 }
script: transform.ts
destination: { file: { dir: out } }
EOF
cat >"$P/interfaces/orders/transform.ts" <<'EOF'
import { quarantine, type Message } from "@ductus/sdk";
export function transform(msg: Message) {
  if (msg.get("PID-3.1") === "100003") throw quarantine("unknown patient 100003 in the order system");
  return msg;
}
EOF
cd "$P"

ENGINE=""
start_engine() {
  "$BIN" run >"$P/run-$1.log" 2>&1 &
  ENGINE=$!
  until grep "started interfaces=" "$P/run-$1.log" 2>/dev/null >/dev/null; do kill -0 $ENGINE || { cat "$P/run-$1.log"; exit 1; }; sleep 0.2; done
}
cleanup() {
  kill -INT $ENGINE 2>/dev/null || true
  psql -q "$DB" -c "DROP TRIGGER IF EXISTS audit_demo_refuse ON audit_log" -c "DROP FUNCTION IF EXISTS audit_demo_refuse()" 2>/dev/null || true
}
trap cleanup EXIT

start_engine 1
echo "engine log: $P/run-1.log"
SUFFIX=$$
OPS=$("$BIN" token create "audit-demo-ops-$SUFFIX" --role operator --permission payloads 2>/dev/null)
AUDITOR=$("$BIN" token create "audit-demo-auditor-$SUFFIX" --role viewer --permission audit 2>/dev/null)
READER=$("$BIN" token create "audit-demo-reader-$SUFFIX" --role viewer --permission payloads 2>/dev/null)
api() { # token method path [json]
  local body=()
  [ -n "${4:-}" ] && body=(-H 'Content-Type: application/json' -d "$4")
  curl -s -w '\n%{http_code}' -X "$2" -H "Authorization: Bearer $1" "${body[@]}" "$DUCTUS_API_URL/api/v1$3"
}
status() { tail -n1 <<<"$1"; }
body() { sed '$d' <<<"$1"; }
js() { python3 -c "import json, sys; d = json.loads(sys.argv[1]); print($2)" "$1"; }

echo "== 10 orders; patient 100003's are quarantined by the code"
"$BIN" gen --to 127.0.0.1:6724 --count 10 --patients 5 | tail -1 | sed 's/^/   /'
for _ in $(seq 1 100); do
  [ "$(q "SELECT count(*) FROM deliveries d JOIN destinations t ON t.id = d.destination_id WHERE t.interface = 'orders' AND d.status = 3")" = 2 ] && break
  sleep 0.2
done
quarantined=$(js "$(body "$(api "$OPS" GET "/quarantine?interface=orders")")" "' '.join(str(x['id']) for x in d['deliveries'])")
set -- $quarantined
[ $# = 2 ] || { echo "FAIL: expected 2 quarantined, got: $quarantined"; exit 1; }
FIRST=$1 SECOND=$2
echo "   quarantined messages: $FIRST $SECOND"

echo "== who may read the log"
[ "$(status "$(api "$OPS" GET /audit)")" = 403 ] || fail+=" operator-reads-log"
[ "$(status "$(api "$READER" GET /audit)")" = 403 ] || fail+=" payloads-reads-log"
[ "$(status "$(api "$AUDITOR" GET /audit)")" = 200 ] || fail+=" auditor-cannot"

echo "== a payload is read (a row), then the quarantines are dismissed"
read_answer=$(api "$OPS" GET "/messages/$FIRST")
[ "$(status "$read_answer")" = 200 ] && grep '"payload":"MSH' <<<"$(body "$read_answer")" >/dev/null || fail+=" payload-not-shown"
no_reason=$(api "$OPS" POST "/messages/$FIRST/dismiss" '{"reason": " "}')
[ "$(status "$no_reason")" = 400 ] && grep "say why" <<<"$(body "$no_reason")" >/dev/null || fail+=" dismissed-without-reason"
one=$(api "$OPS" POST "/messages/$FIRST/dismiss" '{"reason": "order system retired this patient"}')
[ "$(status "$one")" = 200 ] || fail+=" dismiss-one"
all=$(api "$OPS" POST "/messages/dismiss" "{\"ids\": [$FIRST, $SECOND], \"reason\": \"same patient, second order\"}")
[ "$(js "$(body "$all")" "len(d['dismissed']), d['skipped']")" = "1 [$FIRST]" ] || fail+=" dismiss-bulk($(body "$all"))"
[ "$(q "SELECT count(*) FROM deliveries d JOIN destinations t ON t.id = d.destination_id WHERE t.interface = 'orders' AND d.status = 3")" = 0 ] || fail+=" still-quarantined"
echo "   dismissed: nothing is quarantined, nothing was deleted ($(q "SELECT count(*) FROM messages WHERE interface = 'orders'") messages kept)"

echo "== the log answers"
log=$(body "$(api "$AUDITOR" GET "/audit?actor=audit-demo-ops-$SUFFIX&limit=100")")
dismissals=$(js "$log" "[ (e['actor'], e['via'], e['ip'], e['detail'].get('reason')) for e in d['entries'] if e['action'] == 'message.dismiss' ]")
echo "   dismissals: $dismissals"
grep "audit-demo-ops-$SUFFIX', 'token', '127.0.0.1', 'order system retired this patient'" <<<"$dismissals" >/dev/null || fail+=" log-dismiss-one"
grep "'same patient, second order'" <<<"$dismissals" >/dev/null || fail+=" log-dismiss-bulk"
[ "$(js "$log" "len([e for e in d['entries'] if e['action'] == 'message.payload.read' and e['message'] == $FIRST])")" = 1 ] || fail+=" log-payload-read"

echo "== no payload without a log row"
psql -q "$DB" <<'EOF'
CREATE FUNCTION audit_demo_refuse() RETURNS trigger AS $$
BEGIN
  IF NEW.actor_name LIKE 'audit-demo-%' THEN RAISE EXCEPTION 'audit demo: refusing to write'; END IF;
  RETURN NEW;
END $$ LANGUAGE plpgsql;
CREATE TRIGGER audit_demo_refuse BEFORE INSERT ON audit_log FOR EACH ROW EXECUTE FUNCTION audit_demo_refuse();
EOF
refused=$(api "$READER" GET "/messages/$SECOND")
echo "   with the log unwritable: HTTP $(status "$refused"): $(body "$refused")"
[ "$(status "$refused")" = 503 ] || fail+=" read-not-refused"
grep '"payload":"MSH' <<<"$(body "$refused")" >/dev/null && fail+=" payload-leaked"
paused=$(api "$OPS" POST "/interfaces/orders/destinations/default/pause" '{}')
[ "$(status "$paused")" = 503 ] || fail+=" pause-not-refused"
[ "$(api "$OPS" GET /interfaces/orders | tail -n1)" = 200 ] && echo "   (reading the status needs no row: it still works)"
psql -q "$DB" -c "DROP TRIGGER audit_demo_refuse ON audit_log" -c "DROP FUNCTION audit_demo_refuse()"
back=$(api "$READER" GET "/messages/$SECOND")
[ "$(status "$back")" = 200 ] && grep '"payload":"MSH' <<<"$(body "$back")" >/dev/null || fail+=" read-not-back"
[ "$(q "SELECT count(*) FROM audit_log WHERE action = 'message.payload.read' AND message_id = $SECOND AND actor_name = 'audit-demo-reader-$SUFFIX'")" = 1 ] || fail+=" read-row-count"

echo "== ductus audit export (from the database) matches the API"
exported=$("$BIN" audit export --actor "audit-demo-ops-$SUFFIX" --action message.dismiss 2>/dev/null | wc -l)
via_api=$(js "$(body "$(api "$AUDITOR" GET "/audit?actor=audit-demo-ops-$SUFFIX&action=message.dismiss")")" "len(d['entries'])")
echo "   dismissals: $exported exported, $via_api through the API"
[ "$exported" = 2 ] && [ "$via_api" = 2 ] || fail+=" export-mismatch"

echo "== a killed engine shows in the log"
kill -9 $ENGINE
wait $ENGINE 2>/dev/null || true
start_engine 2
previous=$(q "SELECT detail->>'previous' FROM audit_log WHERE action = 'engine.start' ORDER BY id DESC LIMIT 1")
echo "   the start after kill -9 says: previous run $previous"
[ "$previous" = unclean ] || fail+=" crash-not-shown($previous)"
grep "did not stop cleanly" "$P/run-2.log" >/dev/null || fail+=" crash-not-warned"
kill -INT $ENGINE
wait $ENGINE 2>/dev/null || true
start_engine 3
previous=$(q "SELECT detail->>'previous' FROM audit_log WHERE action = 'engine.start' ORDER BY id DESC LIMIT 1")
echo "   the start after a clean stop says: previous run $previous"
[ "$previous" = stopped ] || fail+=" clean-stop-not-shown($previous)"
[ "$(q "SELECT detail->>'forced' FROM audit_log WHERE action = 'engine.stop' ORDER BY id DESC LIMIT 1")" = false ] || fail+=" stop-forced"

echo "== audit.keep below a year is refused"
printf 'api: { listen: 127.0.0.1:7795 }\naudit: { keep: 90d }\n' >"$P/ductus.yaml"
refusal=$(timeout 30 "$BIN" run 2>&1 || true)
grep "audit.keep must be at least a year" <<<"$refusal" >/dev/null || fail+=" keep-accepted($refusal)"

for token in ops auditor reader; do "$BIN" token revoke "audit-demo-$token-$SUFFIX" >/dev/null 2>&1 || true; done
if [ -z "$fail" ]; then echo PASS; else echo "FAIL:$fail"; tail -20 "$P/run-1.log"; exit 1; fi
