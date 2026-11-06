#!/usr/bin/env bash
# What `ductus history` costs (0.7.3.3): a project of 1,001 interfaces with code (as
# bench-versions.sh makes it) in a git repository of COMMITS commits (each changes one interface's
# file; every 10th the measured interface's code, every 25th the shared file it imports), a
# database of its own with DEPLOYMENTS deployments that changed it (each a version of its own, its
# documentation changed with each), and an audit log of AUDIT rows (1,000,000: a twentieth naming
# it, nine in ten of those payload reads). Then `ductus history v-0` timed (with the database, `--all`, `--json`, git
# only) and `GET /interfaces/v-0/history` (first page, `all`, a page deep in the timeline), and the
# plans of the database's reads.
#
#   bash scripts/bench-history.sh            # COMMITS=1000 DEPLOYMENTS=1000 RUNS=7
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/dev/db.sh"

BIN=$(realpath "${BIN:-${CARGO_TARGET_DIR:-target}/release/ductus}")
COUNT=${COUNT:-1001}
COMMITS=${COMMITS:-1000}
DEPLOYMENTS=${DEPLOYMENTS:-1000}
RUNS=${RUNS:-7}
AUDIT=${AUDIT:-1000000}
# On disk, not /tmp (tmpfs in WSL): git reads its objects from it.
WORK=$(mktemp -d -p "$HOME")
ADMIN=$DUCTUS_DATABASE_URL
DB=bench_history_$$
URL=${ADMIN%/*}/$DB
API=17700

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
  printf 'export function clean(s: string): string {\n  return s.trim().toUpperCase();\n}\n' >"$d/map.ts"
  cat >"$d/transform.ts" <<EOF
import type { Message } from "@ductus/sdk";
import { code$l } from "../../lib/codes$l.ts";
import { clean } from "./map.ts";

export function transform(msg: Message) {
  msg.PID[5][1] = clean(msg.PID[5][1]);
  msg.OBR[4][2] = code$l(msg.OBR[4][1]);
  return msg;
}
EOF
done

echo "== a git repository of $COMMITS commits"
start=$(($(date +%s) - (COMMITS + 1) * 600))
git -C "$WORK" init -q -b main
git -C "$WORK" add .
GIT_AUTHOR_DATE="@$start +0000" GIT_COMMITTER_DATE="@$start +0000" \
  git -C "$WORK" -c user.name=bench -c user.email=bench@example.org commit -qm "the project"
# One commit per change, through fast-import (seconds, not minutes): the file as it was, with a
# line naming the change (fast-import's data replaces the whole file).
awk -v n="$COMMITS" -v count="$COUNT" -v start="$start" \
  -v map="$WORK/interfaces/v-1/map.ts" -v code="$WORK/interfaces/v-0/transform.ts" -v codes="$WORK/lib/codes0.ts" '
function slurp(f,   s, line) { s = ""; while ((getline line < f) > 0) s = s line "\n"; close(f); return s }
BEGIN {
  M = slurp(map); T = slurp(code); C = slurp(codes)
  for (i = 1; i <= n; i++) {
    if (i % 25 == 0) { path = "lib/codes0.ts"; base = C }
    else if (i % 10 == 0) { path = "interfaces/v-0/transform.ts"; base = T }
    else { path = "interfaces/v-" (i % count) "/map.ts"; base = M }
    msg = "change " i " to " path
    body = base "// change " i "\n"
    print "commit refs/heads/main"
    print "committer Person " (i % 7) " <p" (i % 7) "@example.org> " (start + i * 600) " +0000"
    print "data " length(msg); print msg
    if (i == 1) print "from refs/heads/main^0"
    print "M 100644 inline " path
    print "data " length(body); printf "%s", body; print ""
  }
}' | git -C "$WORK" fast-import --quiet
# The working folder as the last commit has it (fast-import moved the branch only): each file
# changed holds its last change's line, so rebuild them from the commits.
git -C "$WORK" reset -q --hard main
echo "   $(git -C "$WORK" rev-list --count HEAD) commits, $(git -C "$WORK" log --oneline -- interfaces/v-0/transform.ts lib/codes0.ts interfaces/v-0/interface.yaml interfaces/v-0/map.ts lib/util.ts | wc -l) of them on v-0's files"

PGTZ=UTC psql "$ADMIN" -qc "CREATE DATABASE $DB"
cleanup() {
  pkill -f "ductus run --root $WORK" || true
  sleep 1
  PGTZ=UTC psql "$ADMIN" -qc "DROP DATABASE IF EXISTS $DB WITH (FORCE)" || true
  rm -rf "$WORK"
}
trap cleanup EXIT

echo "== the first start (migrations, every version and file saved)"
export DUCTUS_DATABASE_URL=$URL
RUST_LOG=info,ductus_runtime::history=debug "$BIN" run --root "$WORK" --threads 4 --api-listen "127.0.0.1:$API" >"$WORK/engine.log" 2>&1 &
for _ in $(seq 1 600); do
  grep "deployed:" "$WORK/engine.log" >/dev/null && break
  sleep 0.1
done
if ! grep -o 'deployed:.\{0,60\}' "$WORK/engine.log" | sed -n '1s/^/   /p'; then
  echo "FAIL: no deployment recorded"
  tail -20 "$WORK/engine.log"
  exit 1
fi

echo "== $DEPLOYMENTS deployments that changed v-0, its documentation each time, $AUDIT audit rows"
PGTZ=UTC psql "$URL" -q <<SQL
DO \$\$
DECLARE
  first   deployments%ROWTYPE;
  files   jsonb := (SELECT files FROM code_versions WHERE interface = 'v-0' LIMIT 1);
  before  integer := (SELECT version_id FROM interface_versions WHERE interface = 'v-0');
  version integer;
  d       integer;
BEGIN
  SELECT * INTO first FROM deployments ORDER BY id LIMIT 1;
  FOR g IN 1..$DEPLOYMENTS - 1 LOOP
    INSERT INTO code_versions (hash, interface, files) VALUES (sha256(('bench v-0 ' || g)::bytea), 'v-0', files)
      RETURNING id INTO version;
    INSERT INTO deployments (at, environment, instance, host, version, how, by_name, reason, git_commit, uncommitted,
                             interfaces, changed, interface_set)
    VALUES (now() - ($DEPLOYMENTS - g) * interval '30 minutes', first.environment, 'bench-' || g, first.host,
            first.version, 'handover', 'person ' || (g % 5), 'change ' || g, first.git_commit, '{}', first.interfaces,
            1, first.interface_set)
      RETURNING id INTO d;
    INSERT INTO deployment_changes (deployment_id, interface, from_version, to_version, warning)
      VALUES (d, 'v-0', before, version, CASE WHEN g % 50 = 0 THEN 'replaces someone else''s change' END);
    INSERT INTO docs_history (deployment_id, at, kind, name, docs)
      VALUES (d, now() - ($DEPLOYMENTS - g) * interval '30 minutes', 'interface', 'v-0',
              jsonb_build_object('owner', 'person ' || (g % 5), 'custom', jsonb_build_object('n', g)));
    before := version;
  END LOOP;
END \$\$;
INSERT INTO audit_log (at, actor_kind, actor_name, action, subject, interface, message_id, outcome)
SELECT now() - g * interval '1 minute', 'user', 'person ' || (g % 9),
       CASE WHEN g % 200 = 0 THEN 'destination.pause' ELSE 'message.payload.read' END,
       CASE WHEN g % 200 = 0 THEN 'v-' || (g % 20) || '/receiver' END,
       'v-' || (g % 20), CASE WHEN g % 200 <> 0 THEN g END, 'ok'
FROM generate_series(1, $AUDIT) g;
ANALYZE;
SQL
PGTZ=UTC psql "$URL" -Atc "SELECT count(*) || ' deployments, ' || (SELECT count(*) FROM deployment_changes WHERE interface = 'v-0') || ' changed v-0; audit rows: ' || (SELECT count(*) FROM audit_log) || ', naming v-0: ' || (SELECT count(*) FROM audit_log WHERE interface = 'v-0') FROM deployments" | sed 's/^/   /'

# Milliseconds a command takes, RUNS times: "min / median / max".
times() {
  local all=()
  for _ in $(seq 1 "$RUNS"); do
    local t0 t1
    t0=$(date +%s%N)
    "$@" >/dev/null
    t1=$(date +%s%N)
    all+=($(((t1 - t0) / 1000000)))
  done
  printf '%s\n' "${all[@]}" | sort -n | awk '{ a[NR] = $1 } END { printf "%d / %d / %d ms\n", a[1], a[int((NR + 1) / 2)], a[NR] }'
}

echo "== git itself, for comparison: git log over v-0's five files (min / median / max of $RUNS)"
echo "   $(times git -C "$WORK" log --format=%H -- interfaces/v-0/interface.yaml interfaces/v-0/map.ts interfaces/v-0/transform.ts lib/codes0.ts lib/util.ts)"
echo "== ductus history v-0 (min / median / max of $RUNS)"
echo "   with the database:     $(times "$BIN" history v-0 --root "$WORK" --database-url "$URL")"
echo "   --json:                $(times "$BIN" history v-0 --root "$WORK" --database-url "$URL" --json)"
echo "   --all:                 $(times "$BIN" history v-0 --root "$WORK" --database-url "$URL" --all)"
echo "   git only (no database): $(times env -u DUCTUS_DATABASE_URL "$BIN" history v-0 --root "$WORK")"
"$BIN" history v-0 --root "$WORK" --database-url "$URL" --limit 8 | sed 's/^/   | /'

echo "== GET /interfaces/v-0/history (min / median / max of $RUNS)"
"$BIN" token create bench-history --role viewer >"$WORK/token"
TOKEN=$(head -1 "$WORK/token")
get() { curl -sf -o /dev/null -w '%{time_total}\n' -H "Authorization: Bearer $TOKEN" "http://127.0.0.1:$API/api/v1/interfaces/v-0/history$1"; }
api() {
  local all=()
  for _ in $(seq 1 "$RUNS"); do all+=($(get "$1" | awk '{ printf "%d", $1 * 1000 }')); done
  printf '%s\n' "${all[@]}" | sort -n | awk '{ a[NR] = $1 } END { printf "%d / %d / %d ms\n", a[1], a[int((NR + 1) / 2)], a[NR] }'
}
first=$(get '' | awk '{ printf "%d", $1 * 1000 }')
echo "   the first request (git walked): $first ms"
echo "   first page (50):        $(api '')"
echo "   all=true:               $(api '?all=true')"
deep=$(curl -sf -H "Authorization: Bearer $TOKEN" "http://127.0.0.1:$API/api/v1/interfaces/v-0/history?limit=200" | sed 's/.*"next_before":"\([^"]*\)".*/\1/')
for _ in 1 2 3; do
  deep=$(curl -sf -H "Authorization: Bearer $TOKEN" "http://127.0.0.1:$API/api/v1/interfaces/v-0/history?limit=200&before=$deep" | sed 's/.*"next_before":"\([^"]*\)".*/\1/')
done
echo "   a page 800 entries deep: $(api "?before=$deep")"
echo "   where the time goes (the engine's debug lines, last $RUNS):"
{ grep 'history read' "$WORK/engine.log" || true; } | tail -"$RUNS" | grep -o 'database=[^ ]* git=[^ ]* files=[^ ]*' | sed 's/^/     /' || true

echo "== the database's reads (EXPLAIN ANALYZE, first page)"
plan() {
  PGTZ=UTC psql "$URL" -Atc "EXPLAIN (ANALYZE, BUFFERS OFF, COSTS OFF) $2" >"$WORK/plan"
  grep -E 'Index|Seq Scan|Bitmap|Execution Time' "$WORK/plan" | sed -E 's/  +/ /g' | sed -n "1,6s/^/   $1: /p" || true
}
plan deployments "SELECT d.id FROM deployment_changes c JOIN deployments d ON d.id = c.deployment_id JOIN code_versions t ON t.id = c.to_version LEFT JOIN code_versions f ON f.id = c.from_version WHERE c.interface = 'v-0' ORDER BY c.deployment_id DESC LIMIT 55"
plan all "SELECT d.id FROM deployments d WHERE d.interface_set IN (SELECT id FROM interface_sets WHERE interfaces @> ARRAY['v-0']) AND NOT EXISTS (SELECT 1 FROM deployment_changes c WHERE c.deployment_id = d.id AND c.interface = 'v-0') ORDER BY d.id DESC LIMIT 55"
plan audit "SELECT id FROM audit_log WHERE interface = 'v-0' AND (action <> 'message.payload.read' AND outcome = 'ok') ORDER BY at DESC, id DESC LIMIT 55"
plan docs "SELECT h.id, (SELECT p.docs FROM docs_history p WHERE p.kind = h.kind AND p.name = h.name AND p.id < h.id ORDER BY p.id DESC LIMIT 1) FROM docs_history h WHERE h.kind = 'interface' AND h.name = 'v-0' ORDER BY h.id DESC LIMIT 55"
plan paths "SELECT files FROM code_versions WHERE interface = 'v-0'"
"$BIN" token revoke bench-history >/dev/null
echo PASS
