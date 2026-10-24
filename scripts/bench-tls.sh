#!/usr/bin/env bash
# TLS against plain, measured on one engine (0.6.3): what TLS costs a sender and the engine.
#   1. Strict MLLP ingest (ACKs per second) over TLS and plain, with 1 and 16 connections.
#   2. Handshake time (a sender's connect to the end of its handshake), full and resumed: an
#      ECDSA P-256 certificate (`cert self-signed`), an RSA 2048 one (what many hospital CAs
#      issue), and ECDSA with a client certificate checked. Post-quantum key exchange (the
#      default since 0.6.4.1) against X25519 only (a peer without it).
#   3. The engine's memory per idle connection, 1,000 held open, TLS and plain (one engine each).
#   4. Respond mode over HTTPS and HTTP: p50/p95 with keep-alive and with a new connection (and a
#      full handshake) per request.
#
#   ROUNDS=2 SINGLE=2000 MANY=8000 bash scripts/bench-tls.sh
#
# Not a pass/fail demo: it prints what it measured. Needs openssl (the RSA certificate) and the
# tls-bench example (built if missing).
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/dev/db.sh"

BIN=$(realpath "${BIN:-${CARGO_TARGET_DIR:-target}/release/ductus}")
TB=${TLS_BENCH:-$(dirname "$BIN")/examples/tls-bench}
if [ ! -x "$TB" ]; then
  (cd "$(dirname "${BASH_SOURCE[0]}")/.." && cargo build --release -q -p ductus-runtime --example tls-bench)
fi
ROUNDS=${ROUNDS:-2}
SINGLE=${SINGLE:-2000}
MANY=${MANY:-8000}
HELD=${HELD:-1000}
export DUCTUS_DATABASE_URL
# 1,000 connections held, and the engine's own descriptors.
ulimit -n 8192
P=$(mktemp -d)
cd "$P"
mkdir -p tls
"$BIN" cert self-signed tls/ecdsa --name localhost --name 127.0.0.1 >/dev/null
"$BIN" cert self-signed tls/client --name bench-client >/dev/null
# RSA as a hospital CA issues it: our request, signed by a CA (openssl stands in for it).
openssl req -x509 -newkey rsa:2048 -nodes -keyout tls/ca.key -out tls/ca.pem -days 30 -subj "/CN=Bench CA" 2>/dev/null
"$BIN" cert request tls/rsa --dns localhost --ip 127.0.0.1 --key rsa >/dev/null
openssl x509 -req -in tls/rsa.csr -CA tls/ca.pem -CAkey tls/ca.key -CAcreateserial -out tls/rsa.pem -days 30 -copy_extensions copy 2>/dev/null
cat >resources.yaml <<'EOF'
certificates:
  ecdsa: { cert_file: tls/ecdsa.pem, key_file: tls/ecdsa.key }
  rsa: { cert_file: tls/rsa.pem, key_file: tls/rsa.key }
  clients: { ca_file: tls/client.pem }
EOF
iface() { # name port tls
  mkdir -p "interfaces/$1"
  printf 'name: %s\nsource: { mllp: { port: %s%s } }\nordering: { key: PID-3.1 }\ndestination: { file: { dir: out } }\n' \
    "$1" "$2" "${3:+, tls: $3}" >"interfaces/$1/interface.yaml"
}
iface bench-plain 6761 ""
iface bench-tls 6762 "{ certificate: ecdsa }"
iface bench-rsa 6763 "{ certificate: rsa }"
iface bench-mtls 6764 "{ certificate: ecdsa, clients: { ca: clients } }"
for t in plain:6765: tls:6766:", tls: { certificate: ecdsa }"; do
  IFS=: read -r kind port tls <<<"$t"
  mkdir -p "interfaces/bench-http-$kind"
  printf 'name: bench-http-%s\nsource: { http: { port: %s, path: /ping, method: GET, mode: respond%s } }\nformat: json\nscript: transform.ts\n' \
    "$kind" "$port" "$tls" >"interfaces/bench-http-$kind/interface.yaml"
  printf 'export function transform() {\n  return { ok: true };\n}\n' >"interfaces/bench-http-$kind/transform.ts"
done

