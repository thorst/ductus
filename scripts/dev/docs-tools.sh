#!/usr/bin/env bash
# Install the docs site's tools, pinned by version and checksum, into ~/.local/bin (or $1):
# mdBook (builds the book from docs/) and lychee (checks its links). Used by check.sh's docs step
# (when they're installed), CI's docs.yml, and anyone building the docs locally. Linux x86-64.
#
# A new version is a deliberate change: update the version and its checksum together (checksums
# taken from the release's files when first pinned).
set -euo pipefail

MDBOOK_VERSION=0.4.52
MDBOOK_SHA256=c0b903f01dd8f4edc644372ad2b80b1fdddd12552d37b6a098657cbd8eddd768
LYCHEE_VERSION=0.20.1
LYCHEE_SHA256=1dd08bb064e2114c2c9d0c9009f51577de24e805361d6332162807972d8e9474

bin=${1:-$HOME/.local/bin}
mkdir -p "$bin"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

install() { # name version url sha256
  local name=$1 version=$2 url=$3 sha=$4
  if [ -x "$bin/$name" ] && "$bin/$name" --version 2>/dev/null | grep -F "$version" >/dev/null; then
    echo "$name $version: already installed" >&2
    return
  fi
  curl -fsSL -o "$work/$name.tar.gz" "$url"
  echo "$sha  $work/$name.tar.gz" | sha256sum --check --quiet
  tar -xzf "$work/$name.tar.gz" -C "$work"
  install_from=$(find "$work" -type f -name "$name" | sed -n 1p)
  cp "$install_from" "$bin/$name"
  chmod +x "$bin/$name"
  echo "$name $version: installed in $bin" >&2
}

install mdbook "$MDBOOK_VERSION" \
  "https://github.com/rust-lang/mdBook/releases/download/v$MDBOOK_VERSION/mdbook-v$MDBOOK_VERSION-x86_64-unknown-linux-gnu.tar.gz" \
  "$MDBOOK_SHA256"
install lychee "$LYCHEE_VERSION" \
  "https://github.com/lycheeverse/lychee/releases/download/lychee-v$LYCHEE_VERSION/lychee-x86_64-unknown-linux-gnu.tar.gz" \
  "$LYCHEE_SHA256"
