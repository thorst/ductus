#!/usr/bin/env bash
# v0.7 demo (0.7.4.4.3): promote and roll back. Three engines in release mode, each with its own
# database (dev, test, prod), in their own processes' handovers (no control socket); prod set with
# DUCTUS_DEPLOY_REQUIRES=test and DUCTUS_DEPLOY_BRANCHES=main. A project in git, one interface
# ordered by MRN, sending to a receiver that isn't up yet (messages wait).
#   1. release 1 (main) deployed to prod: refused, it wasn't deployed to test;
#   2. deployed to dev, then test: the file records both (its id unchanged);
#   3. promoted: `ductus deploy ID --from <test> --to <prod>`: fetched with test's record, deployed;
#   4. a release made on a feature branch: refused by prod's rules, and `--force` doesn't change it;
#   5. release 2 (main) to test, promoted to prod;
#   6. Ops' hotfix on a branch of their own, deployed to test;
#   7. release 3 (main, without the hotfix) to test: refused, it would undo Ops' work;
#      `--force -m why`: deployed, recorded (`release.force`); promoted to prod;
#   8. Ops rolls prod back: to release 2, not held to prod's rules, naming whose work it undoes,
#      the gate run (`release.rollback`, the deployment marked);
#   9. rolled back again: to release 1 (release 3, which a rollback left, is skipped);
#  10. release 1's file from prod carries where it went: dev, test, prod;
#  11. release 4 raises the interface's lanes (2 → 8), promoted; a sender keeps sending (10
#      patients) while the receiver is down, and prod is rolled back to release 1 (8 → 2 lanes):
#      nothing refused, new messages go to new lanes (numbered after the old), the old ones go first;
#  12. the receiver comes up: every message arrives once, each patient's in order, and the old
#      lanes settle (`interface.lanes_settled`).
# `TRANSCRIPT=file` keeps what each step printed (docs/operate/releases.md shows it).
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/dev/db.sh"
BIN=$(realpath "${BIN:-${CARGO_TARGET_DIR:-target}/release/ductus}")
ADMIN=$DUCTUS_DATABASE_URL
W=$(mktemp -d)
P=$W/integrations
NAME=promote-demo
OUT=${TRANSCRIPT:-$W/transcript.txt}
: >"$OUT"
fail=""
ENVS=(dev test prod)
declare -A URL API MLLP SINK PID
port=17780
for e in "${ENVS[@]}"; do
  URL[$e]=$(sed -E "s#/[^/?]+([?]|$)#/ductus_promote_$e\1#" <<<"$ADMIN")
  MLLP[$e]=$((port + 1)) SINK[$e]=$((port + 2)) API[$e]=$((port + 3))
  port=$((port + 10))
done
q() { psql -tA "${URL[$1]}" -c "$2"; }
g() { git -C "$P" -c core.autocrlf=false -c user.name="Ann Example" -c user.email=ann@example.org "$@"; }
# A step: its command as written (variables unexpanded), run by bash in the project, then what it
# printed (also into the transcript).
step() {
  local name=$1 command=$2
  echo "\$ $command" >>"$OUT"
  set +e
  (cd "$P" && bash -o pipefail -c "$command") >"$W/$name.txt" 2>&1
  echo $? >"$W/$name.code"
  set -e
  sed "s|$W|/home/ann|g" "$W/$name.txt" | tee -a "$OUT"
  echo >>"$OUT"
}
code() { cat "$W/$1.code"; }
has() { grep -q -- "$2" "$W/$1.txt"; }
# `ductus release`'s file and short id, from what it printed.
released() { sed -n 's/^wrote \(.*\) (.*/\1/p' "$W/$1.txt"; }
short_id() { sed -n 's/^release \([0-9a-f]\{12\}\) of integrations$/\1/p' "$W/$1.txt"; }
mkdir "$W/bin" && ln -s "$BIN" "$W/bin/ductus"
export PATH="$W/bin:$PATH"
cleanup() {
  for e in "${ENVS[@]}"; do [ -n "${PID[$e]:-}" ] && kill -INT "${PID[$e]}" 2>/dev/null || true; done
  [ -n "${RECEIVER:-}" ] && kill -INT "$RECEIVER" 2>/dev/null || true
  wait 2>/dev/null || true
  for e in "${ENVS[@]}"; do PGTZ=UTC psql "$ADMIN" -qc "DROP DATABASE IF EXISTS ductus_promote_$e WITH (FORCE)" 2>/dev/null || true; done
  rm -rf "$W"
}
trap cleanup EXIT

