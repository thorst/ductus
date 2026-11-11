#!/usr/bin/env bash
# v0.7 demo (0.7.4.3.2): a release and its gate. A project made by `ductus init`, in git:
#   1. `ductus release -m why`: HEAD's files, the versions, the tests run on what it carries; the
#      archive's first entry is its manifest;
#   2. the same commit cloned as Windows checks it out (core.autocrlf=true: CRLF in every file)
#      gives the same release id;
#   3. a file changed and not committed: refused, named;
#   4. `ductus release check` without a database: every test runs, said;
#   5. against the database before anything ran it: the interface is new there, its tests run;
#   6. the engine started on that commit (a start records what runs), then the same release:
#      nothing changes, no tests to run;
#   7. a commit changing the interface's code, released and checked: one interface changes (its
#      versions before and after), its tests run;
#   8. a release changed after it was made (a file edited, archived again): refused;
#   9. the gate wrote nothing: the project's files are as they were, and no temporary folder is left.
# 0.7.4.3.3, releases stored on an engine (its REST API):
#  10. `deploy` refused on a viewer token; the engine started with its API on the second commit;
#  11. `POST /releases` with a token that has `deploy`: stored (new); the same commit made again
#      with another reason: the same id, stored before; a viewer: refused;
#  12. `POST /releases/{id}/check?all_tests=true`: the gate ran by the engine, recorded, audited;
#  13. `GET /releases` lists it with its check; `GET /releases/{id}` has its manifest.
# `TRANSCRIPT=file` keeps what each step printed (docs/operate/releases.md shows it).
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/dev/db.sh"
BIN=$(realpath "${BIN:-${CARGO_TARGET_DIR:-target}/release/ductus}")
DB=$DUCTUS_DATABASE_URL
W=$(mktemp -d)
P=$W/integrations
NAME=release-demo-adt
OUT=${TRANSCRIPT:-$W/transcript.txt}
: >"$OUT"
fail=""
q() { psql -tA "$DB" -c "$1"; }
g() { git -c core.autocrlf=false -c user.name="Ann Example" -c user.email=ann@example.org "$@"; }
# A step: its command as a person types it, then what it printed (also into the transcript).
step() {
  local name=$1
  shift
  { echo "\$ $*"; } >>"$OUT"
  set +e
  (cd "$P" && "$@") >"$W/$name.txt" 2>&1
  echo $? >"$W/$name.code"
  set -e
  sed "s|$W|/home/ann|g" "$W/$name.txt" | tee -a "$OUT"
  echo >>"$OUT"
}
code() { cat "$W/$1.code"; }
# A step written as a pipeline: shown as written (variables unexpanded), run by bash.
pipe() {
  local name=$1 command=$2
  echo "\$ $command" | sed "s|$W|/home/ann|g" >>"$OUT"
  set +e
  (cd "$P" && bash -o pipefail -c "$command") >"$W/$name.txt" 2>&1
  echo $? >"$W/$name.code"
  set -e
  sed "s|$W|/home/ann|g" "$W/$name.txt" | tee -a "$OUT"
  echo >>"$OUT"
}
API_PORT=17761
TOKENS=(release-demo-deploy-$$ release-demo-viewer-$$ release-demo-refused-$$)
# This demo's releases (the same files give the same id every run): forgotten before and after.
forget_releases() {
  for id in ${FULL:-} ${NEXT_ID:-}; do
    q "DELETE FROM release_checks WHERE release_id = decode('$id', 'hex'); DELETE FROM releases WHERE id = decode('$id', 'hex')" >/dev/null
  done
}
# The project's files with their hashes (git's own aside, and the releases `ductus release` writes).
files() { (cd "$P" && find . -path ./.git -prune -o -path ./.ductus/releases -prune -o -type f -print | sort | xargs sha256sum); }
# Steps run `ductus` as a person types it.
mkdir "$W/bin" && ln -s "$BIN" "$W/bin/ductus"
export PATH="$W/bin:$PATH"
cleanup() {
  [ -n "${ENGINE:-}" ] && kill -INT "$ENGINE" 2>/dev/null && wait "$ENGINE" 2>/dev/null || true
  q "DELETE FROM interface_versions WHERE interface = '$NAME'" >/dev/null 2>&1 || true
  forget_releases 2>/dev/null || true
  for t in "${TOKENS[@]}"; do "$BIN" token revoke "$t" >/dev/null 2>&1 || true; done
  rm -rf "$W"
}
trap cleanup EXIT

