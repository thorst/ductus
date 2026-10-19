#!/usr/bin/env bash
# v0.5 release demo: one on-call night with the web UI, end to end. The scene:
#   adt-lab     ADT to the lab's receiver (MLLP), owned by "Olaf Night"
#   lab-portal  an HTTPS API whose certificate expires in 7 days (cert-server, an example)
#   directory   a throwaway OpenLDAP (scripts/dev/slapd.sh): Olaf is an operator who may read
#               payloads; Ada is an admin and an auditor
#   alerts      stalls and certificates mailed to on-call and to the owner (ductus sink --smtp)
# The story runs in a real browser (Playwright, ui/story/v05.spec.ts), which also starts and stops
# the lab's receiver: Olaf signs in through the directory (his Interfaces page opens on his own) →
# the receiver rejects one patient's messages (3 quarantined) → it goes down: the destination
# stalls, the page says why, the alert is mailed to on-call and to Olaf (his account matches the
# owner) → meanwhile the engine's own certificate check alerted the portal's certificate → Olaf
# pauses with a note, the receiver comes back, he resumes: the backlog goes and the recovery is
# mailed → of the quarantined, one is resent, one edited, one dismissed → Ada reads the audit log:
# all of it, including who read the payloads. Then this script checks the mail and the audit log
# itself (rows of this run only: audit rows outlive the accounts this demo deletes).
#
# Needs Node and the UI's packages (cd ui && npm ci), Playwright's Chromium
# (npx playwright install --with-deps chromium), and slapd (apt-get install slapd).
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/dev/db.sh"

BIN=$(realpath "${BIN:-${CARGO_TARGET_DIR:-target}/release/ductus}")
UI=$(realpath "$(dirname "$0")/../ui")
CERTS=${CERT_SERVER:-$(dirname "$BIN")/examples/cert-server}
if [ ! -x "$CERTS" ]; then
  (cd "$(dirname "${BASH_SOURCE[0]}")/.." && cargo build --release -q -p ductus-runtime --example cert-server)
fi
DB=$DUCTUS_DATABASE_URL
P=$(mktemp -d)
export DUCTUS_DATABASE_URL=$DB
API=http://127.0.0.1:7799
q() { psql -tA "$DB" -c "$1"; }
fail=""

code=$(curl -s -o /dev/null -w '%{http_code}' "$API/api/v1/health" || true)
[ "$code" = 000 ] || { echo "FAIL: something already listens on $API"; exit 1; }
psql -q "$DB" -c "TRUNCATE messages, deliveries, dedupe_keys"

mkdir -p "$P/interfaces/adt-lab"
cat >"$P/ductus.yaml" <<'EOF'
api: { listen: 127.0.0.1:7799 }
alerts:
  smtp: { host: 127.0.0.1, port: 2528, tls: none, from: ductus@hospital.example }
  channels:
    oncall: { email: [interfaces-oncall@hospital.example] }
  # No rule for `error`: this team works quarantines from the Quarantine screen.
  rules:
    - { when: [stalled, stopped], severity: critical, to: [oncall, owner] }
    - { when: [certificate], severity: warning, to: [oncall, owner] }
  settle: 5s
auth:
  ldap:
    resource: corp-directory
    user_base: ou=people,dc=example,dc=org
    user_filter: (uid={username})
    attributes: { display_name: cn, email: mail }
    groups:
      admin: cn=ductus-admins,ou=groups,dc=example,dc=org
      operator: cn=ductus-operators,ou=groups,dc=example,dc=org
      permissions:
        payloads: # a list in block style: a DN's commas would split a [flow, list]
          - cn=ductus-operators,ou=groups,dc=example,dc=org
          - cn=ductus-phi,ou=groups,dc=example,dc=org
        audit: cn=ductus-auditors,ou=groups,dc=example,dc=org
EOF
cat >"$P/resources.yaml" <<'EOF'
apis:
  lab-portal: { url: "https://localhost:7481/", application: Lab portal, owner: Olaf Night }
