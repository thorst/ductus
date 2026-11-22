#!/usr/bin/env bash
# v0.7 demo (0.7.8.1): OpenPGP on file sources and destinations, against GnuPG on the partner's side.
# Keys: the GnuPG-made test keys in crates/ductus-runtime/tests/fixtures/pgp (ours with a
# passphrase, from a secret; the payer's public key).
#   1. validate lists the keys (user id, fingerprint, never expires)
#   2. a batch the payer encrypted to us and signed (`gpg --sign --encrypt`, binary) is decrypted,
#      its signature checked, split into its 3 messages, archived
#   3. each is sent back sealed (signed with ours, encrypted to the payer's): GnuPG, as the payer,
#      decrypts it and says the signature is good (skipped without gpg)
#   4. a file for another key, then an armored one: the first is quarantined as received with why,
#      and the folder goes on to the second (3 more messages); an unsigned one is quarantined too
#   5. `ductus try` takes the encrypted file as the payer sent it
#   6. a big encrypted CSV (ROWS rows, made with gpg), the engine killed (kill -9) mid-file and
#      restarted: it decrypts again, resumes, and stores every row exactly once (skipped without gpg)
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/dev/db.sh"

BIN=$(realpath "${BIN:-${CARGO_TARGET_DIR:-target}/release/ductus}")
KEYS=$(realpath "$(dirname "${BASH_SOURCE[0]}")/../crates/ductus-runtime/tests/fixtures/pgp")
DB=$DUCTUS_DATABASE_URL
ROWS=${ROWS:-200000}
P=$(mktemp -d)
export DUCTUS_DATABASE_URL=$DB
ENGINE=
GPG=0
command -v gpg >/dev/null && GPG=1
export GNUPGHOME="$P/gnupg"
cleanup() {
  [ -n "$ENGINE" ] && kill "$ENGINE" 2>/dev/null || true
  [ $GPG = 1 ] && gpgconf --kill all 2>/dev/null || true
}
trap cleanup EXIT

FAILED=0
check() { # name, expected, actual
  if [ "$2" = "$3" ]; then echo "  ✓ $1"; else echo "  ✗ $1: expected [$2], got [$3]"; FAILED=1; fi
}
count() { ls "$1" 2>/dev/null | wc -l; }
wait_for() { # seconds, 'condition'
  local deadline=$((SECONDS + $1))
  until eval "$2"; do [ $SECONDS -ge $deadline ] && return 1; sleep 0.1; done
}
sql() { psql -tAq "$DB" -c "$1"; }
for i in claims-pgp big-pgp; do
  sql "DELETE FROM source_state WHERE interface = '$i'"
  sql "DELETE FROM messages WHERE interface = '$i'"
done

mkdir -p "$P/environments" "$P/interfaces"/{claims,big}/in
echo "variables: {}" >"$P/environments/dev.yaml"
cat >"$P/resources.yaml" <<EOF
pgp:
  ours:
    key_file: $KEYS/ours.sec.asc
    passphrase: \${secret:PGP_PASSPHRASE}
  payer:
    public_key_file: $KEYS/payer.pub.asc
EOF
cat >"$P/interfaces/claims/interface.yaml" <<'EOF'
name: claims-pgp
source: { file: { dir: in, poll: 300ms, split: records, pgp: { decrypt: ours, verify: payer } } }
destinations:
  plain: { file: { dir: out } }
  payer: { file: { dir: to-payer, pgp: { encrypt: payer, sign: ours } } }
EOF
cat >"$P/interfaces/big/interface.yaml" <<'EOF'
name: big-pgp
source: { file: { dir: in, poll: 300ms, split: records, pgp: { decrypt: ours, verify: payer } } }
format: csv
durability: relaxed
script: transform.ts
destinations: { files: { file: { dir: out } } }
EOF
# Ingest is what's measured: the transform filters every row.
echo 'export function transform() { return null; }' >"$P/interfaces/big/transform.ts"

cd "$P"
export DUCTUS_SECRET_KEY=$("$BIN" secret init 2>/dev/null)
printf 'ductus-test' | "$BIN" secret set PGP_PASSPHRASE >/dev/null

