#!/usr/bin/env bash
# Build docs.getductus.com into target/site (or $1): the book with mdBook's own search turned off,
# then a Pagefind index of it, which docs/site/search.js puts behind the menu bar's search button.
#
# Why two builds of one book: mdBook's search downloads the whole book's index before its first
# result (5 MB, 0.78 MB gzipped), and Pagefind downloads only what a search needs, but Pagefind
# can't search a page opened from disk. So the site (this script) searches with Pagefind, and the
# offline docs zip (target/book from docs-check.sh, attached to each release) keeps mdBook's search.
#
# Used by docs.yml: every docs change builds it and docs-search-check.cjs tries its search; a release
# tag or a docs-0.6 publish uploads it to Pages. Tools: scripts/dev/docs-tools.sh. Takes 7-10 s on
# WSL's /mnt/d (Pagefind itself under 1 s).
set -euo pipefail
cd "$(dirname "$0")/../.."
export PATH="$HOME/.local/bin:$PATH"
out=$(realpath -m "${1:-target/site}")

rm -rf "$out"
# MDBOOK_OUTPUT__HTML__SEARCH__ENABLE is book.toml's output.html.search.enable, for this build only.
MDBOOK_OUTPUT__HTML__SEARCH__ENABLE=false mdbook build docs -d "$out" 2>&1 | grep -vE '\[INFO\]' >&2 || true
[ -f "$out/index.html" ] || { echo "the docs site didn't build (mdbook build docs)" >&2; exit 1; }
[ ! -e "$out/searchindex.js" ] || { echo "mdBook's search index is in the site: it should be off here" >&2; exit 1; }

# Index each page's own text (its <main>), not the menus around it. print.html (the whole book on
# one page) and 404.html stay out of the results: Pagefind then skips pages without the mark. A
# result's title is the page's own first heading (Pagefind would take the first <h1> anywhere,
# which is the menu bar's "Ductus").
pages=$(find "$out" -name '*.html' ! -name print.html ! -name 404.html ! -name toc.html)
# shellcheck disable=SC2086
sed -i -e '0,/<main>/s//<main data-pagefind-body>/' -e '0,/<h1 id=/s//<h1 data-pagefind-meta="title" id=/' $pages
marked=$(grep -l '<main data-pagefind-body>' $pages | wc -l)

said=$(pagefind --site "$out" 2>&1) || { echo "$said" >&2; exit 1; }
indexed=$(echo "$said" | sed -n 's/^ *Indexed \([0-9]*\) pages$/\1/p')
if [ "$indexed" != "$marked" ]; then
  echo "$said" >&2
  echo "Pagefind indexed ${indexed:-no} pages of the $marked marked for search" >&2
  exit 1
fi
echo "docs site: $out ($marked pages, searched with Pagefind $(pagefind --version | cut -d' ' -f2))" >&2
