#!/usr/bin/env bash
# Install what release-files.sh needs besides the docs tools (docs-tools.sh), pinned by version and
# checksum, into ~/.local/bin (or $1): cargo-cyclonedx, which writes the release's SBOMs. Then, with
# sudo on Ubuntu, rpm and msitools (to open the .rpm and the .msi). Linux x86-64.
#
# A new version is a deliberate change: the version and its checksum together (from the release's
# own .sha256 file beside the archive; 0.5.9's matched).
set -euo pipefail

CYCLONEDX_VERSION=0.5.9
CYCLONEDX_SHA256=fb8dbee9f182173e062a64a387b21a0badc6fab8b2abf9294973f012972bf6d8

bin=${1:-$HOME/.local/bin}
mkdir -p "$bin"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

if [ -x "$bin/cargo-cyclonedx" ] && "$bin/cargo-cyclonedx" cyclonedx --version 2>/dev/null | grep -F "$CYCLONEDX_VERSION" >/dev/null; then
  echo "cargo-cyclonedx $CYCLONEDX_VERSION: already installed" >&2
else
  curl -fsSL -o "$work/c.tar.xz" \
    "https://github.com/CycloneDX/cyclonedx-rust-cargo/releases/download/cargo-cyclonedx-$CYCLONEDX_VERSION/cargo-cyclonedx-x86_64-unknown-linux-gnu.tar.xz"
  echo "$CYCLONEDX_SHA256  $work/c.tar.xz" | sha256sum --check --quiet
  tar -xJf "$work/c.tar.xz" -C "$work"
  cp "$(find "$work" -type f -name cargo-cyclonedx | sed -n 1p)" "$bin/cargo-cyclonedx"
  chmod +x "$bin/cargo-cyclonedx"
  echo "cargo-cyclonedx $CYCLONEDX_VERSION: installed in $bin" >&2
fi

if ! command -v rpm >/dev/null || ! command -v msiinfo >/dev/null; then
  sudo env DEBIAN_FRONTEND=noninteractive apt-get install -y -qq rpm msitools </dev/null >/dev/null
  echo "rpm and msitools: installed" >&2
fi
