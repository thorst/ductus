#!/usr/bin/env bash
# The v0.6 demo: a server from nothing to MLLP over TLS, then upgraded and downgraded under
# traffic, on real systemd (Ubuntu 24.04 in a container; Postgres in its own, as on a database
# server), with the packages demo-packages.sh built (CI runs this after it; BUILD=1 builds them):
#   1. a fresh .deb, configured
#   2. `ductus init` as the ductus user, in /var/lib/ductus/project
#   3. the listener's certificate: `cert request`, signed by the hospital's CA (openssl plays it),
#      `cert show`
#   4. MLLP over TLS with client certificates, the service started, `ductus doctor` passes
#   5. messages from a sender with its certificate (`gen` with --ca, --cert, --key)
#   6. `upgrade --check --replay 1d` from the next version's portable archive
#   7. the upgrade under traffic, a handover: none refused, every message once and in order
#   8. a downgrade under traffic, on a database the newer version migrated
#   9. a sender without a client certificate is refused
# The commands given to `run` are the ones the docs show (first-interface.md, upgrade.md: the
# blocks marked `<!-- scripts/demo-v06.sh -->`; `the_pages_show_what_the_v06_demo_runs` compares).
# @DEB_OLD@, @DEB_NEW@, @ARCHIVE_NEW@ stand for the packages' versions. The whole transcript is
# written to $WORK/transcript.txt (the pages' outputs are from it).
set -euo pipefail

cd "$(dirname "$0")/.."
source scripts/dev/systemd-box.sh
began=$SECONDS
DIST=$PWD/dist
WORK=$(mktemp -d)
T=$WORK/transcript.txt
cleanup() {
  docker rm -f ductus-v06-db ductus-v06 >/dev/null 2>&1 || true
  docker network rm ductus-v06 >/dev/null 2>&1 || true
}
trap cleanup EXIT
cleanup

# The same two package versions of one binary as demo-packages.sh.
BINARY_VERSION=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"$/\1/p' Cargo.toml)
OLD=${BINARY_VERSION%%-*}-dev.1
NEW=${BINARY_VERSION%%-*}-dev.2
if [ "${BUILD:-0}" = 1 ]; then
  rm -rf "$DIST"
  for v in $OLD $NEW; do
    echo "== building the packages ($v)"
    docker build -q --target packages --build-arg VERSION=$v --output "$DIST/$v" . >/dev/null
  done
