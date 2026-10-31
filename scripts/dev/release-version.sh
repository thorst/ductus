#!/usr/bin/env bash
# Which version a CI run builds and releases (ci.yml). Prints NAME=value lines for $GITHUB_ENV:
#
#   bash scripts/dev/release-version.sh REF DRY_RUN RUN_NUMBER [--apply]
#
#   a dry run (DRY_RUN is true)      RELEASE_VERSION=0.0.0-dryrun.<RUN_NUMBER>, RELEASE_KIND=dry-run.
#                                    With --apply, Cargo.toml and Cargo.lock say that version too, in
#                                    this checkout only, so every file the run builds carries it
#                                    (`ductus --version` included), as a release's would carry its own.
#                                    0.0.0: a dry run's files can't be taken for a release's, and sort
#                                    below every real version.
#   a tag (REF is refs/tags/v0.6.0)  RELEASE_VERSION=0.6.0, RELEASE_KIND=tag. Fails unless Cargo.toml
#                                    says 0.6.0: the commit tagged is the release as it is, nothing
#                                    is rewritten.
#   anything else                    nothing: the jobs build as on every push.
set -euo pipefail
cd "$(dirname "$0")/../.."
ref=${1:-} dry_run=${2:-} run=${3:-} apply=${4:-}

# [workspace.package] version, the one every crate takes (version.workspace = true). No `$` after
# the closing quote: Windows' checkout ends each line with \r (core.autocrlf), and it stays.
cargo=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\([^"]*\)".*/\1/p' Cargo.toml)
[ -n "$cargo" ] || { echo "no version in Cargo.toml's [workspace.package]" >&2; exit 1; }

if [ "$dry_run" = true ]; then
  [[ "$run" =~ ^[0-9]+$ ]] || { echo "a dry run needs the run's number, got '$run'" >&2; exit 1; }
  version=0.0.0-dryrun.$run
  if [ "$apply" = --apply ]; then
    sed -i "/^\[workspace.package\]/,/^\[/s/^version = \"$cargo\"/version = \"$version\"/" Cargo.toml
    # Each workspace crate's entry: `name = "ductus..."` then `version = "<cargo>"`.
    awk -v old="$cargo" -v new="$version" '
      { line = $0; sub(/\r$/, "", line) }
      line ~ /^name = "ductus(-[a-z0-9]+)?"$/ { ours = 1; print; next }
      ours && line == "version = \"" old "\"" { sub(/version = "[^"]*"/, "version = \"" new "\""); changed++ }
      { ours = 0; print }
      END { if (changed == 0) exit 1 }' Cargo.lock >Cargo.lock.new || { echo "no ductus crate in Cargo.lock says $cargo" >&2; rm -f Cargo.lock.new; exit 1; }
    mv Cargo.lock.new Cargo.lock
    echo "this checkout now builds $version (Cargo.toml said $cargo)" >&2
  fi
  echo "RELEASE_VERSION=$version"
  echo "RELEASE_KIND=dry-run"
elif [[ "$ref" == refs/tags/v* ]]; then
  version=${ref#refs/tags/v}
  if ! [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$ ]]; then
    echo "the tag ${ref#refs/tags/} isn't a version (v0.6.0, v0.6.1-rc.1)" >&2
    exit 1
  fi
  if [ "$version" != "$cargo" ]; then
    echo "the tag says $version but Cargo.toml says $cargo: set [workspace.package] version = \"$version\" (and run cargo build to update Cargo.lock), commit, and tag that commit" >&2
    exit 1
  fi
  echo "RELEASE_VERSION=$version"
  echo "RELEASE_KIND=tag"
fi
