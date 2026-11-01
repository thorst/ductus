# Helpers for demos that run the Linux packages under real systemd, one container per machine
# ("a box"), with Postgres in a container of its own as on a database server. Sourced by
# demo-packages.sh and demo-v06.sh.
#
#   box_network NET DB [docker run options...]
#                           the network and the Postgres container (user, password and database
#                           ductus / pkg-test / ductus), ready when it returns; sets BOX_NET, BOX_DB
#   box_images              the systemd test images: ductus-pkg-ubuntu (Ubuntu 24.04), ductus-pkg-rocky (Rocky 9)
#   box_start NAME IMAGE [docker run options...]
#                           a container with systemd as PID 1 on BOX_NET, booted when it returns; sets BOX
#   on CMD                  run CMD in $BOX as root (bash -c)
#   wait_healthy            until the engine's API answers in $BOX (20 s); returns 1 if it never did
#   wait_sent [SECONDS]     until no `ductus gen` runs in $BOX (60 s)
#   wait_delivered          until nothing waits in $BOX_DB's database (60 s)
#   gap LABEL OFFSET ACTION what a sender sees while ACTION runs: 4 kept connections, 200 messages/s,
#                           ACTION 1.5 s in, a receiver on 6662; sets $sent and $received (their last lines).
#                           GAP_GEN gives the sender's options (default `--to 127.0.0.1:6661`),
#                           GAP_RUN the function that runs ACTION (default `on`)
#   refused TEXT            the "refused N" count in a sender's last line

box_network() { # box_network NET DB [docker run options...]
  BOX_NET=$1 BOX_DB=$2
  shift 2
  docker network create "$BOX_NET" >/dev/null
  docker run -d --name "$BOX_DB" --network "$BOX_NET" "$@" -e POSTGRES_USER=ductus -e POSTGRES_PASSWORD=pkg-test \
    -e POSTGRES_DB=ductus "${POSTGRES_IMAGE:-public.ecr.aws/docker/library/postgres:18}" >/dev/null
  for _ in $(seq 1 60); do
    docker exec "$BOX_DB" pg_isready -U ductus >/dev/null 2>&1 && break
    sleep 0.5
  done
}

# The distribution + systemd + curl; Ubuntu's also openssl and sudo (demo-v06: the CA, and
# commands as the ductus user, as on a server).
box_images() {
  docker build -q -t ductus-pkg-ubuntu - >/dev/null <<'EOF'
FROM ubuntu:24.04
RUN apt-get update && apt-get install -y --no-install-recommends systemd systemd-sysv curl ca-certificates procps \
      openssl sudo \
    && rm -rf /var/lib/apt/lists/*
CMD ["/sbin/init"]
EOF
  docker build -q -t ductus-pkg-rocky - >/dev/null <<'EOF'
FROM quay.io/rockylinux/rockylinux:9
RUN dnf install -y --setopt=install_weak_deps=False systemd procps-ng && dnf clean all
CMD ["/sbin/init"]
EOF
}

box_start() { # box_start NAME IMAGE [docker run options...]
  BOX=$1
  local image=$2 state
  shift 2
  docker run -d --name "$BOX" --network "$BOX_NET" --privileged --cgroupns=host -v /sys/fs/cgroup:/sys/fs/cgroup:rw \
    --tmpfs /run --tmpfs /run/lock "$@" "$image" >/dev/null
  for _ in $(seq 1 60); do
    state=$(on "systemctl is-system-running 2>/dev/null" || true)
    [[ "$state" == running || "$state" == degraded ]] && break
    sleep 0.5
  done
}

on() { docker exec "$BOX" bash -c "$1"; }

wait_healthy() {
  for _ in $(seq 1 100); do
    on "curl -sf http://127.0.0.1:7700/api/v1/health >/dev/null" && return 0
    sleep 0.2
  done
  return 1
}

wait_sent() { # wait_sent [SECONDS]
  for _ in $(seq 1 $((${1:-60} * 5))); do
    on "pgrep -f '[d]uctus gen' >/dev/null" || return 0 # [d]: not this command's own bash
    sleep 0.2
  done
}

wait_delivered() {
  local left
  for _ in $(seq 1 120); do
    left=$(docker exec "$BOX_DB" psql -U ductus -Atc "SELECT count(*) FROM deliveries WHERE status = 0")
    [ "$left" = 0 ] && return 0
    sleep 0.5
  done
}

# Sequence numbers from OFFSET, so runs don't look like each other's duplicates.
gap() { # gap <label> <offset> <action>
  local label=$1 offset=$2 action=$3 file=${1//[^a-z0-9]/-}
  on "nohup ductus sink --port 6662 >/tmp/sink-$file.log 2>&1 &"
  on "nohup ductus gen ${GAP_GEN:---to 127.0.0.1:6661} --count 1000 --patients 100 --connections 4 --rate 200 --offset $offset >/tmp/gen-$file.log 2>&1 &"
  sleep 1.5
  "${GAP_RUN:-on}" "$action"
  wait_sent
  wait_delivered
  on "pkill -INT -f 'ductus sink'"
  sleep 0.5
  sent=$(on "tail -1 /tmp/gen-$file.log")
  received=$(on "tail -1 /tmp/sink-$file.log")
  echo "   $label: ${sent#*), } | receiver: ${received#*| }"
}

refused() { sed -n 's/.*refused \([0-9]*\),.*/\1/p' <<<"$1"; }
