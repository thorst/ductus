#!/usr/bin/env bash
# v0.6 demo: TLS end to end, with a renewal while traffic flows (0.6.3).
#   gen --tls (a client certificate) → the engine's MLLP listener over TLS (takes only senders
#   with a certificate from the hospital's CA, subject CN=sender.demo.org) → an MLLP destination
#   over TLS (presents the engine's client certificate) → sink --tls (takes only that one)
# The hospital's CA is openssl here (a root and an issuing intermediate), as a PKI team would run
# it. Every certificate of ours starts as `ductus cert request` (a key that never leaves, and a
# CSR); `ductus cert show` judges what comes back. Then:
#   1. The listener's first certificate ends in 20 days: in the certificate table ("ours") and its
#      alert mailed to on-call and its owner.
#   2. Traffic on two generators. While the first sends, the listener's pair is renewed (new key,
#      new certificate copied over the files; "Check now" reads them, as the minute's check would):
#      open connections keep the old one, new connections get the new one, and the sink receives
#      every message once, in order per patient; nothing failed, nothing retried.
#   3. The alert recovers; doctor's full check reads the chain the listener serves (complete, up to
#      the CA named in resources.yaml) and its TLS connection to the sink with the client
#      certificate; a sender without a certificate is refused, and the log says why.
# Needs openssl (3.0 or later: -copy_extensions).
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/dev/db.sh"

BIN=$(realpath "${BIN:-${CARGO_TARGET_DIR:-target}/release/ductus}")
DB=$DUCTUS_DATABASE_URL
P=$(mktemp -d)
export DUCTUS_DATABASE_URL=$DB
export DUCTUS_API_URL=http://127.0.0.1:7788
IFACE=tls-demo
q() { psql -tA "$DB" -c "$1"; }
psql -q "$DB" -c "DELETE FROM alert_events WHERE key LIKE 'cert:engine-tls:%' OR key LIKE 'cert:engine-client:%' OR key LIKE '%$IFACE%'" \
  -c "DELETE FROM alert_state WHERE key LIKE 'cert:engine-tls:%' OR key LIKE 'cert:engine-client:%' OR key LIKE '%$IFACE%'" \
  -c "DELETE FROM certificate_status WHERE resource IN ('engine-tls', 'engine-client', '$IFACE/default')"
fail=""
cd "$P"
mkdir -p ca tls "interfaces/$IFACE"

echo "== the hospital's CA (openssl stands in for it): a root and an issuing intermediate"
openssl req -x509 -newkey rsa:2048 -nodes -keyout ca/root.key -out ca/root.pem -days 3650 -subj "/CN=Demo Hospital Root CA" 2>/dev/null
openssl req -newkey rsa:2048 -nodes -keyout ca/issuing.key -out ca/issuing.csr -subj "/CN=Demo Hospital Issuing CA" 2>/dev/null
printf 'basicConstraints=critical,CA:TRUE\nkeyUsage=critical,keyCertSign,cRLSign\n' >ca/ca.ext
openssl x509 -req -in ca/issuing.csr -CA ca/root.pem -CAkey ca/root.key -CAcreateserial -out ca/issuing.pem -days 1825 -extfile ca/ca.ext 2>/dev/null
# What the PKI team does with a CSR: sign it, keeping the names and uses it asks for.
sign() { # name days
  openssl x509 -req -in "tls/$1.csr" -CA ca/issuing.pem -CAkey ca/issuing.key -CAcreateserial \
    -out "tls/$1.pem" -days "$2" -copy_extensions copy 2>/dev/null
}