echo "== validate"
"$BIN" validate >validate.txt 2>&1 || { cat validate.txt; echo "FAIL: validation"; exit 1; }
check "our key listed" "yes" "$(grep -q 'pgp key `ours`: our secret key, Ductus Test <ductus@example.org>' validate.txt && echo yes || echo no)"
check "the payer's key listed" "yes" "$(grep -q 'pgp key `payer`: a partner.s public key, Payer EDI' validate.txt && echo yes || echo no)"

start() { # log file
  LOG=$1
  "$BIN" run >"$LOG" 2>&1 &
  ENGINE=$!
  wait_for 30 'grep -q "started interfaces=" "$LOG"' || { cat "$LOG"; exit 1; }
}
start run.log

echo "== a batch the payer encrypted and signed (GnuPG, binary)"
cp "$KEYS/from-payer.hl7.pgp" in.tmp && mv in.tmp interfaces/claims/in/batch1.hl7.pgp
OUT=interfaces/claims/out
wait_for 15 '[ "$(count $OUT)" -ge 3 ]' || true
check "3 messages decrypted, checked and split" "3" "$(count $OUT)"
check "PID 100, 200, 300 in order" "100 200 300" "$(for f in $(ls $OUT | sort -n); do tr '\r' '\n' <"$OUT/$f" | grep '^PID' | cut -d'|' -f4; done | tr '\n' ' ' | sed 's/ $//')"
wait_for 5 '[ -f interfaces/claims/in/archive/batch1.hl7.pgp ]' || true
check "archived" "yes" "$([ -f interfaces/claims/in/archive/batch1.hl7.pgp ] && echo yes || echo no)"
check "no decrypted text left in the temporary folder" "0" "$(ls "${TMPDIR:-/tmp}" | grep -c '^ductus-pgp-' || true)"

echo "== sent back: signed with ours, encrypted to the payer's"
SEALED=interfaces/claims/to-payer
wait_for 15 '[ "$(count $SEALED)" -ge 3 ]' || true
check "3 sealed files, named .hl7.pgp" "3" "$(ls $SEALED | grep -c '\.hl7\.pgp$' || true)"
if [ $GPG = 1 ]; then
  mkdir -p -m 700 "$GNUPGHOME"
  gpg --batch --quiet --import "$KEYS/payer.sec.asc" "$KEYS/ours.pub.asc" 2>/dev/null
  F=$SEALED/$(ls $SEALED | sort -n | head -1)
  gpg --batch --decrypt --status-fd 3 "$F" >gpg.out 2>gpg.err 3>gpg.status || true
  check "GnuPG (as the payer) decrypts it" "yes" "$(grep -q '^MSH' gpg.out && echo yes || echo no)"
  check "and says our signature is good" "yes" "$(grep -q 'GOODSIG .* Ductus Test <ductus@example.org>' gpg.status && echo yes || echo no)"
  check "encrypted with AES-256" "yes" "$(grep -q 'DECRYPTION_INFO [0-9]* 9' gpg.status && echo yes || echo no)"
else
  echo "  (gpg not installed: GnuPG's side skipped)"
fi

