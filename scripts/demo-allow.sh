#!/usr/bin/env bash
# v0.6 demo: who may connect (0.6.3.5). `allow:` lists on an MLLP listener, an MLLP listener over
# TLS and the REST API; every 127.x address is this machine on Linux, so `gen --from 127.0.0.2`
# and `curl --interface` play senders elsewhere.
#   1. validate: a list that misses the sender `about.from.hosts` names warns; a malformed entry is
#      refused. ports: the ALLOWED column.
#   2. An allowed sender's messages are stored; one from 127.0.0.2 is closed before anything is read,
#      logged once a minute however often it retries, with the list (names with their addresses).
#   3. Over TLS: a refused address never gets to the handshake (no "TLS handshake refused" line).
#   4. The API: the same, for curl. GET /ports and the interface page say who may connect and what
#      was refused; doctor's own check gets through (the engine lets its checks in).
#   5. TCP keepalive on accepted connections (ss shows the timer).
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/dev/db.sh"

BIN=$(realpath "${BIN:-${CARGO_TARGET_DIR:-target}/release/ductus}")
DB=$DUCTUS_DATABASE_URL
P=$(mktemp -d)
export DUCTUS_DATABASE_URL=$DB
export DUCTUS_API_URL=http://127.0.0.1:7789
fail=""
cd "$P"
mkdir -p tls interfaces/allow-mllp interfaces/allow-tls out-mllp out-tls

"$BIN" cert self-signed tls/listener --name localhost --name 127.0.0.1 >/dev/null
cat >resources.yaml <<'EOF'
certificates:
  listener: { cert_file: tls/listener.pem, key_file: tls/listener.key }
EOF
# `localhost` is a host name: the engine looks it up at start and every 5 minutes.
cat >ductus.yaml <<'EOF'
api: { listen: 127.0.0.1:7789, allow: [127.0.0.1, localhost] }
EOF
cat >interfaces/allow-mllp/interface.yaml <<'EOF'
name: allow-mllp
about: { from: { application: Epic, hosts: [127.0.0.1, 10.1.40.0/24] } }
source: { mllp: { port: 6751, allow: [127.0.0.1, localhost] } }
destination: { file: { dir: ../../out-mllp } }
EOF
cat >interfaces/allow-tls/interface.yaml <<'EOF'
name: allow-tls
source: { mllp: { port: 6752, tls: { certificate: listener }, allow: [127.0.0.1] } }
destination: { file: { dir: ../../out-tls } }
EOF

echo "== validate: the list against the senders \`about\` names"
"$BIN" validate >validate.txt || { cat validate.txt; fail+=" validate"; }
grep -i "allow\|who may connect" validate.txt | sed 's/^/   | /'
grep "✓ who may connect: 3 listener(s)" validate.txt >/dev/null || fail+=" validate-summary"
grep "⚠ allow-mllp: about.from.hosts \`10.1.40.0/24\` isn't covered by \`allow\`" validate.txt >/dev/null || fail+=" validate-about"
cp -r . ../bad-$$ && sed -i 's|allow: \[127.0.0.1\] }|allow: [127.0.0.0/33] }|' ../bad-$$/interfaces/allow-tls/interface.yaml
bad=$(cd ../bad-$$ && "$BIN" validate 2>&1 || true)
rm -rf ../bad-$$
grep "✗ .*allow-tls.*prefix after / must be 0 to 32" <<<"$bad" >/dev/null || fail+=" validate-malformed($bad)"
echo "   a malformed entry: $(grep -o "✗ .*prefix.*" <<<"$bad" | sed -n 1p)"

echo "== ports: who may connect to each listener"
"$BIN" ports >ports.txt || fail+=" ports"
sed -n '/^Listening/,/^$/p' ports.txt | sed 's/^/   | /'
grep "6751 .*127.0.0.1, localhost" ports.txt >/dev/null || fail+=" ports-allowed"

ENGINE="" HELD=""
cleanup() {
  for pid in $ENGINE $HELD; do kill -INT "$pid" 2>/dev/null || true; done
  wait 2>/dev/null || true
  "$BIN" token revoke "allow-demo-$$" >/dev/null 2>&1 || true
}
trap cleanup EXIT
wait_for() { # seconds condition
  local end=$((SECONDS + $1))
  until eval "$2"; do
    [ $SECONDS -ge $end ] && return 1
    sleep 0.5
  done
}
"$BIN" run >run.log 2>&1 &
ENGINE=$!
until grep "started interfaces=" run.log >/dev/null 2>&1; do kill -0 $ENGINE || { cat run.log; exit 1; }; sleep 0.2; done
echo "   engine log: $P/run.log"
TOKEN=$("$BIN" token create "allow-demo-$$" --role viewer 2>/dev/null)
api() { curl -s -H "Authorization: Bearer $TOKEN" "$DUCTUS_API_URL/api/v1$1"; }
js() { python3 -c "import json, sys; d = json.loads(sys.argv[1]); print($2)" "$1"; }
files() { find "$1" -type f | wc -l; }

echo "== an allowed sender (127.0.0.1), and one that isn't (127.0.0.2)"
"$BIN" gen --to 127.0.0.1:6751 --count 50 --patients 5 >gen-ok.log 2>&1 || fail+=" gen-ok"
echo "   from 127.0.0.1: $(tail -1 gen-ok.log)"
wait_for 15 '[ "$(files out-mllp)" -ge 50 ]' || fail+=" not-delivered($(files out-mllp))"
for try in 1 2 3; do
  timeout 2 "$BIN" gen --to 127.0.0.1:6751 --count 1 --from 127.0.0.2 >gen-refused-$try.log 2>&1 || true
