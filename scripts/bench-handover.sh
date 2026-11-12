#!/usr/bin/env bash
# The handover at a thousand interfaces: an engine with 1,001 MLLP interfaces (traffic on one, 4
# kept connections at 200 msg/s), handed over three times (`ductus handover`, what `systemctl
# reload` runs). Reports per handover how long the new engine took to be ready (it receives
# beside the old one meanwhile), the old one's stop, and how long sending paused; and what the
# sender saw over all three (refused, dropped, longest wait). Then that every message arrived once.
#
# WARN=1 (0.7.3.3): the project is in a git repository of 1,000 commits, and before each handover
# the last commit is rewritten (as a colleague's checkout would have it) with one interface changed,
# and a different person asks: every handover then warns that it may undo someone else's change
# (`ductus handover` prints it), which the new engine works out before it's ready, outside the
# sending pause. Run it with a binary before 0.7.3.3 too (BIN=...) to compare the pause.
#
# IN_PROCESS=1 (0.7.4.4.2): no control socket, so each handover runs in the engine's own process
# (as on Windows, in a container, under a plain `ductus run`), asked by SIGHUP; timed from the
# engine's log. Also reports the process's memory and threads, settled before the first round and
# after the last (an engine that ended must leave nothing behind), and its peak (both engines run
# while they overlap).
#
# LANES=1 (0.7.4.4.4): each handover also changes every interface's lanes (8 → 4 → 8 …), so every
# round adds a lane generation to all 1,001 while the sender sends; reports how many interfaces'
# old lanes settled (the log's "lanes settled") and the slowest. Order is the receiver's check.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/dev/db.sh"

BIN=${BIN:-${CARGO_TARGET_DIR:-target}/release/ductus}
BIN=$(realpath "$BIN")
COUNT=${COUNT:-1001}
ROUNDS=${ROUNDS:-3}
WORK=$(mktemp -d)
PREFIX=bench-handover-$$
SINK=19999
BASE=20000

echo "== a project with $COUNT MLLP interfaces (in $WORK)"
for i in $(seq 0 $((COUNT - 1))); do
  name=$(printf '%s-%04d' "$PREFIX" "$i")
  mkdir -p "$WORK/interfaces/$name"
  printf 'name: %s\nsource: { mllp: { host: 127.0.0.1, port: %d } }\nordering: { key: PID-3.1, partitions: 8 }\ndestinations: { receiver: { mllp: { host: 127.0.0.1, port: %d } } }\n' \
    "$name" $((BASE + i)) "$SINK" >"$WORK/interfaces/$name/interface.yaml"
done

FIRST=$WORK/interfaces/$PREFIX-0000/interface.yaml
if [ "${WARN:-0}" = 1 ]; then
  git -C "$WORK" init -q -b main
  git -C "$WORK" add .
  git -C "$WORK" -c user.name=ann -c user.email=ann@example.org commit -qm "the project"
  awk 'BEGIN { for (i = 1; i <= 1000; i++) { msg = "change " i; body = "change " i "\n"
    print "commit refs/heads/main"; print "committer ann <ann@example.org> " (1700000000 + i) " +0000"
    print "data " length(msg); print msg; if (i == 1) print "from refs/heads/main^0"
    print "M 100644 inline NOTES"; print "data " length(body); printf "%s", body; print "" } }' |
    git -C "$WORK" fast-import --quiet
  git -C "$WORK" reset -q --hard main
fi

cleanup() {
  pkill -INT -f "ductus sink --port $SINK" || true
  pkill -f "ductus run --root $WORK" || true
}
trap cleanup EXIT

"$BIN" sink --port $SINK >"$WORK/sink.log" 2>&1 &
export RUST_LOG=${RUST_LOG:-info,ductus_runtime=debug}
if [ "${IN_PROCESS:-0}" = 1 ]; then unset DUCTUS_CONTROL; else export DUCTUS_CONTROL=$WORK/control; fi
"$BIN" run --root "$WORK" --threads 4 --api-listen off >"$WORK/engine.log" 2>&1 &
ENGINE=$!
for _ in $(seq 1 300); do
  grep "started interfaces=" "$WORK/engine.log" >/dev/null && break
  sleep 0.1
done
echo "   started: $(grep -o 'took=[^ ]*' "$WORK/engine.log" | tail -1)"

