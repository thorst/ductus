#!/usr/bin/env bash
# v0.7 demo (0.7.2.1): a breakpoint in interface code, from VS Code's own debugger. js-debug (what
# VS Code runs for the launch configuration `ductus types` writes) starts `ductus test --inspect`
# on the guides' example project, attaches to its port, and stops at a breakpoint on a .ts line of
# the code a test calls: the top frame, the locals, an expression, then the test passes. The .ts
# line is found through the source map (the code is run as JavaScript). Then the same for one
# message (0.7.2.2): "Ductus: debug this message" with a fixture open runs `ductus try --inspect`.
# Then a running engine (0.7.2.3): "Ductus: debug this interface", read from launch.json as VS Code
# reads it, runs `ductus dev --inspect` with the open file's interface; a message stops at a
# breakpoint, another interface takes and delivers 100 meanwhile, and VS Code's stop button stops
# dev (and its Postgres). No editor extension.
# Needs: node, curl (js-debug is fetched once, pinned: scripts/dev/js-debug.sh).
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
BIN=$(realpath "${BIN:-${CARGO_TARGET_DIR:-target}/release/ductus}")
SERVER=$(bash scripts/dev/js-debug.sh)
CLIENT=$(realpath scripts/dev/dap-client.mjs)
P=$(mktemp -d)
# A dev engine left running (a failed step) is stopped first, so it stops its Postgres before the
# folder goes.
cleanup() {
  pkill -TERM -f "ductus dev --inspect $P/" 2>/dev/null || true
  for _ in $(seq 1 50); do pgrep -f "ductus dev --inspect $P/" >/dev/null || break; sleep 0.2; done
  pkill -f "$SERVER" 2>/dev/null || true
  rm -rf "$P"
}
trap cleanup EXIT
cp -r examples/guides/. "$P/"
cd "$P"
"$BIN" types >/dev/null
echo "== .vscode/launch.json, as ductus types wrote it"
cat .vscode/launch.json
echo "== VS Code's debugger: a breakpoint on interfaces/adt-in/route.ts line 18"
CODE=$P/interfaces/adt-in/route.ts
timeout 90 node "$CLIENT" "$SERVER" "$CODE" 18 'msg.PV1[2] + " for a " + msg.MSH[9][2]' \
  "$BIN" test --inspect --interface "$P/interfaces/adt-in" --filter "the lab gets visits" | tee "$P/session.log"

echo "== \"Ductus: debug this message\": the fixture a01.hl7 open, the same breakpoint, ductus try"
FIXTURE=$P/interfaces/adt-in/fixtures/a01.hl7
timeout 90 node "$CLIENT" "$SERVER" "$CODE" 18 'msg.get("MSH-10")' \
  "$BIN" try "$FIXTURE" "$FIXTURE" --inspect | tee "$P/message.log"

echo "== \"Ductus: debug this interface\": route.ts open, ductus dev with it under the debugger,"
echo "   a message sent; while it's paused another interface takes and sends 100; stopped from VS Code"
# The guides' project needs its environment's secrets to run, so a small one: `adt` debugged,
# `other` beside it on the shared pool.
D=$P/dev-project
mkdir -p "$D/interfaces/adt" "$D/interfaces/other"
printf 'name: adt\nscript: route.ts\nsource: { mllp: { host: 127.0.0.1, port: 6681 } }\ndestination: { file: { dir: out } }\n' \
  >"$D/interfaces/adt/interface.yaml"
printf 'import type { Message } from "@ductus/sdk";\n\nexport function transform(msg: Message) {\n  const ward = msg.get("PV1-3.1");\n  msg.set("MSH-5", "LAB");\n  return msg;\n}\n' \
  >"$D/interfaces/adt/route.ts"
printf 'name: other\nsource: { mllp: { host: 127.0.0.1, port: 6682 } }\ndestination: { file: { dir: out } }\n' \
  >"$D/interfaces/other/interface.yaml"
