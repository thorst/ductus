# Downloading and checking a release

Every release is on GitHub: [github.com/thorst/ductus/releases](https://github.com/thorst/ductus/releases).
Each one holds the packages for every way to install, the docs for that version as a download,
lists of every library inside (SBOMs), and what you need to check that a file is the one the
project built: its SHA-256 checksum and GitHub's record of the build (an attestation).

**Check before you install**, every time: a file downloaded through a proxy, a mirror or a
colleague's share could have been changed on the way, and on Windows the installer isn't
code-signed before v1.0, so nothing else tells you it's genuine. **You can skip it** for the
[`ductus dev`](dev.md) laptop setup built from source, where nothing was downloaded.

> **Pre-releases until v1.0.** Every release before 1.0 is marked pre-release: for trying,
> testing and pilots, not production. Settings can change between releases; the
> [upgrade](../upgrade.md) page and `ductus upgrade --check` say what each one changes.

## Which file

| File | For | Page |
|---|---|---|
| `ductus_0.6.0_amd64.deb` | Ubuntu 22.04 and later, Debian 12 and later | [Linux packages](linux.md) |
| `ductus-0.6.0-1.x86_64.rpm` | RHEL, Rocky, Alma 9 | [Linux packages](linux.md) |
| `ductus-0.6.0-x64.msi` | Windows Server, Windows 11 | [Windows installer](windows.md) |
| `ductus-0.6.0-linux-x64.tar.gz`, `ductus-0.6.0-windows-x64.zip` | The program alone: checking an upgrade before installing it, servers without the packages | [The portable archive](archive.md) |
| `ductus-docs-0.6.0.zip` | These docs, as they are for this version, to read offline | [The docs as a download](#the-docs-as-a-download) |
| `ductus-0.6.0-linux-x64.cdx.json`, `ductus-0.6.0-windows-x64.cdx.json`, `ductus-0.6.0-web-ui.cdx.json` | Your security team: every library in each build | [The SBOMs](#the-sboms-every-library-inside) |
| `SHA256SUMS` | Checking any of the files above | [Below](#check-the-checksum) |

Every file comes from the same CI run, on the tagged commit, and only after every test and demo
passed on it: the Rust tests on Linux and Windows, the end-to-end demos, the packages installed,
upgraded and removed under real systemd, the MSI installed, upgraded and removed as a service.
Nothing is built by hand.

## Check the checksum

`SHA256SUMS` lists every file of the release with its SHA-256. Download it beside the files you
took, and check them.

On Linux, in the folder with the downloads (`--ignore-missing` skips the files you didn't
download):

```sh
sha256sum --check --ignore-missing SHA256SUMS
```

Each file says `OK`. Anything else (`FAILED`, `No such file`) means the file isn't the one the
release holds: download it again from the release page, and don't install it. On the release
job's dry run (version `0.0.0-dryrun.118`; a release's files are named the same way with its
version), with only the `.deb` downloaded beside `SHA256SUMS`, then the same file with one byte
added to it:

```text
$ sha256sum --check --ignore-missing SHA256SUMS
ductus_0.0.0~dryrun.118_amd64.deb: OK
$ sha256sum --check --ignore-missing SHA256SUMS
sha256sum: WARNING: 1 computed checksum did NOT match
sha256sum: SHA256SUMS: no file was verified
ductus_0.0.0~dryrun.118_amd64.deb: FAILED
```

The second exits 1, so a script can stop on it. (A pre-release's `.deb` and `.rpm` are named
with `~` where the version has `-`: that is how Debian and RPM spell "sorts before the release".)

On Windows, in PowerShell, the hash of the file, then the line `SHA256SUMS` has for it:

```powershell
(Get-FileHash .\ductus-0.6.0-x64.msi -Algorithm SHA256).Hash
Select-String ductus-0.6.0-x64.msi .\SHA256SUMS
```

The two must be the same 64 characters (`Get-FileHash` writes them in capitals, `SHA256SUMS` in
small letters: the case doesn't matter). On the dry run's MSI:

```text
PS> (Get-FileHash .\ductus-0.0.0-dryrun.118-x64.msi -Algorithm SHA256).Hash
3F8DB975E899350CCC54FC0DAF3F659A7BC5A1BA69DE074ADE95AF47BE43A47D
PS> Select-String ductus-0.0.0-dryrun.118-x64.msi .\SHA256SUMS

SHA256SUMS:6:3f8db975e899350ccc54fc0daf3f659a7bc5a1ba69de074ade95af47be43a47d  ductus-0.0.0-dryrun.118-x64.msi
```

## Check who built it: the attestation

A checksum proves the file matches `SHA256SUMS`; it doesn't prove who wrote `SHA256SUMS`.
Every file of a release, `SHA256SUMS` included, is also **attested** by GitHub: a signed record,
made by the release job as it built the file, that names the repository, the workflow and the
exact commit. GitHub signs it through [Sigstore](https://www.sigstore.dev/)'s public log, so the
record can't be changed afterwards, even by the project.

Check one with the [GitHub CLI](https://cli.github.com/), signed in with any GitHub account
(`gh auth login`):

```sh
gh attestation verify ductus_0.6.0_amd64.deb -R thorst/ductus
```

It looks up the file's SHA-256 in GitHub's record of attestations, checks the signature, and
checks the attestation was made by a workflow of `thorst/ductus`. On v0.6.0's `.deb` (gh 2.87.3):

```text
$ gh attestation verify ductus_0.6.0_amd64.deb -R thorst/ductus
Loaded digest sha256:afb4c7f622a256e1aade44cc9a8a9f023504523c6bdac69a66d4eabc381e2ba9 for file://ductus_0.6.0_amd64.deb
Loaded 1 attestation from GitHub API

The following policy criteria will be enforced:
- Predicate type must match:................ https://slsa.dev/provenance/v1
- Source Repository Owner URI must match:... https://github.com/thorst
- Source Repository URI must match:......... https://github.com/thorst/ductus
- Subject Alternative Name must match regex: (?i)^https://github.com/thorst/ductus/
- OIDC Issuer must match:................... https://token.actions.githubusercontent.com

✓ Verification succeeded!

The following 1 attestation matched the policy criteria

- Attestation #1
  - Build repo:..... thorst/ductus
  - Build workflow:. .github/workflows/ci.yml@refs/tags/v0.6.0
  - Signer repo:.... thorst/ductus
  - Signer workflow: .github/workflows/ci.yml@refs/tags/v0.6.0
```

**What to look for:** `✓ Verification succeeded!`, and a build workflow that names the release's
own tag (`@refs/tags/v0.6.0` for 0.6.0): the file was built by the project's CI from that tag,
not from a branch or someone's fork. The `Loaded digest` line's checksum is the same as the
file's line in `SHA256SUMS`.

On Windows it's the same command (`gh` installs with `winget install GitHub.cli`), and the same
output for the MSI, with its own checksum:

```text
PS> gh attestation verify ductus-0.6.0-x64.msi -R thorst/ductus
Loaded digest sha256:c53fbcf84bf60d21a7a4df857d9258f4a6f626ffc6f32b9ae04c1c368907a1c9 for file://ductus-0.6.0-x64.msi
Loaded 1 attestation from GitHub API
...
✓ Verification succeeded!
...
  - Build workflow:. .github/workflows/ci.yml@refs/tags/v0.6.0
```

It's the check that replaces a code signature until v1.0
([nuance](../nuances/windows.md#the-installer-isnt-signed-yet-smartscreen-says-unknown-publisher-level-1)).

**When it fails**, it says so and exits 1. A file changed on the way (here the `.deb` with one
byte added) has a checksum GitHub has no attestation for:

```text
$ gh attestation verify ductus_0.6.0_amd64.deb -R thorst/ductus
Loaded digest sha256:179c1bd745298606b4f9224421609b247d70f12cb2888d57a8146fd27c75ffaf for file://ductus_0.6.0_amd64.deb
✗ Loading attestations from GitHub API failed

Error: HTTP 404: Not Found (https://api.github.com/repos/thorst/ductus/attestations/sha256:179c1bd745298606b4f9224421609b247d70f12cb2888d57a8146fd27c75ffaf?per_page=30&predicate_type=https://slsa.dev/provenance/v1)
```

The "404" is the point: no attestation exists for a file with that content. Don't install it;
download it again from the release page. The same happens for a file from another repository.

**In a script, check the exit code, not the text:** `gh` writes this report only to a terminal.
Run from a script, a pipe or a CI job, it prints nothing at all and exits 0 (verified) or 1 (not):
`gh attestation verify ductus_0.6.0_amd64.deb -R thorst/ductus || exit 1`. Add `--format json`
for the details as JSON.

**When to use which:** the checksum always (it needs nothing but the system's own tools); the
attestation once per release on the machine you download on, before the file is copied to
servers, or wherever your security policy asks for proof of origin.

## The SBOMs: every library inside

A Software Bill of Materials lists every library a program is made of, with its version and
license, in a format tools read: these are [CycloneDX](https://cyclonedx.org/) 1.5 JSON, the
format Dependency-Track, Microsoft Defender and most scanners import.

| File | Lists |
|---|---|
| `ductus-0.6.0-linux-x64.cdx.json` | Every Rust crate compiled into the Linux binary (the .deb, .rpm and .tar.gz hold the same one) |
| `ductus-0.6.0-windows-x64.cdx.json` | The same for `ductus.exe` (the .msi and .zip): a few crates differ by platform |
| `ductus-0.6.0-web-ui.cdx.json` | The web UI's npm packages (the UI is built into both binaries) |

On the dry run: 530 crates in the Linux binary's, 534 in the Windows one's, 95 npm packages in
the web UI's, about 700 KB, 700 KB and 100 KB. They leave out the tools used only while building (code generators, the build scripts' own
libraries): those aren't in the program you run. The same lists, for reading, with the notable
libraries first (V8, TypeScript, rustls, sqlx, tokio), are on
[Libraries and versions](../reference/libraries.md); that page also counts build-time crates, so
its numbers are a little higher. Use the SBOMs when your security team asks "what's in it", or to
be told when a library in it has a published vulnerability. Scanning them for you is planned for
v0.9.

## The docs as a download

`ductus-docs-0.6.0.zip` is this site as it is for that version: unzip it anywhere, open
`ductus-docs-0.6.0/index.html` in a browser. Every page says the version in its title. It's
about 3.2 MB (3,212,719 bytes on the dry run). It's for:

- **A server room or network without internet access**: the docs are with the download, nothing
  is fetched from anywhere.
- **The version you run, not the newest**: docs.getductus.com describes the latest release; the
  zip describes the one you installed, and stays that way.

Search works from the unzipped folder (opened as `file://`): it's mdBook's own search, which
loads its index from the folder. The site's search (Pagefind) needs a web server, so the zip
doesn't have it. **Don't** put the zip's folder on a web server as your copy of the site: it
works, but people then read an old version's docs as current; link to docs.getductus.com instead.

## Between releases: development builds

Every push to `main` also builds the packages, the archives and the installer in CI as
development builds of what comes next (versions like `0.7.0-dev.2`, kept 90 days as the run's
artifacts `linux-packages` and `windows-installer`; no checksums, attestations or SBOMs), and
[the packages](linux.md) and [the installer](windows.md) can be built from a clone of the
repository. They are for testing the project itself: use a release for anything else.
