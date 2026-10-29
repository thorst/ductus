#!/usr/bin/env bash
# The Linux packages, end to end, on real systemd (a container per distribution, Postgres in a
# container of its own as on a separate database server):
#   install (not started) -> configure -> enable --now -> messages delivered
#   -> the gap a sender sees: a restart without systemd's socket store (refused), with it, and a
#      kill -9 (none refused: systemd keeps the listening sockets)
#   -> upgrade to a newer package under traffic (restarts, none refused, nothing lost or twice)
#   -> systemctl stop (clean, exit 0) -> remove (stopped; settings and project kept).
# Also installs on plain images of the distributions we claim (no systemd: just the scripts and
# `ductus --version`), and runs the portable archive's `upgrade --check` against a new database
# and against one with a running engine. BUILD=0 reuses the packages in dist/.
set -euo pipefail

cd "$(dirname "$0")/.."
DIST=$PWD/dist
NET=ductus-pkg
WORK=$(mktemp -d)
cleanup() {
  docker rm -f ductus-pkg-db ductus-pkg-deb ductus-pkg-rpm >/dev/null 2>&1 || true
  docker network rm "$NET" >/dev/null 2>&1 || true
}
trap cleanup EXIT
cleanup

if [ "${BUILD:-1}" = 1 ]; then
  rm -rf "$DIST"
  for v in 0.6.0-dev.1 0.6.0-dev.2; do # the same binary, two versions: enough to see an upgrade
    echo "== building the packages ($v)"
    docker build -q --target packages --build-arg VERSION=$v --output "$DIST/$v" . >/dev/null
  done
fi
ls "$DIST"/*/ | grep -E '\.(deb|rpm|tar\.gz)$' | sed 's/^/   /'
deb() { ls "$DIST/$1"/*.deb; }
rpm() { ls "$DIST/$1"/*.rpm; }
archive() { ls "$DIST/$1"/*.tar.gz; }
echo "   binary: $(du -h "$(deb 0.6.0-dev.1)" | cut -f1) .deb, $(du -h "$(rpm 0.6.0-dev.1)" | cut -f1) .rpm, $(du -h "$(archive 0.6.0-dev.2)" | cut -f1) .tar.gz"
# The newer version's archive, unpacked where every container sees it (/dist/unpacked/ductus-*).
rm -rf "$DIST/unpacked" && mkdir "$DIST/unpacked"
tar -xzf "$(archive 0.6.0-dev.2)" -C "$DIST/unpacked"
CHECK=$(cd "$DIST/unpacked" && ls -d ductus-*)/ductus

fail=""
check() { # check <name> <condition...>
  local name=$1
  shift
  if ! eval "$@"; then
    fail+=" $name"
    echo "   FAILED: $name"
  fi
}

echo "== installing on plain images (no systemd)"
for image in ubuntu:22.04 ubuntu:24.04 debian:12 debian:13; do
  out=$(docker run --rm --quiet -v "$DIST:/dist:ro" "$image" sh -c \
    "dpkg -i /dist/0.6.0-dev.1/*.deb >/dev/null 2>&1; ductus --version; stat -c '%U:%G %a' /etc/ductus/ductus.env" 2>&1 | tr '\n' ' ')
  echo "   $image: $out"
  check "plain-$image" '[[ "$out" == "ductus "*"root:ductus 640"* ]]'
done
for image in quay.io/rockylinux/rockylinux:9 quay.io/rockylinux/rockylinux:8; do
  out=$(docker run --rm --quiet -v "$DIST:/dist:ro" "$image" sh -c \
    "rpm -i /dist/0.6.0-dev.1/*.rpm >/dev/null 2>&1; ductus --version 2>&1 | head -1" 2>&1 | tr '\n' ' ')
  echo "   ${image##*/}: $out"
  # RHEL 8 (glibc 2.28) is reported, not required: the binary needs 2.34.
  [[ "$image" == *:8 ]] || check "plain-${image##*/}" '[[ "$out" == "ductus "* ]]'
done

echo "== Postgres (its own container, like a database server)"
docker network create "$NET" >/dev/null
docker run -d --name ductus-pkg-db --network "$NET" -e POSTGRES_USER=ductus -e POSTGRES_PASSWORD=pkg-test \
  -e POSTGRES_DB=ductus "${POSTGRES_IMAGE:-public.ecr.aws/docker/library/postgres:18}" >/dev/null
for _ in $(seq 1 60); do
  docker exec ductus-pkg-db pg_isready -U ductus >/dev/null 2>&1 && break
  sleep 0.5
