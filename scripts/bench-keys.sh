#!/usr/bin/env bash
# What a key function (`ordering: { key: { script: key.ts } }`, 0.7.7.3) costs at intake against a
# path that reads the same key.
#
#   COUNT=20000 CONNECTIONS=4 RUNS=3 bash scripts/bench-keys.sh
#
# Under `ductus dev` (its own Postgres, kept between runs), two interfaces alike but for the key:
# `bypath` (`PID-3[.4=MRN].1`) and `byscript` (a key.ts returning `msg.get("PID-3[.4=MRN].1")`), each a
# file destination. Each takes COUNT messages from `ductus gen` twice: as fast as its ACKs come
# back (ingest msg/s), then at 1,000 a second (the longest a message waited for its ACK). Not a
# pass/fail demo. The project lives under ~/.cache (WSL's /tmp is tmpfs: no disk numbers there).
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
BIN=$(realpath "${BIN:-${CARGO_TARGET_DIR:-target}/release/ductus}")
COUNT=${COUNT:-20000}
CONNECTIONS=${CONNECTIONS:-4}
RUNS=${RUNS:-3}
P=${BENCH_DIR:-$HOME/.cache/ductus-bench-keys}
PATHS=6701 SCRIPTS=6702
rm -rf "$P/interfaces"
mkdir -p "$P/interfaces/bypath" "$P/interfaces/byscript"
cat >"$P/interfaces/bypath/interface.yaml" <<EOF
name: bypath
source: { mllp: { host: 127.0.0.1, port: $PATHS } }
ordering: { key: 'PID-3[.4=MRN].1' }
destinations: { files: { file: { dir: out } } }
EOF
cat >"$P/interfaces/byscript/interface.yaml" <<EOF
name: byscript
source: { mllp: { host: 127.0.0.1, port: $SCRIPTS } }
ordering: { key: { script: key.ts } }
destinations: { files: { file: { dir: out } } }
EOF
printf 'import type { Message } from "@ductus/sdk";\n\nexport function key(msg: Message): string {\n  return msg.get("PID-3[.4=MRN].1");\n}\n' \
  >"$P/interfaces/byscript/key.ts"

ENGINE=""
cleanup() { [ -n "$ENGINE" ] && kill -TERM "$ENGINE" 2>/dev/null && wait "$ENGINE" 2>/dev/null || true; }
trap cleanup EXIT

rm -f "$P/dev.log"
(cd "$P" && exec "$BIN" dev --api-listen off) >"$P/dev.log" 2>&1 &
ENGINE=$!
until grep -q 'started interfaces=' "$P/dev.log" 2>/dev/null; do
  kill -0 "$ENGINE" 2>/dev/null || { cat "$P/dev.log"; exit 1; }
  sleep 0.05
done
URL=$(grep -o 'postgresql://[^ ]*' "$P/dev.log" | sed -n 1p)
q() { psql -tA "$URL" -c "$1"; }

# send NAME PORT RATE RUN: COUNT messages; prints gen's rate and its longest wait.
send() {
  local out
  out=$("$BIN" gen --to "127.0.0.1:$2" --count "$COUNT" --patients 500 --connections "$CONNECTIONS" --rate "$3" \
    --offset $(($4 * 1000000 + $3)) | tail -n 1)
  until [ "$(q "SELECT count(*) FROM messages WHERE interface = '$1' AND status = 0")" = 0 ]; do sleep 0.05; done
  printf "%-9s %6s %4s %12s %14s\n" "$1" "${3/#0/max}" "$4" "$(sed -E 's/.*\(([0-9]+)\/s\).*/\1/' <<<"$out")" \
    "$(sed -E 's/.*longest wait ([0-9]+) ms.*/\1/' <<<"$out")"
}

echo "== $COUNT messages, $CONNECTIONS connections (the MLLP sender's ACKs)"
printf "%-9s %6s %4s %12s %14s\n" key rate run "ingest msg/s" "longest ms"
for r in $(seq 1 "$RUNS"); do
  for rate in 0 1000; do
    send bypath $PATHS $rate "$r"
    send byscript $SCRIPTS $rate "$r"
  done
done
echo "== keys stored (byscript's must match bypath's)"
q "SELECT interface, count(DISTINCT ordering_key), count(*) FILTER (WHERE ordering_key IS NULL OR ordering_key = '') FROM messages WHERE interface IN ('bypath', 'byscript') GROUP BY 1 ORDER BY 1"
