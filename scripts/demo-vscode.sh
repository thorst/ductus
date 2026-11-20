#!/usr/bin/env bash
# v0.7 demo (0.7.6.2): the VS Code extension (editors/vscode) in a real VS Code. Its checks (types,
# lint, format, unit tests), its package (one bundled file; its size), then VS Code itself
# (@vscode/test-electron, downloaded once into editors/vscode/.vscode-test) on a copy of the guides'
# project with the extension and this build of ductus: the tests found at their lines, an
# interface's tests run, a failing test shown at its expect with the expected and actual values,
# a mistyped port in Problems on save and gone when fixed, a breakpoint hit from the Test
# Explorer's Debug, and "Ductus: debug these tests" with no launch.json. Measures the extension's
# activation, a save to Problems and the package's size.
# Needs: node 24, xvfb-run (a display for VS Code), VS Code's libraries (libgtk-3, libnss3,
# libasound2, libgbm1; CI's Ubuntu has them).
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
BIN=$(realpath "${BIN:-${CARGO_TARGET_DIR:-target}/release/ductus}")
[ -x "$HOME/.local/node/bin/node" ] && export PATH="$HOME/.local/node/bin:$PATH"
cd editors/vscode
LOG=$(mktemp)
trap 'rm -f "$LOG"' EXIT

echo "== the extension's checks (types, lint, format, unit tests)"
[ -d node_modules ] || npm ci --no-audit --no-fund >/dev/null
npm run -s check >"$LOG" 2>&1 || { cat "$LOG"; echo "FAIL: the extension's checks"; exit 1; }
grep -E '^ℹ (pass|fail) ' "$LOG" || true

echo "== its package"
npm run -s package >/dev/null
size=$(stat -c %s dist/ductus.vsix)
unzip -l dist/ductus.vsix | sed -n '4,$p'
echo "measure: package $((size / 1024)) KB"

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
  "Debug from the Test Explorer stops at a breakpoint" "debugging works without launch.json"; do
  grep -q "✔ $t" "$LOG" || fail+=" \"$t\""
done
grep -q ' 7 passing' "$LOG" || fail+=" not-all-passing"
if [ -z "$fail" ]; then echo PASS; else echo "FAIL:$fail"; exit 1; fi
