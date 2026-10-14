#!/usr/bin/env bash
# v0.5 demo: alerts, the way an on-call night goes.
#   1. `ductus validate` reads `alerts:` with the environment's values; `ductus alerts test`
#      sends a test notification without an engine (the webhook gets it).
#   2. The lab's receiver is down: the destination stalls, and the alert reaches the on-call
#      mailbox, the chat webhook and the interface's owner (a team in `owners:`), once each.
#   3. `ductus alerts` and the API show it; an operator acknowledges it and silences it for a
#      while, then ends the silence: all three are in the audit log.
#   4. The receiver comes back: the backlog drains, and once it has been fine for `settle` the
#      recovery goes to the same people. Prometheus shows the open alert, then none.
#   5. The heartbeat called its URL the whole time (the dead-man's switch).
# Mail and webhook calls go to `ductus sink --smtp` and `--http`.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/dev/db.sh"

BIN=$(realpath "${BIN:-${CARGO_TARGET_DIR:-target}/release/ductus}")
DB=$DUCTUS_DATABASE_URL
P=$(mktemp -d)
export DUCTUS_DATABASE_URL=$DB
export DUCTUS_API_URL=http://127.0.0.1:7796
psql -q "$DB" -c "TRUNCATE messages, deliveries, dedupe_keys"
# Alerts of earlier runs of this demo: start clean.
psql -q "$DB" -c "DELETE FROM alert_events WHERE key LIKE '%alerts-demo%'" -c "DELETE FROM alert_state WHERE key LIKE '%alerts-demo%'"
q() { psql -tA "$DB" -c "$1"; }
fail=""

mkdir -p "$P/interfaces/alerts-demo" "$P/environments"
cat >"$P/ductus.yaml" <<'EOF'
api: { listen: 127.0.0.1:7796 }
alerts:
  smtp: { host: 127.0.0.1, port: 2526, tls: none, from: ductus@hospital.example }
  channels:
    oncall: { email: [interfaces-oncall@hospital.example] }
    chat: { webhook: { url: "${CHAT_WEBHOOK}", format: slack } }
  rules:
    - { when: [error, stalled, stopped], severity: critical, to: [oncall, chat, owner] }
    - { when: [behind, silent, missed], severity: warning, to: [chat] }
  settle: 5s
  heartbeat: { url: "http://127.0.0.1:8096/heartbeat", every: 30s }
owners:
  Lab Systems: { email: lab-systems@hospital.example }
EOF
printf 'variables:\n  CHAT_WEBHOOK: http://127.0.0.1:8096/chat\n' >"$P/environments/dev.yaml"
cat >"$P/interfaces/alerts-demo/interface.yaml" <<'EOF'
name: alerts-demo
source: { mllp: { port: 6726 } }
script: transform.ts
about: { owner: Lab Systems }
destination: { mllp: { host: 127.0.0.1, port: 6727 } }
EOF
cat >"$P/interfaces/alerts-demo/transform.ts" <<'EOF'
import type { Message } from "@ductus/sdk";
export function transform(msg: Message) {
  return msg;
}
EOF
cd "$P"

ENGINE="" MAIL="" HOOK="" LAB=""
cleanup() {
  for pid in $ENGINE $MAIL $HOOK $LAB; do kill -INT "$pid" 2>/dev/null || true; done
}
trap cleanup EXIT
"$BIN" sink --smtp 2526 --dir "$P/mail" >"$P/mail.log" 2>&1 &
MAIL=$!
"$BIN" sink --http 8096 >"$P/hook.log" 2>&1 &
HOOK=$!
wait_for() { # seconds condition
  local end=$((SECONDS + $1))
  until eval "$2"; do
    [ $SECONDS -ge $end ] && return 1
    sleep 1
  done
}
mails() { grep -c "^mail " "$P/mail.log" || true; }

echo "== ductus validate and ductus alerts test (no engine)"
"$BIN" validate >"$P/validate.log" 2>&1 || { cat "$P/validate.log"; fail+=" validate"; }
grep "✓ alerts: 2 channel(s), 2 rule(s)" "$P/validate.log" >/dev/null || fail+=" validate-alerts"
sed -n '/alerts/p' "$P/validate.log" | sed 's/^/   /'
wait_for 10 'grep "listening" "$P/hook.log" >/dev/null'
"$BIN" alerts test chat | sed 's/^/   /'
wait_for 10 'grep "POST /chat: .*test notification" "$P/hook.log" >/dev/null' || fail+=" test-not-received"