fi
deb_version() { local f; f=$(basename "$(ls "$DIST/$1"/*.deb)"); f=${f#ductus_}; echo "${f%_amd64.deb}"; }
DEB_OLD=$(deb_version "$OLD")
DEB_NEW=$(deb_version "$NEW")

fail=""
check() { # check <name> <condition...>
  local name=$1
  shift
  if ! eval "$@"; then
    fail+=" $name"
    echo "   FAILED: $name"
  fi
}

# run '<the command, as the docs show it>': in the box as root, from the folder the last
# `run 'cd ...'` named; prints it and its output and adds both to the transcript. Sets OUT, RC.
DIR=/root
run() {
  local cmd=$1
  cmd=${cmd//@DEB_OLD@/"$DEB_OLD"}
  cmd=${cmd//@DEB_NEW@/"$DEB_NEW"}
  cmd=${cmd//@ARCHIVE_NEW@/"$NEW"}
  echo "\$ $cmd" >>"$T"
  echo "   \$ $cmd"
  if [[ $cmd == "cd "* ]]; then
    DIR=${cmd#cd }
    return 0
  fi
  if OUT=$(on "cd $DIR && $cmd" 2>&1); then RC=0; else RC=$?; fi
  # Progress (dpkg's "(Reading database ... 5%", rewritten in place): the last state only.
  OUT=$(sed 's/\r$//' <<<"$OUT" | tr '\r' '\n' | grep -Ev '^\(Reading database \.\.\. ?([0-9]+%)?$' || true)
  printf '%s\n' "$OUT" >>"$T"
  sed 's/^/     | /' <<<"$OUT"
  [ "$RC" = 0 ] || echo "     (exit $RC)"
}
journal() { on "journalctl -u ductus --no-pager -o cat"; }
installed() { on "dpkg-query -W -f '\${Version}' ductus"; }
handovers() { journal | grep -c 'took over: receiving never stopped' || true; }
messages() { docker exec "$BOX_DB" psql -U ductus -Atc "SELECT count(*) FROM messages"; }

echo "== a database server and a new Ubuntu 24.04 server (systemd)"
box_network ductus-v06 ductus-v06-db --network-alias db1.hospital.org
box_images
# Senders reach the engine as mllp.hospital.org (the hospital's DNS would say so).
box_start ductus-v06 ductus-pkg-ubuntu --hostname ductus1 --add-host mllp.hospital.org:127.0.0.1
# The downloads: both packages and the newer version's portable archive.
for f in "$DIST/$OLD"/*.deb "$DIST/$NEW"/*.deb "$DIST/$NEW"/*.tar.gz; do docker cp "$f" ductus-v06:/tmp/ >/dev/null; done
# The hospital's CA, kept by its PKI team (root here); and the sending system's own folder.
on "mkdir -p /root/hospital-ca /root/epic
    openssl req -x509 -newkey rsa:2048 -nodes -keyout /root/hospital-ca/ca.key -out /root/hospital-ca/ca.pem \
      -days 365 -subj '/CN=Test Hospital CA' 2>/dev/null"

echo "== 1. install the package, tell it where its database is"
run 'cd /tmp'
run 'sudo apt install ./ductus_@DEB_OLD@_amd64.deb'
check install 'grep "It isn.t running yet" <<<"$OUT" >/dev/null && [ "$(on "systemctl is-active ductus" || true)" = inactive ]'
# What a person edits in /etc/ductus/ductus.env: the database (DUCTUS_ENV is prod already).
on "sed -i 's|^DUCTUS_DATABASE_URL=.*|DUCTUS_DATABASE_URL=postgres://ductus:pkg-test@db1.hospital.org/ductus|' /etc/ductus/ductus.env"

echo "== 2. the project, as the ductus user"
run 'cd /var/lib/ductus'
run 'sudo -u ductus ductus init project'
check init '[ "$RC" = 0 ] && on "test -f /var/lib/ductus/project/interfaces/adt-in/interface.yaml"'
run 'cd /var/lib/ductus/project'

echo "== 3. the listener's certificate"
run 'sudo -u ductus ductus cert request tls/mllp --dns mllp.hospital.org'
check request 'grep "Send tls/mllp.csr to the hospital.s CA" <<<"$OUT" >/dev/null && [ "$(on "stat -c %U:%a /var/lib/ductus/project/tls/mllp.key")" = ductus:600 ]'
# The PKI team signs it (here on the same machine) and sends back the certificate and their CA's.
run 'openssl x509 -req -in tls/mllp.csr -CA /root/hospital-ca/ca.pem -CAkey /root/hospital-ca/ca.key -CAcreateserial -out tls/mllp.pem -days 90 -copy_extensions copy'
run 'cp /root/hospital-ca/ca.pem tls/hospital-ca.pem'
cat >"$WORK/resources.yaml" <<'EOF'
# resources.yaml
certificates:
  ductus-mllp: { cert_file: tls/mllp.pem, key_file: tls/mllp.key }
  hospital-ca: { ca_file: tls/hospital-ca.pem }
EOF
cat >"$WORK/interface.yaml" <<'EOF'
# interfaces/adt-in/interface.yaml
name: adt-in
about:
  description: ADT from the EHR to the lab system
  owner: interfaces@hospital.org
  from: { application: Epic, vendor: Epic Systems }
  to: { application: Lab system, vendor: In-house }
source:
  mllp:
    port: 6661
    tls:
      certificate: ductus-mllp
      clients: { ca: hospital-ca, subjects: ["CN=epic.hospital.org"] }
destinations: { lab: { mllp: { host: 127.0.0.1, port: 6662 }, script: archive.ts } }
EOF
docker cp "$WORK/resources.yaml" ductus-v06:/var/lib/ductus/project/resources.yaml >/dev/null
docker cp "$WORK/interface.yaml" ductus-v06:/var/lib/ductus/project/interfaces/adt-in/interface.yaml >/dev/null
on "chown -R ductus:ductus /var/lib/ductus/project"
run 'sudo -u ductus ductus cert show ductus-mllp'
check cert-show '[ "$RC" = 0 ] && grep "chain    complete" <<<"$OUT" >/dev/null'

echo "== 4. MLLP over TLS with client certificates: validate, start, doctor"
run 'sudo -u ductus ductus validate --env-file /etc/ductus/ductus.env'
check validate '[ "$RC" = 0 ] && grep "^valid$" <<<"$OUT" >/dev/null'
# The lab system's receiver, which the interface sends to (ductus sink stands in for it).
on "nohup ductus sink --port 6662 >/tmp/lab.log 2>&1 &"
run 'sudo systemctl enable --now ductus'
check started '[ "$RC" = 0 ] && wait_healthy'
run 'systemctl show -p StatusText ductus'
run 'sudo -u ductus ductus doctor --env-file /etc/ductus/ductus.env'
check doctor '[ "$RC" = 0 ] && grep ", 0 failed$" <<<"$OUT" >/dev/null'
check doctor-tls 'grep -i "tls" <<<"$OUT" >/dev/null'

echo "== 5. the sending system, with its certificate"
run 'ductus cert request /root/epic/sender --dns epic.hospital.org'
run 'openssl x509 -req -in /root/epic/sender.csr -CA /root/hospital-ca/ca.pem -CAkey /root/hospital-ca/ca.key -CAcreateserial -out /root/epic/sender.pem -days 90 -copy_extensions copy'
run 'ductus gen --to mllp.hospital.org:6661 --ca /root/hospital-ca/ca.pem --cert /root/epic/sender.pem --key /root/epic/sender.key --count 100'
check sent-tls '[ "$RC" = 0 ] && grep "sent 100 messages" <<<"$OUT" >/dev/null && grep "refused 0," <<<"$OUT" >/dev/null'
wait_delivered
on "pkill -INT -f 'ductus sink'"
sleep 0.5
lab=$(on "tail -1 /tmp/lab.log")
echo "   the lab system received: ${lab#*| }"
check received-tls 'grep "unique 100 |" <<<"$lab" >/dev/null'

echo "== 6. before upgrading: upgrade --check --replay from the next version's archive"
run 'cd /tmp'
run 'tar -xzf ductus-@ARCHIVE_NEW@-linux-x64.tar.gz'
run 'sudo -u ductus ./ductus-@ARCHIVE_NEW@/ductus upgrade --check --replay 1d --env-file /etc/ductus/ductus.env --root /var/lib/ductus/project'
check upgrade-check '[ "$RC" = 0 ] && grep "^ready to upgrade" <<<"$OUT" >/dev/null'
check upgrade-check-replay 'grep -E "^✓ replay .*100 message\(s\)" <<<"$OUT" >/dev/null'

# Senders with their certificates, through the upgrade and the downgrade.
GAP_GEN="--to mllp.hospital.org:6661 --ca /root/hospital-ca/ca.pem --cert /root/epic/sender.pem --key /root/epic/sender.key"
GAP_RUN=run
gap_ok() { # gap_ok <name>: none refused, every message once and in order
  check "$1-none-refused" '[ "$(refused "$sent")" = 0 ]'
  check "$1-nothing-lost" 'grep "unique 1000 |" <<<"$received" >/dev/null'
  check "$1-nothing-twice" 'grep "duplicates 0 |" <<<"$received" >/dev/null'
  check "$1-in-order" 'grep "out of order 0 |" <<<"$received" >/dev/null'
}

echo "== 7. the upgrade, under traffic: a handover"
before=$(handovers)
gap "upgrade" 100000 'sudo apt install ./ductus_@DEB_NEW@_amd64.deb'
gap_ok upgrade
check upgrade-installed '[ "$(installed)" = "$DEB_NEW" ]'
check upgrade-handed-over '[ "$(handovers)" -gt "$before" ]'
run 'journalctl -u ductus -o cat --no-pager | grep "^handed over"'
check upgrade-pause 'grep "sending paused" <<<"$OUT" >/dev/null'

echo "== 8. the downgrade, on a database the newer version migrated"
# Both packages hold one program, so the newer version's database change is stood in for: one
# migration the older program doesn't know, recorded as applied, with its table (as the store
# test starts_on_a_database_migrated_by_a_newer_version does).
docker exec "$BOX_DB" psql -U ductus -qc "CREATE TABLE v06_demo_newer (id bigint)" \
  -c "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time)
      VALUES (9999, 'the v0.6 demo: a newer version''s change', true, '\\x00', 0)" >/dev/null
before=$(handovers)
gap "downgrade" 200000 'sudo apt install -y --allow-downgrades ./ductus_@DEB_OLD@_amd64.deb'
gap_ok downgrade
check downgrade-installed '[ "$(installed)" = "$DEB_OLD" ]'
check downgrade-handed-over '[ "$(handovers)" -gt "$before" ]'
run 'journalctl -u ductus -o cat --no-pager | grep "^handed over" | tail -1'
run 'cd /var/lib/ductus/project'
run 'sudo -u ductus ductus upgrade --check --env-file /etc/ductus/ductus.env'
check downgrade-runs-on-newer '[ "$RC" = 0 ] && grep "applied by a newer version than this one: 9999" <<<"$OUT" >/dev/null'
check downgrade-kept-table 'docker exec "$BOX_DB" psql -U ductus -Atc "SELECT count(*) FROM v06_demo_newer" >/dev/null'

echo "== 9. a sender without a client certificate"
had=$(messages)
run 'timeout 5 ductus gen --to mllp.hospital.org:6661 --ca /root/hospital-ca/ca.pem --count 1'
check no-cert-refused '[ "$RC" != 0 ] && grep "wants a client certificate" <<<"$OUT" >/dev/null'
check no-cert-nothing-stored '[ "$(messages)" = "$had" ]'
run 'journalctl -u ductus -o cat --no-pager | grep "TLS handshake refused" | tail -1'
check no-cert-logged 'grep "TLS handshake refused" <<<"$OUT" >/dev/null'

echo "transcript: $T"
echo "demo-v06 took $((SECONDS - began)) s"
if [ -z "$fail" ]; then echo PASS; else echo "FAIL:$fail"; exit 1; fi
