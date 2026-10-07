#!/usr/bin/env bash
# v0.4 demo: file, SFTP and scheduled sources.
#   rows       CSV file split into records: one JSON file per row (dedupe on id)
#   lab-batch  HL7 batch file (FHS/BHS/BTS/FTS) split per MSH, delivered in order
#   big        a ROWS-row CSV file (default 300,000). The engine is killed (kill -9) mid-file and
#              restarted: every row must be stored exactly once
#   nightly    schedule (every 1s): transform gets {due, previous} and returns CSV
#   sftp-relay HL7 from an SFTP server to an SFTP server (key login from a secret, pinned host
#              key); skipped if sshd isn't installed. sftp-unpinned shows the host key refusal.
set -euo pipefail

BIN=$(realpath "${BIN:-${CARGO_TARGET_DIR:-target}/release/ductus}")
DB=${DUCTUS_DATABASE_URL:-postgres://todd:ductus@localhost/ductus_dev}
ROWS=${ROWS:-300000}
SSHD=${SSHD:-/usr/sbin/sshd}
SFTP_PORT=${SFTP_PORT:-2222}
P=$(mktemp -d)
export DUCTUS_DATABASE_URL=$DB
ENGINE=; SSHD_PID=
cleanup() {
  for pid in $ENGINE $SSHD_PID; do kill "$pid" 2>/dev/null || true; done
}
trap cleanup EXIT
psql -q "$DB" -c "TRUNCATE messages, deliveries, dedupe_keys, source_state"

FAILED=0
check() { # name, expected, actual
  if [ "$2" = "$3" ]; then echo "  ✓ $1"; else echo "  ✗ $1: expected [$2], got [$3]"; FAILED=1; fi
}
count() { ls "$1" 2>/dev/null | wc -l; }
wait_for() { # seconds, 'condition' (evaluated each time, so quote it)
  local deadline=$((SECONDS + $1))
  until eval "$2"; do [ $SECONDS -ge $deadline ] && return 1; sleep 0.1; done
}
sql() { psql -tAq "$DB" -c "$1"; }

mkdir -p "$P/environments" "$P/interfaces"/{rows,lab-batch,big,nightly}/in
echo "variables: {}" >"$P/environments/dev.yaml"

cat >"$P/interfaces/rows/interface.yaml" <<'EOF'
name: rows
source: { file: { dir: in, pattern: "*.csv", poll: 500ms, split: records } }
format: csv
output: json
ordering: { key: mrn }
dedupe: { keys: [id] }
script: transform.ts
destination: { file: { dir: out } }
EOF
cat >"$P/interfaces/rows/transform.ts" <<'EOF'
import type { CsvRecord } from "@ductus/sdk";
export function transform(row: CsvRecord) {
  return { id: row.id, mrn: row.mrn, note: row.note.replace(/\n/g, " / ") };
}
EOF

cat >"$P/interfaces/lab-batch/interface.yaml" <<'EOF'
name: lab-batch
source: { file: { dir: in, pattern: "*.hl7", poll: 500ms, split: records, after: delete } }
ordering: { key: PID-3.1 }
destination: { file: { dir: out } }
EOF

cat >"$P/interfaces/big/interface.yaml" <<'EOF'
name: big
source: { file: { dir: in, poll: 500ms, split: records } }
format: csv
durability: relaxed
script: transform.ts
destination: { file: { dir: out } }
EOF
# Ingest is what's being shown: the transform filters every row.
echo 'export function transform() { return null; }' >"$P/interfaces/big/transform.ts"

cat >"$P/interfaces/nightly/interface.yaml" <<'EOF'
name: nightly
source: { schedule: { every: 1s } }
format: csv
script: transform.ts
destination: { file: { dir: out } }
EOF
cat >"$P/interfaces/nightly/transform.ts" <<'EOF'
import type { ScheduledRun } from "@ductus/sdk";
export function transform(run: ScheduledRun) {
  return [{ due: run.due, previous: run.previous ?? "" }];
}
EOF

# ---- SFTP: a throwaway sshd as this user, on localhost, with fresh keys
SFTP=0
if [ -x "$SSHD" ]; then
  SFTP=1
  K="$P/ssh"; R="$P/remote"
  mkdir -p "$K" "$R/outbound" "$R/inbound" "$P/interfaces"/{sftp-relay,sftp-unpinned}
  ssh-keygen -q -t ed25519 -N "" -f "$K/host_key"
  ssh-keygen -q -t ed25519 -N "" -f "$K/client_key"
  cp "$K/client_key.pub" "$K/authorized_keys"
  cat >"$K/sshd_config" <<EOF
Port $SFTP_PORT
ListenAddress 127.0.0.1
HostKey $K/host_key
PidFile $K/sshd.pid
AuthorizedKeysFile $K/authorized_keys
StrictModes no
UsePAM no
PasswordAuthentication no
Subsystem sftp internal-sftp
EOF
  "$SSHD" -D -e -f "$K/sshd_config" 2>"$K/sshd.log" &
  SSHD_PID=$!
  FINGERPRINT=$(ssh-keygen -lf "$K/host_key.pub" | awk '{print $2}')
  cat >"$P/resources.yaml" <<EOF
sftp:
  lab:
    host: 127.0.0.1
    port: $SFTP_PORT
    user: $(id -un)
    private_key: \${secret:LAB_SFTP_KEY}
    host_key: $FINGERPRINT
  lab_unpinned:
    host: 127.0.0.1
    port: $SFTP_PORT
    user: $(id -un)
    private_key: \${secret:LAB_SFTP_KEY}
EOF
  cat >"$P/interfaces/sftp-relay/interface.yaml" <<EOF
name: sftp-relay
source: { sftp: { server: lab, dir: $R/outbound, pattern: "*.hl7", poll: 500ms, split: records } }
destination: { sftp: { server: lab, dir: $R/inbound } }
EOF
  cat >"$P/interfaces/sftp-unpinned/interface.yaml" <<EOF
name: sftp-unpinned
source: { sftp: { server: lab_unpinned, dir: $R/outbound, pattern: "*.never", poll: 500ms } }
destination: { file: { dir: out } }
EOF
fi

cd "$P"
export DUCTUS_SECRET_KEY=$("$BIN" secret init 2>/dev/null)
[ $SFTP = 1 ] && "$BIN" secret set LAB_SFTP_KEY <"$K/client_key" >/dev/null
echo "== validate"; "$BIN" validate | tail -1 || { echo "FAIL: validation"; exit 1; }

start() { # log file
  LOG=$1
  "$BIN" run >"$LOG" 2>&1 &
  ENGINE=$!
  wait_for 20 'grep -q "started interfaces=" "$LOG"' || { cat "$LOG"; exit 1; }
}
start run.log

echo "== CSV split into records"
printf 'id,mrn,note\r\n1,100,first\r\n2,200,"two\r\nlines, ""quoted"""\r\n3,100,third\r\n1,100,repeat of 1\r\n' >rows.tmp
mv rows.tmp interfaces/rows/in/admits.csv   # writers should rename into place; a slow write is fine too
OUT=interfaces/rows/out
wait_for 15 '[ "$(count $OUT)" -ge 3 ]' || true
check "3 rows delivered (the repeated id is a duplicate)" "3" "$(count $OUT)"
check "quoted line break kept" '{"id":"2","mrn":"200","note":"two / lines, \"quoted\""}' "$(cat "$(grep -l '"id":"2"' $OUT/*)")"
wait_for 5 '[ -f interfaces/rows/in/archive/admits.csv ]' || true
check "file archived after storing" "yes" "$([ -f interfaces/rows/in/archive/admits.csv ] && echo yes || echo no)"
check "duplicates recorded" "1" "$(sql "SELECT count(*) FROM messages WHERE interface = 'rows' AND status = 4")"

echo "== HL7 batch file split per message"
printf 'FHS|^~\\&|LAB\rBHS|^~\\&|LAB\r' >batch.tmp
for i in 1 2 3; do printf 'MSH|^~\\&|LAB|H|EMR|H|20261006||ORU^R01|C%s|P|2.5\rPID|1||10%s\rOBX|1|NM|GLU||9%s\r' $i $i $i >>batch.tmp; done
printf 'BTS|3\rFTS|1\r' >>batch.tmp
mv batch.tmp interfaces/lab-batch/in/results.hl7
OUT=interfaces/lab-batch/out
wait_for 15 '[ "$(count $OUT)" -ge 3 ]' || true
check "3 messages from one batch file" "3" "$(count $OUT)"
check "envelope segments dropped" "MSH PID OBX" "$(head -c 300 $OUT/$(ls $OUT | sort -n | head -1) | tr '\r' '\n' | cut -c1-3 | tr '\n' ' ' | sed 's/ $//')"
wait_for 5 '[ ! -f interfaces/lab-batch/in/results.hl7 ]' || true
check "after: delete removed the file" "gone" "$([ -f interfaces/lab-batch/in/results.hl7 ] && echo there || echo gone)"

echo "== schedule"
OUT=interfaces/nightly/out
wait_for 10 '[ "$(count $OUT)" -ge 3 ]' || true
FIRST=$(ls $OUT | sort -n | head -1); SECOND=$(ls $OUT | sort -n | sed -n 2p)
check "runs every second" "yes" "$([ "$(count $OUT)" -ge 3 ] && echo yes || echo no)"
check "first run has no previous" "" "$(tail -1 $OUT/$FIRST | cut -d, -f2)"
check "each run gets the previous run's time" "$(tail -1 $OUT/$FIRST | cut -d, -f1)" "$(tail -1 $OUT/$SECOND | cut -d, -f2)"

if [ $SFTP = 1 ]; then
  echo "== SFTP to SFTP (key login from a secret, pinned host key)"
  printf 'MSH|^~\\&|A|B|C|D|20261006||ADT^A01|S1|P|2.5\rPID|1||555\r' >"$R/outbound/.adt.tmp"
  printf 'MSH|^~\\&|A|B|C|D|20261006||ADT^A08|S2|P|2.5\rPID|1||555\r' >>"$R/outbound/.adt.tmp"
  mv "$R/outbound/.adt.tmp" "$R/outbound/adt.hl7"
  wait_for 20 '[ "$(count "$R/inbound")" -ge 2 ]' || true
  check "2 messages written to the SFTP destination" "2" "$(count "$R/inbound")"
  check "no temp files left" "0" "$(ls -A "$R/inbound" | grep -c '^\.' || true)"
  check "A08 after A01" "ADT^A08" "$(cat "$R/inbound/$(ls "$R/inbound" | sort -n | tail -1)" | cut -d'|' -f9)"
  wait_for 5 '[ -f "$R/outbound/archive/adt.hl7" ]' || true
  check "source file archived on the server" "yes" "$([ -f "$R/outbound/archive/adt.hl7" ] && echo yes || echo no)"
  check "unpinned server refused, fingerprint shown" "yes" "$(grep -q "has host key $FINGERPRINT" run.log && echo yes || echo no)"
else
  echo "== SFTP skipped ($SSHD not found)"
fi

echo "== large file, engine killed mid-file (kill -9), then restarted"
python3 - "$ROWS" >big.tmp <<'PY'
import sys
n = int(sys.argv[1])
out = sys.stdout
out.write("id,mrn,test,value,note\n")
for i in range(n):
    out.write(f"{i},{100000 + i % 5000},GLU,{70 + i % 90},\"row {i}, routine\"\n")
PY
SIZE=$(du -h big.tmp | cut -f1)
mv big.tmp interfaces/big/in/labs.csv
# Kill while the file's progress row exists: it's partly stored.
wait_for 30 '[ "$(sql "SELECT count(*) FROM source_state WHERE interface = '"'big'"'")" -gt 0 ]' || true
kill -9 $ENGINE; wait $ENGINE 2>/dev/null || true
PARTIAL=$(sql "SELECT count(*) FROM messages WHERE interface = 'big'")
echo "  killed after $PARTIAL of $ROWS rows were stored"
start run2.log
wait_for 120 '[ -f interfaces/big/in/archive/labs.csv ]' || true
check "resumed where it stopped" "1" "$(grep -c 'resuming a partly read file' run2.log)"
check "every row stored" "$ROWS" "$(sql "SELECT count(*) FROM messages WHERE interface = 'big'")"
check "no row stored twice" "$ROWS" "$(sql "SELECT count(DISTINCT payload) FROM messages WHERE interface = 'big'")"
check "progress cleared" "0" "$(sql "SELECT count(*) FROM source_state WHERE interface = 'big'")"
SECS=$(grep 'file stored' run2.log | grep -o 'seconds="\?[0-9.]*' | grep -o '[0-9.]*$' | tail -1 || true)
REST=$((ROWS - PARTIAL))
[ -n "$SECS" ] && echo "  rate: $REST rows (of a $SIZE file) in ${SECS}s after restart = $(python3 -c "print(f'{$REST/$SECS:,.0f}')") rows/s"

kill -INT $ENGINE; wait $ENGINE 2>/dev/null || true
echo "== engine status"; "$BIN" status
[ "$FAILED" = 0 ] && echo PASS || { echo FAIL; echo "logs: $P/run.log $P/run2.log"; exit 1; }
