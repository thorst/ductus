#!/usr/bin/env bash
# v0.7 demo (0.7.2.2): `ductus try` on a stored message. An engine receives an ADT; then, with the
# engine still running and nothing sent again:
#   1. `ductus try --message ID` reads it from the database: each destination decides as it did,
#      and says so beside what it recorded then;
#   2. the code changed (billing now drops patient updates): the same message, decided by the new
#      code, with the record beside it;
#   3. through the running engine's API (`--from`): a token with `payloads` reads it (the read is in
#      the engine's audit log); a token without is refused in words;
#   4. nothing was sent: the receiver got the one message, once.
# Each record names the code version that decided it (0.7.3.2): the same as the code tried at first
# (nothing said), another once the code changed (said under each `then:`, from the database and
# through the API alike).
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/dev/db.sh"
BIN=$(realpath "${BIN:-${CARGO_TARGET_DIR:-target}/release/ductus}")
DB=$DUCTUS_DATABASE_URL
P=$(mktemp -d)
export DUCTUS_DATABASE_URL=$DB
q() { psql -tA "$DB" -c "$1"; }
SUFFIX=$$
NAME=try-demo
fail=""

mkdir -p "$P/interfaces/adt"
printf 'api: { listen: 127.0.0.1:7796 }\n' >"$P/ductus.yaml"
cat >"$P/interfaces/adt/interface.yaml" <<EOF
name: $NAME
source: { mllp: { port: 6726 } }
script: route.ts
destinations:
  lab:
    mllp: { host: 127.0.0.1, port: 6727 }
    transform: toLab
    save_output: true
  billing:
    file: { dir: out }
    transform: toBilling
EOF
cat >"$P/interfaces/adt/route.ts" <<'EOF'
import type { Message } from "@ductus/sdk";
export function toLab(msg: Message) {
  msg.set("MSH-5", "LAB");
  return msg;
}
export function toBilling(msg: Message) {
  return msg;
}
EOF
cd "$P"
"$BIN" sink --port 6727 >"$P/sink.log" 2>&1 &
SINK=$!
"$BIN" run >"$P/run.log" 2>&1 &
ENGINE=$!
cleanup() {
  kill -INT $ENGINE $SINK 2>/dev/null || true
  for t in "try-demo-reader-$SUFFIX" "try-demo-viewer-$SUFFIX"; do "$BIN" token revoke "$t" >/dev/null 2>&1 || true; done
}
trap cleanup EXIT
until grep "started interfaces=" "$P/run.log" >/dev/null 2>&1; do kill -0 $ENGINE || { cat "$P/run.log"; exit 1; }; sleep 0.2; done
"$BIN" gen --to 127.0.0.1:6726 --count 1 >/dev/null
before=$(q "SELECT count(*) FROM messages WHERE interface = '$NAME' AND status = 1")
for _ in $(seq 1 50); do
  [ "$(q "SELECT count(*) FROM messages WHERE interface = '$NAME' AND status = 1")" -gt "$before" ] && break
  sleep 0.2
done
ID=$(q "SELECT max(id) FROM messages WHERE interface = '$NAME'")
echo "== the engine took message $ID; ductus try --message $ID (from the database)"
"$BIN" try interfaces/adt --message "$ID" | tee "$P/1.txt"

echo "== the code changed: billing drops patient updates (A08) now"
sed -i 's|^export function toBilling(msg: Message) {$|export function toBilling(msg: Message) {\n  if (msg.MSH[9][2] === "A08") return null;|' interfaces/adt/route.ts
"$BIN" try interfaces/adt --message "$ID" | tee "$P/2.txt"

echo "== through the engine's API (--from), with a token that may read payloads"
READER=$("$BIN" token create "try-demo-reader-$SUFFIX" --role viewer --permission payloads 2>/dev/null)
VIEWER=$("$BIN" token create "try-demo-viewer-$SUFFIX" --role viewer 2>/dev/null)
DUCTUS_API_TOKEN=$READER "$BIN" try interfaces/adt --message "$ID" --from http://127.0.0.1:7796 | tee "$P/3.txt"
echo "== and one that may not"
DUCTUS_API_TOKEN=$VIEWER "$BIN" try interfaces/adt --message "$ID" --from http://127.0.0.1:7796 >"$P/4.txt" 2>&1 || true
cat "$P/4.txt"

grep -q "^$NAME: message $ID, received " "$P/1.txt" || fail+=" header"
grep -q "^  lab: sent$" "$P/1.txt" || fail+=" lab-sent"
grep -q "^    then: sent, the same output$" "$P/1.txt" || fail+=" lab-same-output"
grep -q "^  billing: sent$" "$P/1.txt" || fail+=" billing-sent"
grep -q "^    then: sent (the same decision)$" "$P/1.txt" || fail+=" billing-same"
grep -q "^  billing: filtered$" "$P/2.txt" || fail+=" changed-filtered"
grep -q "^    then: sent$" "$P/2.txt" || fail+=" changed-then-sent"
grep -q "^$NAME: message $ID from http://127.0.0.1:7796, received " "$P/3.txt" || fail+=" api-header"
grep -q "^    then: sent, the same output$" "$P/3.txt" || fail+=" api-recorded"
grep -q "the token may not read payloads" "$P/4.txt" || fail+=" viewer-not-refused"
OTHER='^          decided by code version [0-9a-f]\{12\}, not this one ([0-9a-f]\{12\})$'
grep -q "through the code as it is now (code version [0-9a-f]\{12\}); nothing sent$" "$P/1.txt" || fail+=" no-version"
grep "decided by code version" "$P/1.txt" >/dev/null && fail+=" same-code-said-other"
[ "$(grep -c "$OTHER" "$P/2.txt")" = 2 ] || fail+=" changed-code-not-said"
[ "$(grep -c "$OTHER" "$P/3.txt")" = 2 ] || fail+=" api-code-not-said"
[ "$(q "SELECT count(*) FROM audit_log WHERE action = 'message.payload.read' AND message_id = $ID AND actor_name = 'try-demo-reader-$SUFFIX'")" = 1 ] || fail+=" read-not-audited"
kill -INT $SINK && wait $SINK 2>/dev/null || true
grep -q "final: received 1 | unique 1 " "$P/sink.log" || fail+=" sent-again($(tail -1 "$P/sink.log"))"
if [ -z "$fail" ]; then echo PASS; else echo "FAIL:$fail"; tail -20 "$P/run.log"; exit 1; fi
