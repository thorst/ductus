#!/usr/bin/env bash
# VS Code's JavaScript debugger (js-debug) as a standalone DAP server, for the debugging demo:
# what VS Code itself runs for a `"type": "node"` launch configuration. Downloaded once into
# ~/.cache/ductus (or JS_DEBUG_CACHE), the version and checksum pinned; prints the server's path.
#   scripts/dev/js-debug.sh
set -euo pipefail
VERSION=1.140.0
SHA256=27dab92937ec1ab35821ae955aac867544fe06a1b6307229049f2d789af10968
DIR=${JS_DEBUG_CACHE:-$HOME/.cache/ductus}/js-debug-$VERSION
SERVER=$DIR/js-debug/src/dapDebugServer.js
if [ ! -f "$SERVER" ]; then
  mkdir -p "$DIR"
  archive=$DIR/js-debug-dap-v$VERSION.tar.gz
  curl -fsSL -o "$archive" "https://github.com/microsoft/vscode-js-debug/releases/download/v$VERSION/js-debug-dap-v$VERSION.tar.gz"
  echo "$SHA256  $archive" | sha256sum --check --quiet
  tar xzf "$archive" -C "$DIR"
fi
echo "$SERVER"