ldap:
  corp-directory:
    url: ldap://127.0.0.1:3394
    insecure: true
    bind_dn: cn=ductus-svc,ou=service,dc=example,dc=org
    bind_password: svc-password
EOF
cat >"$P/interfaces/adt-lab/interface.yaml" <<'EOF'
name: adt-lab
source: { mllp: { port: 6751 } }
ordering: { key: PID-3.1 }
about:
  description: ADT from Epic to the lab
  owner: Olaf Night
  from: { application: Epic, vendor: Epic Systems }
  to: { application: Sunquest Lab, vendor: Sunquest }
destinations:
  lab: { mllp: { host: 127.0.0.1, port: 6752, timeout_ms: 300 } }
EOF

L=$(mktemp -d /tmp/ductus-ldap.XXXX)
SLAPD_SCRIPT=$(realpath "$(dirname "${BASH_SOURCE[0]}")/dev/slapd.sh")
PEOPLE="'test-user-olaf','test-user-ada'"
# What this demo made: the directory accounts and what hangs off them. Their message actions go
# too (they are this demo's own messages, truncated by the next demo anyway); audit rows stay.
forget_people() {
  psql -q "$DB" -c "DELETE FROM message_actions WHERE user_id IN (SELECT id FROM users WHERE username IN ($PEOPLE))" \
    -c "DELETE FROM api_sessions WHERE user_id IN (SELECT id FROM users WHERE username IN ($PEOPLE))" \
    -c "DELETE FROM user_settings WHERE user_id IN (SELECT id FROM users WHERE username IN ($PEOPLE))" \
    -c "DELETE FROM users WHERE username IN ($PEOPLE)"
}
ENGINE="" MAIL="" TLS=""
cleanup() {
  for pid in $ENGINE $MAIL; do kill -INT "$pid" 2>/dev/null || true; done
  # cert-server and slapd have no SIGINT handler worth relying on: TERM.
  [ -n "$TLS" ] && { kill "$TLS" 2>/dev/null || true; }
  pkill -INT -f "ductus sink --port 6752" 2>/dev/null || true
  bash "$SLAPD_SCRIPT" stop "$L" || true
  rm -rf "$L" || true
  forget_people || true
}
trap cleanup EXIT
forget_people
bash "$SLAPD_SCRIPT" start "$L" 3394 6641

cd "$P"
"$BIN" sink --smtp 2528 --dir "$P/mail" >"$P/mail.log" 2>&1 &
MAIL=$!
"$CERTS" 7481=7 >"$P/certs.log" 2>&1 &
TLS=$!
psql -q "$DB" -c "DELETE FROM certificate_status WHERE resource = 'lab-portal'" \
  -c "DELETE FROM alert_events WHERE key LIKE '%adt-lab%' OR key LIKE 'cert:lab-portal:%'" \
  -c "DELETE FROM alert_deliveries WHERE event_id NOT IN (SELECT id FROM alert_events)" \
  -c "DELETE FROM alert_state WHERE key LIKE '%adt-lab%' OR key LIKE 'cert:lab-portal:%'"
STARTED=$(q "SELECT now()")
"$BIN" run >"$P/run.log" 2>&1 &
ENGINE=$!
until grep "started interfaces=" "$P/run.log" >/dev/null; do kill -0 $ENGINE || { cat "$P/run.log"; exit 1; }; sleep 0.2; done
echo "engine log: $P/run.log; mail: $P/mail.log"

echo "== the night, in the browser (Playwright: ui/story/v05.spec.ts, ~5 min)"
cd "$UI"
if ! DUCTUS_URL=$API DUCTUS_BIN=$BIN DUCTUS_STORY_SOURCE=127.0.0.1:6751 DUCTUS_STORY_LAB_PORT=6752 \
  DUCTUS_STORY_MAIL="$P/mail.log" DUCTUS_STORY_LOGS="$P" \
  DUCTUS_DIRECTORY_OPERATOR="test-user-olaf:test-user-olaf-password" \
  DUCTUS_DIRECTORY_AUDITOR="test-user-ada:test-user-ada-password" \
  npx playwright test --config playwright.story.config.ts --reporter=line; then
  echo "FAIL: the story (report: $UI/playwright-report/story, screenshots: $UI/test-results/story)"
  tail -30 "$P/run.log"
  exit 1
