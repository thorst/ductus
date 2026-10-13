#!/usr/bin/env bash
# v0.5 demo: the web UI, embedded in the engine, driven by a real browser (Playwright) against
# a real engine and database. Three interfaces give it something to show:
#   adt     two destinations: `lab` (nobody listens: stalled, last error shown) and `archive`
#           (files, all sent)
#   orders  its code quarantines one patient's messages ("unknown patient 100003")
#   billing its only destination is paused, with a reason; configured to keep 1w while 7w is
#           applied, so the shorter window waits for a confirmation
# The browser signs in with a token (wrong one first), finds what needs attention, filters the
# interfaces, reads a destination's state, follows a behind destination to its hotspots, opens a
# quarantined message and its HL7 fields, lists the quarantine, looks up a vendor's impact,
# previews moving an owner's interfaces (a patch), reads retention, drain and the API reference
# (loaded only when opened), checks accessibility (axe) in every theme, and signs out. Then, as
# an operator, it corrects a quarantined message (an edited copy), resends from the quarantine
# screen, pauses and resumes a destination, confirms the shorter retention, runs the connection
# checks, and drains and resumes the engine; as an admin, it creates a token (its secret works),
# then revokes it (it doesn't). Screenshots: ui/test-results/.
#
# Needs Node and the UI's packages (cd ui && npm ci), and Playwright's Chromium
# (npx playwright install --with-deps chromium).
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/dev/db.sh"

BIN=$(realpath "${BIN:-${CARGO_TARGET_DIR:-target}/release/ductus}")
UI=$(realpath "$(dirname "$0")/../ui")
DB=$DUCTUS_DATABASE_URL
P=$(mktemp -d)
export DUCTUS_DATABASE_URL=$DB
export DUCTUS_API_URL=http://127.0.0.1:7794
psql -q "$DB" -c "TRUNCATE messages, deliveries, dedupe_keys"

code=$(curl -s -o /dev/null -w '%{http_code}' "$DUCTUS_API_URL/api/v1/health" || true)
[ "$code" = 000 ] || { echo "FAIL: something already listens on $DUCTUS_API_URL"; exit 1; }

# The company's own documentation fields (0.5.7.5): orders is missing a required one on purpose.
cat >"$P/docs-schema.yaml" <<'EOF'
fields:
  data_classification: { type: enum, values: [PHI, Internal, Public], required: true, label: Data classification }
  baa_signed: { type: enum, values: ["yes", "no", n/a], required: true, applies_to: [interface, resource], label: BAA signed }
  go_live: { type: date, label: Go-live }
  downstream: { type: list, label: Downstream systems }
  runbook: { type: url, applies_to: [interface, destination], label: Runbook }
EOF
cat >"$P/resources.yaml" <<'EOF'
apis:
  empi: { url: "http://127.0.0.1:7794/api/v1", application: EMPI, custom: { baa_signed: "yes" } }
EOF

mkdir -p "$P/interfaces/adt" "$P/interfaces/orders" "$P/interfaces/billing"
printf 'api: { listen: 127.0.0.1:7794 }\n' >"$P/ductus.yaml"
cat >"$P/interfaces/adt/interface.yaml" <<'EOF'
name: adt
source: { mllp: { port: 6721 } }
ordering: { key: PID-3.1 }
monitor: { behind: 10 }
about:
  description: ADT from Epic to the lab and the archive
  owner: ADT team
  from: { application: Epic, vendor: Epic Systems }
  to: { application: Sunquest Lab, vendor: Sunquest }
custom:
  data_classification: PHI
  baa_signed: "yes"
  go_live: 2026-03-02
  downstream: [Sunquest Lab, Archive]
  runbook: https://wiki.example.org/adt
destinations:
  lab: { mllp: { host: 127.0.0.1, port: 6799, timeout_ms: 300 }, custom: { runbook: "https://wiki.example.org/adt-lab" } }
  archive: { file: { dir: out/archive } }
EOF
cat >"$P/interfaces/orders/interface.yaml" <<'EOF'
name: orders
source: { mllp: { port: 6722 } }
script: transform.ts
about: { description: Orders checked against the order system, owner: Orders team }
custom: { data_classification: Internal }
destination: { file: { dir: out/orders } }
EOF
cat >"$P/interfaces/orders/transform.ts" <<'EOF'
import { quarantine, type Message } from "@ductus/sdk";
export function transform(msg: Message) {
  if (msg.get("PID-3.1") === "100003") throw quarantine("unknown patient 100003 in the order system");
  return msg;
}
EOF
cat >"$P/interfaces/billing/interface.yaml" <<'EOF'
name: billing
source: { mllp: { port: 6723 } }
retention: 1w
about: { owner: Finance }
custom: { data_classification: PHI, baa_signed: "no" }
destination: { file: { dir: out/billing } }
EOF
# As if billing had kept 7 weeks until now: its shorter 1w waits for someone to confirm it.
psql -q "$DB" -c "DELETE FROM retention WHERE interface IN ('adt', 'orders', 'billing')" \
  -c "INSERT INTO retention (interface, days) VALUES ('billing', 49)"

