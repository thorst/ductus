#!/usr/bin/env bash
# v0.5 demo: certificate expiry, the way it goes over a few weeks (here, a few minutes).
#   1. The engine's own check (scheduled: a minute or two after start) reads the EMPI's
#      certificate: 40 days left, nothing to say.
#   2. "Check now" (POST /doctor, certificates only) as the server's certificate gets closer:
#      20 days → an alert to on-call; again at 20 days → nothing more; 10 days → escalated;
#      3 days → escalated; expired → critical. Each step is mailed once.
#   3. Renewed (90 days): recovered, mailed to the same people.
#   4. GET /certificates, the Prometheus gauge, the kept runs and the audit log agree.
#   5. Ours (0.6.3.3): the demo's MLLP listener presents a certificate of ours (owned by an
#      address). Its files are replaced with one ending in 10 days → Check now picks it up → an
#      alert to on-call and to its owner; a half renewal (a new certificate, the old key) shows on
#      its row while the engine keeps presenting the one that works; the key follows → recovered.
#      Doctor's full mode connects to the listener and reads the chain it serves.
# The certificates are made by `cert-server` (an example, not part of ductus) from a file of days,
# read at every connection; mail goes to `ductus sink --smtp`.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/dev/db.sh"

BIN=$(realpath "${BIN:-${CARGO_TARGET_DIR:-target}/release/ductus}")
CERTS=${CERT_SERVER:-$(dirname "$BIN")/examples/cert-server}
if [ ! -x "$CERTS" ]; then
  echo "   building the cert-server example..."
  (cd "$(dirname "${BASH_SOURCE[0]}")/.." && cargo build --release -q -p ductus-runtime --example cert-server)
fi
DB=$DUCTUS_DATABASE_URL
P=$(mktemp -d)
export DUCTUS_DATABASE_URL=$DB
export DUCTUS_API_URL=http://127.0.0.1:7797
psql -q "$DB" -c "DELETE FROM alert_events WHERE key LIKE 'cert:empi:%' OR key LIKE '%certs-demo%'" \
  -c "DELETE FROM alert_state WHERE key LIKE 'cert:empi:%' OR key LIKE '%certs-demo%'" \
  -c "DELETE FROM certificate_status WHERE resource IN ('empi', 'certs-demo-mllp')"
q() { psql -tA "$DB" -c "$1"; }
fail=""

mkdir -p "$P/interfaces/certs-demo"
cat >"$P/ductus.yaml" <<'EOF'
api: { listen: 127.0.0.1:7797 }
alerts:
  smtp: { host: 127.0.0.1, port: 2527, tls: none, from: ductus@hospital.example }
  channels:
    oncall: { email: [interfaces-oncall@hospital.example] }
  rules:
    - { when: [error, stalled, stopped], severity: critical, to: [oncall] }
    - { when: [certificate], severity: warning, to: [oncall, owner] }
  settle: 0s
doctor: { schedule: 1h }
EOF
cat >"$P/resources.yaml" <<'EOF'
apis:
  empi: { url: 'https://localhost:7447/patients', vendor: Example EMPI }
certificates:
  certs-demo-mllp: { cert_file: tls/mllp.pem, key_file: tls/mllp.key, owner: infra@hospital.example }
EOF
# Ours: a year now; later replaced by `short` (10 days), then `long` (90 days).
for c in mllp:365 short:10 long:90; do
  "$BIN" cert self-signed "$P/tls/${c%%:*}" --name localhost --name 127.0.0.1 --days "${c##*:}" >/dev/null
done
cat >"$P/interfaces/certs-demo/interface.yaml" <<'EOF'
name: certs-demo
source: { mllp: { port: 6736, tls: { certificate: certs-demo-mllp } } }
script: transform.ts
destination: { file: { dir: out } }
EOF
cat >"$P/interfaces/certs-demo/transform.ts" <<'EOF'
import type { Message } from "@ductus/sdk";
export function transform(msg: Message) {
  return msg;
}
EOF
cd "$P"

ENGINE="" MAIL="" TLS=""
cleanup() {
  for pid in $ENGINE $MAIL; do kill -INT "$pid" 2>/dev/null || true; done
  # cert-server has no SIGINT handler (a background job of a script ignores SIGINT): TERM.
  [ -n "$TLS" ] && kill "$TLS" 2>/dev/null || true
}
trap cleanup EXIT
echo 40 >"$P/days"
"$CERTS" "7447=$P/days" >"$P/certs.log" 2>&1 &
TLS=$!
"$BIN" sink --smtp 2527 --dir "$P/mail" >"$P/mail.log" 2>&1 &
MAIL=$!
wait_for() { # seconds condition
  local end=$((SECONDS + $1))
  until eval "$2"; do
    [ $SECONDS -ge $end ] && return 1
    sleep 1
  done
}
mails() { grep -c "^mail " "$P/mail.log" || true; }

