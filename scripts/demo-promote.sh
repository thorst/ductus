#!/usr/bin/env bash
# v0.7 demo (0.7.4.4.3): promote and roll back. Three engines in release mode, each with its own
# database (dev, test, prod), in their own processes' handovers (no control socket); prod set with
# DUCTUS_DEPLOY_REQUIRES=test and DUCTUS_DEPLOY_BRANCHES=main. A project in git, one interface
# ordered by MRN, sending to a receiver that isn't up yet (messages wait).
#   1. release 1 (main) deployed to prod: refused, it wasn't deployed to test;
#   2. deployed to dev, then test: the file records both (its id unchanged);
#   3. promoted: `ductus deploy ID --from <test> --to <prod>`: fetched with test's record, deployed;
#   4. a release made on a feature branch: refused by prod's rules, and `--force` doesn't change it;
#   5. release 2 (main) to test; prod pulls it from test (DUCTUS_PROMOTE_FROM, a token on test that
#      can only download: what prod's Releases page does) and deploys it: only its code changed,
#      so it's applied in place (0.7.5.1), recorded as `how = deploy`;
#   6. Ops' hotfix on a branch of their own, deployed to test;
#   7. release 3 (main, without the hotfix) to test: refused, it would undo Ops' work;
#      `--force -m why`: deployed, recorded (`release.force`); promoted to prod;
#   8. Ops rolls prod back: to release 2, not held to prod's rules, naming whose work it undoes,
#      the gate run (`release.rollback`, the deployment marked), applied in place;
#   9. rolled back again: to release 1 (release 3, which a rollback left, is skipped);
#  10. release 1's file from prod carries where it went: dev, test, prod;
#  11. release 4 raises the interface's lanes (2 → 8), promoted (the interface restarted alone, in
#      place: 0.7.5.2.4, which the answer names); a sender keeps sending (10
#      patients) while the receiver is down, and prod is rolled back to release 1 (8 → 2 lanes, in
#      place too): nothing refused, new messages go to new lanes (numbered after the old), the old
#      ones go first;
#  12. the receiver comes up: every message arrives once, each patient's in order, and the old
#      lanes settle (`interface.lanes_settled`);
#  13. release 5 adds an archive interface listening where the lab's receiver did (0.7.5.2.1):
#      promoted, it's added in place on prod (claimed, receiving what the feed sends the lab);
#      release 6 removes it, in place too (its claim given back);
#  14. release 7 moves the interface's MLLP port by its variable (MLLP_PORT's value in every
#      environment file; interface.yaml unchanged): applied in place on test and prod (0.7.5.2.3:
#      the filled-in source compared; 0.7.5.2.2: the new port open before the old closed), the
#      values recorded in the interface's history; the sender repointed is taken there;
#  15. release 8 adds billing and claims (both load lib/payers.ts); release 9 has the feed's fix and
#      billing's unfinished change to the shared file: the feed alone (`--interface`, 0.7.5.3.2)
#      goes to test and then prod (composed over what runs there, in place); release 9 whole is
#      refused by prod (only the feed went to test); billing alone is refused (claims loads it);
#  16. release 10: Ann deploys billing and Bob claims to test at once: Bob's waits its turn and
#      composes over Ann's, which is release 10 itself.
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
variables() { # variables <environment> <its feed's MLLP port>
  printf 'variables:\n  MLLP_PORT: %s\n  RECEIVER_PORT: %s\n  BILLING_PORT: %s\n  CLAIMS_PORT: %s\n' \
    "$2" "${SINK[$1]}" $((MLLP[$1] + 6)) $((MLLP[$1] + 7)) >"$P/environments/$1.yaml"
}
for e in "${ENVS[@]}"; do variables "$e" "${MLLP[$e]}"; done
lanes() { # lanes <partitions> [another destination's line]
  cat >"$P/interfaces/$NAME/interface.yaml" <<EOF
name: $NAME
source: { mllp: { host: 127.0.0.1, port: \${MLLP_PORT} } }
ordering: { key: PID-3.1, partitions: $1 }
destinations:
  lab: { mllp: { host: 127.0.0.1, port: \${RECEIVER_PORT}, timeout_ms: 500 }, script: lab.ts }
${2:-}
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
export DEV=http://127.0.0.1:${API[dev]} TEST=http://127.0.0.1:${API[test]} PROD=http://127.0.0.1:${API[prod]}
# Prod pulls from test (0.7.4.5.2) with a token on test that can only download.
PULL=$(DUCTUS_DATABASE_URL="${URL[test]}" "$BIN" token create prod-pulls --role viewer --permission download 2>/dev/null)
start prod DUCTUS_DEPLOY_REQUIRES=test DUCTUS_DEPLOY_BRANCHES=main DUCTUS_PROMOTE_FROM="$TEST" DUCTUS_PROMOTE_FROM_TOKEN="$PULL"
token() { DUCTUS_DATABASE_URL="${URL[$1]}" "$BIN" token create "$2" --role operator --permission deploy 2>/dev/null; }
export DEV_TOKEN=$(token dev ci) TEST_TOKEN=$(token test ci) PROD_TOKEN=$(token prod ci)
export OPS_TEST=$(token test ops) OPS_PROD=$(token prod ops)
echo "prod: DUCTUS_DEPLOY_REQUIRES=test DUCTUS_DEPLOY_BRANCHES=main DUCTUS_PROMOTE_FROM=\$TEST" | tee -a "$OUT"

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

echo "== 5. release 2 (main) to test; prod pulls it from test (as its Releases page's Bring to prod), deploys it"
lab LAB2
g commit -qam "lab: LAB2"
step r2 'env -u DUCTUS_DATABASE_URL ductus release -m "lab: LAB2"'
REL2=.ductus/releases/$(basename "$(released r2)") S2=$(short_id r2)
step test2 'DUCTUS_API_TOKEN=$TEST_TOKEN ductus deploy '"$REL2"' --to $TEST -m "lab: LAB2"'
[ "$(code test2)" = 0 ] || fail+=" test2"
step upstream2 'curl -sS -H "Authorization: Bearer $PROD_TOKEN" $PROD/api/v1/upstream | python3 -c "import json, sys; u = json.load(sys.stdin); print(\"on\", u[\"environment\"] + \":\", \", \".join(r[\"short\"] + (\" (stored here)\" if r[\"stored_here\"] else \"\") for r in u[\"releases\"]))"'
has upstream2 "^on test: $S2, " && has upstream2 "$S1 (stored here)" || fail+=" upstream"
step pull2 'curl -sS -X POST -H "Authorization: Bearer $PROD_TOKEN" $PROD/api/v1/upstream/releases/'"$S2"' | python3 -c "import json, sys; p = json.load(sys.stdin); print(\"brought\", p[\"short\"], \"from\", p[\"environment\"], \"(new)\" if p[\"new\"] else \"(stored before)\", \"with\", p[\"new_places\"], \"record(s) of where it went\")"'
has pull2 "^brought $S2 from test (new) with 1 record(s) of where it went" || fail+=" pull"
[ "$(q prod "SELECT count(*) FROM audit_log WHERE action = 'release.pull' AND subject = '$S2' AND detail->>'from' = '$TEST'")" = 1 ] || fail+=" pull-audit"
step promote2 'DUCTUS_API_TOKEN=$PROD_TOKEN ductus deploy '"$S2"' --to $PROD -m "lab: LAB2, tested"'
[ "$(code promote2)" = 0 ] || fail+=" promote2"
# Only its code changed: applied in place (0.7.5.1), no new engine.
has promote2 "^· $NAME: code (in place)" && has promote2 "^✓ applied in place: 1 interface took its new code" || fail+=" promote2-in-place"
[ "$(q prod "SELECT how FROM deployments ORDER BY id DESC LIMIT 1")" = deploy ] || fail+=" promote2-recorded"
# The pull token can't deploy on test, nor bring anything to prod.
[ "$(curl -s -o /dev/null -w '%{http_code}' -X POST -H "Authorization: Bearer $PULL" -H 'Content-Type: application/json' -d '{}' "$TEST/api/v1/deployments")" = 403 ] || fail+=" pull-token-deploys"

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
[ "$(code rollback)" = 0 ] && has rollback "^rollback to release $S2" && has rollback "^✓ applied in place" || fail+=" rollback"
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

echo "== 11. lanes raised (release 4), then rolled back (8 → 2) under traffic, the receiver down: each restarts it alone"
lanes 8
g commit -qam "8 lanes"
step r4 'env -u DUCTUS_DATABASE_URL ductus release -m "8 lanes"'
REL4=.ductus/releases/$(basename "$(released r4)") S4=$(short_id r4)
step test4 'DUCTUS_API_TOKEN=$TEST_TOKEN ductus deploy '"$REL4"' --to $TEST -m "8 lanes"'
step promote4 'DUCTUS_FROM_TOKEN=$TEST_TOKEN DUCTUS_API_TOKEN=$PROD_TOKEN ductus deploy '"$S4"' --from $TEST --to $PROD -m "8 lanes, tested"'
[ "$(code test4)" = 0 ] && [ "$(code promote4)" = 0 ] || fail+=" promote4"
# Its lanes changed: it restarts alone (0.7.5.2.4), and the answer says so.
has promote4 "^· $NAME: .*(in place, restarting alone: \`ordering\` changed" && has promote4 "^✓ applied in place" || fail+=" promote4-restart"
# 200 messages for 10 patients over 10 s; the rollback 3 s in.
"$BIN" gen --to "127.0.0.1:${MLLP[prod]}" --count 200 --patients 10 --rate 20 >"$W/gen.log" 2>&1 &
SENDER=$!
sleep 3
waiting() { q prod "SELECT count(*) FROM messages WHERE interface = '$NAME' AND status = 0"; }
generations() { q prod "SELECT string_agg(first_lane || '+' || lanes, ' ' ORDER BY first_lane) FROM lane_generations WHERE interface = '$NAME'"; }
echo "waiting on prod: $(waiting); lanes (first+count): $(generations)" | tee -a "$OUT"
step rollback-lanes 'DUCTUS_API_TOKEN=$OPS_PROD ductus deploy --rollback --to $PROD -m "8 lanes broke the lab feed"'
[ "$(code rollback-lanes)" = 0 ] && has rollback-lanes "^rollback to release $S1" && has rollback-lanes "^✓ applied in place" || fail+=" lanes-rollback"
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

echo "== 13. an interface added in place (release 5: an archive on the lab's port), then removed (release 6)"
# The archive listens where the lab's receiver did (stopped after step 12), so promote-demo sends
# to it unchanged.
ARCHIVE=$NAME-archive
mkdir -p "$P/interfaces/$ARCHIVE"
printf 'name: %s\nsource: { mllp: { host: 127.0.0.1, port: ${RECEIVER_PORT} } }\ndestinations:\n  files: { file: { dir: out/archive } }\n' "$ARCHIVE" >"$P/interfaces/$ARCHIVE/interface.yaml"
lanes 2
g add . && g commit -qm "an archive where the lab's receiver was"
step r5 'env -u DUCTUS_DATABASE_URL ductus release -m "the archive"'
REL5=.ductus/releases/$(basename "$(released r5)") S5=$(short_id r5)
step test5 'DUCTUS_API_TOKEN=$TEST_TOKEN ductus deploy '"$REL5"' --to $TEST -m "the archive"'
step promote5 'DUCTUS_FROM_TOKEN=$TEST_TOKEN DUCTUS_API_TOKEN=$PROD_TOKEN ductus deploy '"$S5"' --from $TEST --to $PROD -m "the archive, tested"'
[ "$(code test5)" = 0 ] && [ "$(code promote5)" = 0 ] || fail+=" promote5"
has promote5 "^· $ARCHIVE: added (in place)" && has promote5 "^✓ applied in place" || fail+=" archive-added-in-place"
claimed() { q prod "SELECT count(*) FROM interface_owners WHERE interface = '$ARCHIVE'"; }
[ "$(claimed)" = 1 ] || fail+=" archive-claimed"
"$BIN" gen --to "127.0.0.1:${MLLP[prod]}" --count 20 --patients 5 --offset 1000 >"$W/gen2.log" 2>&1
archived() { q prod "SELECT count(*) FROM messages WHERE interface = '$ARCHIVE'"; }
for _ in $(seq 1 100); do [ "$(archived)" = 20 ] && break; sleep 0.1; done
echo "archived on prod: $(archived)" | tee -a "$OUT"
[ "$(archived)" = 20 ] || fail+=" archive-receives"
g rm -rq "interfaces/$ARCHIVE"
lanes 2
g commit -qam "no archive after all"
step r6 'env -u DUCTUS_DATABASE_URL ductus release -m "no archive"'
S6=$(short_id r6) REL6=.ductus/releases/$(basename "$(released r6)")
step test6 'DUCTUS_API_TOKEN=$TEST_TOKEN ductus deploy '"$REL6"' --to $TEST -m "no archive"'
step promote6 'DUCTUS_FROM_TOKEN=$TEST_TOKEN DUCTUS_API_TOKEN=$PROD_TOKEN ductus deploy '"$S6"' --from $TEST --to $PROD -m "no archive after all"'
[ "$(code test6)" = 0 ] && [ "$(code promote6)" = 0 ] || fail+=" promote6"
has promote6 "^· $ARCHIVE: removed (in place)" && has promote6 "^✓ applied in place" || fail+=" archive-removed-in-place"
[ "$(claimed)" = 0 ] || fail+=" archive-claim-kept"

echo "== 14. the feed's MLLP port moved in place by its variable (release 7), the sender repointed"
# Only the environment files change (MLLP_PORT's value in each): the deploy compares the feed's
# source filled in with the old and new values (0.7.5.2.3), and each engine opens the new port
# before it closes the old one (0.7.5.2.2), no handover.
MOVED=$((MLLP[prod] + 5))
for e in "${ENVS[@]}"; do variables "$e" $((MLLP[$e] + 5)); done
g commit -qam "the feed on a new port"
step r7 'env -u DUCTUS_DATABASE_URL ductus release -m "the feed on a new port"'
S7=$(short_id r7) REL7=.ductus/releases/$(basename "$(released r7)")
step test7 'DUCTUS_API_TOKEN=$TEST_TOKEN ductus deploy '"$REL7"' --to $TEST -m "the feed on a new port"'
received() { q prod "SELECT count(*) FROM messages WHERE interface = '$NAME'"; }
BEFORE=$(received)
step promote7 'DUCTUS_FROM_TOKEN=$TEST_TOKEN DUCTUS_API_TOKEN=$PROD_TOKEN ductus deploy '"$S7"' --from $TEST --to $PROD -m "the feed on a new port, tested"'
[ "$(code test7)" = 0 ] && [ "$(code promote7)" = 0 ] || fail+=" promote7"
for s in test7 promote7; do
  has $s "^· $NAME: source (MLLP_PORT) (in place)" && has $s "^✓ applied in place" || fail+=" $s-not-in-place"
done
has promote7 "^· environments/prod.yaml: MLLP_PORT \"${MLLP[prod]}\" → \"$MOVED\"" || fail+=" promote7-values"
has promote7 "^  $NAME new values: MLLP_PORT" || fail+=" promote7-reached"
# The interface's history names the values (its code version is the same).
[ "$(q prod "SELECT count(*) FROM deployment_variables WHERE interface = '$NAME'")" = 1 ] || fail+=" history-values"
# The old port is closed (connecting is refused); the sender, repointed, is taken at the new one.
if (exec 3<>"/dev/tcp/127.0.0.1/${MLLP[prod]}") 2>/dev/null; then fail+=" old-port-open"; fi
"$BIN" gen --to "127.0.0.1:$MOVED" --count 20 --patients 5 --offset 2000 >"$W/gen3.log" 2>&1
grep -q "rejected 0, refused 0," "$W/gen3.log" || { cat "$W/gen3.log"; fail+=" moved-sender"; }
echo "received on prod's new port: $(($(received) - BEFORE))" | tee -a "$OUT"
[ "$(($(received) - BEFORE))" = 20 ] || fail+=" moved-receives"

echo "== 15. one interface out of a release (0.7.5.3.2): the lab feed's fix without billing's unfinished change"
# Release 8 adds billing and claims, both loading lib/payers.ts; release 9 has the lab feed's fix
# (LAB9) and billing's half-done change to the shared file. The feed alone goes to test, then to
# prod (prod requires test: the feed went there alone, which counts for the feed alone); release 9
# whole is refused there. Billing alone is refused: claims loads the shared file too.
BILLING=$NAME-billing CLAIMS=$NAME-claims
side() { # side <interface> <port variable> <tag>
  mkdir -p "$P/interfaces/$1"
  printf 'name: %s\nsource: { mllp: { host: 127.0.0.1, port: ${%s} } }\ndestinations:\n  files: { file: { dir: out/%s }, script: payer.ts }\n' "$1" "$2" "$1" >"$P/interfaces/$1/interface.yaml"
  printf 'import type { Message } from "@ductus/sdk";\nimport { PAYER } from "../../lib/payers.ts";\n\nexport function transform(m: Message) {\n  m.set("MSH-6", PAYER + "-%s");\n  return m;\n}\n' "$3" >"$P/interfaces/$1/payer.ts"
}
mkdir -p "$P/lib"
echo 'export const PAYER = "P1";' >"$P/lib/payers.ts"
side "$BILLING" BILLING_PORT B1
side "$CLAIMS" CLAIMS_PORT C1
g add . && g commit -qm "billing and claims"
step r8 'env -u DUCTUS_DATABASE_URL ductus release -m "billing and claims"'
S8=$(short_id r8) REL8=.ductus/releases/$(basename "$(released r8)")
step test8 'DUCTUS_API_TOKEN=$TEST_TOKEN ductus deploy '"$REL8"' --to $TEST -m "billing and claims"'
step promote8 'DUCTUS_FROM_TOKEN=$TEST_TOKEN DUCTUS_API_TOKEN=$PROD_TOKEN ductus deploy '"$S8"' --from $TEST --to $PROD -m "billing and claims, tested"'
[ "$(code test8)" = 0 ] && [ "$(code promote8)" = 0 ] || fail+=" promote8"
lab LAB9
echo 'export const PAYER = "P2"; // billing: not finished' >"$P/lib/payers.ts"
side "$BILLING" BILLING_PORT B2
g commit -qam "the lab's fix; billing half done"
step r9 'env -u DUCTUS_DATABASE_URL ductus release -m "LAB9, and billing in progress"'
S9=$(short_id r9) REL9=.ductus/releases/$(basename "$(released r9)")
step test9 'DUCTUS_API_TOKEN=$TEST_TOKEN ductus deploy '"$REL9"' --interface '"$NAME"' --to $TEST -m "the lab fix alone"'
[ "$(code test9)" = 0 ] || fail+=" test9"
has test9 "^· $NAME (changed) from release $S9, over $S8 as it runs here (composed and stored now)" || fail+=" test9-composed"
has test9 "^· $NAME: code (in place)" && ! has test9 "^· $BILLING" || fail+=" test9-only-the-feed"
step promote9 'DUCTUS_FROM_TOKEN=$TEST_TOKEN DUCTUS_API_TOKEN=$PROD_TOKEN ductus deploy '"$S9"' --interface '"$NAME"' --from $TEST --to $PROD -m "the lab fix alone, tested"'
[ "$(code promote9)" = 0 ] && has promote9 "^✓ applied in place" || fail+=" promote9"
step whole9 'DUCTUS_API_TOKEN=$PROD_TOKEN ductus deploy '"$S9"' --to $PROD -m "everything in 9"'
[ "$(code whole9)" != 0 ] && has whole9 "takes only releases deployed to test first .*test (only $NAME)" || fail+=" whole9-refused"
step billing9 'DUCTUS_API_TOKEN=$TEST_TOKEN ductus deploy '"$S9"' --interface '"$BILLING"' --to $TEST -m "billing, early"'
[ "$(code billing9)" != 0 ] && has billing9 "\`$CLAIMS\` runs it too" || fail+=" billing9-refused"
# Prod runs the feed's fix, billing and claims as they were.
fix() { q prod "SELECT count(*) FROM deployments WHERE how = 'deploy' AND reason = 'the lab fix alone, tested'"; }
[ "$(fix)" = 1 ] || fail+=" prod-recorded"

echo "== 16. two deploys at once on test: Ann's billing and Bob's claims (release 10), in turn"
echo 'export const PAYER = "P1";' >"$P/lib/payers.ts"
side "$BILLING" BILLING_PORT B3
side "$CLAIMS" CLAIMS_PORT C3
g commit -qam "billing and claims, each their own change"
step r10 'env -u DUCTUS_DATABASE_URL ductus release -m "billing B3, claims C3"'
S10=$(short_id r10) REL10=.ductus/releases/$(basename "$(released r10)")
export BOB_TEST=$(token test bob)
step store10 'curl -sS -o /dev/null -w "stored: %{http_code}\n" -H "Authorization: Bearer $TEST_TOKEN" -H "Content-Type: application/gzip" --data-binary @'"$REL10"' $TEST/api/v1/releases'
step at-once 'DUCTUS_API_TOKEN=$TEST_TOKEN ductus deploy '"$S10"' --interface '"$BILLING"' --to $TEST -m "billing B3" >ann.txt 2>&1 & sleep 0.2; DUCTUS_API_TOKEN=$BOB_TEST ductus deploy '"$S10"' --interface '"$CLAIMS"' --to $TEST -m "claims C3" >bob.txt 2>&1; wait $!; cat ann.txt; echo; cat bob.txt'
has at-once "waited [0-9.]* s for its turn" || fail+=" at-once-waited"
[ "$(grep -c '^✓ applied in place' "$W/at-once.txt")" = 2 ] || fail+=" at-once-both"
has at-once "it's release $S10 itself" || fail+=" at-once-second-over-first"

if [ -z "$fail" ]; then echo PASS; else echo "FAIL:$fail"; exit 1; fi
