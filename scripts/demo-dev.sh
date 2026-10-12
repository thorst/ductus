#!/usr/bin/env bash
# `ductus dev`: the engine with a Postgres of its own, in a fresh project.
#   1. starts (downloads Postgres once), prints a working URL, takes messages;
#   2. a second `ductus dev` for the same project is refused;
#   3. Ctrl-C stops the engine and the Postgres, exit 0;
#   4. started again: same port, same data;
#   5. kill -9 leaves a Postgres behind; the next start stops it and carries on;
#   6. no default database for `ductus run`.
# Needs the release binary, psql, and (first time) internet access to github.com.
set -euo pipefail

BIN=${BIN:-${CARGO_TARGET_DIR:-target}/release/ductus}
WORK=$(mktemp -d)
P=$WORK/project
DEV=""
trap 'kill -9 $DEV 2>/dev/null || true; pkill -f "postgres.*$WORK" 2>/dev/null || true' EXIT
unset DUCTUS_DATABASE_URL

fail=""
check() { # check <name> <condition...>
  local name=$1
  shift
  if ! eval "$@"; then fail+=" $name"; fi
}

"$BIN" init "$P" >/dev/null

start() { # start <name>: runs `ductus dev` and waits until the engine has started
  "$BIN" dev --root "$P" >"$WORK/$1.log" 2>&1 &
  DEV=$!
  for _ in $(seq 1 600); do # the first run downloads
    grep 'started interfaces=' "$WORK/$1.log" >/dev/null && return
    kill -0 "$DEV" 2>/dev/null || { echo "ductus dev ended early:"; tail -8 "$WORK/$1.log"; exit 1; }
    sleep 0.5
  done
  echo "ductus dev didn't start:"
  tail -8 "$WORK/$1.log"
  exit 1
}
url_of() { sed -n 's/^  \(postgresql:\/\/[^ ]*\)$/\1/p' "$WORK/$1.log" | sed -n 1p; }
count() { psql -qAt "$1" -c "SELECT count(*) FROM messages"; }
stop() { # stop <signal>: sets $code
  code=0
  kill "-$1" "$DEV"
  wait "$DEV" || code=$?
}

echo "== 1. first start"
start first
URL=$(url_of first)
PORT=$(sed -n 's/.*:\([0-9]*\)\/ductus$/\1/p' <<<"$URL")
echo "   $(sed -n 's/^\(Postgres [0-9.]* is running\).*/\1/p' "$WORK/first.log") on port $PORT"
check url-works '[ "$(psql -qAt "$URL" -c "SELECT 1")" = 1 ]'
"$BIN" gen --to 127.0.0.1:6661 --count 50 --patients 5 >"$WORK/gen.log" 2>&1
check took-messages '[ "$(count "$URL")" = 50 ]'

echo "== 2. a second one for the same project"
second=0
"$BIN" dev --root "$P" >"$WORK/second.log" 2>&1 || second=$?
check second-refused '[ "$second" != 0 ] && grep "already running" "$WORK/second.log" >/dev/null'

echo "== 3. Ctrl-C"
stop INT
check clean-exit '[ "$code" = 0 ]'
check postgres-stopped '! psql -qAt "$URL" -c "SELECT 1" >/dev/null 2>&1'
check says-stopped 'grep "Postgres stopped" "$WORK/first.log" >/dev/null'

echo "== 4. again: same port, same data"
start again
check same-port '[ "$(url_of again)" = "$URL" ]'
check same-data '[ "$(count "$URL")" = 50 ]'

echo "== 5. kill -9 leaves a Postgres behind"
kill -9 "$DEV"
wait "$DEV" 2>/dev/null || true
sleep 1
check orphan-left '[ "$(psql -qAt "$URL" -c "SELECT 1" 2>/dev/null)" = 1 ]'
start orphan
check orphan-same-data '[ "$(count "$(url_of orphan)")" = 50 ]'
stop INT
check orphan-exit '[ "$code" = 0 ]'

echo "== 6. no default database"
none=0
"$BIN" run --root "$P" >"$WORK/none.log" 2>&1 || none=$?
check run-needs-a-url '[ "$none" != 0 ] && grep "no database: set DUCTUS_DATABASE_URL" "$WORK/none.log" >/dev/null'

echo "logs: $WORK"
if [ -z "$fail" ]; then echo PASS; else echo "FAIL:$fail"; exit 1; fi