echo "== the engine checks by itself: 40 days left"
"$BIN" run >"$P/run.log" 2>&1 &
ENGINE=$!
until grep "started interfaces=" "$P/run.log" 2>/dev/null >/dev/null; do kill -0 $ENGINE || { cat "$P/run.log"; exit 1; }; sleep 0.2; done
echo "   engine log: $P/run.log"
OPS=$("$BIN" token create "certs-demo-ops-$$" --role operator 2>/dev/null)
api() { # method path [json]
  local body=()
  [ -n "${3:-}" ] && body=(-H 'Content-Type: application/json' -d "$3")
  curl -s -X "$1" -H "Authorization: Bearer $OPS" "${body[@]}" "$DUCTUS_API_URL/api/v1$2"
}
js() { python3 -c "import json, sys; d = json.loads(sys.argv[1]); print($2)" "$1"; }
started=$SECONDS
wait_for 150 'grep "connection checks done" "$P/run.log" >/dev/null' || fail+=" no-scheduled-check"
echo "   the scheduled check ran $((SECONDS - started)) s after start"
certs=$(api GET /certificates)
row="d['certificates'][0]"
echo "   GET /certificates: $(js "$certs" "f\"{$row['resource']} {$row['host']}:{$row['port']} {$row['state']}, {$row['days_left']} days, trusted {$row['trusted']}; last scheduled by {d['last_scheduled']['started_by']}\"")"
[ "$(js "$certs" "$row['state'], $row['days_left']")" = "ok 40" ] || fail+=" forty($certs)"
sleep 16 # a judgement: 40 days is nothing to say
[ "$(mails)" = 0 ] || fail+=" mailed-at-40"

check_now() { # days
  echo "$1" >"$P/days"
  local report
  report=$(api POST /doctor '{"mode": "certificates"}')
  [ "$(js "$report" "len(d['checks'])")" = 1 ] || fail+=" checks($report)"
}

step() { # days, mails expected after, what the newest says
  echo "== $1 days left: Check now"
  check_now "$1"
  wait_for 40 "[ \"\$(mails)\" -ge $2 ]" || fail+=" no-mail-at-$1"
  grep "^mail $2 " "$P/mail.log" | sed 's/^/   | /'
  grep "^mail $2 .*to interfaces-oncall@hospital.example: \[Ductus dev\] $3" "$P/mail.log" >/dev/null || fail+=" mail-at-$1"
}

step 20 1 "WARNING Certificate of localhost:7447 (empi) expires in 20 days"
echo "== 20 days again: the same step is told once"
check_now 20
sleep 20
[ "$(mails)" = 1 ] || fail+=" told-twice($(mails))"
step 10 2 "WARNING Certificate of localhost:7447 (empi) expires in 10 days"
step 3 3 "WARNING Certificate of localhost:7447 (empi) expires in 3 days"
step -2 4 "CRITICAL Certificate of localhost:7447 (empi) expired on"
step 90 5 "RECOVERED Certificate of localhost:7447 (empi) expires in 90 days"

echo "== what the engine says now"
events=$(q "SELECT string_agg(kind || ':' || coalesce(severity, ''), ',' ORDER BY id) FROM alert_events WHERE key = 'cert:empi:localhost:7447'")
echo "   alert history: $events"
[ "$events" = "fired:warning,escalated:warning,escalated:warning,escalated:critical,recovered:critical" ] || fail+=" history($events)"
certs=$(api GET /certificates)
[ "$(js "$certs" "$row['state'], $row['days_left'], $row['trusted']")" = "ok 90 False" ] || fail+=" renewed($certs)"
grep "ERROR" "$P/run.log" >/dev/null && fail+=" errors-in-the-log"
metrics=$(curl -s -H "Authorization: Bearer $OPS" "$DUCTUS_API_URL/api/v1/metrics")
gauge=$(grep '^ductus_certificate_expiry_seconds{side="remote",resource="empi",host="localhost",port="7447"}' <<<"$metrics" || true)
echo "   $gauge"
days=$(awk '{ printf "%d", $2 / 86400 }' <<<"$gauge")
[ "$days" -ge 89 ] && [ "$days" -le 90 ] || fail+=" gauge($gauge)"
runs=$(api GET /doctor/runs)
echo "   kept runs: $(js "$runs" "', '.join(f\"{r['mode']} by {r['started_by']}\" for r in d[:3])") ..."
[ "$(js "$runs" "sum(1 for r in d if r['started_by'] == 'schedule') >= 1 and sum(1 for r in d if r['started_by'] == 'certs-demo-ops-$$') == 6")" = True ] || fail+=" runs"
report=$(api GET "/doctor/runs/$(js "$runs" "d[0]['id']")")
[ "$(js "$report" "d['checks'][0]['certificate']['resource']")" = empi ] || fail+=" stored-report"
logged=$(q "SELECT count(*) FROM audit_log WHERE action = 'doctor.run' AND actor_name = 'certs-demo-ops-$$'")
[ "$logged" = 6 ] || fail+=" audit($logged)"
alert=$(api GET /alerts)
[ "$(js "$alert" "[a['state'] for a in d['alerts'] if a['key'] == 'cert:empi:localhost:7447']")" = "['ok']" ] || fail+=" alerts-api"