"$BIN" init "$P" >/dev/null
sed -i "s/^name: adt-in$/name: $NAME/" "$P/interfaces/adt-in/interface.yaml"
grep -q "^name: $NAME$" "$P/interfaces/adt-in/interface.yaml" || { echo "FAIL: init's interface name changed"; exit 1; }
(cd "$P" && g init -q -b main && g add . && g commit -qm "first interface")
# What runs there now: nothing of this demo's (an earlier run's record is removed).
q "DELETE FROM interface_versions WHERE interface = '$NAME'" >/dev/null
TEMPS=$(find "${TMPDIR:-/tmp}" -maxdepth 1 -name 'ductus-release-*' | wc -l)

echo "== 1. a release of the commit"
step make env -u DUCTUS_DATABASE_URL ductus release -m "first release"
FILE=$(sed -n 's/^wrote \(.*\) (.*/\1/p' "$W/make.txt")
ID=$(sed -n 's/^release \([0-9a-f]\{12\}\) of integrations$/\1/p' "$W/make.txt")
[ "$(code make)" = 0 ] && [ -f "$FILE" ] && [ -n "$ID" ] || fail+=" release"
grep -q "tests: 1 passed (environment dev)" "$W/make.txt" || fail+=" release-tests"
[ "$(tar tzf "$FILE" | sed -n 1p)" = release.json ] || fail+=" manifest-not-first"
FULL=$(tar xzOf "$FILE" release.json | sed -n 's/^{"format":1,"id":"\([0-9a-f]\{64\}\)".*/\1/p')
[ "${FULL:0:12}" = "$ID" ] || fail+=" manifest-id"

echo "== 2. the same commit as Windows checks it out (CRLF): the same id"
git -c core.autocrlf=true clone -q "$P" "$W/windows"
git -C "$W/windows" config core.autocrlf true
grep -q $'\r$' "$W/windows/interfaces/adt-in/archive.ts" || fail+=" clone-not-crlf"
WINDOWS=$("$BIN" release --root "$W/windows" --no-tests --json --out "$W" 2>/dev/null | sed -n 's/^  "id": "\([0-9a-f]*\)",$/\1/p')
echo "release id from the CRLF checkout: ${WINDOWS:0:12}"
[ "$WINDOWS" = "$FULL" ] || fail+=" windows-id($WINDOWS)"

echo "== 3. a change not committed: refused"
echo "// not committed" >>"$P/interfaces/adt-in/archive.ts"
step dirty env -u DUCTUS_DATABASE_URL ductus release
[ "$(code dirty)" = 1 ] && grep -q "interfaces/adt-in/archive.ts" "$W/dirty.txt" || fail+=" dirty-not-refused"
(cd "$P" && g checkout -q -- interfaces/adt-in/archive.ts)

BEFORE=$(files)

echo "== 4. the gate without a database"
step nodb env -u DUCTUS_DATABASE_URL ductus release check "$FILE" --env prod
[ "$(code nodb)" = 0 ] && grep -q "no database (DUCTUS_DATABASE_URL)" "$W/nodb.txt" || fail+=" nodb"
grep -q "every interface's (what runs isn't known): 1 passed" "$W/nodb.txt" || fail+=" nodb-tests"

echo "== 5. against the database, before anything ran it"
step new ductus release check "$FILE" --env prod
[ "$(code new)" = 0 ] && grep -q "0 interface(s) change, 1 new there, 0 as they run now" "$W/new.txt" || fail+=" new"
grep -q "every interface's (each one changes there): 1 passed" "$W/new.txt" || fail+=" new-tests"
[ "$(files)" = "$BEFORE" ] || fail+=" project-written"