# The project: one interface, its ports per environment.
mkdir -p "$P/interfaces/$NAME" "$P/environments"
cd "$P"
echo "{}" >ductus.yaml
printf 'out/\n*.key\n' >.gitignore
for e in "${ENVS[@]}"; do
  printf 'variables:\n  MLLP_PORT: %s\n  RECEIVER_PORT: %s\n' "${MLLP[$e]}" "${SINK[$e]}" >"environments/$e.yaml"
done
lanes() {
  cat >"$P/interfaces/$NAME/interface.yaml" <<EOF
name: $NAME
source: { mllp: { host: 127.0.0.1, port: \${MLLP_PORT} } }
ordering: { key: PID-3.1, partitions: $1 }
destinations:
  lab: { mllp: { host: 127.0.0.1, port: \${RECEIVER_PORT}, timeout_ms: 500 }, script: lab.ts }
EOF
}
lanes 2
lab() {
  printf 'import type { Message } from "@ductus/sdk";\n\nexport function transform(m: Message) {\n  m.set("MSH-5", "%s");\n  return m;\n}\n' "$1" >"$P/interfaces/$NAME/lab.ts"
  printf 'import { parse, test, expect } from "@ductus/sdk";\nimport { transform } from "./lab.ts";\n\ntest("sends to the lab", () => {\n  expect(transform(parse("MSH|^~\\\\&|A|B|C|D|20260101||ADT^A01|1|P|2.5\\r")).get("MSH-5")).toBe("%s");\n});\n' "$1" >"$P/interfaces/$NAME/lab.test.ts"
}
lab LAB
g init -q -b main && g add . && g commit -qm "the lab feed"
cd - >/dev/null

# Three databases, three engines, a deploy token on each (and Ops' on test and prod).
for e in "${ENVS[@]}"; do
  PGTZ=UTC psql "$ADMIN" -qc "DROP DATABASE IF EXISTS ductus_promote_$e WITH (FORCE)" -c "CREATE DATABASE ductus_promote_$e"
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
token() { DUCTUS_DATABASE_URL="${URL[$1]}" "$BIN" token create "$2" --role operator --permission deploy 2>/dev/null; }
export DEV=http://127.0.0.1:${API[dev]} TEST=http://127.0.0.1:${API[test]} PROD=http://127.0.0.1:${API[prod]}
export DEV_TOKEN=$(token dev ci) TEST_TOKEN=$(token test ci) PROD_TOKEN=$(token prod ci)
export OPS_TEST=$(token test ops) OPS_PROD=$(token prod ops)
echo "prod: DUCTUS_DEPLOY_REQUIRES=test DUCTUS_DEPLOY_BRANCHES=main" | tee -a "$OUT"

echo "== 1. release 1 to prod: refused, not deployed to test"
step r1 'env -u DUCTUS_DATABASE_URL ductus release -m "the lab feed"'
R1=$(released r1) S1=$(short_id r1)
[ "$(code r1)" = 0 ] && [ -n "$S1" ] && has r1 "on branch main" || fail+=" r1"
REL1=.ductus/releases/$(basename "$R1")
step prod-first 'DUCTUS_API_TOKEN=$PROD_TOKEN ductus deploy '"$REL1"' --to $PROD -m "the lab feed"'
[ "$(code prod-first)" = 1 ] && has prod-first "✗ this environment takes only releases deployed to test first" || fail+=" requires-not-refused"
has prod-first "it hasn't been deployed anywhere that this engine knows of" && ! has prod-first "^gate:" || fail+=" requires-why"

echo "== 2. to dev, then test: the file records both"
step dev1 'DUCTUS_API_TOKEN=$DEV_TOKEN ductus deploy '"$REL1"' --to $DEV -m "the lab feed"'
[ "$(code dev1)" = 0 ] && has dev1 "recorded in $REL1: deployed to dev" || fail+=" dev1"
step test1 'DUCTUS_API_TOKEN=$TEST_TOKEN ductus deploy '"$REL1"' --to $TEST -m "the lab feed"'
[ "$(code test1)" = 0 ] && has test1 "recorded in $REL1: deployed to test" || fail+=" test1"
step carried "tar xzOf $REL1 release.json | python3 -c 'import json, sys; m = json.load(sys.stdin); print(m[\"id\"][:12], [(p[\"environment\"], p[\"by\"]) for p in m[\"deployed\"]])'"
has carried "$S1 \[('dev', 'ci'), ('test', 'ci')\]" || fail+=" carried"

echo "== 3. promoted from test to prod"
step promote1 'DUCTUS_FROM_TOKEN=$TEST_TOKEN DUCTUS_API_TOKEN=$PROD_TOKEN ductus deploy '"$S1"' --from $TEST --to $PROD -m "tested in test"'
[ "$(code promote1)" = 0 ] && has promote1 "fetched release $S1 from .* deployed to: dev, test)" || fail+=" promote1"
has promote1 "release $S1 was stored on the engine before (2 records of where it went added)" && has promote1 "^✓ handed over to a new engine in this process" || fail+=" promote1-deployed"

