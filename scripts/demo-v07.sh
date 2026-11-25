#!/usr/bin/env bash
# The v0.7 demo: from the editor to production, and who changed what and why. Ann builds an
# interface, finds her bug with VS Code's debugger, and her fix is promoted dev → test → prod; Bob's
# change follows; `ductus history` on prod tells the story. Three engines in release mode, each with
# its own database, prod set with DUCTUS_DEPLOY_REQUIRES=test and DUCTUS_DEPLOY_BRANCHES=main:
#   1. `ductus init`, git; Ann makes adt-in send to the lab with the lab's ward codes; her test fails
#   2. VS Code's debugger (js-debug, with the configuration `ductus types` writes in launch.json,
#      "Ductus: debug these tests", lab.ts open) stops at a breakpoint: `ward` is the whole field;
#   3. fixed (PV1-3.1), the test passes, committed;
#   4. released; prod refuses it (not deployed to test); dev, then test, then promoted to prod;
#   5. a sender's 50 messages reach the lab's receiver through prod;
#   6. Bob's change (another ward), released, test, promoted, applied in place;
#   7. `ductus history adt-in` on prod: both commits, both deployments, who and why.
# The VS Code extension does steps 2 and 4 from the editor (`demo-vscode.sh`; recorded by hand).
# `TRANSCRIPT=file` keeps what each step printed.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
source scripts/dev/db.sh
began=$SECONDS
BIN=$(realpath "${BIN:-${CARGO_TARGET_DIR:-target}/release/ductus}")
SERVER=$(bash scripts/dev/js-debug.sh)
CLIENT=$(realpath scripts/dev/dap-client.mjs)
ADMIN=$DUCTUS_DATABASE_URL
W=$(mktemp -d)
P=$W/integrations
OUT=${TRANSCRIPT:-$W/transcript.txt}
: >"$OUT"
fail=""
ENVS=(dev test prod)
declare -A URL API MLLP LAB PID
port=17880
for e in "${ENVS[@]}"; do
  URL[$e]=$(sed -E "s#/[^/?]+([?]|$)#/ductus_v07_$e\1#" <<<"$ADMIN")
  MLLP[$e]=$((port + 1)) LAB[$e]=$((port + 2)) API[$e]=$((port + 3))
  port=$((port + 10))
done
q() { psql -tA "${URL[$1]}" -c "$2"; }
as() { # as <Name> <git args...>: a commit by that person
  local who=$1
  shift
  git -C "$P" -c core.autocrlf=false -c user.name="$who Example" -c user.email="${who,,}@example.org" "$@"
}
# A step: its command as written, run by bash in the project, then what it printed (also into the
# transcript), the temporary folder shown as Ann's home.
step() {
  local name=$1 command=$2
  echo "\$ $command" | tee -a "$OUT"
  set +e
  (cd "$P" && bash -o pipefail -c "$command") >"$W/$name.txt" 2>&1
  echo $? >"$W/$name.code"
  set -e
  sed "s|$W|/home/ann|g" "$W/$name.txt" | tee -a "$OUT"
  echo >>"$OUT"
}
code() { cat "$W/$1.code"; }
has() { grep -q -- "$2" "$W/$1.txt"; }
released() { sed -n 's/^wrote \(.*\) (.*/\1/p' "$W/$1.txt"; }
short_id() { sed -n 's/^release \([0-9a-f]\{12\}\) of integrations$/\1/p' "$W/$1.txt"; }
mkdir "$W/bin" && ln -s "$BIN" "$W/bin/ductus"
export PATH="$W/bin:$PATH"
cleanup() {
  for e in "${ENVS[@]}"; do [ -n "${PID[$e]:-}" ] && kill -INT "${PID[$e]}" 2>/dev/null || true; done
  [ -n "${RECEIVER:-}" ] && kill -INT "$RECEIVER" 2>/dev/null || true
  pkill -f "$SERVER" 2>/dev/null || true
  wait 2>/dev/null || true
  for e in "${ENVS[@]}"; do PGTZ=UTC psql "$ADMIN" -qc "DROP DATABASE IF EXISTS ductus_v07_$e WITH (FORCE)" 2>/dev/null || true; done
  rm -rf "$W"
}
trap cleanup EXIT

echo "== 1. a new project; Ann's interface to the lab, and its test fails"
(cd "$W" && ductus init integrations >/dev/null)
step types 'ductus types'
for e in "${ENVS[@]}"; do
  printf 'variables:\n  MLLP_PORT: %s\n  LAB_PORT: %s\n' "${MLLP[$e]}" "${LAB[$e]}" >"$P/environments/$e.yaml"