cd "$P"
# Validation fails on purpose, for one reason only (orders lacks a required documentation field);
# the engine runs anyway.
validation=$("$BIN" validate || true)
[ "$(grep '^✗' <<<"$validation")" = "✗ orders: custom.baa_signed is required (one of: yes, no, n/a)" ] || {
  echo "$validation"
  echo "FAIL: validation"
  exit 1
}
"$BIN" run >"$P/run.log" 2>&1 &
ENGINE=$!
trap 'kill -INT $ENGINE 2>/dev/null || true' EXIT
until grep "started interfaces=" "$P/run.log" >/dev/null; do kill -0 $ENGINE || { cat "$P/run.log"; exit 1; }; sleep 0.2; done
echo "engine log: $P/run.log"
export DUCTUS_API_TOKEN=$("$BIN" token create "ui-demo-operator-$$" --role operator --permission payloads 2>/dev/null)
VIEWER=$("$BIN" token create "ui-demo-viewer-$$" --role viewer --permission payloads 2>/dev/null)
ADMIN=$("$BIN" token create "ui-demo-admin-$$" --role admin 2>/dev/null)
# People with passwords (named test-user-*: the store tests clean such admins up). The operator may
# read payloads; the viewer may not.
PASSWORD="correct horse staple"
"$BIN" user create "test-user-ui-admin-$$" --role admin --password-stdin <<<"$PASSWORD" >/dev/null
"$BIN" user create "test-user-ui-operator-$$" --role operator --permission payloads --password-stdin <<<"$PASSWORD" >/dev/null
"$BIN" user create "test-user-ui-viewer-$$" --role viewer --password-stdin <<<"$PASSWORD" >/dev/null

echo "== traffic"
"$BIN" pause billing --reason "invoice system upgrade tonight" | sed 's/^/   /'
"$BIN" gen --to 127.0.0.1:6721 --count 30 --patients 5 | tail -1 | sed 's/^/   adt: /'
"$BIN" gen --to 127.0.0.1:6722 --count 20 --patients 5 | tail -1 | sed 's/^/   orders: /'
"$BIN" gen --to 127.0.0.1:6723 --count 10 --patients 5 | tail -1 | sed 's/^/   billing: /'
# The engine calls a destination stalled after a minute without progress; until then, the lab
# shows as behind with its last error. Wait for the error and for orders' quarantines.
for _ in $(seq 1 100); do
  status=$(curl -s -H "Authorization: Bearer $VIEWER" "$DUCTUS_API_URL/api/v1/interfaces")
  grep '"last_error":{' <<<"$status" >/dev/null && grep '"quarantined":4' <<<"$status" >/dev/null && break
  sleep 0.2
done

echo "== the UI is in the binary"
head=$(curl -s -D - -o /dev/null -H 'Accept-Encoding: gzip' "$DUCTUS_API_URL/interfaces/adt")
grep -i '^content-type: text/html' <<<"$head" >/dev/null || { echo "FAIL: no UI page: $head"; exit 1; }
grep -i '^content-encoding: gzip' <<<"$head" >/dev/null || { echo "FAIL: not compressed: $head"; exit 1; }
grep -i "^content-security-policy: default-src 'self'" <<<"$head" >/dev/null || { echo "FAIL: no CSP: $head"; exit 1; }
echo "   /interfaces/adt → index.html, gzip, CSP"

echo "== the browser (Playwright)"
cd "$UI"
export DUCTUS_URL=$DUCTUS_API_URL DUCTUS_VIEWER_TOKEN=$VIEWER DUCTUS_OPERATOR_TOKEN=$DUCTUS_API_TOKEN \
  DUCTUS_ADMIN_TOKEN=$ADMIN DUCTUS_PERSON_ADMIN="test-user-ui-admin-$$:$PASSWORD" \
  DUCTUS_PERSON_OPERATOR="test-user-ui-operator-$$:$PASSWORD" DUCTUS_PERSON_VIEWER="test-user-ui-viewer-$$:$PASSWORD"
if npx playwright test --reporter=line; then
  echo PASS
else
  echo "FAIL: browser tests (report: $UI/playwright-report, screenshots: $UI/test-results)"
  tail -20 "$P/run.log"
  exit 1
fi
