#!/usr/bin/env bash
# A deploy in place at a thousand interfaces (0.7.5.1): an engine in release mode running a project
# of COUNT MLLP interfaces on 4 pool threads (interfaces spread over them round-robin by name, so
# v-0000, v-0004, v-0008 … share a thread), each with a transform that imports a shared table.
# Traffic at RATE msg/s to each of:
#   A  v-0000  the interface deployed;
#   S  v-0004  on A's thread, whose transform awaits a call to a slow HTTP server (a lookup in
#              flight): `fetch()`, answered at once, then after SLOW ms in the last phase (S is
#              sent 2 messages a second, which one worker keeps up with even then);
#   T  v-0008  on A's thread, synchronous;
#   O  v-0001  on another thread.
# Each phase sends for SECONDS seconds; a phase with a deploy starts it after 3 s. Before the
# deploy the release is stored and checked (`POST /releases`, `/check`) so the deploy reuses the
# check: what's timed is the deploy itself. Reports per phase each interface's send latency (from
# stored to sent: `deliveries.updated_at - received_at`, p50/p99/max, ms) and from the engine's log
# the time to applied, what changing what's planned took (`plan_over`), the certificates worked
# out again, and per thread the code's load and the changed interface's pause. Phases:
#   1. no deploy (the baseline);
#   2. a code change to A (ROUNDS times);
#   3. a code change to 50 interfaces (v-0000 … v-0049; A and its neighbours among them);
#   4. a code change to A while S's lookups take SLOW ms;
#   5. (0.7.5.2.1) an interface added, then removed; 50 added, then removed (w-0000 …, MLLP on
#      ports of their own, no traffic to them);
#   6. (0.7.5.2.2) a port moved: v-0012's MLLP port, then back;
#   7. (0.7.5.2.3) a variable's new value: V13_PORT (v-0013's port), then SEND_TIMEOUT (every
#      destination's `timeout_ms`: all COUNT interfaces reached);
#   8. (0.7.5.2.4) A restarted alone: an ordering key, 2 dedicated workers, then back;
#   9. (0.7.5.3.2) A alone from a release changing A and T (`--interface`: composed, its gate run
#      in the deploy), then A and T from the next one sent at once (the second waits its turn).
# Then CYCLES interfaces added and removed again (two deploys each) and DEPLOYS deploys of A in a
# row (no traffic), each with the engine's memory before, every 25, and after; and that every
# message sent arrived once.
#
#   bash scripts/bench-deploy.sh            # COUNT=1001 ROUNDS=3 CYCLES=100 DEPLOYS=100 by default
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/dev/db.sh"