echo "== our certificates: a key and a request each (cert request), signed by the CA"
"$BIN" cert request tls/engine-tls --dns localhost --ip 127.0.0.1 --subject "O=Demo Hospital, OU=Interfaces" >request.txt
grep "Send tls/engine-tls.csr to the hospital's CA" request.txt >/dev/null || fail+=" request-words"
[ "$(stat -c %a tls/engine-tls.key)" = 600 ] || fail+=" key-mode"
"$BIN" cert request tls/engine-tls --dns localhost >/dev/null 2>&1 && fail+=" overwrote-a-key"
"$BIN" cert request tls/engine-client --dns ductus.demo.org >/dev/null
"$BIN" cert request tls/sender --dns sender.demo.org --key ecdsa >/dev/null
"$BIN" cert request tls/sink --ip 127.0.0.1 >/dev/null
# The listener's first certificate ends in 20 days (an alert); the others last a year.
sign engine-tls 20
for c in engine-client sender sink; do sign "$c" 365 && cat ca/issuing.pem >>"tls/$c.pem"; done
echo "   engine-tls, engine-client, sender, sink: requested, signed by Demo Hospital Issuing CA"

echo "== cert show: what came back for the listener"
show=$("$BIN" cert show tls/engine-tls.pem || true)
sed 's/^/   | /' <<<"$show"
grep "^⚠ chain    incomplete: only our certificate is served (localhost), not the one that issued it (Demo Hospital Issuing CA)" <<<"$show" >/dev/null || fail+=" show-incomplete"
grep "key      matches (tls/engine-tls.key)" <<<"$show" >/dev/null || fail+=" show-key"
echo "   (the intermediate appended, as the advice says)"
cat ca/issuing.pem >>tls/engine-tls.pem

cat >resources.yaml <<'EOF'
certificates:
  engine-tls: { cert_file: tls/engine-tls.pem, key_file: tls/engine-tls.key, owner: infra@hospital.example }
  engine-client: { cert_file: tls/engine-client.pem, key_file: tls/engine-client.key, owner: infra@hospital.example }
  hospital-ca: { ca_file: ca/root.pem }
EOF
show=$("$BIN" cert show engine-tls) || fail+=" show-by-name"
sed 's/^/   | /' <<<"$show"
grep "order    complete: localhost → Demo Hospital Issuing CA, issued by Demo Hospital Root CA (the CA \`hospital-ca\` in resources.yaml)" <<<"$show" >/dev/null || fail+=" show-complete"

cat >ductus.yaml <<'EOF'
api: { listen: 127.0.0.1:7788 }
alerts:
  smtp: { host: 127.0.0.1, port: 2529, tls: none, from: ductus@hospital.example }
  channels:
    oncall: { email: [interfaces-oncall@hospital.example] }
  rules:
    - { when: [error, stalled, stopped], severity: critical, to: [oncall] }
    - { when: [certificate], severity: warning, to: [oncall, owner] }
  settle: 0s
doctor: { schedule: 1h }
EOF
cat >"interfaces/$IFACE/interface.yaml" <<EOF
name: $IFACE
source:
  mllp:
    port: 6741
    tls: { certificate: engine-tls, clients: { ca: hospital-ca, subjects: ["CN=sender.demo.org"] } }
ordering: { key: PID-3.1 }
destination: { mllp: { host: 127.0.0.1, port: 6742, tls: { ca: hospital-ca, certificate: engine-client } } }
EOF
"$BIN" validate >validate.txt || { cat validate.txt; fail+=" validate"; }

