#!/usr/bin/env bash
# v0.4 demo: HTTP in every direction.
#   GET  /patients/{mrn}  respond mode: database lookup -> JSON (404 when unknown)
#   GET  /                respond mode: an HTML page
#   POST /orders          queue mode: JSON orders, ordered by patient, converted to CSV and
#                         POSTed to /inbox (an HTTP destination)
#   POST /inbox           queue mode: CSV written to files
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/dev/db.sh"

BIN=$(realpath "${BIN:-${CARGO_TARGET_DIR:-target}/release/ductus}")
DB=$DUCTUS_DATABASE_URL
PGUSER_=${PGUSER_:-todd}
PGPASS_=${PGPASS_:-ductus}
P=$(mktemp -d)
export DUCTUS_DATABASE_URL=$DB
BASE=http://127.0.0.1:8090

psql -q "$DB" <<'SQL'
DROP TABLE IF EXISTS demo_patients;
CREATE TABLE demo_patients (mrn text PRIMARY KEY, display_name text, vip boolean DEFAULT false);
INSERT INTO demo_patients VALUES ('100001', 'Pat One', false), ('100002', 'Pat Two', false);
TRUNCATE messages, deliveries, dedupe_keys;
SQL

mkdir -p "$P/environments" "$P/interfaces"/{patient-api,home,orders,inbox}
cat >"$P/environments/dev.yaml" <<EOF
variables:
  API_PORT: 8090
  DB_NAME: $(sed -E 's#.*/([^/?]+).*#\1#' <<<"$DB")
  DB_USER: $PGUSER_
EOF
cat >"$P/resources.yaml" <<'EOF'
databases:
  patient_db:
    postgres: { host: localhost, database: "${DB_NAME}", user: "${DB_USER}", password: "${secret:DB_PASSWORD}" }
EOF

cat >"$P/interfaces/patient-api/interface.yaml" <<'EOF'
name: patient-api
source: { http: { port: ${API_PORT}, path: "/patients/{mrn}", method: GET, mode: respond } }
format: json
script: transform.ts
EOF
cat >"$P/interfaces/patient-api/transform.ts" <<'EOF'
import { db, type HttpRequest } from "@ductus/sdk";
export async function transform(req: HttpRequest) {
  return await db("patient_db").one("SELECT mrn, display_name AS name FROM demo_patients WHERE mrn = $1", [req.params.mrn]);
}
EOF

cat >"$P/interfaces/home/interface.yaml" <<'EOF'
name: home
source: { http: { port: ${API_PORT}, path: /, method: GET, mode: respond } }
format: text
script: transform.ts
EOF
cat >"$P/interfaces/home/transform.ts" <<'EOF'
import { html, type HttpRequest } from "@ductus/sdk";
export function transform(req: HttpRequest) {
  const who = req.query.name ?? "world";
  return html(`<!doctype html><h1>Hello, ${who}</h1><p>Served by Ductus</p>`);
}
EOF

cat >"$P/interfaces/orders/interface.yaml" <<'EOF'
name: orders
source: { http: { port: ${API_PORT}, path: /orders } }
format: json
output: csv
ordering: { key: patient.mrn }
dedupe: { keys: [orderId] }
script: transform.ts
destination: { http: { url: "http://127.0.0.1:${API_PORT}/inbox" } }
EOF
cat >"$P/interfaces/orders/transform.ts" <<'EOF'
interface Order { orderId: string; patient: { mrn: string }; tests: string[] }
export function transform(order: Order) {
  return order.tests.map((code) => ({ order: order.orderId, mrn: order.patient.mrn, test: code }));
}
EOF

cat >"$P/interfaces/inbox/interface.yaml" <<'EOF'
name: inbox
source: { http: { port: ${API_PORT}, path: /inbox } }
format: csv
destination: { file: { dir: received } }
EOF

cd "$P"
export DUCTUS_SECRET_KEY=$("$BIN" secret init 2>/dev/null)
echo "$PGPASS_" | "$BIN" secret set DB_PASSWORD >/dev/null
echo "== validate"; "$BIN" validate || { echo "FAIL: validation"; exit 1; }

"$BIN" run >"$P/run.log" 2>&1 &
ENGINE=$!
until grep -q "started" "$P/run.log"; do kill -0 $ENGINE || { cat "$P/run.log"; exit 1; }; sleep 0.2; done

check() { # name, expected, actual
  if [ "$2" = "$3" ]; then echo "  ✓ $1"; else echo "  ✗ $1: expected [$2], got [$3]"; FAILED=1; fi
}
FAILED=0
echo "== respond mode"
check "GET /patients/100001" '{"mrn":"100001","name":"Pat One"}' "$(curl -s $BASE/patients/100001)"
check "content type" "application/json" "$(curl -s -o /dev/null -w '%{content_type}' $BASE/patients/100001)"
check "unknown patient -> 404" "404" "$(curl -s -o /dev/null -w '%{http_code}' $BASE/patients/999)"
check "HTML page" "<!doctype html><h1>Hello, Todd</h1><p>Served by Ductus</p>" "$(curl -s "$BASE/?name=Todd")"
check "unknown route -> 404" "404" "$(curl -s -o /dev/null -w '%{http_code}' $BASE/nope)"

echo "== queue mode: JSON orders -> CSV -> HTTP destination -> files"
for i in 1 2 3; do
  code=$(curl -s -o /dev/null -w '%{http_code}' -X POST $BASE/orders -H 'content-type: application/json' \
    -d "{\"orderId\":\"ORD$i\",\"patient\":{\"mrn\":\"10000$i\"},\"tests\":[\"GLU\",\"NA\"]}")
  check "POST /orders ORD$i -> 202" "202" "$code"
done
check "resend ORD1 -> duplicate" '{"duplicate":true}' "$(curl -s -X POST $BASE/orders -d '{"orderId":"ORD1","patient":{"mrn":"100001"},"tests":["GLU"]}')"

OUT=interfaces/inbox/received   # file destinations are relative to their interface folder
for _ in $(seq 1 40); do [ "$(ls $OUT 2>/dev/null | wc -l)" -ge 3 ] && break; sleep 0.25; done
check "3 CSV files delivered" "3" "$(ls $OUT | wc -l)"
check "CSV content (ORD2)" "order,mrn,test ORD2,100002,GLU ORD2,100002,NA" "$(cat "$(grep -l ORD2 $OUT/*.csv)" | tr '\n' ' ' | sed 's/ $//')"

kill -INT $ENGINE; wait 2>/dev/null || true
echo "== engine status"; "$BIN" status
[ "$FAILED" = 0 ] && echo PASS || { echo FAIL; exit 1; }