fi
cd "$P"

echo "== the mail (ductus sink --smtp)"
grep "^mail " "$P/mail.log" | sed 's/^/   /'
line() { grep -n "^mail .*$1" "$P/mail.log" | sed -n 1p | cut -d: -f1; }
for who in interfaces-oncall@hospital.example olaf@example.org; do
  for what in "WARNING Certificate of localhost:7481 (lab-portal) expires in 7 days" \
    "CRITICAL adt-lab → lab is stalled" "RECOVERED adt-lab → lab: no longer stalled"; do
    grep "^mail [0-9]* from .* to $who: \[Ductus dev\] $what" "$P/mail.log" >/dev/null || fail+=" mail($who: $what)"
  done
done
[ "$(grep -c '^mail ' "$P/mail.log")" = 6 ] || fail+=" mail-count($(grep -c '^mail ' "$P/mail.log"))"
cert=$(line "expires in 7 days") recovered=$(line "RECOVERED")
[ -n "$cert" ] && [ -n "$recovered" ] && [ "$cert" -lt "$recovered" ] || fail+=" certificate-not-earlier"
grep -A6 "CRITICAL adt-lab → lab is stalled" "$P/mail.log" | grep "Connection refused" >/dev/null || fail+=" diagnosis-in-mail"

echo "== the audit log, this run (psql)"
actions=$(q "SELECT string_agg(DISTINCT action, ',' ORDER BY action) FROM audit_log WHERE at >= '$STARTED' AND actor_name = 'test-user-olaf'")
echo "   test-user-olaf: $actions"
for a in session.sign_in destination.pause destination.resume message.payload.read message.release message.edit message.dismiss; do
  grep -w "$a" <<<"${actions//,/ }" >/dev/null || fail+=" audit($a)"
done
made=$(q "SELECT count(*) FROM audit_log WHERE at >= '$STARTED' AND action = 'user.create' AND subject IN ('test-user-olaf', 'test-user-ada')")
[ "$made" = 2 ] || fail+=" accounts-made($made)"
reason=$(q "SELECT detail->>'reason' FROM audit_log WHERE at >= '$STARTED' AND action = 'destination.pause' AND actor_name = 'test-user-olaf'")
[ "$reason" = "lab vendor restarting their engine, ticket 4471" ] || fail+=" pause-reason($reason)"
q "SELECT 'Ada read the log: ' || count(*) || ' sign-in(s)' FROM audit_log WHERE at >= '$STARTED' AND actor_name = 'test-user-ada' AND action = 'session.sign_in'" | sed 's/^/   /'

echo "== where the messages ended (psql)"
states=$(q "SELECT string_agg(s || ' ' || n, ', ' ORDER BY s) FROM (SELECT d.status AS s, count(*) AS n FROM deliveries d JOIN messages m ON m.id = d.message_id AND m.received_at = d.received_at WHERE m.interface = 'adt-lab' GROUP BY d.status) x")
echo "   lab deliveries by status (0 pending, 1 sent, 2 filtered, 3 quarantined): $states"
# 14 received + 1 edited copy: 13 sent (incl. the resent one and the copy), 2 filtered (replaced, dismissed).
[ "$states" = "1 13, 2 2" ] || fail+=" deliveries($states)"
events=$(q "SELECT string_agg(kind, ',' ORDER BY id) FROM alert_events WHERE key = 'dest:adt-lab/lab'")
echo "   alert history of adt-lab → lab: $events"
[ "$events" = "fired,recovered" ] || fail+=" alert-history($events)"

if [ -z "$fail" ]; then echo PASS; else echo "FAIL:$fail"; tail -20 "$P/run.log"; exit 1; fi