echo "== ours: the listener's certificate, read from what the engine loaded"
OURS="[c for c in d['certificates'] if c['side'] == 'ours'][0]"
ours=$(api GET /certificates)
HOST=$(js "$ours" "$OURS['host']")
echo "   $(js "$ours" "f\"{$OURS['resource']} on {$OURS['host']} (port {$OURS['port']}): {$OURS['days_left']} days, presented at {', '.join($OURS['presented'])}\"")"
[ "$(js "$ours" "$OURS['resource'], $OURS['port'], $OURS['presented'], $OURS['days_left']")" = \
  "certs-demo-mllp 0 ['0.0.0.0:6736 MLLP over TLS (certs-demo)'] 365" ] || fail+=" ours-listed($ours)"
[ "$(q "SELECT side || ' ' || port FROM certificate_status WHERE resource = 'certs-demo-mllp' AND host = '$HOST'")" = "ours 0" ] || fail+=" ours-row"
grep "^ductus_certificate_expiry_seconds{side=\"ours\",resource=\"certs-demo-mllp\",host=\"$HOST\",port=\"0\"}" \
  <(curl -s -H "Authorization: Bearer $OPS" "$DUCTUS_API_URL/api/v1/metrics") >/dev/null || fail+=" ours-gauge"
report=$(api POST /doctor '{"mode": "full", "timeout_ms": 2000}')
chain=$(js "$report" "[s['detail'] for c in d['checks'] if c['endpoint'] == '0.0.0.0:6736 MLLP over TLS' for s in c['steps'] if s['name'] == 'chain']")
echo "   doctor (full) on our listener: $chain"
[[ "$chain" == *"self-signed: senders must trust this very certificate"* ]] || fail+=" doctor-listener($chain)"

echo "== ours, replaced by one ending in 10 days (Check now reads the files): to on-call and its owner"
cp "$P/tls/short.pem" "$P/tls/mllp.pem" && cp "$P/tls/short.key" "$P/tls/mllp.key"
check_now 90
wait_for 40 '[ "$(mails)" -ge 7 ]' || fail+=" no-ours-mail"
grep "^mail [67] " "$P/mail.log" | sed 's/^/   | /'
grep "^mail [67] .*to infra@hospital.example: \[Ductus dev\] WARNING Our certificate certs-demo-mllp (on $HOST) expires in 10 days" "$P/mail.log" >/dev/null || fail+=" ours-to-owner"
[ "$(q "SELECT chain_min_not_after::date - current_date FROM certificate_status WHERE resource = 'certs-demo-mllp' AND host = '$HOST'")" = 10 ] || fail+=" ours-row-renewed"

echo "== ours, half renewed (a new certificate, the old key): shown, the one that works kept"
cp "$P/tls/long.pem" "$P/tls/mllp.pem"
check_now 90
ours=$(api GET /certificates)
echo "   $(js "$ours" "$OURS['error']")"
[ "$(js "$ours" "$OURS['days_left'], 'key' in ($OURS['error'] or '')")" = "10 True" ] || fail+=" ours-broken($ours)"
[ "$(q "SELECT error IS NOT NULL FROM certificate_status WHERE resource = 'certs-demo-mllp' AND host = '$HOST'")" = t ] || fail+=" ours-broken-row"

echo "== ours, the key follows: renewed for 90 days, recovered"
cp "$P/tls/long.key" "$P/tls/mllp.key"
check_now 90
wait_for 40 '[ "$(mails)" -ge 9 ]' || fail+=" no-ours-recovery"
grep "^mail [89] .*to infra@hospital.example: \[Ductus dev\] RECOVERED Our certificate certs-demo-mllp" "$P/mail.log" >/dev/null || fail+=" ours-recovered"
ours=$(api GET /certificates)
[ "$(js "$ours" "$OURS['days_left'], $OURS['error']")" = "90 None" ] || fail+=" ours-renewed($ours)"

"$BIN" token revoke "certs-demo-ops-$$" >/dev/null 2>&1 || true
if [ -z "$fail" ]; then echo PASS; else echo "FAIL:$fail"; tail -20 "$P/run.log"; exit 1; fi
