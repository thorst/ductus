#!/usr/bin/env bash
# The container image: build it, run the example relay with Postgres and a receiver in compose,
# stream ADT through it, `docker compose stop` the engine mid-stream (SIGTERM), start it again,
# and check nothing was lost or sent twice. Also: runs as a non-root user, project read-only.
# BUILD=0 skips the build (an image tagged $IMAGE must exist). Needs only Docker and curl.
set -euo pipefail

cd "$(dirname "$0")/.."
IMAGE=${IMAGE:-ductus:dev}
COUNT=${COUNT:-3000}
RATE=${RATE:-1000}
WORK=$(mktemp -d)
export IMAGE PROJECT=$WORK/project
compose() { docker compose -p ductus-demo -f packaging/docker/compose.yaml -f "$WORK/demo.yaml" "$@"; }
trap 'compose down -v --timeout 5 >/dev/null 2>&1 || true' EXIT

if [ "${BUILD:-1}" = 1 ]; then
  echo "== building $IMAGE"
  started=$(date +%s)
  docker build -q -t "$IMAGE" --build-arg VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)" . >/dev/null
  echo "   built in $(($(date +%s) - started)) s"
fi
echo "   image: $(docker image inspect -f '{{.Size}}' "$IMAGE" | awk '{printf "%.0f MB", $1 / 1e6}'), $(docker run --rm "$IMAGE" --version)"

# The example relay, sending to the receiver container instead of this machine.
cp -r examples/adt-relay "$PROJECT"
sed -i 's/host: 127.0.0.1, port: 6662/host: sink, port: 6662/' "$PROJECT/interfaces/adt-in/interface.yaml"
chmod -R a+rX "$WORK"
cat >"$WORK/demo.yaml" <<EOF
services:
  sink:
    image: $IMAGE
    command: [sink, --port, "6662"]
    stop_signal: SIGINT # it prints its summary on Ctrl-C
EOF

echo "== starting Postgres, the engine and a receiver"
compose up -d --quiet-pull 2>&1 | grep -v -e Pulling -e Pulled || true
healthy() { curl -sf http://127.0.0.1:7700/api/v1/health >/dev/null; }
wait_healthy() {
  for _ in $(seq 1 100); do
    healthy && return
    sleep 0.2
  done
  compose logs engine | tail -20
  exit 1
}
wait_healthy
pending() { compose exec -T postgres psql -U ductus -Atc "SELECT count(*) FROM deliveries WHERE status = 0"; }

echo "== $COUNT messages at $RATE/s; docker compose stop engine a third of the way in"
docker run --rm --network host "$IMAGE" gen --to 127.0.0.1:6661 --count "$COUNT" --patients 300 --rate "$RATE" \
  >"$WORK/gen.log" 2>&1 &
GEN=$!
sleep "$(awk "BEGIN { print $COUNT / $RATE / 3 }")"
at=$(date +%s%N)
compose stop engine >/dev/null 2>&1
took=$((($(date +%s%N) - at) / 1000000))
code=$(docker inspect -f '{{.State.ExitCode}}' "$(compose ps -aq engine)")
echo "   stopped in $took ms, exit code $code: $(compose logs engine | grep -o 'stopped took=[^ ]*' | tail -1)"
compose start engine >/dev/null 2>&1
wait_healthy
wait "$GEN"
for _ in $(seq 1 120); do
  [ "$(pending)" = 0 ] && break
  sleep 0.5
done
uid=$(compose exec -T engine id -u)
compose stop sink >/dev/null 2>&1
sink=$(compose logs sink | grep 'unique' | tail -1)
echo "   receiver: ${sink#*| }"
echo "   engine runs as uid $uid"

fail=""
grep "unique $COUNT |" <<<"$sink" >/dev/null || fail+=" lost-messages"
grep "duplicates 0 |" <<<"$sink" >/dev/null || fail+=" sent-twice"
grep "out of order 0 |" <<<"$sink" >/dev/null || fail+=" out-of-order"
[ "$code" = 0 ] || fail+=" exit-code"
[ "$uid" = 10001 ] || fail+=" runs-as-root"
if [ -n "$fail" ]; then
  echo "-- sender:"
  tail -3 "$WORK/gen.log"
  echo "-- engine:"
  compose logs --no-log-prefix engine | tail -15
  echo "FAIL:$fail"
  exit 1
fi
echo PASS