done
A=$P/interfaces/adt-in
rm "$A/archive.ts" "$A/archive.test.ts"
cat >"$A/interface.yaml" <<'EOF'
name: adt-in
about:
  description: ADT from the EHR to the lab system, with the lab's ward codes
  owner: ann@example.org
  from: { application: Epic, vendor: Epic Systems }
  to: { application: Lab system, vendor: In-house }
source: { mllp: { port: ${MLLP_PORT} } }
destinations:
  lab: { mllp: { host: 127.0.0.1, port: ${LAB_PORT} }, script: lab.ts }
EOF
wards() { # wards <the field read> <the lab's codes>
  cat >"$A/lab.ts" <<EOF
import type { Message } from "@ductus/sdk";

/** Our wards as the lab system names them. */
const WARDS: Record<string, string> = { $2 };

export function transform(msg: Message) {
  const ward = msg.get("$1");
  msg.set("PV1-3.1", WARDS[ward] ?? ward);
  msg.set("MSH-5", "LAB");
  return msg;
}
EOF
}
wards PV1-3 '"4W": "MED4"'
cat >"$A/lab.test.ts" <<'EOF'
import { expect, input, type Message, test } from "@ductus/sdk";
import { transform } from "./lab.ts";

test("the lab gets its own ward code", () => {
  const out = transform(input<Message>("sample.hl7"));
  expect(out.get("PV1-3.1")).toBe("MED4");
  expect(out.get("PV1-3.2")).toBe("401");
});
EOF
as Ann init -q -b main
step test-fails 'ductus test'
[ "$(code test-fails)" = 1 ] && has test-fails "0 passed, 1 failed" || fail+=" test-not-failed"
has test-fails "lab.test.ts:6" || fail+=" failure-not-placed"

echo "== 2. VS Code's debugger: \"Ductus: debug these tests\", lab.ts open, a breakpoint on line 8"
echo "\$ (VS Code) Ductus: debug these tests, a breakpoint on interfaces/adt-in/lab.ts:8, watching \`ward\`" >>"$OUT"
(cd "$P" && DAP_CONFIG="Ductus: debug these tests" timeout 90 node "$CLIENT" "$SERVER" "$A/lab.ts" 8 ward "$BIN") \
  >"$W/debug.txt" 2>&1 || true
sed "s|$W|/home/ann|g" "$W/debug.txt" | { grep -E '^(configuration:|stopped:|at:|ward =|terminated)' || true; } | tee -a "$OUT"
grep -q '^configuration: Ductus: debug these tests: ductus test --inspect --interface \${fileDirname}$' "$W/debug.txt" || fail+=" debug-configuration"
grep -q '^stopped: breakpoint$' "$W/debug.txt" && grep -q "^at: transform $A/lab.ts:8$" "$W/debug.txt" || fail+=" debug-not-stopped"
grep -q "^ward = '4W^401^1'$" "$W/debug.txt" || fail+=" debug-ward"
grep -q '^terminated$' "$W/debug.txt" || fail+=" debug-not-ended"
[[ $fail != *debug-* ]] || cat "$W/debug.txt"
echo >>"$OUT"

echo "== 3. fixed: the ward is PV1-3's first component"
wards PV1-3.1 '"4W": "MED4"'
step test-passes 'ductus test'
[ "$(code test-passes)" = 0 ] && has test-passes "1 passed, 0 failed" || fail+=" test-not-passed"
as Ann add -A && as Ann commit -qm "adt-in: ADT to the lab with its ward codes"

echo "== 4. released; prod wants it tested first; dev, test, then promoted to prod"
for e in "${ENVS[@]}"; do
  PGTZ=UTC psql "$ADMIN" -qc "DROP DATABASE IF EXISTS ductus_v07_$e WITH (FORCE)" -c "CREATE DATABASE ductus_v07_$e"
