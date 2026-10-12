#!/usr/bin/env bash
# v0.4 demo: an async interface that looks patients up in a database (connection from
# resources.yaml, password from the encrypted secret store), kills unknown patients with a
# reason, quarantines VIPs, and tags the rest.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/dev/db.sh"

BIN=$(realpath "${BIN:-${CARGO_TARGET_DIR:-target}/release/ductus}")
DB=$DUCTUS_DATABASE_URL
PGUSER_=${PGUSER_:-todd}
PGPASS_=${PGPASS_:-ductus}
P=$(mktemp -d)
export DUCTUS_DATABASE_URL=$DB

psql -q "$DB" <<'SQL'
DROP TABLE IF EXISTS demo_patients;
CREATE TABLE demo_patients (mrn text PRIMARY KEY, display_name text, vip boolean DEFAULT false);
INSERT INTO demo_patients SELECT (100000 + i)::text, 'Patient ' || i, i IN (3, 7) FROM generate_series(0, 9) i;
TRUNCATE messages, deliveries, dedupe_keys;
SQL

mkdir -p "$P/interfaces/lookup" "$P/environments"
cat >"$P/environments/dev.yaml" <<EOF
variables:
  FACILITY: YORK-MAIN
  DB_HOST: localhost
  DB_NAME: $(sed -E 's#.*/([^/?]+).*#\1#' <<<"$DB")
  DB_USER: $PGUSER_
EOF
cat >"$P/resources.yaml" <<'EOF'
databases:
  patient_db:
    postgres:
      host: ${DB_HOST}
      database: ${DB_NAME}
      user: ${DB_USER}
      password: ${secret:DB_PASSWORD}
      max_connections: 4
EOF
cat >"$P/interfaces/lookup/interface.yaml" <<'EOF'
name: lookup
source: { mllp: { port: 6671 } }
script: transform.ts
ordering: { key: PID-3.1 }
destination: { mllp: { host: 127.0.0.1, port: 6662 } }
EOF
cat >"$P/interfaces/lookup/transform.ts" <<'EOF'
import { db, env, kill, quarantine, type Message } from "@ductus/sdk";

interface Patient { display_name: string; vip: boolean }

export async function transform(msg: Message) {
  const patient = await db("patient_db").one<Patient>(
    "SELECT display_name, vip FROM demo_patients WHERE mrn = $1",
    [msg.PID[3][1]],
  );
  if (!patient) return kill("unknown patient");
  if (patient.vip) throw quarantine("VIP patient: route manually");
  msg.add("ZLK", [patient.display_name, env("FACILITY")]);
  return msg;
}
EOF

cd "$P"
export DUCTUS_SECRET_KEY=$("$BIN" secret init 2>/dev/null)
echo "$PGPASS_" | "$BIN" secret set DB_PASSWORD >/dev/null
echo "== validate"; "$BIN" validate || { echo "FAIL: validation"; exit 1; }

"$BIN" sink --port 6662 >"$P/sink.log" 2>&1 &
SINK=$!
"$BIN" run >"$P/run.log" 2>&1 &
ENGINE=$!
until grep -q "started" "$P/run.log"; do kill -0 $ENGINE || { cat "$P/run.log"; exit 1; }; sleep 0.2; done

echo "== 200 messages for 20 patients (10 known, 2 of them VIP)"
"$BIN" gen --to 127.0.0.1:6671 --count 200 --patients 20 | tail -1
for _ in $(seq 1 60); do "$BIN" status | grep pending >/dev/null || break; sleep 0.5; done
kill -INT $SINK $ENGINE; wait 2>/dev/null || true

echo "== results"
"$BIN" status
psql -q -t "$DB" -c "SELECT status, coalesce(last_error, ''), count(*) FROM messages WHERE interface = 'lookup' GROUP BY 1, 2 ORDER BY 1"
echo "   sink $(tail -1 "$P/sink.log")"

sent=$(psql -tA "$DB" -c "SELECT count(*) FROM messages WHERE interface='lookup' AND status=1")
killed=$(psql -tA "$DB" -c "SELECT count(*) FROM messages WHERE interface='lookup' AND status=2 AND last_error = 'killed: unknown patient'")
vip=$(psql -tA "$DB" -c "SELECT count(*) FROM messages WHERE interface='lookup' AND status=3 AND last_error = 'VIP patient: route manually'")
# 20 patients x 10 messages each: 8 known non-VIP, 10 unknown, 2 VIP.
if [ "$sent" = 80 ] && [ "$killed" = 100 ] && [ "$vip" = 20 ]; then echo PASS; else echo "FAIL: sent=$sent killed=$killed vip=$vip"; exit 1; fi