echo "== 6. the engine runs that commit; the same release changes nothing"
# Started directly, not in a subshell: a background subshell ignores SIGINT.
"$BIN" run --root "$P" --env prod --api-listen off >"$W/run.log" 2>&1 &
ENGINE=$!
until grep "started interfaces=" "$W/run.log" >/dev/null 2>&1; do kill -0 $ENGINE || { cat "$W/run.log"; exit 1; }; sleep 0.2; done
for _ in $(seq 1 50); do
  [ "$(q "SELECT count(*) FROM interface_versions WHERE interface = '$NAME'")" = 1 ] && break
  sleep 0.2
done
kill -INT $ENGINE && wait $ENGINE 2>/dev/null || true
ENGINE=
step same ductus release check "$FILE" --env prod
[ "$(code same)" = 0 ] && grep -q "0 interface(s) change, 0 new there, 1 as they run now" "$W/same.txt" || fail+=" same"
grep -q "no interface changes what runs: no tests to run" "$W/same.txt" || fail+=" same-tests"

echo "== 7. a commit changing the code: one interface changes, its tests run"
sed -i 's|^export function transform(msg: Message) {$|export function transform(msg: Message) {\n  msg.set("MSH-6", "ARCHIVE");|' "$P/interfaces/adt-in/archive.ts"
grep -q 'MSH-6' "$P/interfaces/adt-in/archive.ts" || fail+=" edit"
(cd "$P" && g commit -qam "archive: MSH-6 says where it went")
step next env -u DUCTUS_DATABASE_URL ductus release -m "archive: MSH-6"
NEXT=$(sed -n 's/^wrote \(.*\) (.*/\1/p' "$W/next.txt")
BEFORE=$(files)
step changed ductus release check "$NEXT" --env prod
[ "$(code changed)" = 0 ] && grep -q "1 interface(s) change, 0 new there, 0 as they run now" "$W/changed.txt" || fail+=" changed"
grep -q "^    $NAME: [0-9a-f]\{12\} → [0-9a-f]\{12\}$" "$W/changed.txt" || fail+=" changed-versions"
grep -q "every interface's (each one changes there): 1 passed" "$W/changed.txt" || fail+=" changed-tests"

echo "== 8. a release edited after it was made: refused"
mkdir "$W/edited" && tar xzf "$NEXT" -C "$W/edited"
echo "// edited" >>"$W/edited/project/interfaces/adt-in/archive.ts"
(cd "$W/edited" && tar czf "$W/edited.tar.gz" release.json ancestry project)
step edited ductus release check "$W/edited.tar.gz" --env prod
[ "$(code edited)" = 1 ] && grep -q "its files don't match its id" "$W/edited.txt" || fail+=" edited-not-refused"

echo "== 9. the gate wrote nothing"
[ "$(files)" = "$BEFORE" ] && echo "the project's files: as they were" || fail+=" project-written"
[ "$(find "${TMPDIR:-/tmp}" -maxdepth 1 -name 'ductus-release-*' | wc -l)" = "$TEMPS" ] && echo "temporary folders: none left" || fail+=" temp-left"

echo "== 10. the deploy permission; the engine with its API"
NEXT_ID=$(tar xzOf "$NEXT" release.json | sed -n 's/^{"format":1,"id":"\([0-9a-f]\{64\}\)".*/\1/p')
NEXT_SHORT=${NEXT_ID:0:12}
NEXT_REL=.ductus/releases/$(basename "$NEXT")
forget_releases
step viewer-deploy ductus token create "${TOKENS[2]}" --role viewer --permission deploy
[ "$(code viewer-deploy)" = 1 ] && grep -q "operators and admins, not viewers" "$W/viewer-deploy.txt" || fail+=" viewer-deploy"
DEPLOY=$("$BIN" token create "${TOKENS[0]}" --role operator --permission deploy 2>/dev/null)
VIEWER=$("$BIN" token create "${TOKENS[1]}" --role viewer 2>/dev/null)
"$BIN" run --root "$P" --env prod --api-listen "127.0.0.1:$API_PORT" >"$W/api.log" 2>&1 &
ENGINE=$!
until grep "started interfaces=" "$W/api.log" >/dev/null 2>&1; do kill -0 $ENGINE || { cat "$W/api.log"; exit 1; }; sleep 0.2; done
export DUCTUS_URL=http://127.0.0.1:$API_PORT TOKEN=$DEPLOY VIEWER