cat >"$D/while-paused.sh" <<EOF
"$BIN" gen --to 127.0.0.1:6682 --count 100 | tail -n 1
for i in \$(seq 1 100); do [ "\$(ls "$D/interfaces/other/out" 2>/dev/null | wc -l)" -ge 100 ] && break; sleep 0.1; done
echo "other delivered \$(ls "$D/interfaces/other/out" | wc -l); adt wrote \$(ls "$D/interfaces/adt/out" 2>/dev/null | wc -l)"
EOF
cd "$D"
"$BIN" types >/dev/null
DEV_CODE=$D/interfaces/adt/route.ts
DAP_CONFIG="Ductus: debug this interface" DAP_TRIGGER="'$BIN' gen --to 127.0.0.1:6681 --count 1" \
  DAP_WHILE_PAUSED="bash '$D/while-paused.sh'" \
  DAP_STOP="for i in \$(seq 1 100); do [ -n \"\$(ls '$D/interfaces/adt/out' 2>/dev/null)\" ] && break; sleep 0.1; done" \
  timeout 120 node "$CLIENT" "$SERVER" "$DEV_CODE" 5 'ward + " " + msg.get("MSH-9")' "$BIN" | tee "$P/dev.log"
cd "$P"

fail=""
grep -q '^configuration: Ductus: debug this interface: ductus dev --inspect \${fileDirname}$' "$P/dev.log" || fail+=" dev-configuration"
grep -q '^stopped: breakpoint$' "$P/dev.log" || fail+=" dev-not-stopped"
grep -q "^at: transform $DEV_CODE:5$" "$P/dev.log" || fail+=" dev-wrong-line"
grep -q "^ward + \" \" + msg.get(\"MSH-9\") = '4W ADT^A08'$" "$P/dev.log" || fail+=" dev-evaluate"
grep -q '^while paused: other delivered 100; adt wrote 0$' "$P/dev.log" || fail+=" dev-other-held-up"
grep -q 'a debugger attached: this interface.s code has no time limit' "$P/dev.log" || fail+=" dev-no-attached-line"
# Stopped cleanly, it removes its pid file (dev's last lines may come after the session's end).
for _ in $(seq 1 50); do [ -f "$D/.ductus/postgres/data/postmaster.pid" ] || break; sleep 0.2; done
[ ! -f "$D/.ductus/postgres/data/postmaster.pid" ] || fail+=" dev-postgres-not-stopped"
grep -q '^terminated$' "$P/dev.log" || fail+=" dev-not-ended"
[ "$(ls "$D/interfaces/adt/out" | wc -l)" = 1 ] || fail+=" dev-message-not-written"
grep -q '^stopped: breakpoint$' "$P/message.log" || fail+=" try-not-stopped"
grep -q "^at: toLab $CODE:18$" "$P/message.log" || fail+=" try-wrong-line"
grep -q "^msg.get(\"MSH-10\") = 'MSG00001'$" "$P/message.log" || fail+=" try-evaluate"
grep -q '^output:   lab: sent$' "$P/message.log" || fail+=" try-not-sent"
grep -q '^terminated$' "$P/message.log" || fail+=" try-not-ended"
grep -q '^stopped: breakpoint$' "$P/session.log" || fail+=" not-stopped"
grep -q "^at: toLab $CODE:18$" "$P/session.log" || fail+=" wrong-line"
grep -q '^locals: msg=Proxy(MessageImpl)' "$P/session.log" || fail+=" locals"
grep -q "^msg.PV1\[2\] + \" for a \" + msg.MSH\[9\]\[2\] = 'IN for a A01'$" "$P/session.log" || fail+=" evaluate"
# The program's output can arrive in pieces (one `output:` event each; CI split this line at
# "127.0.0.1"): read them joined.
sed ':a;N;$!ba;s/\noutput: //g' "$P/session.log" | grep 'waiting for a debugger on 127.0.0.1:9229' >/dev/null ||
  fail+=" no-waiting-line"
grep -q '1 passed, 0 failed' "$P/session.log" || fail+=" test-not-passed"
grep -q '^terminated$' "$P/session.log" || fail+=" not-ended"
if [ -z "$fail" ]; then echo PASS; else echo "FAIL:$fail"; exit 1; fi
