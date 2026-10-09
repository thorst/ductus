#!/usr/bin/env bash
# v0.5 demo: the web UI, embedded in the engine, driven by a real browser (Playwright) against
# a real engine and database. Three interfaces give it something to show:
#   adt     two destinations: `lab` (nobody listens: stalled, last error shown) and `archive`
#           (files, all sent)
#   orders  its code quarantines one patient's messages ("unknown patient 100003")
#   billing its only destination is paused, with a reason
# The browser signs in with a token (wrong one first), finds what needs attention, filters the
# interfaces, reads a destination's state, opens a quarantined message and its HL7 fields, checks
# accessibility (axe) in every theme, and signs out. Screenshots: ui/test-results/.
#
# Needs Node and the UI's packages (cd ui && npm ci), and Playwright's Chromium
# (npx playwright install --with-deps chromium).
set -euo pipefail

BIN=$(realpath "${BIN:-${CARGO_TARGET_DIR:-target}/release/ductus}")
UI=$(realpath "$(dirname "$0")/../ui")
DB=${DUCTUS_DATABASE_URL:-postgres://todd:ductus@localhost/ductus_dev}
P=$(mktemp -d)
export DUCTUS_DATABASE_URL=$DB
export DUCTUS_API_URL=http://127.0.0.1:7794
psql -q "$DB" -c "TRUNCATE messages, deliveries, dedupe_keys"

code=$(curl -s -o /dev/null -w '%{http_code}' "$DUCTUS_API_URL/api/v1/health" || true)
[ "$code" = 000 ] || { echo "FAIL: something already listens on $DUCTUS_API_URL"; exit 1; }

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
destinations:
  lab: { mllp: { host: 127.0.0.1, port: 6799, timeout_ms: 300 } }
  archive: { file: { dir: out/archive } }
EOF
cat >"$P/interfaces/orders/interface.yaml" <<'EOF'
name: orders
source: { mllp: { port: 6722 } }
script: transform.ts
about: { description: Orders checked against the order system, owner: Orders team }
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
about: { owner: Finance }
destination: { file: { dir: out/billing } }
EOF

cd "$P"
"$BIN" validate >/dev/null || { "$BIN" validate; echo "FAIL: validation"; exit 1; }
"$BIN" run >"$P/run.log" 2>&1 &
ENGINE=$!
trap 'kill -INT $ENGINE 2>/dev/null || true' EXIT
until grep "started interfaces=" "$P/run.log" >/dev/null; do kill -0 $ENGINE || { cat "$P/run.log"; exit 1; }; sleep 0.2; done
echo "engine log: $P/run.log"
export DUCTUS_API_TOKEN=$("$BIN" token create "ui-demo-operator-$$" --role operator 2>/dev/null)
VIEWER=$("$BIN" token create "ui-demo-viewer-$$" --role viewer 2>/dev/null)

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
export DUCTUS_URL=$DUCTUS_API_URL DUCTUS_VIEWER_TOKEN=$VIEWER
if npx playwright test --reporter=line; then
  echo PASS
else
  echo "FAIL: browser tests (report: $UI/playwright-report, screenshots: $UI/test-results)"
  tail -20 "$P/run.log"
  exit 1
fi
