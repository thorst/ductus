#!/usr/bin/env bash
# v0.4 release demo: one project, run with --env dev and then --env prod.
#   patient-api  GET /patients/{mrn}: patient from the database, its latest lab results, and its
#                enterprise id from the EMPI through fetch() -> JSON (404 when unknown)
#   empi         stands in for the vendor EMPI API that patient-api calls (bearer token from secrets)
#   lab-results  the lab's nightly CSV, picked up from its SFTP server, split into rows, each row
#                upserted into the database. isolation: dedicated with its own connection pool, so
#                the nightly batch can't take the API's connections.
# Each environment has its own database, ports, SFTP folder, facility name and encrypted secrets.
# The interfaces start from `ductus new interface`; their tests run against mocks (no database).
set -euo pipefail

BIN=$(realpath "${BIN:-${CARGO_TARGET_DIR:-target}/release/ductus}")
DB=${DUCTUS_DATABASE_URL:-postgres://todd:ductus@localhost/ductus_dev}
PGUSER_=${PGUSER_:-todd}
PGPASS_=${PGPASS_:-ductus}
SSHD=${SSHD:-/usr/sbin/sshd}
SFTP_PORT=${SFTP_PORT:-2222}
P=$(mktemp -d)
R="$P/lab-server"
export DUCTUS_DATABASE_URL=$DB
export PGOPTIONS="--client-min-messages=warning" # no "does not exist, skipping" notices
ENGINE=; SSHD_PID=
cleanup() {
  for pid in $ENGINE $SSHD_PID; do kill "$pid" 2>/dev/null || true; done
}
trap cleanup EXIT

FAILED=0
check() { # name, expected, actual
  if [ "$2" = "$3" ]; then echo "  ✓ $1"; else echo "  ✗ $1: expected [$2], got [$3]"; FAILED=1; fi
}
wait_for() { # seconds, 'condition' (evaluated each time, so quote it)
  local deadline=$((SECONDS + $1))
  until eval "$2"; do [ $SECONDS -ge $deadline ] && return 1; sleep 0.1; done
}
field() { # json, python expression over d
  python3 -c "import json, sys; d = json.loads(sys.argv[1]); print($2)" "$1"
}
[ -x "$SSHD" ] || { echo "FAIL: $SSHD not found (install openssh-server)"; exit 1; }

# ---- two databases: dev uses the engine's own database, prod a separate one
DEV_DB=$(sed -E 's#.*/([^/?]+).*#\1#' <<<"$DB")
PROD_DB=ductus_v04_prod
PROD_URL=$(sed -E "s#/[^/?]+([?]|$)#/$PROD_DB\1#" <<<"$DB")
psql -q "$DB" -c "TRUNCATE messages, deliveries, dedupe_keys, source_state" -c "DROP DATABASE IF EXISTS $PROD_DB" -c "CREATE DATABASE $PROD_DB"
seed() { # url, name suffix
  psql -q "$1" <<SQL
DROP TABLE IF EXISTS v04_patients, v04_results;
CREATE TABLE v04_patients (mrn text PRIMARY KEY, name text NOT NULL);
CREATE TABLE v04_results (mrn text, test text, value numeric, resulted_at timestamptz, PRIMARY KEY (mrn, test));
INSERT INTO v04_patients VALUES ('100001', 'SMITH, JOHN$2'), ('100002', 'DOE, JANE$2');
SQL
}
seed "$DB" " (dev)"
seed "$PROD_URL" " (prod)"

# ---- the lab's SFTP server: a throwaway sshd as this user, with fresh keys
K="$P/ssh"
mkdir -p "$K" "$R/dev/outbound" "$R/prod/outbound"
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

# ---- the project
mkdir -p "$P/project/environments"
cd "$P/project"
env_file() { # name, facility, database, api port, empi port, poll
  cat >"environments/$1.yaml" <<EOF
variables:
  FACILITY: $2
  DB_NAME: $3
  DB_USER: $PGUSER_
  API_PORT: $4
  EMPI_PORT: $5
  EMPI_URL: http://127.0.0.1:$5
  LAB_DIR: $R/$1/outbound
  LAB_POLL: $6
  SFTP_PORT: $SFTP_PORT
  LAB_HOST_KEY: $FINGERPRINT
EOF
}
env_file dev YORK-TEST "$DEV_DB" 8481 8491 500ms
env_file prod YORK-MAIN "$PROD_DB" 8482 8492 1s
cat >resources.yaml <<EOF
databases:
  patient_db:
    postgres: { host: localhost, database: "\${DB_NAME}", user: "\${DB_USER}", password: "\${secret:DB_PASSWORD}", max_connections: 4 }
sftp:
  lab:
    host: 127.0.0.1
    port: \${SFTP_PORT}
    user: $(id -un)
    private_key: \${secret:LAB_SFTP_KEY}
    host_key: \${LAB_HOST_KEY}
EOF
export DUCTUS_SECRET_KEY=$("$BIN" secret init 2>/dev/null)
for e in dev prod; do
  echo "$PGPASS_" | "$BIN" secret set DB_PASSWORD --env $e >/dev/null
  "$BIN" secret set LAB_SFTP_KEY --env $e <"$K/client_key" >/dev/null
  echo "empi-token-$e" | "$BIN" secret set EMPI_TOKEN --env $e >/dev/null
done

echo "== scaffold with ductus new interface"
"$BIN" new interface patient-api --source http | head -1
"$BIN" new interface empi --source http | head -1
"$BIN" new interface lab-results --source sftp --format csv | head -1
check "generated interfaces pass their tests" "3 passed, 0 failed" "$("$BIN" test | tail -1)"

echo "== write the real interfaces"
cat >interfaces/empi/interface.yaml <<'EOF'
name: empi
# Stands in for the vendor EMPI that patient-api calls.
source: { http: { port: ${EMPI_PORT}, path: "/empi/{mrn}", method: GET, mode: respond } }
format: json
script: transform.ts
EOF
cat >interfaces/empi/transform.ts <<'EOF'
import { type HttpRequest, response, secret } from "@ductus/sdk";

export function transform(req: HttpRequest) {
  if (req.headers.authorization !== `Bearer ${secret("EMPI_TOKEN")}`) {
    return response({ status: 401, body: { error: "unauthorized" } });
  }
  return { mrn: req.params.mrn, enterpriseId: `E-${req.params.mrn}` };
}
EOF
cat >interfaces/empi/transform.test.ts <<'EOF'
import { expect, type HttpRequest, mock, test } from "@ductus/sdk";
import { transform } from "./transform.ts";

const get = (mrn: string, token: string): HttpRequest =>
  ({ method: "GET", path: `/empi/${mrn}`, params: { mrn }, query: {}, headers: { authorization: `Bearer ${token}` }, text: "", body: undefined });

test("answers with the enterprise id", () => {
  mock.secret({ EMPI_TOKEN: "t" });
  expect(transform(get("1", "t"))).toEqual({ mrn: "1", enterpriseId: "E-1" });
});

test("refuses a wrong token", () => {
  mock.secret({ EMPI_TOKEN: "t" });
  expect((transform(get("1", "nope")) as { status: number }).status).toBe(401);
});
EOF

cat >interfaces/patient-api/interface.yaml <<'EOF'
name: patient-api
source: { http: { port: ${API_PORT}, path: "/patients/{mrn}", method: GET, mode: respond } }
format: json
script: transform.ts
EOF
cat >interfaces/patient-api/transform.ts <<'EOF'
import { db, env, fetch, type HttpRequest, secret } from "@ductus/sdk";

interface Patient { mrn: string; name: string }
interface Result { test: string; value: number; resulted_at: string }

export async function transform(req: HttpRequest) {
  const mrn = req.params.mrn;
  const patient = await db("patient_db").one<Patient>("SELECT mrn, name FROM v04_patients WHERE mrn = $1", [mrn]);
  if (!patient) return null; // 404
  const results = await db("patient_db").query<Result>(
    "SELECT test, value, resulted_at FROM v04_results WHERE mrn = $1 ORDER BY test",
    [mrn],
  );
  const empi = await fetch(`${env("EMPI_URL")}/empi/${mrn}`, {
    headers: { authorization: `Bearer ${secret("EMPI_TOKEN")}` },
  });
  return { ...patient, enterpriseId: empi.json().enterpriseId, facility: env("FACILITY"), results };
}
EOF
cat >interfaces/patient-api/transform.test.ts <<'EOF'
import { expect, type HttpRequest, mock, test } from "@ductus/sdk";
import { transform } from "./transform.ts";

const get = (mrn: string): HttpRequest =>
  ({ method: "GET", path: `/patients/${mrn}`, params: { mrn }, query: {}, headers: {}, text: "", body: undefined });

test("patient, results and enterprise id", async () => {
  const glucose = { test: "GLU", value: 90, resulted_at: "2026-10-06T02:00:00Z" };
  mock.db("patient_db", (sql) => (sql.includes("v04_patients") ? [{ mrn: "1", name: "SMITH" }] : [glucose]));
  const calls = mock.fetch({ body: { enterpriseId: "E-1" } });
  mock.env({ EMPI_URL: "https://empi.test", FACILITY: "TEST" });
  mock.secret({ EMPI_TOKEN: "t" });
  expect(await transform(get("1"))).toEqual({ mrn: "1", name: "SMITH", enterpriseId: "E-1", facility: "TEST", results: [glucose] });
  expect(calls[0].url).toBe("https://empi.test/empi/1");
  expect(calls[0].headers.authorization).toBe("Bearer t");
});

test("unknown patient: 404", async () => {
  mock.db("patient_db", []);
  expect(await transform(get("2"))).toBeNull();
});
EOF

cat >interfaces/lab-results/interface.yaml <<'EOF'
name: lab-results
# The lab drops a CSV on its SFTP server every night; each row is stored in patient_db.
source: { sftp: { server: lab, dir: "${LAB_DIR}", pattern: "*.csv", poll: "${LAB_POLL}", split: records } }
format: csv
output: json
ordering: { key: mrn }
isolation: dedicated
workers: 2
dedicated_pools: { patient_db: 2 }
script: transform.ts
destination: { file: { dir: stored } }   # a JSON copy of each stored row, for audit
EOF
cat >interfaces/lab-results/transform.ts <<'EOF'
import { type CsvRecord, db } from "@ductus/sdk";

export async function transform(row: CsvRecord) {
  await db("patient_db").execute(
    `INSERT INTO v04_results (mrn, test, value, resulted_at) VALUES ($1, $2, $3::numeric, $4::timestamptz)
     ON CONFLICT (mrn, test) DO UPDATE SET value = excluded.value, resulted_at = excluded.resulted_at`,
    [row.mrn, row.test, row.value, row.resulted_at],
  );
  return row;
}
EOF
cat >interfaces/lab-results/transform.test.ts <<'EOF'
import { type CsvRecord, expect, mock, test } from "@ductus/sdk";
import { transform } from "./transform.ts";

test("stores the row", async () => {
  const calls = mock.db("patient_db", () => 1);
  const row: CsvRecord = { mrn: "1", test: "GLU", value: "90", resulted_at: "2026-10-06T02:00:00Z" };
  expect(await transform(row)).toEqual(row);
  expect(calls[0].params).toEqual(["1", "GLU", "90", "2026-10-06T02:00:00Z"]);
});
EOF
check "tests pass against mocks (no database, SFTP or EMPI)" "5 passed, 0 failed" "$("$BIN" test | tail -1)"

run_env() { # env, api port, database url, name suffix, glucose value
  local env=$1 port=$2 url=$3 suffix=$4 glucose=$5
  echo "== --env $env"
  check "validate" "valid" "$("$BIN" validate --env "$env" | tail -1)"
  "$BIN" ports --env "$env" | sed -n '/^Listening/,/^$/p' | sed '/^$/d'
  LOG=$P/run-$env.log
  "$BIN" run --env "$env" >"$LOG" 2>&1 &
  ENGINE=$!
  wait_for 20 'grep -q "started interfaces=" "$LOG"' || { cat "$LOG"; exit 1; }

  # The lab's nightly file arrives (written under a temporary name, then renamed).
  printf 'mrn,test,value,resulted_at\n100001,GLU,%s,2026-10-06T02:00:00Z\n100001,NA,140,2026-10-06T02:00:00Z\n100002,GLU,88,2026-10-06T02:05:00Z\n' \
    "$glucose" >"$R/$env/outbound/.results.tmp"
  mv "$R/$env/outbound/.results.tmp" "$R/$env/outbound/results.csv"
  wait_for 20 '[ "$(psql -tA "$url" -c "SELECT count(*) FROM v04_results")" = 3 ]' || true
  check "3 lab rows stored in the $env database" "3" "$(psql -tA "$url" -c "SELECT count(*) FROM v04_results")"
  wait_for 5 '[ -f "$R/$env/outbound/archive/results.csv" ]' || true
  check "file archived on the SFTP server" "yes" "$([ -f "$R/$env/outbound/archive/results.csv" ] && echo yes || echo no)"

  local body
  body=$(curl -s "http://127.0.0.1:$port/patients/100001")
  check "GET /patients/100001: name from the $env database" "SMITH, JOHN$suffix" "$(field "$body" 'd["name"]')"
  check "enterprise id from the EMPI (fetch with a bearer token)" "E-100001" "$(field "$body" 'd["enterpriseId"]')"
  check "facility from environments/$env.yaml" "$(sed -n 's/^  FACILITY: //p' "environments/$env.yaml")" "$(field "$body" 'd["facility"]')"
  check "latest results from the nightly file" "GLU=$glucose NA=140" \
    "$(field "$body" '" ".join(f"{r["test"]}={r["value"]}" for r in d["results"])')"
  check "unknown patient -> 404" "404" "$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$port/patients/100009")"

  # Timed by curl (request sent to response read), so curl's own start-up isn't counted.
  local n=200 times
  times=$(for _ in $(seq 1 $n); do curl -s -o /dev/null -w '%{time_total}\n' "http://127.0.0.1:$port/patients/100001"; done)
  echo "  $n requests one at a time (2 queries + 1 EMPI call each): $(python3 -c "
import statistics, sys
t = sorted(float(x) * 1000 for x in sys.argv[1].split())
print(f'median {statistics.median(t):.1f} ms, p95 {t[int(len(t) * 0.95) - 1]:.1f} ms')" "$times")"

  kill -INT $ENGINE; wait $ENGINE 2>/dev/null || true; ENGINE=
}
run_env dev 8481 "$DB" " (dev)" 91
run_env prod 8482 "$PROD_URL" " (prod)" 104

echo "== environments kept apart"
check "dev database has dev's glucose" "91" "$(psql -tA "$DB" -c "SELECT value FROM v04_results WHERE mrn = '100001' AND test = 'GLU'")"
check "prod database has prod's glucose" "104" "$(psql -tA "$PROD_URL" -c "SELECT value FROM v04_results WHERE mrn = '100001' AND test = 'GLU'")"
check "secrets are per environment" "DB_PASSWORD EMPI_TOKEN LAB_SFTP_KEY" "$("$BIN" secret list --env prod | sort | tr '\n' ' ' | sed 's/ $//')"

[ "$FAILED" = 0 ] && echo PASS || { echo FAIL; echo "logs: $P/run-dev.log $P/run-prod.log"; exit 1; }