ENGINE="" SINK="" MAIL="" GEN_A="" GEN_B=""
cleanup() {
  for pid in $ENGINE $SINK $MAIL $GEN_A $GEN_B; do kill -INT "$pid" 2>/dev/null || true; done
  wait 2>/dev/null || true
  psql -q "$DB" -c "DELETE FROM alert_events WHERE key LIKE 'cert:engine-tls:%' OR key LIKE 'cert:engine-client:%' OR key LIKE '%$IFACE%'" \
    -c "DELETE FROM alert_state WHERE key LIKE 'cert:engine-tls:%' OR key LIKE 'cert:engine-client:%' OR key LIKE '%$IFACE%'" || true
  "$BIN" token revoke "tls-demo-ops-$$" >/dev/null 2>&1 || true
}
trap cleanup EXIT
wait_for() { # seconds condition
  local end=$((SECONDS + $1))
  until eval "$2"; do
    [ $SECONDS -ge $end ] && return 1
    sleep 0.5
  done
}
mails() { grep -c "^mail " mail.log || true; }
"$BIN" sink --smtp 2529 >mail.log 2>&1 &
MAIL=$!
"$BIN" sink --port 6742 --tls --cert tls/sink.pem --key tls/sink.key --clients-ca ca/root.pem --subject CN=ductus.demo.org >sink.log 2>&1 &
SINK=$!
# This run's messages (earlier runs' stay in the database, under the same interface name).
SINCE=$(q "SELECT now()")
"$BIN" run >run.log 2>&1 &
ENGINE=$!
until grep "started interfaces=" run.log >/dev/null 2>&1; do kill -0 $ENGINE || { cat run.log; exit 1; }; sleep 0.2; done
echo "   engine log: $P/run.log"
OPS=$("$BIN" token create "tls-demo-ops-$$" --role operator 2>/dev/null)
api() { # method path [json]
  local body=()
  [ -n "${3:-}" ] && body=(-H 'Content-Type: application/json' -d "$3")
  curl -s -X "$1" -H "Authorization: Bearer $OPS" "${body[@]}" "$DUCTUS_API_URL/api/v1$2"
}
js() { python3 -c "import json, sys; d = json.loads(sys.argv[1]); print($2)" "$1"; }

echo "== ours in the certificate table, and its alert (20 days left)"
OURS="{c['resource']: c for c in d['certificates'] if c['side'] == 'ours'}"
certs=$(api GET /certificates)
js "$certs" "'\n'.join(f\"   {n}: {c['days_left']} days, presented at {', '.join(c['presented'])}\" for n, c in ($OURS).items())"
[ "$(js "$certs" "($OURS)['engine-tls']['days_left'] in (19, 20)")" = True ] || fail+=" ours-in-table($certs)"
[ "$(js "$certs" "($OURS)['engine-client']['days_left'] >= 364")" = True ] || fail+=" client-in-table"
wait_for 40 '[ "$(mails)" -ge 2 ]' || fail+=" no-alert-mail"
grep "^mail " mail.log | sed 's/^/   | /'
grep "to infra@hospital.example: \[Ductus dev\] WARNING Our certificate engine-tls (on .*) expires in \(19\|20\) days" mail.log >/dev/null || fail+=" alert-to-owner"

echo "== traffic over TLS, with the listener's certificate renewed in the middle"
"$BIN" gen --to 127.0.0.1:6741 --count 600 --rate 40 --connections 2 --patients 20 \
  --ca ca/root.pem --cert tls/sender.pem --key tls/sender.key >gen-a.log 2>&1 &
GEN_A=$!
sleep 5
"$BIN" cert request tls/engine-tls-next --dns localhost --ip 127.0.0.1 --subject "O=Demo Hospital, OU=Interfaces" >/dev/null
sign engine-tls-next 397
cat ca/issuing.pem >>tls/engine-tls-next.pem
"$BIN" cert show tls/engine-tls-next.pem --root . >/dev/null || fail+=" renewal-show"
cp tls/engine-tls-next.pem tls/engine-tls.pem && cp tls/engine-tls-next.key tls/engine-tls.key
echo "   renewed: a new key and a certificate for 397 days copied over the files (gen is still sending)"
api POST /doctor '{"mode": "certificates"}' >/dev/null
grep "certificate renewed: new connections use it" run.log >/dev/null || fail+=" not-reloaded"
"$BIN" gen --to 127.0.0.1:6741 --count 200 --connections 2 --patients 20 --offset 100000 \
  --ca ca/root.pem --cert tls/sender.pem --key tls/sender.key >gen-b.log 2>&1 &
