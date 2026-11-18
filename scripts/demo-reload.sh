#!/usr/bin/env bash
# `ductus reload` (0.7.5.3.1): an engine in folder mode applies the folder as it is now, in place.
# Two interfaces, A and B, each relaying ADT to a sink while a sender sends to each:
#   1. A's code changes (a file it imports): `ductus reload` applies it in place: the same engine,
#      A alone reloaded, neither sender refused, every message delivered once, A's later messages
#      tagged by the new code; `ductus history` says "in place by a reload";
#   2. an interface folder added starts (it takes messages), removed stops;
#   3. a broken interface.yaml is refused, nothing changed;
#   4. ductus.yaml changed: refused with `--in-place-only`, then applied by the handover (a new
#      engine, the senders never refused).
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/dev/db.sh"

BIN=$(realpath "${BIN:-${CARGO_TARGET_DIR:-target}/release/ductus}")
DB=$DUCTUS_DATABASE_URL
WORK=$(mktemp -d)
P=$WORK/project
export DUCTUS_CONTROL=$WORK/control
SUFFIX=$$
A=reload-a-$SUFFIX B=reload-b-$SUFFIX C=reload-c-$SUFFIX
PA=26661 PB=26662 PC=26663 SINK=26669
ENGINE="" SINK_PID=""
cleanup() {
  set +e
  pkill -INT -f "ductus run --root $P"
  [ -n "$SINK_PID" ] && kill -INT "$SINK_PID"
  sleep 1
}
trap cleanup EXIT

fail=""
check() { # check <name> <condition...>: record a failure instead of stopping at the first
  local name=$1
  shift
  if ! eval "$@"; then fail+=" $name"; echo "   ✗ $name"; fi
}

interface() { # interface <name> <port>
  mkdir -p "$P/interfaces/$1"
  printf 'name: %s\nsource: { mllp: { host: 127.0.0.1, port: %d } }\ndestinations: { lab: { mllp: { host: 127.0.0.1, port: %d }, script: tag.ts } }\n' \
    "$1" "$2" "$SINK" >"$P/interfaces/$1/interface.yaml"
  cat >"$P/interfaces/$1/tag.ts" <<'EOF'
import type { Message } from "@ductus/sdk";
import { TAG } from "../../lib/tag.ts";

export function transform(msg: Message) {
  msg.set("MSH-5", TAG);
  return msg;
}
EOF
}
mkdir -p "$P/lib" "$P/environments"
printf 'variables: {}\n' >"$P/environments/dev.yaml"
printf 'api: { listen: "off" }\n' >"$P/ductus.yaml"
printf 'export const TAG = "LAB1";\n' >"$P/lib/tag.ts"
interface "$A" $PA
interface "$B" $PB
# B tags with its own value: a change to A's file doesn't reach it.
sed -i 's|import { TAG } from "../../lib/tag.ts";|const TAG = "B1";|' "$P/interfaces/$B/tag.ts"
git -C "$P" init -q -b main
git -C "$P" -c user.name=demo -c user.email=demo@example.org add .
git -C "$P" -c user.name=demo -c user.email=demo@example.org commit -qm "two interfaces"

"$BIN" sink --port $SINK >"$WORK/sink.log" 2>&1 &
SINK_PID=$!
LOG=$WORK/engine.log
"$BIN" run --root "$P" --threads 2 >"$LOG" 2>&1 &
ENGINE=$!
for _ in $(seq 1 300); do grep -q "deployed:" "$LOG" && break; sleep 0.1; done
grep -q "deployed:" "$LOG" || { echo "the engine didn't start:"; tail -5 "$LOG"; exit 1; }
pid() { psql -qAt "$DB" -c "SELECT pid FROM engines e JOIN interface_owners o ON o.instance = e.instance WHERE o.interface = '$1' AND e.stopped_at IS NULL"; }
reload() { "$BIN" reload --control "$DUCTUS_CONTROL" "$@"; }
send() { # send <port> <count> <offset>: in the background, its log in gen-<port>.log
  "$BIN" gen --to "127.0.0.1:$1" --count "$2" --rate 50 --patients 20 --offset "$3" >"$WORK/gen-$1.log" 2>&1 &
}

