#!/usr/bin/env bash
# v0.7 demo (0.7.2.1): a breakpoint in interface code, from VS Code's own debugger. js-debug (what
# VS Code runs for the launch configuration `ductus types` writes) starts `ductus test --inspect`
# on the guides' example project, attaches to its port, and stops at a breakpoint on a .ts line of
# the code a test calls: the top frame, the locals, an expression, then the test passes. The .ts
# line is found through the source map (the code is run as JavaScript). Then the same for one
# message (0.7.2.2): "Ductus: debug this message" with a fixture open runs `ductus try --inspect`.
# No editor extension.
# Needs: node, curl (js-debug is fetched once, pinned: scripts/dev/js-debug.sh).
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
BIN=$(realpath "${BIN:-${CARGO_TARGET_DIR:-target}/release/ductus}")
SERVER=$(bash scripts/dev/js-debug.sh)
CLIENT=$(realpath scripts/dev/dap-client.mjs)
P=$(mktemp -d)
trap 'rm -rf "$P"; pkill -f "$SERVER" 2>/dev/null || true' EXIT
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

fail=""
grep -q '^stopped: breakpoint$' "$P/message.log" || fail+=" try-not-stopped"
grep -q "^at: toLab $CODE:18$" "$P/message.log" || fail+=" try-wrong-line"
grep -q "^msg.get(\"MSH-10\") = 'MSG00001'$" "$P/message.log" || fail+=" try-evaluate"
grep -q '^output:   lab: sent$' "$P/message.log" || fail+=" try-not-sent"
grep -q '^terminated$' "$P/message.log" || fail+=" try-not-ended"
grep -q '^stopped: breakpoint$' "$P/session.log" || fail+=" not-stopped"
grep -q "^at: toLab $CODE:18$" "$P/session.log" || fail+=" wrong-line"
grep -q '^locals: msg=Proxy(MessageImpl)' "$P/session.log" || fail+=" locals"
grep -q "^msg.PV1\[2\] + \" for a \" + msg.MSH\[9\]\[2\] = 'IN for a A01'$" "$P/session.log" || fail+=" evaluate"
grep -q 'waiting for a debugger on 127.0.0.1:9229' "$P/session.log" || fail+=" no-waiting-line"
grep -q '1 passed, 0 failed' "$P/session.log" || fail+=" test-not-passed"
grep -q '^terminated$' "$P/session.log" || fail+=" not-ended"
if [ -z "$fail" ]; then echo PASS; else echo "FAIL:$fail"; exit 1; fi