done
start() { # start <env> [DUCTUS_* settings...]
  local e=$1
  shift
  env -u DUCTUS_CONTROL -u DUCTUS_PROJECT DUCTUS_DATABASE_URL="${URL[$e]}" "$@" \
    "$BIN" run --releases "$W/releases-$e" --env "$e" --threads 1 --api-listen "127.0.0.1:${API[$e]}" >"$W/$e.log" 2>&1 &
  PID[$e]=$!
  until grep -q "started interfaces=" "$W/$e.log" 2>/dev/null; do kill -0 "${PID[$e]}" || { cat "$W/$e.log"; exit 1; }; sleep 0.2; done
}
start dev
start test
start prod DUCTUS_DEPLOY_REQUIRES=test DUCTUS_DEPLOY_BRANCHES=main
export DEV=http://127.0.0.1:${API[dev]} TEST=http://127.0.0.1:${API[test]} PROD=http://127.0.0.1:${API[prod]}
token() { DUCTUS_DATABASE_URL="${URL[$1]}" "$BIN" token create "$2" --role operator --permission deploy 2>/dev/null; }
export ANN_DEV=$(token dev ann) ANN_TEST=$(token test ann) ANN_PROD=$(token prod ann)
export BOB_TEST=$(token test bob) BOB_PROD=$(token prod bob)
echo "prod: DUCTUS_DEPLOY_REQUIRES=test DUCTUS_DEPLOY_BRANCHES=main" | tee -a "$OUT"
step r1 'env -u DUCTUS_DATABASE_URL ductus release -m "the lab feed"'
REL1=.ductus/releases/$(basename "$(released r1)") S1=$(short_id r1)
[ "$(code r1)" = 0 ] && [ -n "$S1" ] || fail+=" r1"
step prod-first 'DUCTUS_API_TOKEN=$ANN_PROD ductus deploy '"$REL1"' --to $PROD -m "the lab feed"'
[ "$(code prod-first)" = 1 ] && has prod-first "takes only releases deployed to test first" || fail+=" prod-not-refused"
step dev1 'DUCTUS_API_TOKEN=$ANN_DEV ductus deploy '"$REL1"' --to $DEV -m "the lab feed"'
step test1 'DUCTUS_API_TOKEN=$ANN_TEST ductus deploy '"$REL1"' --to $TEST -m "the lab feed, for the lab to try"'
[ "$(code dev1)" = 0 ] && [ "$(code test1)" = 0 ] || fail+=" dev-test"
step prod1 'DUCTUS_FROM_TOKEN=$ANN_TEST DUCTUS_API_TOKEN=$ANN_PROD ductus deploy '"$S1"' --from $TEST --to $PROD -m "the lab signed off in test"'
[ "$(code prod1)" = 0 ] && has prod1 "fetched release $S1 from .* deployed to: dev, test)" || fail+=" prod1"

echo "== 5. the EHR sends 50 messages to prod; the lab's receiver gets each once, in order"
"$BIN" sink --port "${LAB[prod]}" >"$W/lab.log" 2>&1 &
RECEIVER=$!
step send '"$BIN" gen --to 127.0.0.1:'"${MLLP[prod]}"' --count 50 --patients 10 | tail -n 1'
grep -q "rejected 0, refused 0," "$W/send.txt" || fail+=" send"
delivered() { q prod "SELECT count(*) FROM deliveries WHERE status = 1"; }
for _ in $(seq 1 100); do [ "$(delivered)" = 50 ] && break; sleep 0.1; done
kill -INT "$RECEIVER"
for _ in $(seq 1 50); do grep -q "final:" "$W/lab.log" && break; sleep 0.1; done
RECEIVER=""
grep "final:" "$W/lab.log" | tee -a "$OUT"
grep -q "unique 50 | duplicates 0 | out of order 0 |" "$W/lab.log" || fail+=" lab-received"

echo "== 6. Bob's change: the lab names 5E too; tested, promoted, applied in place"
wards PV1-3.1 '"4W": "MED4", "5E": "SURG5"'
as Bob commit -qam "adt-in: 5E is SURG5 at the lab"
step r2 'env -u DUCTUS_DATABASE_URL ductus release -m "5E for the lab"'
REL2=.ductus/releases/$(basename "$(released r2)") S2=$(short_id r2)
step test2 'DUCTUS_API_TOKEN=$BOB_TEST ductus deploy '"$REL2"' --to $TEST -m "5E for the lab (ticket 4411)"'
step prod2 'DUCTUS_FROM_TOKEN=$BOB_TEST DUCTUS_API_TOKEN=$BOB_PROD ductus deploy '"$S2"' --from $TEST --to $PROD -m "5E for the lab, tested (ticket 4411)"'
[ "$(code test2)" = 0 ] && [ "$(code prod2)" = 0 ] && has prod2 "^✓ applied in place" || fail+=" prod2"

echo "== 7. who changed what and why, on prod"
export PROD_DB=${URL[prod]}
step history 'ductus history adt-in --database-url "$PROD_DB" --env prod'
[ "$(code history)" = 0 ] || fail+=" history"
has history "Ann Example  committed [0-9a-f]\{12\}: adt-in: ADT to the lab with its ward codes" || fail+=" history-ann-commit"
has history "Bob Example  committed [0-9a-f]\{12\}: adt-in: 5E is SURG5 at the lab" || fail+=" history-bob-commit"
has history "deployed  ann  .*the lab signed off in test" || fail+=" history-ann-deploy"
has history "deployed  bob  .*5E for the lab, tested (ticket 4411)" || fail+=" history-bob-deploy"

echo "transcript: $OUT"
echo "demo-v07 took $((SECONDS - began)) s"
if [ -z "$fail" ]; then echo PASS; else echo "FAIL:$fail"; exit 1; fi
