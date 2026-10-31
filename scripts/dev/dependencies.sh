#!/usr/bin/env bash
# Write docs/reference/libraries.md: every Rust crate in the Linux and Windows builds of `ductus`
# and every npm package in the web UI it embeds, with version and license, the notable ones
# first, and the Postgres that `ductus dev` downloads. It answers a security or legal team's
# "what's inside this release, which versions, under which licenses".
#
#   bash scripts/dev/dependencies.sh
#
# Run it after any change to Cargo.lock or ui/package-lock.json: the page records both files'
# checksums, and a CLI test (`the_libraries_page_is_current`) fails until it's made again.
# Needs cargo and Node (check.sh's). Reads no network: cargo resolves the Windows build's crates
# from Cargo.lock without that target installed.
set -euo pipefail
cd "$(dirname "$0")/../.."
[ -f ~/.cargo/env ] && source ~/.cargo/env
[ -x "$HOME/.local/node/bin/node" ] && export PATH="$HOME/.local/node/bin:$PATH"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# Everything compiled into each build: the binary's crates and the ones that run while compiling
# it (macros, build scripts), which shape it too. Our own workspace crates are left out.
for target in x86_64-unknown-linux-gnu x86_64-pc-windows-msvc; do
  cargo tree -p ductus -e normal,build --target "$target" --prefix none --format '{p}|{l}' >"$work/$target.txt"
done

