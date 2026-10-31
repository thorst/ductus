#!/usr/bin/env bash
# What a release holds, made and checked (ci.yml's `release` job, a tag or a dry run):
#
#   bash scripts/dev/release-files.sh VERSION IN OUT
#
# IN holds the jobs' artifacts as downloaded (the .deb, .rpm and .tar.gz from `packaging`, the .msi
# and .zip from `windows`, in any folders). OUT gets the release's files, nothing else:
#
#   ductus_<v>_amd64.deb  ductus-<v>-1.x86_64.rpm  ductus-<v>-linux-x64.tar.gz  the Linux packages
#   ductus-<v>-x64.msi  ductus-<v>-windows-x64.zip                            Windows
#   ductus-docs-<v>.zip            the docs as a site on disk (mdBook's search works from file://)
#   ductus-<v>-{linux,windows}-x64.cdx.json  ductus-<v>-web-ui.cdx.json      CycloneDX SBOMs
#   SHA256SUMS                     every file above
#
# (<v> in the .deb and .rpm names is nfpm's: a pre-release 0.6.1-rc.1 is written 0.6.1~rc.1.)
# Each package is opened and must say VERSION; the docs zip must open at index.html with every
# link inside it working; SHA256SUMS must verify. Exit 1 with what's wrong. Needs dpkg-deb, rpm,
# msiinfo (msitools), unzip, mdBook and lychee (docs-tools.sh), cargo-cyclonedx (release-tools.sh),
# cargo and npm. Takes about a minute, mostly the docs build and lychee.
set -euo pipefail
cd "$(dirname "$0")/../.."
export PATH="$HOME/.local/bin:$PATH"
[ $# -eq 3 ] || { echo "usage: release-files.sh VERSION IN OUT" >&2; exit 2; }
version=$1 in=$(realpath "$2") out=$(realpath -m "$3")
nfpm=${version/-/\~} # nfpm's spelling of a pre-release (Debian's and RPM's "sorts before")
msi=${version%%-*}   # an MSI's ProductVersion: three numbers (packaging/windows/build.ps1)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
rm -rf "$out" && mkdir -p "$out"
fail=""
check() { # check <what> <condition...>
  local what=$1
  shift
  if eval "$@"; then echo "ok: $what" >&2; else echo "FAILED: $what" >&2; fail+="
  $what"; fi
}

# The packages, found by name wherever the download put them.
take() {
  local found
  found=$(find "$in" -type f -name "$1")
  if [ "$(echo "$found" | grep -c .)" != 1 ]; then
    echo "FAILED: one $1 among the artifacts (found: ${found:-none})" >&2
    fail+="
  $1"
    return
  fi
  cp "$found" "$out/"
}
deb=ductus_${nfpm}_amd64.deb rpm=ductus-${nfpm}-1.x86_64.rpm tgz=ductus-$version-linux-x64.tar.gz
msifile=ductus-$version-x64.msi zip=ductus-$version-windows-x64.zip
for f in "$deb" "$rpm" "$tgz" "$msifile" "$zip"; do take "$f"; done
[ -z "$fail" ] || { echo "release files missing:$fail" >&2; exit 1; }

# Each one says the version inside, not only in its name.
check "$deb is version $nfpm" '[ "$(dpkg-deb -f "$out/$deb" Version)" = "$nfpm" ]'
check "$rpm is version $nfpm" '[ "$(rpm -qp --qf "%{VERSION}" "$out/$rpm" 2>/dev/null)" = "$nfpm" ]'
tar -xzf "$out/$tgz" -C "$work"
check "$tgz's ductus says $version" '[ "$("$work/ductus-$version/ductus" --version)" = "ductus $version" ]'
check "$tgz holds the license, the unit and the settings file" \
  '[ -f "$work/ductus-$version/LICENSE" ] && [ -f "$work/ductus-$version/ductus.service" ] && [ -f "$work/ductus-$version/ductus.env" ]'
check "$msifile is ProductVersion $msi" \
  '[ "$(msiinfo export "$out/$msifile" Property | tr -d "\r" | sed -n "s/^ProductVersion\t//p")" = "$msi" ]'
check "$zip holds ductus-$version/ductus.exe" 'unzip -Z1 "$out/$zip" | grep -Fx "ductus-$version/ductus.exe" >/dev/null'

# The docs, as the site's pages with mdBook's search (it works from file://; Pagefind, the site's,
# doesn't), titled with the version on every page, under one folder.
echo "== the docs" >&2
MDBOOK_BOOK__TITLE="Ductus $version" bash scripts/dev/docs-check.sh >"$work/docs-check.log" 2>&1 || {
  cat "$work/docs-check.log" >&2
  echo "FAILED: the docs build and their links (docs-check.sh)" >&2
  exit 1
}
docs=ductus-docs-$version
rm -rf "${work:?}/$docs" && cp -r target/book "$work/$docs"
(cd "$work" && zip -qr9 "$out/$docs.zip" "$docs")
mkdir "$work/unpacked" && unzip -q "$out/$docs.zip" -d "$work/unpacked"
check "$docs.zip opens at $docs/index.html, titled with the version" \
  'grep -F "<title>" "$work/unpacked/$docs/index.html" | grep -F "Ductus $version" >/dev/null'
check "every link inside $docs.zip works, unpacked (lychee, offline)" \
  'lychee --no-progress --offline --include-fragments --root-dir "$work/unpacked/$docs" "$work/unpacked/$docs/**/*.html" >"$work/lychee.log" 2>&1 || { cat "$work/lychee.log" >&2; false; }'

# The SBOMs: every crate in each binary (build-only tools left out) and the web UI's npm packages,
# dated by the commit so a rebuild writes the same files.
echo "== the SBOMs" >&2
export SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct)}
for target in x86_64-unknown-linux-gnu:linux x86_64-pc-windows-msvc:windows; do
  cargo cyclonedx -q --manifest-path crates/ductus-cli/Cargo.toml --format json --spec-version 1.5 \
    --target "${target%%:*}" --no-build-deps --override-filename "release-sbom"
  mv crates/ductus-cli/release-sbom.json "$out/ductus-$version-${target##*:}-x64.cdx.json"
  find crates -name 'release-sbom.json' -delete # it writes one beside every crate of the workspace
