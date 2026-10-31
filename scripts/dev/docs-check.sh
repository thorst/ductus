#!/usr/bin/env bash
# Build the docs site (docs/ -> target/book) and check its links. The same check runs in three
# places: CI's docs.yml on every docs change, check.sh when the tools are installed, and by hand.
# Tools: scripts/dev/docs-tools.sh (mdBook and lychee, pinned).
#
#   bash scripts/dev/docs-check.sh            every link inside the site, offline: pages, files and
#                                             #anchors. A broken one fails (exit 1).
#   bash scripts/dev/docs-check.sh --network  also the links to other sites. Prints what's broken
#                                             and exits with lychee's code; docs.yml's weekly run
#                                             reports it without failing (other people's sites go
#                                             down, and that shouldn't block a docs change).
#
# Anything after that goes to lychee as is (docs.yml adds `--format markdown --output FILE`).
# The built HTML is checked, not the markdown: mdBook makes the heading ids (#anchors) and rewrites
# `.md` links to `.html`, so only the output shows whether a link really lands. Takes a few seconds.
set -euo pipefail
cd "$(dirname "$0")/../.."
export PATH="$HOME/.local/bin:$PATH"
mode=offline
[ "${1:-}" = --network ] && { mode=network; shift; }

mdbook build docs 2>&1 | grep -vE '\[INFO\]' >&2 || true
[ -f target/book/index.html ] || { echo "the docs site didn't build (mdbook build docs)" >&2; exit 1; }

# --root-dir: links that start with / are read inside the site, not the machine's root.
args=(--no-progress --include-fragments --root-dir "$PWD/target/book")
if [ $mode = offline ]; then
  args+=(--offline)
else
  # Every page's "edit" link goes to GitHub's editor, which wants a sign-in: not a link to check,
  # and ~40 requests to github.com in a row draw its rate limit (429) for the links that matter.
  args+=(--exclude '^https://github\.com/thorst/ductus/edit/' --max-retries 2)
fi
lychee "${args[@]}" "$@" 'target/book/**/*.html'
