#!/usr/bin/env bash
# What a release costs at scale (0.7.4.3.2): a project of COUNT interfaces (as bench-versions.sh
# makes them, each also with a test file of two tests and a fixture), in git, on a database of its
# own. Measures, ROUNDS times each:
#   - `ductus release` (the tests run) and `--no-tests`: time, peak memory, the archive's size,
#     the manifest's and the ancestry's;
#   - `ductus release check --env prod`: before anything ran (every interface new: all tests),
#     after a start ran the commit (nothing changes: no tests), and for a commit changing the shared
#     code of a tenth of the interfaces (those tested).
#
#   bash scripts/bench-release.sh                 # COUNT=1001 ROUNDS=3 by default
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/dev/db.sh"

BIN=$(realpath "${BIN:-${CARGO_TARGET_DIR:-target}/release/ductus}")
COUNT=${COUNT:-1001}
ROUNDS=${ROUNDS:-3}
WORK=$(mktemp -d)
P=$WORK/project
DB=bench_release_$$
URL=${DUCTUS_DATABASE_URL%/*}/$DB

echo "== a project with $COUNT interfaces, tests and fixtures (in $P)"
mkdir -p "$P/lib" "$P/environments"
printf 'variables: {}\n' >"$P/environments/dev.yaml"
printf 'variables: {}\nconsole: warn\n' >"$P/environments/prod.yaml"
printf 'api: { listen: "off" }\n' >"$P/ductus.yaml"
printf '.ductus/\nout/\n*.key\n' >"$P/.gitignore"
printf 'export function pad(s: string, n: number): string {\n  return s.padEnd(n, " ");\n}\n' >"$P/lib/util.ts"
for l in $(seq 0 9); do
  {
    printf 'import { pad } from "./util.ts";\nexport const CODES_%d: Record<string, string> = {\n' "$l"
    for k in $(seq 1 40); do printf '  "C%03d": "Code %d of table %d",\n' "$k" "$k" "$l"; done
    printf '};\nexport function code%d(c: string): string {\n  return CODES_%d[c] ?? pad(c, 8);\n}\n' "$l" "$l"
  } >"$P/lib/codes$l.ts"
done
for i in $(seq 0 $((COUNT - 1))); do
  d="$P/interfaces/v-$i"
  mkdir -p "$d/fixtures"
  l=$((i % 10))
  printf 'name: v-%d\nsource: { mllp: { host: 127.0.0.1, port: %d } }\ndestinations: { receiver: { mllp: { host: 127.0.0.1, port: 19999 }, script: transform.ts } }\n' \
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
  printf 'MSH|^~\\&|EHR|HOSP|LAB|HOSP|20261107||ORM^O01|%d|P|2.5\rPID|1||%d||  doe^jane\rOBR|1|||C001\r' "$i" "$i" >"$d/fixtures/orm.hl7"
  cat >"$d/transform.test.ts" <<'EOF'
import { fixture, parse, test, expect } from "@ductus/sdk";
import { transform } from "./transform.ts";

test("upper-cases the last name", () => {
  expect(transform(parse(fixture("orm.hl7")))!.get("PID-5.1")).toBe("DOE");
});

test("drops what isn't an order", () => {
  const m = parse(fixture("orm.hl7"));
  m.set("MSH-9.1", "ADT");
  expect(transform(m)).toBe(null);
});
EOF
done
g() { git -C "$P" -c user.name=bench -c user.email=bench@example.org "$@"; }
# The editor files a project has (`ductus init` writes them; `validate` and `test` would otherwise).
"$BIN" types --root "$P" >/dev/null
g init -q -b main
g add .
g commit -qm "the project"
echo "tracked files: $(g ls-files | wc -l), $(du -sb --exclude=.git "$P" | cut -f1) bytes"

ADMIN=$DUCTUS_DATABASE_URL
PGTZ=UTC psql "$ADMIN" -qc "CREATE DATABASE $DB"
cleanup() {
  pkill -f "ductus run --root $P" || true
  sleep 1
  PGTZ=UTC psql "$ADMIN" -qc "DROP DATABASE IF EXISTS $DB WITH (FORCE)" || true
  rm -rf "$WORK"
}
trap cleanup EXIT

# One timed run: seconds and peak MB, then the command's last line.
timed() {
  local label=$1
  shift
  /usr/bin/time -f "%e %M" -o "$WORK/time" "$@" >"$WORK/out" 2>&1 || { cat "$WORK/out"; return 1; }
  read -r s kb <"$WORK/time"
  printf '%-34s %6.2f s  %4d MB  %s\n' "$label" "$s" $((kb / 1024)) "$(grep -v '^ *$' "$WORK/out" | tail -1)"
}

echo "== ductus release"
for r in $(seq 1 "$ROUNDS"); do
  timed "release (tests run) #$r" "$BIN" release --root "$P" --out "$WORK/r.tar.gz" -m bench
  # Its own file: the gate runs every test for a release made without them.
  timed "release --no-tests #$r" "$BIN" release --root "$P" --out "$WORK/untested.tar.gz" --no-tests
done
FILE=$WORK/r.tar.gz
echo "archive: $(stat -c %s "$FILE") bytes; manifest: $(tar xzOf "$FILE" release.json | wc -c) bytes; ancestry: $(tar xzOf "$FILE" ancestry | wc -c) bytes; unpacked tar: $(gzip -dc "$FILE" | wc -c) bytes"

echo "== ductus release check --env prod"
export DUCTUS_DATABASE_URL=$URL
# The gate's two parts alone, for comparison (on the project; read-only, as the gate runs them).
for r in $(seq 1 "$ROUNDS"); do
  timed "validate --read-only alone #$r" "$BIN" validate --root "$P" --env prod --read-only
  timed "test --read-only alone #$r" "$BIN" test --root "$P" --env prod --read-only
done
for r in $(seq 1 "$ROUNDS"); do
  timed "check, all new (all tests) #$r" "$BIN" release check "$FILE" --env prod
done
"$BIN" run --root "$P" --env prod >"$WORK/run.log" 2>&1 &
ENGINE=$!
until grep "started interfaces=" "$WORK/run.log" >/dev/null 2>&1; do sleep 0.5; done
until [ "$(psql -tA "$URL" -c "SELECT count(*) FROM interface_versions")" = "$COUNT" ]; do sleep 0.5; done
# Stopped, and gone, before anything is timed.
kill -INT $ENGINE
wait $ENGINE || true
for r in $(seq 1 "$ROUNDS"); do
  timed "check, nothing changes #$r" "$BIN" release check "$FILE" --env prod
done
# A tenth of the interfaces import lib/codes0.ts: change it.
sed -i 's/"Code 1 of table 0"/"Code one of table 0"/' "$P/lib/codes0.ts"
g commit -qam "codes0: a new name"
# With its tests run (a release without them makes the gate run every interface's).
"$BIN" release --root "$P" --out "$WORK/changed.tar.gz" >/dev/null
for r in $(seq 1 "$ROUNDS"); do
  timed "check, a tenth changes #$r" "$BIN" release check "$WORK/changed.tar.gz" --env prod
done
grep "interface(s) change" "$WORK/out" || true
for r in $(seq 1 "$ROUNDS"); do
  timed "check, nothing changes, again #$r" "$BIN" release check "$FILE" --env prod
done
