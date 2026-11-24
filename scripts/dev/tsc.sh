#!/usr/bin/env bash
# TypeScript 7's compiler: `tsc`, a native program (no Node needed), with the lib.*.d.ts files it
# reads beside it (it refuses to run without them). It is what `ductus validate` type-checks with,
# and every package ships it in a `typescript` folder beside the binary. Pinned by version and
# SHA-256 (npm's own package for the platform, @typescript/typescript-<platform>); bump both on
# purpose, with crates/ductus-script/src/tsc.rs's TSC_VERSION (a test keeps them the same).
#
#   bash scripts/dev/tsc.sh                  this machine's, into the per-user cache where ductus
#                                            looks (builds from source, tests, CI); prints the folder
#   bash scripts/dev/tsc.sh PLATFORM DIR     a platform's (linux-x64, win32-x64) into DIR, for a package
#
# Downloaded once: a folder already holding tsc is left as it is.
set -euo pipefail
VERSION=7.0.2
sha256() {
  case $1 in
  linux-x64) echo 7ecad6f67377e831856367ab062ef394f21506a611405bf8ac0ff039348637d3 ;;
  win32-x64) echo 61fc4e141d2bc687db580e71bbfa63b9c209f0310645d82ca1b457eb3a24fd19 ;;
  *) echo "tsc.sh: no checksum for platform $1 (linux-x64, win32-x64)" >&2; exit 2 ;;
  esac
}

windows=false
case "$(uname -s)" in MINGW* | MSYS* | CYGWIN*) windows=true ;; esac
if [ $# -eq 0 ]; then
  if $windows; then
    platform=win32-x64
    dir=$(cygpath -u "$LOCALAPPDATA")/ductus/typescript-$VERSION
  else
    platform=linux-x64
    dir=${XDG_CACHE_HOME:-$HOME/.cache}/ductus/typescript-$VERSION
  fi
elif [ $# -eq 2 ]; then
  platform=$1 dir=$2
else
  echo "usage: tsc.sh [PLATFORM DIR]" >&2
  exit 2
fi
exe=tsc
[ "$platform" = win32-x64 ] && exe=tsc.exe
expected=$(sha256 "$platform")

if [ ! -f "$dir/$exe" ]; then
  work=$(mktemp -d)
  trap 'rm -rf "$work"' EXIT
  archive=$work/typescript-$platform-$VERSION.tgz
  curl -fsSL -o "$archive" "https://registry.npmjs.org/@typescript/typescript-$platform/-/typescript-$platform-$VERSION.tgz"
  echo "$expected  $archive" | sha256sum --check --quiet
  tar -xzf "$archive" -C "$work"
  # The compiler and its lib.*.d.ts, with the package's license and notices.
  mkdir -p "$work/out"
  cp -r "$work/package/lib/." "$work/out/"
  cp "$work/package/LICENSE" "$work/package/NOTICE.txt" "$work/out/"
  chmod 0755 "$work/out/$exe"
  # Moved into place whole, so a folder with tsc in it is always complete.
  mkdir -p "$(dirname "$dir")"
  rm -rf "$dir"
  mv "$work/out" "$dir"
fi
if $windows; then cygpath -w "$dir"; else echo "$dir"; fi