v8=$(grep -E '^v8 v' "$work/x86_64-unknown-linux-gnu.txt" | sed -n '1s/^v8 v\([^ |]*\).*/\1/p')
v8_header=$(ls -d "${CARGO_HOME:-$HOME/.cargo}"/registry/src/*/"v8-$v8"/v8/include/v8-version.h 2>/dev/null | sed -n 1p || true)
v8_engine=""
if [ -n "$v8_header" ]; then
  v8_engine=$(awk '/define V8_(MAJOR|MINOR|BUILD|PATCH)_(VERSION|NUMBER|LEVEL) /{printf "%s%s", sep, $3; sep="."}' "$v8_header")
fi

LINUX="$work/x86_64-unknown-linux-gnu.txt" WINDOWS="$work/x86_64-pc-windows-msvc.txt" \
  POSTGRES=$(sed -n 's/^const VERSION: &str = "=\(.*\)";/\1/p' crates/ductus-cli/src/dev.rs) \
  TYPESCRIPT=$(tr -d '\r\n' <vendor/typescript/VERSION) \
  V8_ENGINE="$v8_engine" \
  CARGO_LOCK_SHA=$(sha256sum Cargo.lock | cut -d' ' -f1) \
  NPM_LOCK_SHA=$(sha256sum ui/package-lock.json | cut -d' ' -f1) \
  node - >"$work/libraries.md" <<'JS'
const fs = require("fs");
const env = process.env;

// Rust: `name vX.Y.Z (source)|license (*)`. A source in parentheses that starts with `/` is one
// of our own crates; a git URL (our tiberius fork) is kept and said.
const crates = new Map();
for (const [build, file] of [["linux", env.LINUX], ["windows", env.WINDOWS]]) {
  for (const line of fs.readFileSync(file, "utf8").split("\n")) {
    const m = line.match(/^(\S+) v(\S+)(?: \(([^)]*)\))?(?: \(proc-macro\))?\|(.*?)(?: \(\*\))?$/);
    if (!m) continue;
    const [, name, version, source = "", license] = m;
    if (source.startsWith("/")) continue;
    const key = `${name} ${version}`;
    const c = crates.get(key) ?? { name, version, license, git: source.startsWith("http") ? source : "", builds: new Set() };
    c.builds.add(build);
    crates.set(key, c);
  }
}
const rust = [...crates.values()].sort((a, b) => a.name.localeCompare(b.name) || a.version.localeCompare(b.version, undefined, { numeric: true }));

// The web UI: what its production build bundles (the lock file's entries not marked dev).
const lock = JSON.parse(fs.readFileSync("ui/package-lock.json", "utf8"));
const npm = Object.entries(lock.packages)
  .filter(([path, p]) => path.startsWith("node_modules/") && !p.dev)
  .map(([path, p]) => ({ name: path.slice(path.lastIndexOf("node_modules/") + "node_modules/".length), version: p.version, license: p.license || "see its package" }))
  .sort((a, b) => a.name.localeCompare(b.name));

const crate = (name) => {
  const found = rust.filter((c) => c.name === name);
  if (!found.length) throw new Error(`notable crate ${name} isn't in the build any more: update dependencies.sh's list`);
  return found;
};
const pkg = (name) => {
  const found = npm.filter((p) => p.name === name);
  if (!found.length) throw new Error(`notable npm package ${name} isn't in the UI any more: update dependencies.sh's list`);
  return found;
};
const where = (c) => (c.builds.size === 2 ? "Linux, Windows" : c.builds.has("linux") ? "Linux" : "Windows");

// The libraries a security review asks about first: what runs code, speaks TLS, stores data,
// signs people in. Each with what it does in Ductus.
const notable = [
  ["v8", `Google's V8 JavaScript engine${env.V8_ENGINE ? ` (V8 ${env.V8_ENGINE})` : ""}: runs interface code`],
  ["deno_core", "Embeds V8: modules, async calls, the bridge between interface code and the engine"],
  ["rustls", "TLS for every connection the engine makes or accepts"],
  ["aws-lc-rs", "The cryptography under TLS (AWS-LC; post-quantum key exchange preferred)"],
  ["rustls-platform-verifier", "Checks certificates against the operating system's trusted roots"],
  ["sqlx", "PostgreSQL: the engine's store and queue, and `db()` calls to Postgres"],
  ["tiberius", "SQL Server for `db()` calls (our fork: Windows logins)"],
  ["tokio", "The asynchronous runtime: sockets, timers, tasks"],
  ["hyper", "HTTP/1 and HTTP/2, under the API, HTTP sources and destinations"],
  ["axum", "The REST API and the web UI's server"],
  ["reqwest", "Outgoing HTTP: destinations, `fetch()`, webhooks"],
  ["russh", "SFTP sources and destinations (SSH)"],
  ["ldap3", "Sign-in with LDAP or Active Directory"],
  ["lettre", "Alert email (SMTP)"],
  ["argon2", "Hashes local accounts' passwords (argon2id)"],
  ["chacha20poly1305", "Encrypts secrets files"],
  ["postgresql_embedded", "`ductus dev`: downloads and runs Postgres for a laptop"],
  ["serde_yaml_ng", "Reads the project's YAML files"],
  ["clap", "The `ductus` command line"],
  ["windows-service", "Runs as a Windows service"],
];
const notableNpm = [
  ["react", "The web UI's components"],
  ["react-dom", "Renders them in the browser"],
  ["@tanstack/react-router", "The web UI's pages and links"],
  ["@tanstack/react-query", "Reads the REST API and keeps pages current"],
  ["radix-ui", "Accessible menus, dialogs and controls"],
  ["openapi-fetch", "The typed REST API client"],
];

const out = [];
const w = (s = "") => out.push(s);
const cell = (s) => String(s).replace(/\|/g, "\\|");
w("# Libraries and versions");
w();
w("Everything this version of Ductus is built from, for a security or legal review: every Rust crate in the");
w("Linux and Windows builds of the `ductus` binary and every npm package in the web UI it serves, with its");
w("version and license. The ones a review usually asks about first come first. The lists include the crates");
w("that only run while the binary is compiled (macros, build scripts), since they shape it too.");
w();
w("Nothing on this page is downloaded when Ductus runs, except Postgres for `ductus dev` (below). The binary");
w("needs only the operating system's C library on Linux (glibc 2.34 or later).");
w();
w(`- Rust crates: **${rust.length}** (${rust.filter((c) => c.builds.has("linux")).length} in the Linux build, ${rust.filter((c) => c.builds.has("windows")).length} in the Windows build)`);
w(`- npm packages in the web UI: **${npm.length}**`);
w(`- TypeScript **${env.TYPESCRIPT}** (Microsoft, Apache-2.0), inside the binary: \`ductus validate\` and \`ductus types\` type-check interface code with it`);
w();
w("## The notable ones");
w();
w("| Library | Version | License | What it does in Ductus | In |");
w("|---|---|---|---|---|");
// A crate in two versions (another library wants the older one) is one row with both.
for (const [name, what] of notable) {
  const found = crate(name);
  const c = { ...found[0], builds: new Set(found.flatMap((f) => [...f.builds])) };
  const versions = found.map((f) => f.version).join(", ");
  const licenses = [...new Set(found.map((f) => f.license))].join("; ");
  w(`| \`${c.name}\`${c.git ? " (fork)" : ""} | ${versions} | ${cell(licenses)} | ${what} | ${where(c)} |`);
}
w(`| TypeScript | ${env.TYPESCRIPT} | Apache-2.0 | Type-checks interface code (\`validate\`, \`types\`); kept in \`vendor/typescript\` | Linux, Windows |`);
for (const [name, what] of notableNpm) {
  for (const p of pkg(name)) w(`| \`${p.name}\` (npm) | ${p.version} | ${cell(p.license)} | ${what} | the web UI |`);
}
w();
w("## Downloaded by `ductus dev`");
w();
w(`\`ductus dev\` (the engine with its own database, for a developer's machine) downloads **PostgreSQL ${env.POSTGRES}**`);
w("once per user: zonky's binaries from Maven Central (about 30 MB), under the PostgreSQL License. Servers never");
w("download it: production uses your own PostgreSQL.");
w();
w("## Licenses");
w();
w("How many libraries use each license expression (`A OR B`: the library may be used under either).");
w();
w("| License | Rust crates | npm packages |");
w("|---|---|---|");
const licenses = new Map();
for (const c of rust) (licenses.get(c.license) ?? licenses.set(c.license, [0, 0]).get(c.license))[0]++;
for (const p of npm) (licenses.get(p.license) ?? licenses.set(p.license, [0, 0]).get(p.license))[1]++;
for (const [license, [r, n]] of [...licenses].sort((a, b) => b[1][0] + b[1][1] - (a[1][0] + a[1][1]) || a[0].localeCompare(b[0]))) {
  w(`| ${cell(license)} | ${r || ""} | ${n || ""} |`);
}
w();
w("## Every Rust crate");
w();
w("| Crate | Version | License | Linux | Windows |");
w("|---|---|---|---|---|");
for (const c of rust) {
  w(`| \`${c.name}\`${c.git ? ` (from ${c.git.replace(/#.*/, "")})` : ""} | ${c.version} | ${cell(c.license)} | ${c.builds.has("linux") ? "✓" : ""} | ${c.builds.has("windows") ? "✓" : ""} |`);
}
w();
w("## Every npm package in the web UI");
w();
w("The packages its production build bundles (the build tools and test tools aren't shipped).");
w();
w("| Package | Version | License |");
w("|---|---|---|");
for (const p of npm) w(`| \`${p.name}\` | ${p.version} | ${cell(p.license)} |`);
w();
w("---");
w();
w("Made by `scripts/dev/dependencies.sh` from these files (a test fails when either changes and this page");
w("isn't made again):");
w();
w(`<!-- made from: Cargo.lock ${env.CARGO_LOCK_SHA} ui/package-lock.json ${env.NPM_LOCK_SHA} -->`);
w(`- \`Cargo.lock\`, SHA-256 \`${env.CARGO_LOCK_SHA}\``);
w(`- \`ui/package-lock.json\`, SHA-256 \`${env.NPM_LOCK_SHA}\``);
process.stdout.write(out.join("\n") + "\n");
JS
# Only a whole page replaces the old one.
cp "$work/libraries.md" docs/reference/libraries.md
echo "wrote docs/reference/libraries.md" >&2
