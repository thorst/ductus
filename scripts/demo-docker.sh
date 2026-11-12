#!/usr/bin/env bash
# The container image: build it, run the example relay with Postgres and a receiver in compose,
# stream ADT through it, `docker compose stop` the engine mid-stream (SIGTERM), start it again,
# and check nothing was lost or sent twice. Also: runs as a non-root user, project read-only.
# Then release mode as the container's PID 1 (0.7.4.4.2): two deploys and a SIGHUP reload hand
# over in the engine's own process (the container never restarts), a sender sending through a
# deploy is refused nothing, and `docker stop` still stops it cleanly.
# BUILD=0 skips the build (an image tagged $IMAGE must exist). Needs Docker, curl and git.
set -euo pipefail

cd "$(dirname "$0")/.."
IMAGE=${IMAGE:-ductus:dev}
COUNT=${COUNT:-3000}
RATE=${RATE:-1000}
WORK=$(mktemp -d)
export IMAGE PROJECT=$WORK/project
compose() { docker compose -p ductus-demo -f packaging/docker/compose.yaml -f "$WORK/demo.yaml" "$@"; }
# --profile releases: down leaves a profile's services otherwise (and the next run fails on its network).
trap 'compose --profile releases down -v --timeout 5 >/dev/null 2>&1 || true' EXIT

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
  releases:
    image: $IMAGE
    profiles: [releases]
    depends_on:
      postgres: { condition: service_healthy }
    environment:
      DUCTUS_DATABASE_URL: postgres://ductus:ductus@postgres/ductus
      DUCTUS_RELEASES: /tmp/ductus-releases
    ports:
      - 127.0.0.1:7701:7700
      - 6671:6661
    stop_grace_period: 45s
    restart: unless-stopped
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

echo "== release mode, the engine the container's PID 1"
compose stop engine >/dev/null 2>&1 # one engine per interface: the release runs the same one
compose start sink >/dev/null 2>&1
compose --profile releases up -d releases 2>&1 | grep -v -e Pulling -e Pulled -e Running -e Healthy || true
for _ in $(seq 1 100); do
  curl -sf http://127.0.0.1:7701/api/v1/health >/dev/null && break
  sleep 0.2
done
container=$(compose ps -q releases)
started() { docker inspect -f '{{.State.StartedAt}} restarts={{.RestartCount}} running={{.State.Running}}' "$container"; }
before=$(started)
token=$(compose exec -T releases ductus token create demo-deployer --role operator --permission deploy 2>/dev/null | head -1)
REL=$WORK/release
cp -r examples/adt-relay "$REL"
sed -i 's/host: 127.0.0.1, port: 6662/host: sink, port: 6662/' "$REL/interfaces/adt-in/interface.yaml"
g() { git -C "$REL" -c user.name=Demo -c user.email=demo@example.org -c core.autocrlf=false "$@"; }
g init -q -b main && g add . && g commit -qm first
chmod -R a+rwX "$REL"
in_project() { docker run --rm --network host -u "$(id -u):$(id -g)" -e HOME=/tmp -v "$REL:/project" -w /project "$@"; }
release() { in_project "$IMAGE" release -m "$1" >"$WORK/release.log" 2>&1 || { cat "$WORK/release.log"; exit 1; }; }
deploy() {
  in_project -e DUCTUS_API_TOKEN="$token" "$IMAGE" deploy "$(ls -t "$REL"/.ductus/releases/*.tar.gz | head -1 | sed "s|$REL|/project|")" \
    --to http://127.0.0.1:7701 -m "$1"
}
release first >/dev/null
deploy "first deploy" >"$WORK/deploy1.log" 2>&1 || true
sed 's/^/   /' "$WORK/deploy1.log"

echo "== the next release, deployed while $COUNT messages arrive at $RATE/s"
echo '// the second release' >>"$REL/interfaces/adt-in/transform.ts"
g commit -qam second
release second >/dev/null
docker run --rm --network host "$IMAGE" gen --to 127.0.0.1:6671 --count "$COUNT" --patients 300 --rate "$RATE" \
  >"$WORK/gen2.log" 2>&1 &
GEN=$!
sleep 1
deploy "second deploy" >"$WORK/deploy2.log" 2>&1 || true
grep -E '✓|✗' "$WORK/deploy2.log" | sed 's/^/   /'
wait "$GEN"
echo "   sender: $(grep -o 'rejected [0-9]*, refused [0-9]*' "$WORK/gen2.log")"

echo "== a reload: docker kill --signal HUP"
docker kill --signal HUP "$container" >/dev/null
for _ in $(seq 1 150); do
  compose logs releases 2>&1 | grep -q 'handover: handed over.*sending paused' && [ "$(compose logs releases 2>&1 | grep -c 'handover: handed over')" -ge 3 ] && break
  sleep 0.2
done
echo "   $(compose logs --no-log-prefix releases 2>&1 | grep 'handover: handed over' | tail -1 | sed 's/.*handover: //')"
after=$(started)
echo "   container before: $before"
echo "   container after:  $after"
pid1=$(compose exec -T releases cat /proc/1/comm)
compose stop releases >/dev/null 2>&1
code2=$(docker inspect -f '{{.State.ExitCode}}' "$container")
echo "   PID 1: $pid1; docker stop: exit code $code2"

fail=""
grep -q '✓ handed over to a new engine in this process' "$WORK/deploy1.log" || fail+=" first-deploy"
grep -q '✓ handed over to a new engine in this process' "$WORK/deploy2.log" || fail+=" second-deploy"
grep -q 'rejected 0, refused 0' "$WORK/gen2.log" || fail+=" refused-during-deploy"
[ "$(compose logs releases 2>&1 | grep -c 'handover: handed over')" -ge 3 ] || fail+=" sighup-reload"
[ "$before" = "$after" ] || fail+=" container-restarted"
[ "$pid1" = ductus ] || fail+=" pid1"
[ "$code2" = 0 ] || fail+=" release-mode-exit-code"
[ -n "$fail" ] && { echo "-- deploys:"; cat "$WORK/deploy1.log" "$WORK/deploy2.log"; echo "-- engine:"; compose logs --no-log-prefix releases | tail -25; }
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