ENGINE=""
start() {
  "$BIN" run --api-listen off >run.log 2>&1 &
  ENGINE=$!
  until grep "started interfaces=" run.log >/dev/null 2>&1; do kill -0 $ENGINE || { cat run.log; exit 1; }; sleep 0.2; done
}
stop() { kill -INT $ENGINE; wait $ENGINE || true; }
trap 'kill -INT $ENGINE 2>/dev/null || true' EXIT
rss() { awk '/^VmRSS/ { print $2 }' /proc/$ENGINE/status; } # kB
start
echo "engine log: $P/run.log"

echo "== strict MLLP ingest, ACKs per second (gen; each round alternates plain and TLS)"
TLSARGS=(--ca tls/ecdsa.pem)
for round in $(seq 1 "$ROUNDS"); do
  for c in 1 16; do
    n=$([ $c = 1 ] && echo "$SINGLE" || echo "$MANY")
    plain=$("$BIN" gen --to 127.0.0.1:6761 --count "$n" --connections $c --patients 400 --offset $((round * 1000000)) | sed -n 's/.*(\([0-9]*\)\/s).*/\1/p')
    tls=$("$BIN" gen --to 127.0.0.1:6762 --count "$n" --connections $c --patients 400 --offset $((round * 1000000)) "${TLSARGS[@]}" | sed -n 's/.*(\([0-9]*\)\/s).*/\1/p')
    printf "   round %s, %2s connection(s), %5s messages: plain %5s/s, TLS %5s/s\n" "$round" $c "$n" "$plain" "$tls"
  done
done

echo "== handshake: a sender's connect to the end of its handshake (500 one after another)"
"$TB" connect 127.0.0.1:6761 500 | sed 's/^/   plain TCP            /'
"$TB" connect 127.0.0.1:6762 500 --ca tls/ecdsa.pem | sed 's/^/   ECDSA, full          /'
"$TB" connect 127.0.0.1:6762 500 --ca tls/ecdsa.pem --classic | sed 's/^/   ECDSA, X25519 only   /'
"$TB" connect 127.0.0.1:6762 500 --ca tls/ecdsa.pem --resume | sed 's/^/   ECDSA, resumed       /'
"$TB" connect 127.0.0.1:6763 500 --ca tls/ca.pem | sed 's/^/   RSA 2048, full       /'
"$TB" connect 127.0.0.1:6763 500 --ca tls/ca.pem --classic | sed 's/^/   RSA 2048, X25519 only/'
"$TB" connect 127.0.0.1:6764 500 --ca tls/ecdsa.pem --cert tls/client.pem --key tls/client.key | sed 's/^/   ECDSA + client cert  /'

echo "== respond mode, 2,000 GETs one after another (p50/p95)"
"$TB" http http://127.0.0.1:6765/ping 2000 | sed 's/^/   HTTP,  /'
"$TB" http http://127.0.0.1:6765/ping 1000 --new | sed 's/^/   HTTP,  /'
"$TB" http https://127.0.0.1:6766/ping 2000 --ca tls/ecdsa.pem | sed 's/^/   HTTPS, /'
"$TB" http https://127.0.0.1:6766/ping 1000 --ca tls/ecdsa.pem --new | sed 's/^/   HTTPS, /'

echo "== memory: $HELD idle connections held open (a fresh engine each)"
for kind in tls plain; do
  stop
  start
  sleep 2
  before=$(rss)
  if [ $kind = tls ]; then
    "$TB" hold 127.0.0.1:6762 "$HELD" --ca tls/ecdsa.pem >hold.log &
  else
    "$TB" hold 127.0.0.1:6761 "$HELD" >hold.log &
  fi
  HOLD=$!
  until grep holding hold.log >/dev/null 2>&1; do kill -0 $HOLD || { cat hold.log; exit 1; }; sleep 0.2; done
  sleep 3
  after=$(rss)
  kill $HOLD
  wait $HOLD 2>/dev/null || true
  awk -v k=$kind -v b="$before" -v a="$after" -v n="$HELD" \
    'BEGIN { printf "   %-5s engine %6d kB → %6d kB: %5.1f kB per connection\n", k, b, a, (a - b) / n }'
done
stop
echo "binary: $(stat -c %s "$BIN") bytes"
