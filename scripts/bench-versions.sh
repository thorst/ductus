#!/usr/bin/env bash
# What recording what runs (0.7.3) costs a start: an engine with 1,001 interfaces, each with code
# (a transform importing one of ten shared libraries, which import another, and a file of its
# own: 3,013 files), started ROUNDS times with BASE (a binary before 0.7.3) and BIN alternately,
# on a database of its own. Reports each start's `took` and, for BIN, the phases 0.7.3 added
# (code versions computed, saved: what the first decisions wait for (0.7.3.2), deployment
# recorded); BIN's first start (every version and file new) separately.
#
#   BASE=~/target-base/release/ductus bash scripts/bench-versions.sh   # BASE: an earlier binary
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/dev/db.sh"

BIN=$(realpath "${BIN:-${CARGO_TARGET_DIR:-target}/release/ductus}")
BASE=$(realpath "${BASE:?a binary before 0.7.3 to compare with}")
COUNT=${COUNT:-1001}
ROUNDS=${ROUNDS:-5}
WORK=$(mktemp -d)
DB=bench_versions_$$
URL=${DUCTUS_DATABASE_URL%/*}/$DB

echo "== a project with $COUNT interfaces with code (in $WORK)"
mkdir -p "$WORK/lib"
printf 'export function pad(s: string, n: number): string {\n  return s.padEnd(n, " ");\n}\n' >"$WORK/lib/util.ts"
for l in $(seq 0 9); do
  {
    printf 'import { pad } from "./util.ts";\nexport const CODES_%d: Record<string, string> = {\n' "$l"
    for k in $(seq 1 40); do printf '  "C%03d": "Code %d of table %d",\n' "$k" "$k" "$l"; done
    printf '};\nexport function code%d(c: string): string {\n  return CODES_%d[c] ?? pad(c, 8);\n}\n' "$l" "$l"
  } >"$WORK/lib/codes$l.ts"
done
for i in $(seq 0 $((COUNT - 1))); do
  d="$WORK/interfaces/v-$i"
  mkdir -p "$d"
  l=$((i % 10))
  printf 'name: v-%d\nsource: { mllp: { host: 127.0.0.1, port: %d } }\nscript: transform.ts\ndestinations: { receiver: { mllp: { host: 127.0.0.1, port: 19999 } } }\n' \
    "$i" $((20000 + i)) >"$d/interface.yaml"
  printf 'export const UPPER = ["last", "first"];\nexport function clean(s: string): string {\n  return s.trim().toUpperCase();\n}\n' >"$d/map.ts"
  cat >"$d/transform.ts" <<EOF
import type { Message } from "@ductus/sdk";
import { code$l } from "../../lib/codes$l.ts";
import { clean } from "./map.ts";

/** Interface $i: the lab's codes, the last name upper case. */
export function transform(msg: Message) {
  if (msg.MSH[9][1] !== "ORM") return null;
  msg.PID[5][1] = clean(msg.PID[5][1]);
  msg.OBR[4][2] = code$l(msg.OBR[4][1]);
  return msg;
}
EOF
done

# In a git repository, as projects usually are (GIT=0: without): the start then compares every
# file it runs with the commit.
if [ "${GIT:-1}" = 1 ]; then
  git -C "$WORK" init -q
  git -C "$WORK" add .
  git -C "$WORK" -c user.name=bench -c user.email=bench@example.org commit -qm "the project"
fi

PGTZ=UTC psql "$DUCTUS_DATABASE_URL" -qc "CREATE DATABASE $DB"
cleanup() {
  pkill -f "ductus run --root $WORK" || true
  sleep 1
  PGTZ=UTC psql "$DUCTUS_DATABASE_URL" -qc "DROP DATABASE IF EXISTS $DB WITH (FORCE)" || true
}
trap cleanup EXIT

# One start: until `started interfaces=`, then a clean stop. Prints its took and the phases asked for.
start() {
  local bin=$1 log=$WORK/engine.log
  DUCTUS_DATABASE_URL=$URL RUST_LOG=info,ductus_runtime=debug "$bin" run --root "$WORK" --threads 4 --api-listen off >"$log" 2>&1 &
  local pid=$!
  for _ in $(seq 1 600); do
    grep "started interfaces=" "$log" >/dev/null && break
    sleep 0.1
  done
  local took versions recorded loaded
  took=$(grep "started interfaces=" "$log" | grep -o 'took=[^ ]*' | sed 's/took=//')
  loaded=$(grep "start: interface code loaded" "$log" | grep -o 'took=[^ ]*' | sed 's/took=//' || true)
  versions=$(grep "start: code versions computed" "$log" | grep -o 'took=[^ ]*' | sed 's/took=//' || true)
  # Recorded beside the start (after it, not in it): the `deployed:` line's own time.
  for _ in $(seq 1 100); do
    grep "deployed:" "$log" >/dev/null && break
    sleep 0.1
  done
  recorded=$(grep "deployed:" "$log" | grep -o 'took=[^ ]*' | sed 's/took=//' || true)
  # Saved right after they're computed: until then no decision is saved (0.7.3.2).
  saved=$(grep "code versions saved" "$log" | grep -o 'took=[^ ]*' | sed 's/took=//' || true)
  kill -INT $pid
  wait $pid || true
  printf '%-5s start %-10s code loaded %-10s versions %-10s saved %-10s recorded %s\n' "$2" "$took" "$loaded" \
    "${versions:--}" "${saved:--}" "${recorded:--}"
}

echo "== a first start of each (migrations; for BIN every version and file is new)"
start "$BASE" base
start "$BIN" new
echo "== $ROUNDS starts each, alternating (nothing changed)"
for _ in $(seq 1 "$ROUNDS"); do
  start "$BASE" base
  start "$BIN" new
done
echo "== what was recorded"
PGTZ=UTC psql "$URL" -Atc "SELECT count(*) || ' deployments, ' || sum(changed) || ' changes' FROM deployments"
PGTZ=UTC psql "$URL" -Atc "SELECT count(*) || ' code versions, ' || (SELECT count(*) FROM code_files) || ' files, ' || pg_size_pretty(pg_total_relation_size('code_files')) || ' of file text' FROM code_versions"
echo PASS