done

# The example relay, sending to a receiver on the same machine.
cp -r examples/adt-relay "$WORK/project"
rm -rf "$WORK/project/.ductus"

echo "== the portable archive: upgrade --check from it, on a plain Ubuntu, against a new database"
# The project is mounted read-only: the check writes nothing into it.
out=$(docker run --rm --network "$NET" -v "$DIST:/dist:ro" -v "$WORK/project:/seed:ro" \
  -e DUCTUS_DATABASE_URL=postgres://ductus:pkg-test@ductus-pkg-db/ductus ubuntu:24.04 sh -c \
  "ls /dist/unpacked/ductus-*/; /dist/unpacked/$CHECK --version
   /dist/unpacked/$CHECK upgrade --check --root /seed; echo exit \$?" 2>&1)
echo "$out" >"$WORK/archive-new.log"
echo "   $(grep -E '^(ready|[0-9]+ problem)' <<<"$out" || echo "no verdict: see $WORK/archive-new.log")"
check archive-files 'grep -E "^LICENSE$" <<<"$out" >/dev/null && grep -E "^ductus.service$" <<<"$out" >/dev/null'
check archive-version 'grep -E "^ductus 0\.6\." <<<"$out" >/dev/null'
check archive-new-database 'grep "a new database: the first start creates every table" <<<"$out" >/dev/null'
check archive-project 'grep "✓ validate: valid" <<<"$out" >/dev/null && grep -E "✓ test: [0-9]+ passed, 0 failed" <<<"$out" >/dev/null'
check archive-ready 'grep "^exit 0$" <<<"$out" >/dev/null'

# systemd test images: the distribution + systemd + curl.
build_image() { docker build -q -t "$1" - >/dev/null; }
build_image ductus-pkg-ubuntu <<'EOF'
FROM ubuntu:24.04
RUN apt-get update && apt-get install -y --no-install-recommends systemd systemd-sysv curl ca-certificates procps \
    && rm -rf /var/lib/apt/lists/*
CMD ["/sbin/init"]
EOF
build_image ductus-pkg-rocky <<'EOF'
FROM quay.io/rockylinux/rockylinux:9
RUN dnf install -y --setopt=install_weak_deps=False systemd procps-ng && dnf clean all
CMD ["/sbin/init"]
EOF

# lifecycle <name> <image> <install command> <upgrade command> <remove command> <ext>
lifecycle() {
  local name=$1 image=$2 install=$3 upgrade=$4 remove=$5 ext=$6
  local c=ductus-pkg-$ext
  echo "== $name: the whole life of the package under systemd"
  docker run -d --name "$c" --network "$NET" --privileged --cgroupns=host -v /sys/fs/cgroup:/sys/fs/cgroup:rw \
    --tmpfs /run --tmpfs /run/lock -v "$DIST:/dist:ro" -v "$WORK/project:/seed:ro" "$image" >/dev/null
  on() { docker exec "$c" bash -c "$1"; }
  for _ in $(seq 1 60); do
    state=$(on "systemctl is-system-running 2>/dev/null" || true)
    [[ "$state" == running || "$state" == degraded ]] && break
    sleep 0.5
  done

  on "$install /dist/0.6.0-dev.1/*.$ext" >"$WORK/$ext-install.log" 2>&1
  check "$ext-install-message" 'grep "It isn'"'"'t running yet" "$WORK/$ext-install.log" >/dev/null'
  check "$ext-not-started" '[ "$(on "systemctl is-active ductus" || true)" = inactive ]'
  check "$ext-not-enabled" '[ "$(on "systemctl is-enabled ductus" || true)" = disabled ]'
  echo "   installed: not enabled, not started, and it says what to do next"

  on "sed -i 's|^DUCTUS_DATABASE_URL=.*|DUCTUS_DATABASE_URL=postgres://ductus:pkg-test@ductus-pkg-db/ductus|; s|^DUCTUS_ENV=prod|DUCTUS_ENV=dev|' /etc/ductus/ductus.env
      cp -r /seed /var/lib/ductus/project && chown -R ductus:ductus /var/lib/ductus/project
      systemctl enable --now ductus 2>/dev/null"
  local healthy=0
  for _ in $(seq 1 100); do
    on "curl -sf http://127.0.0.1:7700/api/v1/health >/dev/null" && healthy=1 && break
    sleep 0.2
  done
  check "$ext-healthy" '[ $healthy = 1 ]'
  [ $healthy = 1 ] || on "journalctl -u ductus --no-pager | tail -20"
  echo "   configured and started: $(on "journalctl -u ductus --no-pager -o cat | grep -o 'started interfaces=[0-9]*' | tail -1")"
  # Type=notify: systemd heard READY=1 (`systemctl start` waited for every interface).
  check "$ext-notify-ready" '[ "$(on "systemctl show -p StatusText --value ductus")" = "running 1 interface" ]'

  # What a sender sees while systemd restarts or kills the engine: 4 kept connections, 200
  # messages/s, the action 1.5 s in. Sequence numbers from <offset> (the example dedupes MSH-10).
  gap() { # gap <label> <offset> <action>
    local label=$1 offset=$2 action=$3 file=${1//[^a-z0-9]/-}
    on "nohup ductus sink --port 6662 >/tmp/sink-$file.log 2>&1 &"
    on "nohup ductus gen --to 127.0.0.1:6661 --count 1000 --patients 100 --connections 4 --rate 200 --offset $offset >/tmp/gen-$file.log 2>&1 &"
    sleep 1.5
    on "$action"
    for _ in $(seq 1 300); do
      on "pgrep -f '[d]uctus gen' >/dev/null" || break # [d]: not this command's own bash
      sleep 0.2
    done
    for _ in $(seq 1 120); do
      left=$(docker exec ductus-pkg-db psql -U ductus -Atc "SELECT count(*) FROM deliveries WHERE status = 0")
      [ "$left" = 0 ] && break
      sleep 0.5
    done
    on "pkill -INT -f 'ductus sink'"
    sleep 0.5
    sent=$(on "tail -1 /tmp/gen-$file.log")
    received=$(on "tail -1 /tmp/sink-$file.log")
    echo "   $label: ${sent#*), } | receiver: ${received#*| }"
  }
  refused() { sed -n 's/.*refused \([0-9]*\),.*/\1/p' <<<"$1"; }

  # Before: the same restart without systemd's store (as up to 0.6.6.1). A stop flushes the store.
  on "mkdir -p /etc/systemd/system/ductus.service.d
      printf '[Service]\nFileDescriptorStoreMax=0\n' >/etc/systemd/system/ductus.service.d/no-store.conf
      systemctl daemon-reload && systemctl stop ductus && systemctl start ductus"
  gap "restart without the store" 100000 "systemctl restart ductus"
  check "$ext-gap-measured" '[ "$(refused "$sent")" -gt 0 ]' # the measurement sees a gap when there is one
  on "rm -r /etc/systemd/system/ductus.service.d && systemctl daemon-reload && systemctl stop ductus && systemctl start ductus"

  gap "restart" 200000 "systemctl restart ductus"
  check "$ext-restart-none-refused" '[ "$(refused "$sent")" = 0 ]'
  check "$ext-restart-nothing-lost" 'grep "unique 1000 |" <<<"$received" >/dev/null'
  check "$ext-restart-nothing-twice" 'grep "duplicates 0 |" <<<"$received" >/dev/null'
  check "$ext-restart-in-order" 'grep "out of order 0 |" <<<"$received" >/dev/null'
  check "$ext-restart-adopted" 'on "journalctl -u ductus --no-pager -o cat | grep \"systemd kept the listening sockets\" >/dev/null"'

  # A crash: systemd starts it again after RestartSec (5 s); senders wait in the backlog meanwhile.
  # A message stored whose ACK was lost is sent again and recognized (dedupe); one sent whose
  # status wasn't saved goes again (at least once), so duplicates are reported, not failed.
  local restarts
  restarts=$(on "systemctl show -p NRestarts --value ductus")
  gap "kill -9" 300000 "systemctl kill -s KILL ductus"
  check "$ext-crash-restarted" '[ "$(on "systemctl show -p NRestarts --value ductus")" -gt "$restarts" ]'
  check "$ext-crash-none-refused" '[ "$(refused "$sent")" = 0 ]'
  check "$ext-crash-nothing-lost" 'grep "unique 1000 |" <<<"$received" >/dev/null'

  # A receiver, then traffic; the upgrade lands a third of the way in.
  on "nohup ductus sink --port 6662 >/tmp/sink.log 2>&1 &"
  on "nohup ductus gen --to 127.0.0.1:6661 --count 3000 --patients 300 --rate 1000 >/tmp/gen.log 2>&1 &"
  sleep 1
  local pid_before
  pid_before=$(on "systemctl show -p MainPID --value ductus")
  on "$upgrade /dist/0.6.0-dev.2/*.$ext" >"$WORK/$ext-upgrade.log" 2>&1
  local pid_after
  pid_after=$(on "systemctl show -p MainPID --value ductus")
  check "$ext-restarted" '[ "$pid_before" != "$pid_after" ] && [ "$pid_after" != 0 ]'
  echo "   upgraded under traffic: $(on "$7") (pid $pid_before -> $pid_after)"
  for _ in $(seq 1 100); do
    on "pgrep -f '[d]uctus gen' >/dev/null" || break
    sleep 0.2
  done
  for _ in $(seq 1 120); do
    left=$(docker exec ductus-pkg-db psql -U ductus -Atc "SELECT count(*) FROM deliveries WHERE status = 0")
    [ "$left" = 0 ] && break
    sleep 0.5
  done
  on "pkill -INT -f 'ductus sink'"
  sleep 0.5
  local sink
  sink=$(on "tail -1 /tmp/sink.log")
  echo "   receiver: ${sink#*| }"
  check "$ext-nothing-lost" 'grep "unique 3000 |" <<<"$sink" >/dev/null'
  check "$ext-nothing-twice" 'grep "duplicates 0 |" <<<"$sink" >/dev/null'
  check "$ext-in-order" 'grep "out of order 0 |" <<<"$sink" >/dev/null'
  local sender
  sender=$(on "tail -1 /tmp/gen.log")
  echo "   sender: ${sender#*), }"
  check "$ext-upgrade-none-refused" '[ "$(refused "$sender")" = 0 ]'
  check "$ext-env-kept" 'on "grep -q pkg-test /etc/ductus/ductus.env"'

  # The archive's check beside the running service, with the service's settings: it sees the
  # engine, its version, and nothing left to migrate.
  local verdict
  verdict=$(on "/dist/unpacked/$CHECK upgrade --check --env-file /etc/ductus/ductus.env; echo exit \$?" 2>&1 || true)
  echo "$verdict" >"$WORK/$ext-check.log"
  echo "   upgrade --check beside it: $(grep -E '^✓ .* on .*\(pid' <<<"$verdict" | head -1 | cut -c1-90)"
  check "$ext-check-engine" 'grep -E "^✓ .* \(pid [0-9]+\): version 0\.6\." <<<"$verdict" >/dev/null'
  check "$ext-check-migrations" 'grep "✓ migrations: nothing to apply" <<<"$verdict" >/dev/null'
  check "$ext-check-ready" 'grep "^exit 0$" <<<"$verdict" >/dev/null'

  on "systemctl stop ductus"
  local result
  result=$(on "systemctl show -p Result --value ductus")
  echo "   systemctl stop: result $result, $(on "journalctl -u ductus --no-pager -o cat | grep -o 'stopped took=[^ ]*' | tail -1")"
  check "$ext-clean-stop" '[ "$result" = success ]'
  local verify
  verify=$(on "systemd-analyze verify /usr/lib/systemd/system/ductus.service 2>&1" || true)
  check "$ext-unit-verifies" '[ -z "$verify" ]'
  [ -z "$verify" ] || echo "$verify" | sed 's/^/   /'
  echo "   exposure: $(on "systemd-analyze security ductus --no-pager 2>/dev/null | tail -1" || true)"

  on "systemctl start ductus && $remove ductus" >/dev/null 2>&1
  check "$ext-removed-stops" '[ "$(on "systemctl is-active ductus" || true)" != active ]'
  check "$ext-removed-binary" '! on "test -e /usr/bin/ductus"'
  # rpm keeps changed settings as ductus.env.rpmsave; dpkg keeps the file itself.
  check "$ext-kept-data" 'on "test -f /var/lib/ductus/project/interfaces/adt-in/interface.yaml && ls /etc/ductus/ductus.env* >/dev/null && getent passwd ductus >/dev/null"'
  echo "   removed: service stopped, binary gone; project, settings ($(on "ls /etc/ductus | tr '\n' ' '")) and user kept"
  docker rm -f "$c" >/dev/null
  docker exec ductus-pkg-db psql -U ductus -qc "DROP SCHEMA public CASCADE; CREATE SCHEMA public" >/dev/null 2>&1
}

lifecycle "Ubuntu 24.04 (.deb)" ductus-pkg-ubuntu "dpkg -i" "dpkg -i" "dpkg -r" deb "dpkg-query -W -f '\${Version}' ductus"
lifecycle "Rocky Linux 9 (.rpm)" ductus-pkg-rocky "rpm -i" "rpm -U" "rpm -e" rpm "rpm -q ductus"

echo "logs: $WORK"
if [ -z "$fail" ]; then echo PASS; else echo "FAIL:$fail"; exit 1; fi