TOTAL=$((ROUNDS * 4 * 200 + 1200)) # 200 msg/s for each round's 4 s, and a little before and after
"$BIN" gen --to 127.0.0.1:$BASE --count $TOTAL --patients 200 --connections 4 --rate 200 >"$WORK/gen.log" 2>&1 &
SENDER=$!
sleep 3
memory() { awk '/^VmRSS/ { rss = $2 } /^VmHWM/ { peak = $2 } /^Threads/ { t = $2 } END { printf "%.0f MB, %d threads (peak so far %.0f MB)", rss / 1024, t, peak / 1024 }' "/proc/$1/status"; }
[ "${IN_PROCESS:-0}" = 1 ] && echo "   the process before the first round: $(memory $ENGINE)"
for round in $(seq 1 "$ROUNDS"); do
  if [ "${LANES:-0}" = 1 ]; then
    lanes=$((round % 2 == 1 ? 4 : 8))
    sed -i -E "s/partitions: [0-9]+/partitions: $lanes/" "$WORK"/interfaces/*/interface.yaml
  fi
  if [ "${IN_PROCESS:-0}" = 1 ]; then
    done_before=$(grep -c 'handover: handed over' "$WORK/engine.log" || true)
    lines_before=$(wc -l <"$WORK/engine.log")
    started=$(date +%s.%N)
    kill -HUP $ENGINE
    for _ in $(seq 1 1200); do
      [ "$(grep -c 'handover: handed over\|handover: .*can.t take over' "$WORK/engine.log" || true)" -gt "$done_before" ] && break
      sleep 0.05
    done
    took=$(echo "$(date +%s.%N) - $started" | bc)
    out=$(tail -n +$((lines_before + 1)) "$WORK/engine.log" | grep 'handover: ' || true)
    ready=$(grep -o 'ready after [0-9.]* m*s' <<<"$out" | tail -1 | sed 's/ready after //')
    stop=$(grep -o 'stopped in [0-9.]* m*s' <<<"$out" | tail -1 | sed 's/stopped in //')
    paused=$(grep -o 'sending paused [0-9.]* m*s' <<<"$out" | tail -1 | sed 's/sending paused //')
    sleep 4
    printf '   in-process handover %d: ready after %s, old engine stopped in %s, sending paused %s (SIGHUP to its end: %.2f s); then %s\n' \
      "$round" "$ready" "$stop" "$paused" "$took" "$(memory $ENGINE)"
    continue
  fi
  asker=()
  if [ "${WARN:-0}" = 1 ]; then
    echo "# round $round" >>"$FIRST"
    git -C "$WORK" -c user.name=bob -c user.email=bob@example.org commit -qa --amend -m "round $round"
    asker=(env "SUDO_USER=person-$round")
  fi
  started=$(date +%s.%N)
  out=$("${asker[@]}" "$BIN" handover --control "$WORK/control")
  took=$(echo "$(date +%s.%N) - $started" | bc)
  if grep '⚠' <<<"$out" >/dev/null; then
    echo "   handover $round warned: $(grep '⚠' <<<"$out" | sed -n 1p | cut -c1-110)…"
  fi
  ready=$(grep -o 'ready after [0-9.]* m*s' <<<"$out" | sed 's/ready after //')
  stop=$(grep -o 'stopped in [0-9.]* m*s' <<<"$out" | sed 's/stopped in //')
  paused=$(grep -o 'sending paused [0-9.]* m*s' <<<"$out" | sed 's/sending paused //')
  printf '   handover %d: ready after %s, old engine stopped in %s, sending paused %s (the command: %.2f s)\n' \
    "$round" "$ready" "$stop" "$paused" "$took"
  sleep 4
done
# What the second engine costs while both run: one engine's memory, once settled.
if [ "${IN_PROCESS:-0}" = 1 ]; then
  sleep 3
  echo "   the process after the last round: $(memory $ENGINE)"
else
  pid=$(pgrep -f "ductus run --root $WORK" | head -1)
  echo "   one engine's memory: $(ps -o rss= -p "$pid" | awk '{printf "%.0f MB", $1 / 1024}') (two run while they overlap)"
fi
wait $SENDER
echo "   sender: $(tail -1 "$WORK/gen.log" | sed 's/.*), //')"
echo "   the last takeover's phases (debug):"
awk '/taking over from the running engine/ { n = NR } { line[NR] = $0 } END { for (i = n; i <= NR; i++) print line[i] }' "$WORK/engine.log" |
  grep -E 'start: |took over' | sed -E 's/^[^ ]+ +[A-Z]+ [^ ]+ //' | sed 's/^/     /'
for _ in $(seq 1 120); do
  left=$(PGTZ=UTC psql "$DUCTUS_DATABASE_URL" -Atc "SELECT count(*) FROM deliveries d JOIN destinations t ON t.id = d.destination_id WHERE t.interface LIKE '$PREFIX-%' AND d.status = 0")
  [ "$left" = 0 ] && break
  sleep 0.5
done
if [ "${LANES:-0}" = 1 ]; then
  sleep 2
  settled=$(grep -c "lanes settled" "$WORK/engine.log" || true)
  slowest=$(grep -o 'lanes settled.* took=[0-9.]*m*s' "$WORK/engine.log" | grep -o 'took=[0-9.]*m*s' | sed 's/took=//' |
    awk '{ v = $1; if (v ~ /µs$/) { sub(/µs$/, "", v); v /= 1000000 } else if (v ~ /ms$/) { sub(/ms$/, "", v); v /= 1000 } else sub(/s$/, "", v); if (v > m) m = v } END { printf "%.2f s", m }')
  generations=$(PGTZ=UTC psql "$DUCTUS_DATABASE_URL" -Atc "SELECT count(*) FROM lane_generations WHERE interface LIKE '$PREFIX-%'")
  echo "   lanes: $settled interface changes settled (slowest $slowest after its engine started); $generations generations left for $COUNT interfaces"
fi
pkill -INT -f "ductus sink --port $SINK"
sleep 0.5
echo "   receiver: $(tail -1 "$WORK/sink.log")"
refused=$(sed -n 's/.*refused \([0-9]*\),.*/\1/p' "$WORK/gen.log" | tail -1)
if [ "$refused" = 0 ] && grep "unique $TOTAL | duplicates 0 | out of order 0" "$WORK/sink.log" >/dev/null; then
  echo PASS
else
  echo "FAIL (logs: $WORK)"
  exit 1
fi