echo "== a file for another key, then an armored one: the first quarantined, the folder goes on"
cp "$KEYS/not-for-us.hl7.pgp" in.tmp && mv in.tmp interfaces/claims/in/batch2.hl7.pgp
sleep 1.1   # an older time: taken first
cp "$KEYS/from-payer.hl7.asc" in.tmp && mv in.tmp interfaces/claims/in/batch3.hl7.asc
wait_for 15 '[ "$(count $OUT)" -ge 6 ]' || true
check "the armored file's 3 messages came after it" "6" "$(count $OUT)"
Q="SELECT count(*) FROM messages m JOIN deliveries d ON d.message_id = m.id AND d.received_at = m.received_at WHERE m.interface = 'claims-pgp' AND m.status = 3"
check "the other key's file quarantined at both destinations" "2" "$(sql "$Q AND d.last_error LIKE 'OpenPGP: it isn''t encrypted to our key \`ours\`%'")"
check "stored as received" "$(wc -c <"$KEYS/not-for-us.hl7.pgp")" "$(sql "SELECT length(payload) FROM messages WHERE interface = 'claims-pgp' AND status = 3")"
check "both archived" "yes" "$([ -f interfaces/claims/in/archive/batch2.hl7.pgp ] && [ -f interfaces/claims/in/archive/batch3.hl7.asc ] && echo yes || echo no)"
cp "$KEYS/unsigned.hl7.pgp" in.tmp && mv in.tmp interfaces/claims/in/batch4.hl7.pgp
wait_for 10 '[ "$(sql "$Q AND d.last_error LIKE '"'"'OpenPGP: it isn'"''"'t signed%'"'"'")" = 2 ]' || true
check "an unsigned file quarantined (verify: payer)" "2" "$(sql "$Q AND d.last_error LIKE 'OpenPGP: it isn''t signed%'")"
check "the log says so, once per file" "2" "$(grep -c 'file quarantined: it can.t be opened' run.log)"

echo "== ductus try takes the file as the payer sent it"
"$BIN" try claims-pgp "$KEYS/from-payer.hl7.pgp" >try.txt 2>&1 || true
check "decrypted and tried" "yes" "$(grep -q 'plain' try.txt && ! grep -qi 'error' try.txt && echo yes || echo no)"
"$BIN" try claims-pgp "$KEYS/not-for-us.hl7.pgp" >try2.txt 2>&1 && T=ok || T=refused
check "one for another key refused, saying why" "refused yes" "$T $(grep -q 'would quarantine this file' try2.txt && echo yes || echo no)"

if [ $GPG = 1 ]; then
  echo "== a big encrypted file, the engine killed mid-file (kill -9), then restarted"
  python3 - "$ROWS" >big.csv <<'PY'
import sys
n = int(sys.argv[1])
out = sys.stdout
out.write("id,mrn,test,value,note\n")
for i in range(n):
    out.write(f"{i},{100000 + i % 5000},GLU,{70 + i % 90},\"row {i}, routine\"\n")
PY
  SIZE=$(du -h big.csv | cut -f1)
  gpg --batch --quiet --trust-model always -u edi@payer.example -r ductus@example.org --sign --encrypt -o big.csv.pgp big.csv
  mv big.csv.pgp interfaces/big/in/labs.csv.pgp
  wait_for 60 '[ "$(sql "SELECT count(*) FROM source_state WHERE interface = '"'big-pgp'"'")" -gt 0 ]' || true
  kill -9 $ENGINE; wait $ENGINE 2>/dev/null || true
  PARTIAL=$(sql "SELECT count(*) FROM messages WHERE interface = 'big-pgp'")
  echo "  killed after $PARTIAL of $ROWS rows were stored"
  start run2.log
  wait_for 180 '[ -f interfaces/big/in/archive/labs.csv.pgp ]' || true
  wait_for 10 '[ "$(sql "SELECT count(*) FROM source_state WHERE interface = '"'big-pgp'"'")" = 0 ]' || true
  check "resumed where it stopped" "1" "$(grep -c 'resuming a partly read file' run2.log)"
  check "every row stored" "$ROWS" "$(sql "SELECT count(*) FROM messages WHERE interface = 'big-pgp'")"
  check "no row stored twice" "$ROWS" "$(sql "SELECT count(DISTINCT payload) FROM messages WHERE interface = 'big-pgp'")"
  SECS=$(grep 'file stored' run2.log | grep 'big-pgp' | grep -o 'seconds="\?[0-9.]*' | grep -o '[0-9.]*$' | tail -1 || true)
  REST=$((ROWS - PARTIAL))
  [ -n "$SECS" ] && echo "  rate: $REST rows (of a $SIZE file, decrypted again first) in ${SECS}s after restart = $(python3 -c "print(f'{$REST/$SECS:,.0f}')") rows/s"
fi

kill -INT $ENGINE; wait $ENGINE 2>/dev/null || true
ENGINE=
[ "$FAILED" = 0 ] && echo PASS || { echo FAIL; echo "logs: $P/run.log $P/run2.log"; exit 1; }
