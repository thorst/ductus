#!/usr/bin/env bash
# v0.7 demo (0.7.6.2): the VS Code extension (editors/vscode) in a real VS Code. Its checks (types,
# lint, format, unit tests), its package (one bundled file; its size), then VS Code itself
# (@vscode/test-electron, downloaded once into editors/vscode/.vscode-test) on a copy of the guides'
# project with the extension and this build of ductus: the tests found at their lines, an
# interface's tests run, a failing test shown at its expect with the expected and actual values,
# a mistyped port in Problems on save and gone when fixed, a breakpoint hit from the Test
# Explorer's Debug, "Ductus: debug these tests" with no launch.json, (0.7.6.3.1) Deploy to dev
# against a real `ductus dev`, by the command and on save, and (0.7.6.3.2) Promote… across two
# engines in release mode started here (test, and prod requiring test first, each with its own
# database): a release file refused by prod's rules, deployed to test, then promoted from test to
# prod, each token asked for once and checked first (a viewer's turned away). Measures the
# extension's activation, a save to Problems, a deploy to dev, the promotions and the package's size.
# Needs: node 24, xvfb-run (a display for VS Code), VS Code's libraries (libgtk-3, libnss3,
# libasound2, libgbm1; CI's Ubuntu has them), the development database's server (scripts/dev/db.sh).
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/dev/db.sh"
cd "$(dirname "${BASH_SOURCE[0]}")/.."
BIN=$(realpath "${BIN:-${CARGO_TARGET_DIR:-target}/release/ductus}")
[ -x "$HOME/.local/node/bin/node" ] && export PATH="$HOME/.local/node/bin:$PATH"
cd editors/vscode
LOG=$(mktemp)
W=$(mktemp -d)
ADMIN=$DUCTUS_DATABASE_URL
ENVS=(test prod)
declare -A URL API PID
API[test]=17883 API[prod]=17893
for e in "${ENVS[@]}"; do URL[$e]=$(sed -E "s#/[^/?]+([?]|$)#/ductus_vscode_$e\1#" <<<"$ADMIN"); done
cleanup() {
  for e in "${ENVS[@]}"; do [ -n "${PID[$e]:-}" ] && kill -INT "${PID[$e]}" 2>/dev/null || true; done
  wait 2>/dev/null || true
  for e in "${ENVS[@]}"; do PGTZ=UTC psql "$ADMIN" -qc "DROP DATABASE IF EXISTS ductus_vscode_$e WITH (FORCE)" 2>/dev/null || true; done
  rm -rf "$W" "$LOG"
}
trap cleanup EXIT

echo "== the extension's checks (types, lint, format, unit tests)"
[ -d node_modules ] || npm ci --no-audit --no-fund >/dev/null
npm run -s check >"$LOG" 2>&1 || { cat "$LOG"; echo "FAIL: the extension's checks"; exit 1; }
grep -E '^ℹ (pass|fail) ' "$LOG" || true

echo "== its package"
npm run -s package >/dev/null
size=$(stat -c %s dist/ductus.vsix)
unzip -l dist/ductus.vsix | sed -n '4,$p'
echo "measure: package $((size / 1024)) KB"

echo "== two engines in release mode for Promote…: test, and prod (requires test first)"
for e in "${ENVS[@]}"; do
  PGTZ=UTC psql "$ADMIN" -qc "DROP DATABASE IF EXISTS ductus_vscode_$e WITH (FORCE)" -c "CREATE DATABASE ductus_vscode_$e"
done
start() { # start <env> [DUCTUS_* settings...]
  local e=$1
  shift
  env -u DUCTUS_CONTROL -u DUCTUS_PROJECT DUCTUS_DATABASE_URL="${URL[$e]}" "$@" \
    "$BIN" run --releases "$W/releases-$e" --env "$e" --threads 1 --api-listen "127.0.0.1:${API[$e]}" >"$W/$e.log" 2>&1 &
  PID[$e]=$!
  until grep -q "started interfaces=" "$W/$e.log" 2>/dev/null; do kill -0 "${PID[$e]}" || { cat "$W/$e.log"; exit 1; }; sleep 0.2; done
}
start test
start prod DUCTUS_DEPLOY_REQUIRES=test
token() { DUCTUS_DATABASE_URL="${URL[$1]}" "$BIN" token create "$2" --role "$3" "${@:4}" 2>/dev/null; }
DUCTUS_E2E_ENGINES=$(printf '{"test":{"url":"http://127.0.0.1:%s","token":"%s"},"prod":{"url":"http://127.0.0.1:%s","token":"%s","viewer":"%s"}}' \
  "${API[test]}" "$(token test vscode-ci operator --permission deploy)" \
  "${API[prod]}" "$(token prod vscode-ci operator --permission deploy)" "$(token prod vscode-viewer viewer)")
export DUCTUS_E2E_ENGINES

echo "== in VS Code, on a copy of the guides' project"
npm run -s build
# VS Code's own lines (GPU, D-Bus, its built-in chat looking for a sign-in) are noise here.
DUCTUS_BIN=$BIN timeout 600 xvfb-run -a node dist/test/e2e/run.js 2>&1 |
  sed -u 's/\x1b\[[0-9;]*m//g' | # Mocha colours its marks where FORCE_COLOR is set (CI)
  grep -vE '^\[[0-9:/.]+|Gtk-|dbus|libva|DevTools|AgentHost|ChatModel|ERROR:|trace-deprecation|DEP0169|^$' |
  tee "$LOG" || true

fail=""
[ "$size" -lt 100000 ] || fail+=" package-over-100KB"
for t in "finds ductus and activates" "the tests are found by interface" "an interface's tests run" \
  "a failure is shown at its expect" "validate's findings are in Problems" \
  "Debug from the Test Explorer stops at a breakpoint" "debugging works without launch.json" \
  "Deploy to dev: a running ductus dev takes a change in place" \
  "Promote…: a release file to test, then from test to prod"; do
  grep -q "✔ $t" "$LOG" || fail+=" \"$t\""
done
grep -q ' 9 passing' "$LOG" || fail+=" not-all-passing"
if [ -z "$fail" ]; then echo PASS; else echo "FAIL:$fail"; exit 1; fi