echo "== 4. a feature branch's release: refused by prod's rules, --force or not"
g checkout -qb feature/lab-codes
lab LAB-NEW
g commit -qam "lab codes, not reviewed yet"
step feature 'env -u DUCTUS_DATABASE_URL ductus release -m "trying the new codes"'
REL_F=.ductus/releases/$(basename "$(released feature)")
step feature-prod 'DUCTUS_API_TOKEN=$PROD_TOKEN ductus deploy '"$REL_F"' --to $PROD --force -m "just this once"'
[ "$(code feature-prod)" = 1 ] && has feature-prod "made on \`feature/lab-codes\`: merge it, then release from there" || fail+=" branch-not-refused"
g checkout -q main

echo "== 5. release 2 (main) to test, promoted to prod"
lab LAB2
g commit -qam "lab: LAB2"
step r2 'env -u DUCTUS_DATABASE_URL ductus release -m "lab: LAB2"'
REL2=.ductus/releases/$(basename "$(released r2)") S2=$(short_id r2)
step test2 'DUCTUS_API_TOKEN=$TEST_TOKEN ductus deploy '"$REL2"' --to $TEST -m "lab: LAB2"'
[ "$(code test2)" = 0 ] || fail+=" test2"
step promote2 'DUCTUS_FROM_TOKEN=$TEST_TOKEN DUCTUS_API_TOKEN=$PROD_TOKEN ductus deploy '"$S2"' --from $TEST --to $PROD -m "lab: LAB2, tested"'
[ "$(code promote2)" = 0 ] || fail+=" promote2"

echo "== 6. Ops' hotfix, on a branch of their own, deployed to test"
g checkout -qb hotfix/lab
lab LAB2-HOTFIX
g commit -qam "hotfix: lab code"
step hotfix 'env -u DUCTUS_DATABASE_URL ductus release -m "hotfix"'
REL_H=.ductus/releases/$(basename "$(released hotfix)")
step test-hotfix 'DUCTUS_API_TOKEN=$OPS_TEST ductus deploy '"$REL_H"' --to $TEST -m "hotfix: trying it in test"'
[ "$(code test-hotfix)" = 0 ] || fail+=" test-hotfix"
g checkout -q main

echo "== 7. release 3 (main, without the hotfix) to test: refused over Ops' work; forced; promoted"
lab LAB3
g commit -qam "lab: LAB3"
step r3 'env -u DUCTUS_DATABASE_URL ductus release -m "lab: LAB3"'
REL3=.ductus/releases/$(basename "$(released r3)") S3=$(short_id r3)
step test3 'DUCTUS_API_TOKEN=$TEST_TOKEN ductus deploy '"$REL3"' --to $TEST -m "lab: LAB3"'
[ "$(code test3)" = 1 ] && has test3 "^✗ would undo $NAME: .* by ops " || fail+=" replace-not-refused"
step test3-force 'DUCTUS_API_TOKEN=$TEST_TOKEN ductus deploy '"$REL3"' --to $TEST --force -m "the hotfix goes into the next release"'
[ "$(code test3-force)" = 0 ] && has test3-force "^⚠ deployed over $NAME" || fail+=" force"
[ "$(q test "SELECT count(*) FROM audit_log WHERE action = 'release.force' AND actor_name = 'ci'")" = 1 ] || fail+=" force-audit"
step promote3 'DUCTUS_FROM_TOKEN=$TEST_TOKEN DUCTUS_API_TOKEN=$PROD_TOKEN ductus deploy '"$S3"' --from $TEST --to $PROD -m "lab: LAB3, tested"'
[ "$(code promote3)" = 0 ] || fail+=" promote3"

echo "== 8. Ops rolls prod back: to release 2, naming whose work it undoes"
step rollback 'DUCTUS_API_TOKEN=$OPS_PROD ductus deploy --rollback --to $PROD -m "LAB3 broke the lab feed"'
[ "$(code rollback)" = 0 ] && has rollback "^rollback to release $S2" && has rollback "^✓ handed over" || fail+=" rollback"
has rollback "· a rollback isn't held to this environment's rules" && has rollback "^⚠ undoes $NAME: .* by ci " || fail+=" rollback-said"
has rollback "^gate: passed" || fail+=" rollback-gate"
[ "$(q prod "SELECT count(*) FROM deployments WHERE rollback AND by_name = 'ops'")" = 1 ] || fail+=" rollback-recorded"
[ "$(q prod "SELECT count(*) FROM audit_log WHERE action = 'release.rollback' AND detail->>'ok' = 'true' AND detail->>'from' = '$S3'")" = 1 ] || fail+=" rollback-audit"