echo "== 1. A's shared file changes while both send: ductus reload"
first=$(pid "$A")
send $PA 300 0
GA=$!
send $PB 300 100000
GB=$!
sleep 2
printf 'export const TAG = "LAB2";\n' >"$P/lib/tag.ts"
git -C "$P" -c user.name=demo -c user.email=demo@example.org commit -qam "lab wants MSH-5 = LAB2"
lines=$(wc -l <"$LOG")
echo '$ ductus reload -m "lab wants MSH-5 = LAB2"'
reload -m "lab wants MSH-5 = LAB2" | tee "$WORK/reload1.out"
wait $GA $GB
check one-engine '[ "$(pid "$A")" = "$first" ] && [ "$(pid "$B")" = "$first" ]'
check in-place 'grep -q "took its new code between two messages" "$WORK/reload1.out"'
check a-alone '! tail -n +$((lines + 1)) "$LOG" | grep "its new code runs" | grep -q "interface=$B"'
for port in $PA $PB; do
  check "sender-$port" 'grep -q "retries 0, rejected 0, refused 0, dropped 0" "$WORK/gen-$port.log"'
done
for _ in $(seq 1 60); do grep -q "unique 600 " "$WORK/sink.log" && break; sleep 0.5; done
check delivered-once 'tail -1 "$WORK/sink.log" | grep -q "unique 600 .*duplicates 0"'
history=$("$BIN" history "$A" --root "$P" --database-url "$DB" 2>&1)
check history 'grep -q "in place by a reload: lab wants MSH-5 = LAB2" <<<"$history"'
tagged=$(psql -qAt "$DB" -c "SELECT count(*) FROM messages m JOIN deliveries d ON d.message_id = m.id
  JOIN code_versions v ON v.id = d.code_version WHERE m.interface = '$A' AND v.hash = (
  SELECT c.hash FROM code_versions c JOIN interface_versions i ON i.version_id = c.id WHERE i.interface = '$A')")
echo "   A: $tagged of 300 decided by the new code"
check new-code '[ "$tagged" -gt 0 ] && [ "$tagged" -lt 300 ]'

echo "== 2. an interface added, then removed"
interface "$C" $PC
reload -m "C added" >"$WORK/reload2.out"
check c-runs '[ "$(pid "$C")" = "$first" ]'
"$BIN" gen --to "127.0.0.1:$PC" --count 5 --patients 5 --offset 200000 >"$WORK/gen-c.log" 2>&1
check c-takes 'grep -q "sent 5 messages" "$WORK/gen-c.log"'
rm -rf "${P:?}/interfaces/$C"
reload -m "C removed" >"$WORK/reload3.out"
check c-stopped '[ -z "$(pid "$C")" ]'

echo "== 3. a broken interface.yaml: refused, nothing changed"
cp "$P/interfaces/$A/interface.yaml" "$WORK/a.yaml"
printf 'name: [broken\n' >"$P/interfaces/$A/interface.yaml"
if reload >"$WORK/reload4.out" 2>&1; then fail+=" broken-refused"; fi
check broken-said 'grep -q "the folder doesn'"'"'t load" "$WORK/reload4.out"'
cp "$WORK/a.yaml" "$P/interfaces/$A/interface.yaml"

echo "== 4. ductus.yaml changed: refused with --in-place-only, then by the handover"
printf 'api: { listen: "off" }\nisolates: { heap_mb: 256 }\n' >"$P/ductus.yaml"
if reload --in-place-only >"$WORK/reload5.out" 2>&1; then fail+=" in-place-only-refused"; fi
check names-the-file 'grep -q "ductus.yaml changed" "$WORK/reload5.out"'
check still-the-same '[ "$(pid "$A")" = "$first" ]'
send $PB 200 300000
GB=$!
sleep 1
reload -m "smaller heap" >"$WORK/reload6.out"
wait $GB
check handed-over 'grep -q "by the handover" "$WORK/reload6.out"'
for _ in $(seq 1 50); do [ -n "$(pid "$A")" ] && [ "$(pid "$A")" != "$first" ] && break; sleep 0.2; done
check new-engine '[ -n "$(pid "$A")" ] && [ "$(pid "$A")" != "$first" ]'
# A handover closes each connection after its last ACK: a frame the sender had already written
# behind it is resent (`dropped` counts those), never refused, never lost.
check sender-handover 'grep -q "sent 200 messages.*rejected 0, refused 0" "$WORK/gen-$PB.log"'
for _ in $(seq 1 60); do grep -q "unique 805 " "$WORK/sink.log" && break; sleep 0.5; done
check all-delivered-once 'tail -1 "$WORK/sink.log" | grep -q "unique 805 .*duplicates 0"'

echo "logs: $WORK"
if [ -z "$fail" ]; then echo PASS; else echo "FAIL:$fail"; exit 1; fi