echo "== 11. stored (POST /releases): new, then the same id again, then a viewer"
pipe store 'curl -sS -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/gzip" --data-binary @'"$NEXT_REL"' "$DUCTUS_URL/api/v1/releases" | python3 -m json.tool'
grep -q '"new": true' "$W/store.txt" && grep -q "\"id\": \"$NEXT_ID\"" "$W/store.txt" || fail+=" store"
(cd "$P" && "$BIN" release -m "the same commit, made again" --no-tests --out "$W/again.tar.gz" >/dev/null 2>&1) || fail+=" again-made"
cmp -s "$NEXT" "$W/again.tar.gz" && fail+=" again-same-bytes"
pipe again 'curl -sS -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/gzip" --data-binary @'"$W"'/again.tar.gz "$DUCTUS_URL/api/v1/releases" | python3 -m json.tool | head -4'
grep -q '"new": false' "$W/again.txt" || fail+=" again-not-known"
pipe viewer 'curl -sS -H "Authorization: Bearer $VIEWER" -H "Content-Type: application/gzip" --data-binary @'"$NEXT_REL"' "$DUCTUS_URL/api/v1/releases"'
grep -q "doesn't have the \`deploy\` permission" "$W/viewer.txt" || fail+=" viewer-stored"
[ "$(q "SELECT count(*) FROM releases WHERE id = decode('$NEXT_ID', 'hex')")" = 1 ] || fail+=" stored-rows"
[ "$(q "SELECT count(*) FROM audit_log WHERE action = 'release.store' AND actor_name = '${TOKENS[0]}'")" = 1 ] || fail+=" store-audit"

echo "== 12. checked by the engine against its environment"
CHECKS=$(find "${TMPDIR:-/tmp}" -maxdepth 1 -name 'ductus-check-*' | wc -l)
pipe check 'curl -sS -X POST -H "Authorization: Bearer $TOKEN" "$DUCTUS_URL/api/v1/releases/'"$NEXT_SHORT"'/check?all_tests=true" | python3 -m json.tool'
grep -q '"ok": true' "$W/check.txt" && grep -q '"environment": "prod"' "$W/check.txt" || fail+=" check"
grep -q "every interface's (--all-tests): 1 passed" "$W/check.txt" || fail+=" check-tests"
grep -q "0 interface(s) change, 0 new there, 1 as they run now" "$W/check.txt" || fail+=" check-changes"
[ "$(q "SELECT count(*) FROM release_checks WHERE release_id = decode('$NEXT_ID', 'hex') AND ok")" = 1 ] || fail+=" check-row"
[ "$(q "SELECT count(*) FROM audit_log WHERE action = 'release.check' AND actor_name = '${TOKENS[0]}'")" = 1 ] || fail+=" check-audit"
[ "$(find "${TMPDIR:-/tmp}" -maxdepth 1 -name 'ductus-check-*' | wc -l)" = "$CHECKS" ] || fail+=" check-temp-left"

echo "== 13. listed, and read"
pipe list 'curl -sS -H "Authorization: Bearer $VIEWER" "$DUCTUS_URL/api/v1/releases?limit=1" | python3 -m json.tool'
grep -q "\"short\": \"$NEXT_SHORT\"" "$W/list.txt" && grep -q '"ok": true' "$W/list.txt" || fail+=" list"
curl -sS -H "Authorization: Bearer $VIEWER" "$DUCTUS_URL/api/v1/releases/$NEXT_SHORT" >"$W/one.json"
grep -q "\"interfaces\":{\"$NAME\"" "$W/one.json" || fail+=" one-manifest"
kill -INT $ENGINE && wait $ENGINE 2>/dev/null || true
ENGINE=

if [ -z "$fail" ]; then echo PASS; else echo "FAIL:$fail"; exit 1; fi
