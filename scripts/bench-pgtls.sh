#!/usr/bin/env bash
# What TLS to the engine's own database costs (0.6.4.2), on one Postgres: a throwaway server with
# its own CA (scripts/dev/pgtls.sh) whose data is on this machine's disk, plain (sslmode=disable)
# against verified TLS (sslmode=verify-full).
#   1. Strict MLLP ingest, ACKs per second (each waits for its commit), with 1 and 16 sender
#      connections: a fresh engine per mode, the modes alternating each round.
#   2. A durable commit (ductus doctor's median of 20).
#   3. Opening a connection as the store's pool does (TLS handshake and login; sqlx reads the
#      system's certificates at each): 200 one after another, disable, prefer and verify-full.
#
#   ROUNDS=2 SINGLE=2000 MANY=8000 bash scripts/bench-pgtls.sh
#
# Not a pass/fail demo: it prints what it measured. Needs the Postgres server binaries and the
# tls-bench example (built if missing). The server's data goes under ~/ (/tmp is tmpfs in WSL).
set -euo pipefail
DEV=$(realpath "$(dirname "${BASH_SOURCE[0]}")/dev")
BIN=$(realpath "${BIN:-${CARGO_TARGET_DIR:-target}/release/ductus}")
TB=${TLS_BENCH:-$(dirname "$BIN")/examples/tls-bench}
if [ ! -x "$TB" ]; then
  (cd "$DEV/../.." && cargo build --release -q -p ductus-runtime --example tls-bench)
fi
ROUNDS=${ROUNDS:-2}
SINGLE=${SINGLE:-2000}
MANY=${MANY:-8000}
PG=$(mktemp -d "$HOME/ductus-bench-pgtls.XXXX")
PORT=$(python3 -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')
P=$(mktemp -d)
ENGINE=""
cleanup() {
  [ -n "$ENGINE" ] && kill -INT "$ENGINE" 2>/dev/null && wait "$ENGINE" 2>/dev/null || true
  bash "$DEV/pgtls.sh" stop "$PG" || true
  rm -rf "$PG"
}
trap cleanup EXIT
bash "$DEV/pgtls.sh" start "$PG" "$PORT"
url() { echo "postgres://ductus:ductus@127.0.0.1:$PORT/ductus?$1"; }
PLAIN=$(url sslmode=disable)
VERIFIED=$(url "sslmode=verify-full&sslrootcert=$PG/ca.pem")

cd "$P"
mkdir -p interfaces/bench-db
printf 'name: bench-db\nsource: { mllp: { port: 6767 } }\nordering: { key: PID-3.1 }\ndestination: { file: { dir: out } }\n' \
  >interfaces/bench-db/interface.yaml
start() { # url
  DUCTUS_DATABASE_URL=$1 "$BIN" run --api-listen off >run.log 2>&1 &
  ENGINE=$!
  until grep "started interfaces=" run.log >/dev/null 2>&1; do kill -0 $ENGINE || { cat run.log; exit 1; }; sleep 0.2; done
}
stop() { kill -INT $ENGINE; wait $ENGINE || true; ENGINE=""; }
rate() { # connections count offset
  "$BIN" gen --to 127.0.0.1:6767 --count "$2" --connections "$1" --patients 400 --offset "$3" | sed -n 's/.*(\([0-9]*\)\/s).*/\1/p'
}

echo "== strict MLLP ingest, ACKs per second (a fresh engine per mode; the modes alternate)"
for round in $(seq 1 "$ROUNDS"); do
  declare -A got=()
  for mode in plain verified; do
    if [ $mode = plain ]; then start "$PLAIN"; else start "$VERIFIED"; fi
    got[$mode,1]=$(rate 1 "$SINGLE" $((round * 1000000)))
    got[$mode,16]=$(rate 16 "$MANY" $((round * 1000000 + 500000)))
    stop
  done
  for c in 1 16; do
    printf "   round %s, %2s connection(s): plain %5s/s, verified TLS %5s/s\n" "$round" $c "${got[plain,$c]}" "${got[verified,$c]}"
  done
done

echo "== a durable commit (ductus doctor, median of 20)"
for mode in plain verified; do
  u=$PLAIN
  [ $mode = verified ] && u=$VERIFIED
  printf "   %-8s %s\n" $mode "$(DUCTUS_DATABASE_URL=$u "$BIN" doctor 2>&1 | grep -o 'commits .* median of 20' | sed -n 1p)"
done

echo "== opening a connection (TLS and login), 200 one after another"
"$TB" pg-connect "$PLAIN" 200 | sed 's/^/   disable      /'
"$TB" pg-connect "$(url sslmode=prefer)" 200 | sed 's/^/   prefer       /'
"$TB" pg-connect "$VERIFIED" 200 | sed 's/^/   verify-full  /'
echo "binary: $(stat -c %s "$BIN") bytes"