BIN=$(realpath "${BIN:-${CARGO_TARGET_DIR:-target}/release/ductus}")
COUNT=${COUNT:-1001}
ROUNDS=${ROUNDS:-3}
DEPLOYS=${DEPLOYS:-100}
CYCLES=${CYCLES:-100}
RATE=${RATE:-100}
SECONDS_=${SECONDS_:-8}
SLOW=${SLOW:-300}
WORK=$(mktemp -d)
P=$WORK/project
DB=bench_deploy_$$
URL=${DUCTUS_DATABASE_URL%/*}/$DB
API=17763
SINK=19998
SLOW_PORT=18093
BASE=21000

echo "== a project with $COUNT interfaces (in $P)"
mkdir -p "$P/lib" "$P/environments"
# SEND_TIMEOUT: every destination's; V13_PORT: v-0013's port (phase 7 changes their values).
variables() { # variables <SEND_TIMEOUT> <V13_PORT>
  for e in dev prod; do
    printf 'variables: { SLOW_URL: "http://127.0.0.1:%d/", SEND_TIMEOUT: %d, V13_PORT: %d }\nconsole: warn\n' \
      "$SLOW_PORT" "$1" "$2" >"$P/environments/$e.yaml"
  done
}
variables 5000 $((BASE + 13))
printf 'api: { listen: "off" }\n' >"$P/ductus.yaml"
printf '.ductus/\n' >"$P/.gitignore"
for l in $(seq 0 9); do
  {
    printf 'export const CODES_%d: Record<string, string> = {\n' "$l"
    for k in $(seq 1 40); do printf '  "C%03d": "Code %d of table %d",\n' "$k" "$k" "$l"; done
    printf '};\nexport function code%d(c: string): string {\n  return CODES_%d[c] ?? c;\n}\n' "$l" "$l"
  } >"$P/lib/codes$l.ts"
done
for i in $(seq 0 $((COUNT - 1))); do
  n=$(printf 'v-%04d' "$i")
  d="$P/interfaces/$n"
  mkdir -p "$d"
  port=$((BASE + i))
  [ "$i" = 13 ] && port='${V13_PORT}'
  printf 'name: %s\nsource: { mllp: { host: 127.0.0.1, port: %s } }\ndestinations: { receiver: { mllp: { host: 127.0.0.1, port: %d, timeout_ms: ${SEND_TIMEOUT} }, script: transform.ts } }\n' \
    "$n" "$port" "$SINK" >"$d/interface.yaml"
  printf 'export const ROUND = 0;\nexport function clean(s: string): string {\n  return s.trim().toUpperCase();\n}\n' >"$d/map.ts"
  if [ "$i" = 4 ]; then
    cat >"$d/transform.ts" <<'EOF'
import { env, fetch, type Message } from "@ductus/sdk";

/** A lookup in flight: the slow server answers after the bench's delay. */
export async function transform(msg: Message) {
  await fetch(env("SLOW_URL"));
  return msg;
}
EOF
  else
    cat >"$d/transform.ts" <<EOF
import type { Message } from "@ductus/sdk";
import { code$((i % 10)) } from "../../lib/codes$((i % 10)).ts";
import { clean, ROUND } from "./map.ts";

export function transform(msg: Message) {
  msg.PID[5][1] = clean(msg.PID[5][1]);
  msg.PV1[3][2] = code$((i % 10))(msg.PV1[3][2]) + (ROUND > 0 ? "" : "");
  return msg;
}
EOF
  fi
done
g() { git -C "$P" -c user.name=bench -c user.email=bench@example.org "$@"; }
"$BIN" types --root "$P" >/dev/null
g init -q -b main
g add .
g commit -qm "the project"

cat >"$WORK/slow.py" <<'EOF'
# Answers every request after the delay in the file given (ms), read per request.
import http.server, socketserver, sys, time
delay_file = sys.argv[2]
class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        try:
            ms = int(open(delay_file).read().strip() or 0)
        except Exception:
            ms = 0
        time.sleep(ms / 1000)
        self.send_response(200)
        self.send_header("Content-Length", "2")
        self.end_headers()
        self.wfile.write(b"ok")
    def log_message(self, *a):
        pass
class S(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True
S(("127.0.0.1", int(sys.argv[1])), H).serve_forever()
EOF
echo 0 >"$WORK/delay"

ADMIN=$DUCTUS_DATABASE_URL
PGTZ=UTC psql "$ADMIN" -qc "CREATE DATABASE $DB"
export DUCTUS_DATABASE_URL=$URL
running() { psql -tA "$URL" -c "SELECT pid FROM engines WHERE stopped_at IS NULL ORDER BY started_at DESC LIMIT 1"; }
cleanup() {
  set +e
  # Every engine of this run, by its folder (not from the database: the deploy that failed may
  # have left one the database doesn't name).
  pkill -INT -f "ductus run --releases $WORK/releases"
  pkill -INT -f "ductus sink --port $SINK"
  pkill -f "slow.py $SLOW_PORT"
  sleep 2
  PGTZ=UTC psql "$ADMIN" -qc "DROP DATABASE IF EXISTS $DB WITH (FORCE)"
  [ "${KEEP:-0}" = 1 ] && echo "kept: $WORK" || rm -rf "$WORK"
}
trap cleanup EXIT

python3 "$WORK/slow.py" "$SLOW_PORT" "$WORK/delay" &
"$BIN" sink --port $SINK >"$WORK/sink.log" 2>&1 &
TOKEN=$("$BIN" token create bench-deploy --role operator --permission deploy 2>/dev/null)
export RUST_LOG=${RUST_LOG:-info,ductus_runtime=debug}
LOG=$WORK/engine.log
DUCTUS_CONTROL=$WORK/control "$BIN" run --releases "$WORK/releases" --env prod --threads 4 \
  --api-listen "127.0.0.1:$API" >"$LOG" 2>&1 &
ENGINE=$!
until grep "started interfaces=" "$LOG" >/dev/null 2>&1; do
  kill -0 $ENGINE 2>/dev/null || { echo "FAIL: the engine ended:"; tail -5 "$LOG"; exit 1; }
  sleep 0.2
done

# A release of the project as committed: made, stored and checked on the engine (outside every
# timed window). Prints its id.
release() {
  "$BIN" release --root "$P" --out "$WORK/r.tar.gz" --no-tests -m "$1" >/dev/null 2>"$WORK/release.err" || { cat "$WORK/release.err"; exit 1; }
  curl -sSf -o "$WORK/post.json" -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/gzip" \
    --data-binary @"$WORK/r.tar.gz" "http://127.0.0.1:$API/api/v1/releases"
  local id
  id=$(tar xzOf "$WORK/r.tar.gz" release.json | sed -n 's/^{"format":1,"id":"\([0-9a-f]\{12\}\).*/\1/p')
  curl -sSf -o "$WORK/check.json" -X POST -H "Authorization: Bearer $TOKEN" "http://127.0.0.1:$API/api/v1/releases/$id/check"
  grep -q '"ok":true' "$WORK/check.json" || { cat "$WORK/check.json"; exit 1; }
  echo "$id"
}
# Deploy release $1; fails unless it went the way $2 says (in_place or handover).
deploy() {
  DUCTUS_API_TOKEN=$TOKEN "$BIN" deploy "$1" --to "http://127.0.0.1:$API" -m bench --json >"$WORK/deploy.json" 2>"$WORK/deploy.err" ||
    { cat "$WORK/deploy.json" "$WORK/deploy.err"; exit 1; }
  if [ "$2" = in_place ]; then
    grep -q '"applied": "in_place"' "$WORK/deploy.json" || { echo "not in place:"; cat "$WORK/deploy.json"; exit 1; }
    grep -q '"reused": true' "$WORK/deploy.json" || { echo "the check wasn't reused:"; cat "$WORK/deploy.json"; exit 1; }
  fi
}
# A change to the code of interfaces $1 … $2 (their map.ts), committed.
change() {
  for i in $(seq "$1" "$2"); do
    sed -i -E "s/ROUND = [0-9]+/ROUND = $3/" "$P/interfaces/$(printf 'v-%04d' "$i")/map.ts"
  done
  g commit -qam "round $3"
}

echo "== the first release, deployed (the handover from nothing)"
deploy "$(release first)" handover
until [ "$(psql -tA "$URL" -c "SELECT count(*) FROM interface_versions")" = "$COUNT" ]; do sleep 0.5; done
sleep 3 # the engine the handover started logs to the same file
memory() { awk '/^VmRSS/ { printf "%.0f MB", $2 / 1024 }' "/proc/$(running)/status"; }
echo "   engine: $(memory) once started"

declare -A PORT=([A]=$BASE [S]=$((BASE + 4)) [T]=$((BASE + 8)) [O]=$((BASE + 1)))
declare -A NAME=([A]=v-0000 [S]=v-0004 [T]=v-0008 [O]=v-0001)
PHASE=0
SENT=0
round_no=0
# One phase: traffic to A, S, T, O for SECONDS_ s; `$@` (a deploy) run after 3 s, timed.
phase() {
  local label=$1
  shift
  PHASE=$((PHASE + 1))
  local from lines n
  from=$(psql -tA "$URL" -c "SELECT now()")
  lines=$(wc -l <"$LOG")
  local pids=()
  for k in A S T O; do
    # S at 2 a second: its worker does one lookup at a time, so at SLOW ms it can't keep up with
    # more (a backlog isn't what's measured).
    local rate=$RATE
    [ "$k" = S ] && rate=2
    n=$((rate * SECONDS_))
    "$BIN" gen --to "127.0.0.1:${PORT[$k]}" --count $n --connections 2 --rate "$rate" --offset $((PHASE * 1000000 + ${#pids[@]} * 100000)) \
      >"$WORK/gen-$k.log" 2>&1 &
    pids+=($!)
    SENT=$((SENT + n))
  done
  local took=""
  if [ $# -gt 0 ]; then
    sleep 3
    local began
    began=$(date +%s.%N)
    "$@"
    took=$(awk "BEGIN { printf \"%.2f\", $(date +%s.%N) - $began }")
  fi
  for p in "${pids[@]}"; do wait "$p"; done
  # Everything sent in the phase delivered before it's read.
  for _ in $(seq 1 120); do
    [ "$(psql -tA "$URL" -c "SELECT count(*) FROM deliveries WHERE status = 0 AND received_at >= '$from'")" = 0 ] && break
    sleep 0.5
  done
  echo "-- $label${took:+ (the deploy command: $took s)}"
  for k in A S T O; do
    printf '   %s %s ' "$k" "${NAME[$k]}"
    psql -tA -F ' ' "$URL" -c "SELECT count(*), round(percentile_cont(0.5) WITHIN GROUP (ORDER BY ms)::numeric, 1),
        round(percentile_cont(0.99) WITHIN GROUP (ORDER BY ms)::numeric, 1), round(max(ms)::numeric, 1)
      FROM (SELECT extract(epoch FROM d.updated_at - d.received_at) * 1000 AS ms FROM deliveries d
            JOIN destinations t ON t.id = d.destination_id
            WHERE t.interface = '${NAME[$k]}' AND d.received_at >= '$from' AND d.status = 1) x" |
      awk '{ printf "sent %5d  latency ms p50 %6s  p99 %7s  max %7s\n", $1, $2, $3, $4 }'
    grep -o 'longest wait [0-9]* ms' "$WORK/gen-$k.log" | sed 's/^/      ACK: /' || true
  done
  if [ $# -gt 0 ]; then
    tail -n +$((lines + 1)) "$LOG" >"$WORK/phase.log"
    grep -o 'deploy: what it changes here worked out.*' "$WORK/phase.log" | grep -o 'changes=[0-9]* took=[^ ]*' | sed 's/^/   plan_over: /' || true
    grep -o 'deploy applied in place.*' "$WORK/phase.log" | grep -o 'interfaces=[0-9]* took=[^ ]*' | sed 's/^/   applied: /' || true
    grep 'certificates worked out again' "$WORK/phase.log" | grep -o 'took=[^ ]*' | sed 's/took=//' |
      awk '{ v = $1; if (v ~ /µs$/) { sub(/µs$/, "", v); v /= 1000 } else if (v ~ /ms$/) sub(/ms$/, "", v); else { sub(/s$/, "", v); v *= 1000 }
             n++; s += v; if (v > m) m = v } END { if (n) printf "   certificates_again: %d times, mean %.2f ms, max %.2f ms\n", n, s / n, m }'
    grep 'its new code runs from the next message' "$WORK/phase.log" |
      grep -E "interface=${NAME[A]} " | sed -E 's/.*(thread=[^ ]+) (interface=[^ ]+) (loading=[^ ]+) (paused=[^ ]+).*/   \1 \2 \3 \4/' || true
    grep 'its new code runs from the next message' "$WORK/phase.log" | grep -o 'loading=[^ ]*' | sed 's/loading=//' |
      awk '{ v = $1; if (v ~ /µs$/) { sub(/µs$/, "", v); v /= 1000 } else if (v ~ /ms$/) sub(/ms$/, "", v); else { sub(/s$/, "", v); v *= 1000 }
             n++; s += v; if (v > m) m = v } END { if (n > 1) printf "   loads: %d (interface × thread), mean %.2f ms, max %.2f ms\n", n, s / n, m }' || true
    # A restart in place (0.7.5.2.4): how long its sending paused.
    grep -o 'restarted alone on its new settings.*' "$WORK/phase.log" | grep -oE 'interface=[^ ]+|workers=[^ ]+|lanes=[^ ]+|sending_paused=[^ ]+' |
      paste -sd' ' | sed -n 's/^\(..*\)/   restart: \1/p' || true
  fi
}
deploy_round() { # $1 first interface, $2 last
  round_no=$((round_no + 1))
  change "$1" "$2" "$round_no"
  local id
  id=$(release "round $round_no")
  phase "$3 #$4" deploy "$id" in_place
}

phase "no deploy (baseline)"
for r in $(seq 1 "$ROUNDS"); do deploy_round 0 0 "a code change to A" "$r"; done
FIFTY=$((COUNT < 50 ? COUNT : 50))
for r in $(seq 1 "$ROUNDS"); do deploy_round 0 $((FIFTY - 1)) "a code change to $FIFTY interfaces" "$r"; done
echo "$SLOW" >"$WORK/delay"
phase "no deploy, S's lookups take $SLOW ms (baseline)"
for r in $(seq 1 "$ROUNDS"); do deploy_round 0 0 "a code change to A, S's lookups take $SLOW ms" "$r"; done
echo 0 >"$WORK/delay"

# Interfaces added and removed in place (0.7.5.2.1): w-0000 … on ports after the v- ones, shaped
# like them (no traffic: what's measured is the others' latency and the time to applied).
add_interfaces() { # $1 first, $2 last
  for j in $(seq "$1" "$2"); do
    local n d
    n=$(printf 'w-%04d' "$j") d="$P/interfaces/$(printf 'w-%04d' "$j")"
    mkdir -p "$d"
    printf 'name: %s\nsource: { mllp: { host: 127.0.0.1, port: %d } }\ndestinations: { receiver: { mllp: { host: 127.0.0.1, port: %d }, script: transform.ts } }\n' \
      "$n" $((BASE + COUNT + j)) "$SINK" >"$d/interface.yaml"
    printf 'import type { Message } from "@ductus/sdk";\nimport { code%d } from "../../lib/codes%d.ts";\n\nexport function transform(msg: Message) {\n  msg.PV1[3][2] = code%d(msg.PV1[3][2]);\n  return msg;\n}\n' \
      $((j % 10)) $((j % 10)) $((j % 10)) >"$d/transform.ts"
  done
  g add . && g commit -qm "w-$1 … w-$2 added"
}
remove_interfaces() { # $1 first, $2 last
  for j in $(seq "$1" "$2"); do g rm -rq "interfaces/$(printf 'w-%04d' "$j")"; done
  g commit -qm "w-$1 … w-$2 removed"
}
add_interfaces 0 0
phase "1 interface added" deploy "$(release "w-0000 added")" in_place
remove_interfaces 0 0
phase "1 interface removed" deploy "$(release "w-0000 removed")" in_place
add_interfaces 0 49
phase "50 interfaces added" deploy "$(release "50 added")" in_place
remove_interfaces 0 49
phase "50 interfaces removed" deploy "$(release "50 removed")" in_place

# A port moved in place (0.7.5.2.2): v-0012 (on A's thread, no traffic) to a port after the w- ones
# and back: the new port bound before the old one closes; what's measured is the others' latency
# and the time to applied.
move_port() { # $1 the port
  sed -i -E "s/port: $((BASE + 12)) |port: $((BASE + COUNT + 100)) /port: $1 /" "$P/interfaces/v-0012/interface.yaml"
  g commit -qam "v-0012 on $1"
}
move_port $((BASE + COUNT + 100))
phase "a port moved (v-0012)" deploy "$(release "v-0012 moved")" in_place
grep -q 'source (in place)\|"source"' "$WORK/deploy.json" || { echo "not a source change:"; cat "$WORK/deploy.json"; exit 1; }
move_port $((BASE + 12))
phase "a port moved back (v-0012)" deploy "$(release "v-0012 moved back")" in_place

# A variable's new value in place (0.7.5.2.3): only the environment files change. V13_PORT moves
# v-0013's port (one interface reached); SEND_TIMEOUT is every destination's `timeout_ms` (all
# COUNT reached, each restarted between messages). The plan reads every interface.yaml and code
# file then: `plan_over` says how long.
variables 5000 $((BASE + COUNT + 101))
g commit -qam "V13_PORT moved"
phase "a variable moved a port (v-0013)" deploy "$(release "V13_PORT moved")" in_place
grep -q 'source (V13_PORT)' "$WORK/deploy.json" || { echo "V13_PORT reached nothing:"; cat "$WORK/deploy.json"; exit 1; }
variables 6000 $((BASE + COUNT + 101))
g commit -qam "SEND_TIMEOUT 6000"
phase "a variable reaching all $COUNT interfaces" deploy "$(release "SEND_TIMEOUT 6000")" in_place
reached=$(python3 -c 'import json, sys
plan = json.load(open(sys.argv[1]))["plan"]
print(sum("destinations.receiver (SEND_TIMEOUT)" in c["what"] for c in plan["changes"]))' "$WORK/deploy.json")
[ "$reached" = "$COUNT" ] || { echo "SEND_TIMEOUT reached $reached of $COUNT"; exit 1; }

# A restarted alone (0.7.5.2.4), under its traffic: an ordering key (its lanes decided again, a
# generation added), then 2 dedicated workers (new threads, every old worker stopped first), then
# back as it was (shared, no key). What's measured: A's own send latency (its sending pauses while
# its workers restart), the others' (within noise), and the time to applied.
settings() { # $1: what A's interface.yaml gets after its first three lines
  head -n 3 "$P/interfaces/v-0000/interface.yaml" >"$WORK/a.yaml"
  printf '%b' "$1" >>"$WORK/a.yaml"
  mv "$WORK/a.yaml" "$P/interfaces/v-0000/interface.yaml"
  g commit -qam "A's settings: $1"
}
settings 'ordering: { key: PID-3.1 }\n'
phase "A restarted alone: an ordering key" deploy "$(release "A keyed")" in_place
grep -q 'restarts' "$WORK/deploy.json" || { echo "A didn't restart:"; cat "$WORK/deploy.json"; exit 1; }
settings 'ordering: { key: PID-3.1 }\nisolation: dedicated\nworkers: 2\n'
phase "A restarted alone: 2 dedicated workers" deploy "$(release "A dedicated")" in_place
settings ''
phase "A restarted alone: back to shared, no key" deploy "$(release "A as it was")" in_place

# One interface out of a release (0.7.5.3.2): a release changing A and T, A taken alone. The
# composed release isn't known before, so its gate runs inside the timed deploy (validate over
# every interface: seconds); the log says what reading both releases and composing took. Then A
# and T from the next one, sent at once: the second waits for the first, then composes over it.
composed() { # $1 release, $2… interfaces; the report into $WORK/deploy-<first interface>.json
  local id=$1 out="$WORK/deploy-$2.json"
  shift
  local flags=()
  for i in "$@"; do flags+=(--interface "$i"); done
  DUCTUS_API_TOKEN=$TOKEN "$BIN" deploy "$id" "${flags[@]}" --to "http://127.0.0.1:$API" -m bench --json >"$out" 2>&1 ||
    { cat "$out"; exit 1; }
  grep -q '"applied": "in_place"' "$out" || { echo "not in place:"; cat "$out"; exit 1; }
}
composed_report() {
  grep -o 'deploy: a release composed and stored.*' "$WORK/phase.log" | grep -oE 'files=[^ ]+|read=[^ ]+|took=[^ ]+' |
    paste -sd' ' | sed 's/^/   composed: /' || true
  for f in "$WORK"/deploy-v-*.json; do
    python3 -c 'import json, sys
r = json.load(open(sys.argv[1]))
print("   %s: waited %d ms, gate %.2f s, %s" % (", ".join(r["composed"]["interfaces"]), r["waited_ms"], r["gate"]["check"]["seconds"], r["outcome"]))' "$f"
  done
  rm -f "$WORK"/deploy-v-*.json
}
round_no=$((round_no + 1))
change 0 0 "$round_no"
change 8 8 "$round_no"
phase "A alone from a release changing A and T" composed "$(release "A and T, round $round_no")" v-0000
composed_report
round_no=$((round_no + 1))
change 0 0 "$round_no"
change 8 8 "$round_no"
at_once() { composed "$1" v-0000 & sleep 0.2; composed "$1" v-0008; wait $!; }
phase "A and T from the next release, sent at once" at_once "$(release "A and T, round $round_no")"
composed_report

echo "== $CYCLES interfaces added and removed again in a row (no traffic)"
echo "   before: $(memory)"
for r in $(seq 1 "$CYCLES"); do
  add_interfaces 0 0
  deploy "$(release "w-0000 added, $r")" in_place
  remove_interfaces 0 0
  deploy "$(release "w-0000 removed, $r")" in_place
  [ $((r % 25)) = 0 ] && echo "   after $r: $(memory)"
done

echo "== $DEPLOYS deploys of A in a row (no traffic)"
echo "   before: $(memory)"
for r in $(seq 1 "$DEPLOYS"); do
  round_no=$((round_no + 1))
  change 0 0 "$round_no"
  deploy "$(release "round $round_no")" in_place
  [ $((r % 25)) = 0 ] && echo "   after $r: $(memory); last: $(grep -o 'deploy applied in place.*' "$LOG" | tail -1 | grep -o 'took=[^ ]*')"
done
sleep 5
echo "   settled: $(memory); unpacked: $(find "$WORK/releases" -mindepth 1 -maxdepth 1 -type d | wc -l) folders"

pkill -INT -f "ductus sink --port $SINK"
sleep 0.5
echo "   receiver: $(tail -1 "$WORK/sink.log")"
if grep "unique $SENT | duplicates 0" "$WORK/sink.log" >/dev/null; then
  echo PASS
else
  echo "FAIL: $SENT sent (KEEP=1 keeps $WORK)"
  exit 1
fi
