#!/usr/bin/env bash
# Run every end-to-end demo (each prints PASS/FAIL). Same as the CI demo steps.
# Needs: a built release binary (scripts/dev/check.sh), Postgres, psql.
set -euo pipefail
cd "$(dirname "$0")/../.."
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$HOME/target}
export BIN=$CARGO_TARGET_DIR/release/ductus
[ -x "$HOME/.local/node/bin/node" ] && export PATH="$HOME/.local/node/bin:$PATH"
pkill -f 'ductus (run|sink|gen)' 2>/dev/null || true

echo "##### crash recovery";  COUNT=3000 bash scripts/demo-crash-recovery.sh | tail -3
echo "##### clean stop";      bash scripts/demo-stop.sh 2>&1 | grep -v Killed | tail -1
echo "##### database lookup"; bash scripts/demo-lookup.sh | tail -1
echo "##### http";            bash scripts/demo-http.sh | tail -1
echo "##### files";           bash scripts/demo-files.sh | tail -1
echo "##### v0.4 release";    bash scripts/demo-v04.sh | tail -1
echo "##### runaway scripts"; bash scripts/demo-runaway.sh | tail -1
echo "##### destinations";    bash scripts/demo-destinations.sh | tail -1
echo "##### routing";         bash scripts/demo-routing.sh | tail -1
echo "##### retention";       bash scripts/demo-retention.sh | tail -1
echo "##### held area";       bash scripts/demo-held.sh | tail -1
echo "##### hotspots";        bash scripts/demo-hotspots.sh | tail -1
echo "##### web UI";          bash scripts/demo-ui.sh | tail -1
echo "##### scale";           SHARED=300 EPIC_SECONDS=10 SAMPLED=50 bash scripts/demo-scale.sh | tail -1