echo "== 9. rolled back again: to release 1, release 3 (which a rollback left) skipped"
step rollback2 'DUCTUS_API_TOKEN=$OPS_PROD ductus deploy --rollback --to $PROD -m "LAB2 too"'
[ "$(code rollback2)" = 0 ] && has rollback2 "^rollback to release $S1" || fail+=" rollback2"

echo "== 10. release 1's file from prod: where it went"
step where "curl -sS -H \"Authorization: Bearer \$PROD_TOKEN\" \$PROD/api/v1/releases/$S1/file | tar xzO release.json | python3 -c 'import json, sys; print(sorted(p[\"environment\"] for p in json.load(sys.stdin)[\"deployed\"]))'"
has where "\['dev', 'prod', 'test'\]" || fail+=" where"

echo "== 11. lanes raised (release 4), then rolled back (8 → 2) under traffic, the receiver down"
lanes 8
g commit -qam "8 lanes"
step r4 'env -u DUCTUS_DATABASE_URL ductus release -m "8 lanes"'
REL4=.ductus/releases/$(basename "$(released r4)") S4=$(short_id r4)
step test4 'DUCTUS_API_TOKEN=$TEST_TOKEN ductus deploy '"$REL4"' --to $TEST -m "8 lanes"'
step promote4 'DUCTUS_FROM_TOKEN=$TEST_TOKEN DUCTUS_API_TOKEN=$PROD_TOKEN ductus deploy '"$S4"' --from $TEST --to $PROD -m "8 lanes, tested"'
[ "$(code test4)" = 0 ] && [ "$(code promote4)" = 0 ] || fail+=" promote4"
# 200 messages for 10 patients over 10 s; the rollback 3 s in.
"$BIN" gen --to "127.0.0.1:${MLLP[prod]}" --count 200 --patients 10 --rate 20 >"$W/gen.log" 2>&1 &
SENDER=$!
sleep 3
waiting() { q prod "SELECT count(*) FROM messages WHERE interface = '$NAME' AND status = 0"; }
generations() { q prod "SELECT string_agg(first_lane || '+' || lanes, ' ' ORDER BY first_lane) FROM lane_generations WHERE interface = '$NAME'"; }
echo "waiting on prod: $(waiting); lanes (first+count): $(generations)" | tee -a "$OUT"
step rollback-lanes 'DUCTUS_API_TOKEN=$OPS_PROD ductus deploy --rollback --to $PROD -m "8 lanes broke the lab feed"'
[ "$(code rollback-lanes)" = 0 ] && has rollback-lanes "^rollback to release $S1" && has rollback-lanes "^✓ handed over" || fail+=" lanes-rollback"
wait "$SENDER"
grep -q "rejected 0, refused 0," "$W/gen.log" || { cat "$W/gen.log"; fail+=" lanes-sender"; }
echo "waiting on prod: $(waiting); lanes (first+count): $(generations)" | tee -a "$OUT"
# Release 4's 8 lanes (2–9; release 1's 0–1 settled at its start, nothing waited), then release
# 1's 2 again, numbered after them (10–11).
[ "$(generations)" = "2+8 10+2" ] || fail+=" lanes-generations"

echo "== 12. the receiver comes up: every message once, each patient's in order; the old lanes settle"
"$BIN" sink --port "${SINK[prod]}" >"$W/sink.log" 2>&1 &
RECEIVER=$!
for _ in $(seq 1 150); do [ "$(waiting)" = 0 ] && [ "$(generations)" = "10+2" ] && break; sleep 0.2; done
echo "waiting on prod: $(waiting); lanes (first+count): $(generations)" | tee -a "$OUT"
[ "$(generations)" = "10+2" ] || fail+=" lanes-settled"
[ "$(q prod "SELECT count(*) FROM audit_log WHERE interface = '$NAME' AND action = 'interface.lanes_settled'")" -ge 1 ] || fail+=" lanes-settled-audit"
kill -INT "$RECEIVER"
for _ in $(seq 1 50); do grep -q "final:" "$W/sink.log" && break; sleep 0.1; done
RECEIVER=""
grep "final:" "$W/sink.log" | tee -a "$OUT"
grep -q "| duplicates 0 | out of order 0 |" "$W/sink.log" && grep -q "unique 200 |" "$W/sink.log" || fail+=" lanes-order"

if [ -z "$fail" ]; then echo PASS; else echo "FAIL:$fail"; exit 1; fi