done
(cd ui && npm sbom --sbom-format cyclonedx --omit dev --package-lock-only) >"$out/ductus-$version-web-ui.cdx.json"
sbom() { # sbom FILE: the component it describes and how many it lists
  node -e 'const b = require(process.argv[1]); if (b.bomFormat !== "CycloneDX") process.exit(1);
    console.log(`${b.metadata.component.name} ${b.metadata.component.version}, ${b.components.length} components`)' "$1"
}
for os in linux windows; do
  check "the $os SBOM describes ductus $version ($(sbom "$out/ductus-$version-$os-x64.cdx.json"))" \
    '[[ "$(sbom "$out/ductus-$version-$os-x64.cdx.json")" == "ductus $version, "* ]]'
done
check "the web UI's SBOM lists its packages ($(sbom "$out/ductus-$version-web-ui.cdx.json"))" \
  '[[ "$(sbom "$out/ductus-$version-web-ui.cdx.json")" == *", "[1-9]*" components" ]]'

# Checksums last, over everything.
(cd "$out" && sha256sum -- * >SHA256SUMS)
check "SHA256SUMS verifies" '(cd "$out" && sha256sum --check --quiet SHA256SUMS)'

[ -z "$fail" ] || { echo "the release isn't right:$fail" >&2; exit 1; }
echo "== $out" >&2
(cd "$out" && ls -l --time-style=+ -- * | awk '{ printf "%12d  %s\n", $5, $NF }') >&2