GEN_B=$!
wait $GEN_A || fail+=" gen-a"
wait $GEN_B || fail+=" gen-b"
GEN_A="" GEN_B=""
echo "   before the renewal, still sending on its connections: $(tail -1 gen-a.log)"
echo "   after it, on new connections:                         $(tail -1 gen-b.log)"
for g in gen-a gen-b; do
  grep "retries 0, rejected 0" $g.log >/dev/null || fail+=" $g-retried($(cat $g.log))"
done
wait_for 30 'grep "received 800 | unique 800 | duplicates 0 | out of order 0" sink.log >/dev/null' || fail+=" sink($(tail -1 sink.log))"
echo "   sink: $(tail -1 sink.log)"
THIS_RUN="interface = '$IFACE' AND received_at >= '$SINCE'"
counts=$(q "SELECT string_agg(status || ':' || n, ',' ORDER BY status) FROM (SELECT status, count(*) n FROM messages WHERE $THIS_RUN GROUP BY status) s")
[ "$counts" = "1:800" ] || fail+=" engine-counts($counts)"
failed=$(q "SELECT count(*) FROM deliveries d JOIN messages m ON m.id = d.message_id AND m.received_at = d.received_at WHERE m.interface = '$IFACE' AND m.received_at >= '$SINCE' AND (d.attempts > 0 OR d.status <> 1)")
[ "$failed" = 0 ] || fail+=" deliveries-retried($failed)"
echo "   engine: 800 stored, 800 sent, none retried"

echo "== the alert recovers; the table shows the new certificate"
wait_for 40 'grep "RECOVERED Our certificate engine-tls" mail.log >/dev/null' || fail+=" no-recovery"
grep "RECOVERED Our certificate engine-tls" mail.log | sed 's/^/   | /'
certs=$(api GET /certificates)
[ "$(js "$certs" "($OURS)['engine-tls']['days_left'] in (396, 397)")" = True ] || fail+=" renewed-in-table($certs)"

echo "== doctor (full): the chain our listener serves, and the TLS connection to the sink"
report=$(api POST /doctor '{"mode": "full", "timeout_ms": 3000}')
step() { js "$report" "[s['outcome'] + ' ' + s['detail'] for c in d['checks'] if c['endpoint'].startswith('$1') for s in c['steps'] if s['name'] == '$2']"; }
chain=$(step 0.0.0.0:6741 chain)
served=$(step 0.0.0.0:6741 TLS)
echo "   listener: $served"
echo "   chain:    $chain"
[[ "$chain" == *"ok complete: localhost → Demo Hospital Issuing CA, issued by Demo Hospital Root CA (the CA \`hospital-ca\` in resources.yaml)"* ]] || fail+=" doctor-chain($chain)"
until=$(date -u -d "+397 days" +%Y-%m-%d)
[[ "$served" == *"$until"* || "$served" == *"$(date -u -d "+396 days" +%Y-%m-%d)"* ]] || fail+=" doctor-serves-new($served)"
out=$(js "$report" "[c['outcome'] for c in d['checks'] if c['endpoint'].startswith('127.0.0.1:6742')]")
echo "   destination 127.0.0.1:6742 (MLLP over TLS, our client certificate): $out"
[ "$out" = "['ok']" ] || fail+=" doctor-destination($report)"

echo "== a sender without a client certificate: refused, and the log says why"
timeout 2 "$BIN" gen --to 127.0.0.1:6741 --count 1 --ca ca/root.pem 2>&1 | sed -n 1p | sed 's/^/   gen: /' || true
wait_for 5 'grep "TLS handshake refused.*reason=the sender presented no client certificate" run.log >/dev/null' || fail+=" refusal-not-logged"
grep "TLS handshake refused" run.log | sed -n 1p | sed 's/^.*WARN /   engine: /'
grep " ERROR " run.log >/dev/null && fail+=" errors-in-the-log"

if [ -z "$fail" ]; then echo PASS; else echo "FAIL:$fail"; tail -20 run.log; exit 1; fi
