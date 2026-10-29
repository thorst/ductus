#!/usr/bin/env bash
# What `ductus replay` costs: an engine records COUNT ADT messages through one interface with three
# destinations (an HL7 mapping kept with `save_output`, a JSON mapping, the message as is); then the
# replay reads them back and runs them again, alone (against the record and kept outputs) and with a
# second version run beside it. Prints messages per second for each.
#
#   COUNT=100000 bash scripts/bench-replay.sh
#
# Not a pass/fail demo, but it fails if a replay of unchanged code finds a difference. Its messages
# stay in the database under their own interface name (retention removes them).
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/dev/db.sh"

BIN=$(realpath "${BIN:-${CARGO_TARGET_DIR:-target}/release/ductus}")
COUNT=${COUNT:-100000}
CONNECTIONS=${CONNECTIONS:-16}
PORT=${PORT:-6697}
NAME="bench-replay-$(date +%s)"
P=$(mktemp -d)
trap 'kill $ENGINE 2>/dev/null || true; rm -rf "$P"' EXIT
mkdir -p "$P/interfaces/$NAME"
cat >"$P/interfaces/$NAME/interface.yaml" <<EOF
name: $NAME
source: { mllp: { port: $PORT } }
script: transform.ts
durability: relaxed
destinations:
  lab: { file: { dir: out/lab }, save_output: true }
  billing: { file: { dir: out/billing }, transform: toBilling, output: json }
  archive: { file: { dir: out/archive }, transform: asIs }
EOF
cat >"$P/interfaces/$NAME/transform.ts" <<'EOF'
import { type Message } from "@ductus/sdk";

export function transform(m: Message) {
  m.set("MSH-5", "LAB");
  m.set("PID-5", m.PID[5].toString().toUpperCase());
  m.set("PV1-3", "WARD-" + m.PID[3][1].slice(-2));
  return m;
}

export function toBilling(m: Message) {
  return { mrn: m.PID[3][1], name: m.PID[5][1], event: m.MSH[9][2], at: new Date().toISOString() };
}

export function asIs(m: Message) {
  return m;
}
EOF

cd "$P"
"$BIN" run --api-listen off >"$P/run.log" 2>&1 &
ENGINE=$!
until grep -q "started interfaces=" "$P/run.log"; do kill -0 $ENGINE || { cat "$P/run.log"; exit 1; }; sleep 0.2; done
since=$(date -u +%Y-%m-%dT%H:%M:%SZ)
"$BIN" gen --to 127.0.0.1:$PORT --count "$COUNT" --patients 5000 --connections "$CONNECTIONS" | tail -1 >&2
q() { psql -tA "$DUCTUS_DATABASE_URL" -c "$1"; }
until [ "$(q "SELECT count(*) FROM messages WHERE interface = '$NAME' AND status = 0")" = 0 ]; do sleep 0.5; done
kill -INT $ENGINE; wait $ENGINE 2>/dev/null || true
cp "$BIN" "$P/other-ductus"

printf "%-34s %10s %10s %12s\n" replay messages seconds "messages/s"
measure() {
  local label=$1; shift
  local start end
  start=$(date +%s%N)
  "$BIN" replay --since "$since" --root "$P" --json "$@" >"$P/replay.json" 2>"$P/replay.err" || {
    echo "FAIL: $label found differences or failed"; tail -5 "$P/replay.err"; head -c 2000 "$P/replay.json"; exit 1
  }
  end=$(date +%s%N)
  local s n
  s=$(bc -l <<<"($end - $start) / 1000000000")
  n=$(sed -n 's/^  "messages": \([0-9]*\),/\1/p' "$P/replay.json")
  printf "%-34s %10s %10.1f %12.0f\n" "$label" "$n" "$s" "$(bc -l <<<"$n / $s")"
}
measure "alone (record, kept outputs)" --against stored
measure "beside a second version" --against "$P/other-ductus"