echo "== the lab's receiver is down; 5 messages arrive"
"$BIN" run >"$P/run.log" 2>&1 &
ENGINE=$!
until grep "started interfaces=" "$P/run.log" 2>/dev/null >/dev/null; do kill -0 $ENGINE || { cat "$P/run.log"; exit 1; }; sleep 0.2; done
echo "   engine log: $P/run.log"
OPS=$("$BIN" token create "alerts-demo-ops-$$" --role operator 2>/dev/null)
export DUCTUS_API_TOKEN=$OPS
api() { # method path [json]
  local body=()
  [ -n "${3:-}" ] && body=(-H 'Content-Type: application/json' -d "$3")
  curl -s -X "$1" -H "Authorization: Bearer $OPS" "${body[@]}" "$DUCTUS_API_URL/api/v1$2"
}
js() { python3 -c "import json, sys; d = json.loads(sys.argv[1]); print($2)" "$1"; }
"$BIN" gen --to 127.0.0.1:6726 --count 5 --patients 5 | tail -1 | sed 's/^/   /'
started=$SECONDS
echo "   waiting for the stall (a minute with nothing sent) and the alert..."
wait_for 180 '[ "$(mails)" -ge 2 ]' || fail+=" no-alert-mail"
echo "   alerted after $((SECONDS - started)) s:"
sed 's/^/   | /' "$P/mail.log" | grep -v "^   | *$" | head -12
grep "^mail [12] .*to interfaces-oncall@hospital.example: \[Ductus dev\] CRITICAL alerts-demo is stalled" "$P/mail.log" >/dev/null || fail+=" oncall-mail"
grep "^mail [12] .*to lab-systems@hospital.example: \[Ductus dev\] CRITICAL alerts-demo is stalled" "$P/mail.log" >/dev/null || fail+=" owner-mail"
wait_for 10 'grep "POST /chat: .*CRITICAL alerts-demo is stalled" "$P/hook.log" >/dev/null' || fail+=" chat-webhook"
sleep 16 # another judgement: still stalled, nobody is told twice
[ "$(mails)" = 2 ] || fail+=" told-twice($(mails))"

echo "== ductus alerts, acknowledge, silence"
"$BIN" alerts | sed 's/^/   /'
"$BIN" alerts | grep "CRITICAL  alerts-demo: stalled" >/dev/null || fail+=" cli-list"
ID=$(q "SELECT id FROM alert_state WHERE key = 'dest:alerts-demo/default'")
VIEWER=$("$BIN" token create "alerts-demo-viewer-$$" --role viewer 2>/dev/null)
as_viewer() { curl -s -o /dev/null -w '%{http_code}' -X "$1" -H "Authorization: Bearer $VIEWER" "$DUCTUS_API_URL/api/v1$2"; }
[ "$(as_viewer GET /alerts)" = 200 ] && [ "$(as_viewer POST "/alerts/$ID/ack")" = 403 ] || fail+=" viewer-acks"
"$BIN" token revoke "alerts-demo-viewer-$$" >/dev/null 2>&1 || true
acked=$(api POST "/alerts/$ID/ack")
[ "$(js "$acked" "d['acknowledged_by']")" = "alerts-demo-ops-$$" ] || fail+=" ack($acked)"
silenced=$(api POST "/alerts/$ID/silence" '{"for": "1h", "reason": "lab vendor is restarting their interface engine"}')
[ "$(js "$silenced" "d['silence_reason']")" = "lab vendor is restarting their interface engine" ] || fail+=" silence($silenced)"
api DELETE "/alerts/$ID/silence" >/dev/null
logged=$(q "SELECT string_agg(action, ',' ORDER BY id) FROM audit_log WHERE subject = 'dest:alerts-demo/default' AND actor_name = 'alerts-demo-ops-$$'")
echo "   audit log: $logged"
[ "$logged" = "alert.ack,alert.silence,alert.unsilence" ] || fail+=" audit($logged)"
metrics=$(curl -s -H "Authorization: Bearer $OPS" "$DUCTUS_API_URL/api/v1/metrics")
grep '^ductus_alerts_open{severity="critical"} 1$' <<<"$metrics" >/dev/null || fail+=" metric-open"

echo "== the receiver is back"
"$BIN" sink --port 6727 >"$P/lab.log" 2>&1 &
LAB=$!
recovered=$SECONDS
wait_for 120 '[ "$(mails)" -ge 4 ]' || fail+=" no-recovery-mail"
echo "   recovered and told after $((SECONDS - recovered)) s:"
grep "^mail [34] " "$P/mail.log" | sed 's/^/   | /'
[ "$(grep -c "RECOVERED alerts-demo: no longer stalled" "$P/mail.log")" = 2 ] || fail+=" recovery-mails"
wait_for 10 'grep "POST /chat: .*RECOVERED alerts-demo" "$P/hook.log" >/dev/null' || fail+=" recovery-webhook"
events=$(q "SELECT string_agg(kind, ',' ORDER BY id) FROM alert_events WHERE key = 'dest:alerts-demo/default'")
echo "   alert history: $events"
[ "$events" = "fired,acknowledged,silenced,unsilenced,recovered" ] || fail+=" history($events)"
metrics=$(curl -s -H "Authorization: Bearer $OPS" "$DUCTUS_API_URL/api/v1/metrics")
grep '^ductus_alerts_open{severity="critical"} 0$' <<<"$metrics" >/dev/null || fail+=" metric-closed"
channels=$(api GET /alerts/channels)
echo "   channels: $(js "$channels" "', '.join(f\"{c['name']}: {c['sent']} sent\" for c in d['channels'])"); owners: $(js "$channels" "d['owners']")"
[ "$(js "$channels" "d['owners'][0]['addresses']")" = "['lab-systems@hospital.example']" ] || fail+=" owner-reach"
grep "GET /heartbeat" "$P/hook.log" >/dev/null || fail+=" heartbeat"
echo "   heartbeat calls: $(grep -c "GET /heartbeat" "$P/hook.log")"

"$BIN" token revoke "alerts-demo-ops-$$" >/dev/null 2>&1 || true
if [ -z "$fail" ]; then echo PASS; else echo "FAIL:$fail"; tail -20 "$P/run.log"; exit 1; fi