done
echo "   from 127.0.0.2 (three tries): $(sed -n 1p gen-refused-1.log)"
wait_for 5 'grep "connection refused: not on the allow list.*listener=\"interface \`allow-mllp\` (MLLP)\" peer=127.0.0.2 allowed=\"127.0.0.1, localhost (" run.log >/dev/null' || fail+=" refusal-not-logged"
grep "not on the allow list.*allow-mllp" run.log | sed 's/^.*WARN /   engine: /'
lines=$(grep -c "not on the allow list.*allow-mllp" run.log || true)
[ "$lines" = 1 ] || fail+=" logged-$lines-times"
[ "$(files out-mllp)" = 50 ] || fail+=" refused-one-stored"

echo "== over TLS: refused before the handshake"
timeout 2 "$BIN" gen --to 127.0.0.1:6752 --count 1 --from 127.0.0.3 --ca tls/listener.pem >gen-tls.log 2>&1 || true
wait_for 5 'grep "not on the allow list.*listener=\"interface \`allow-tls\` (MLLP)\" peer=127.0.0.3" run.log >/dev/null' || fail+=" tls-refusal-not-logged"
grep "not on the allow list.*allow-tls" run.log | sed 's/^.*WARN /   engine: /'
grep "TLS handshake refused" run.log >/dev/null && fail+=" reached-the-handshake"
"$BIN" gen --to 127.0.0.1:6752 --count 5 --ca tls/listener.pem >gen-tls-ok.log 2>&1 || fail+=" gen-tls-ok"
echo "   from 127.0.0.1 over TLS: $(tail -1 gen-tls-ok.log)"

echo "== the API: the same list"
if curl -s --interface 127.0.0.2 -H "Authorization: Bearer $TOKEN" "$DUCTUS_API_URL/api/v1/engine" >/dev/null; then
  fail+=" api-let-127.0.0.2-in"
fi
echo "   curl from 127.0.0.2: $(curl -sS --interface 127.0.0.2 "$DUCTUS_API_URL/api/v1/engine" 2>&1 | sed -n 1p || true)"
wait_for 5 'grep "not on the allow list.*listener=\"the REST API\" peer=127.0.0.2" run.log >/dev/null' || fail+=" api-refusal-not-logged"

echo "== GET /ports and the interface page: who may connect, what was refused"
ports=$(api /ports)
js "$ports" "'\n'.join(f\"   {l['port']} {l['interface']}: {l['allow']}\" + (f\" (refused {l['allow_detail']['refused']}, last from {l['allow_detail']['last_refused']}; names: {', '.join(n['name'] + ' = ' + ' '.join(n['addresses']) for n in l['allow_detail']['names'])})\" if l['allow_detail'] else '') for l in d['listening'])"
[ "$(js "$ports" "[l['allow_detail']['refused'] >= 3 and l['allow_detail']['last_refused'] for l in d['listening'] if l['port'] == 6751][0]")" = 127.0.0.2 ] || fail+=" ports-refused($ports)"
[ "$(js "$ports" "[len(l['allow_detail']['names'][0]['addresses']) > 0 for l in d['listening'] if l['port'] == 6751][0]")" = True ] || fail+=" ports-name-resolved"
detail=$(api /interfaces/allow-tls)
[ "$(js "$detail" "d['source_allow']['entries']")" = "['127.0.0.1']" ] || fail+=" detail-allow($detail)"

echo "== doctor: the names looked up; the engine's own check is let in"
report=$(curl -s -X POST -H "Authorization: Bearer $("$BIN" token create "allow-demo-ops-$$" --role operator 2>/dev/null)" \
  -H 'Content-Type: application/json' -d '{"mode": "full", "timeout_ms": 3000}' "$DUCTUS_API_URL/api/v1/doctor")
"$BIN" token revoke "allow-demo-ops-$$" >/dev/null 2>&1 || true
step() { js "$report" "[s['outcome'] + ' ' + s['detail'] for c in d['checks'] if c['endpoint'].startswith('$1') for s in c['steps'] if s['name'] == '$2']"; }
echo "   6751 allow: $(step 0.0.0.0:6751 allow)"
echo "   6752 TLS:   $(step 0.0.0.0:6752 TLS)"
[[ "$(step 0.0.0.0:6751 allow)" == *"ok only 127.0.0.1, localhost → "* ]] || fail+=" doctor-allow"
[[ "$(step 0.0.0.0:6752 TLS)" == *"ok "* ]] || fail+=" doctor-let-in($(step 0.0.0.0:6752 TLS))"
lines=$(grep -c "not on the allow list" run.log || true)
[ "$lines" = 3 ] || fail+=" doctor-logged-as-refused($lines)"

echo "== TCP keepalive on an accepted connection"
"$BIN" gen --to 127.0.0.1:6751 --count 8 --rate 1 --patients 1 --offset 1000 >gen-held.log 2>&1 &
HELD=$!
sleep 2
held=$(ss -tno state established '( sport = :6751 )' | sed -n 2,3p)
echo "   $held" | tr -s ' '
grep "timer:(keepalive" <<<"$held" >/dev/null || fail+=" no-keepalive"
wait $HELD || fail+=" gen-held"
HELD=""
grep " ERROR " run.log >/dev/null && fail+=" errors-in-the-log"

if [ -z "$fail" ]; then echo PASS; else echo "FAIL:$fail"; tail -20 run.log; exit 1; fi
