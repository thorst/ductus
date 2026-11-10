# Releases and the gate

A **release** is one file holding exactly what a commit of your project has, with every
interface's code version, who made it, why, from which commit, and whether its tests passed. The
**gate** (`ductus release check`) checks a release against an environment (its values, secrets
and database) before anything deploys it, and changes nothing. This page is for whoever ships
changes: a developer, a release manager, a CI pipeline.

Releases are new in 0.7.4.3. Today you make them and check them from a shell or CI; storing them
in the engine's database, deploying them with `ductus deploy` and promoting one from test to
prod come next. Until then you deploy as before (`git pull` and
`systemctl reload ductus`, [what runs, recorded](deployments.md)), and a release is the thing
you check first and keep as the record of what you meant to ship.

**When not to bother:** a project you deploy only to your own machine with `ductus dev`. A
release earns its place once code goes through review, or to more than one environment.

## Make a release

In the project's folder, with everything committed:

```text
$ ductus release -m "first release"
running the tests on what the release carries…
release 7427b4d0a2b8 of integrations
  1 interface(s), 13 file(s), 7.6 KB unpacked, 4.3 KB as a release
  from commit 154175bd8a1c (1 commit(s) of history carried)
  tests: 1 passed (environment dev) in 0.0 s
  project version 56cb5df5ea64
wrote ~/integrations/.ductus/releases/integrations-7427b4d0a2b8.tar.gz (0.1 s)
check it against an environment: ductus release check ~/integrations/.ductus/releases/integrations-7427b4d0a2b8.tar.gz --env prod
```

That project is `ductus init`'s (one interface) in git; the outputs on this page are the demo's
([below](#try-it)), whose interface is named `release-demo-adt`. What `ductus release` did, in
order:

1. **Took HEAD's files** from git's own store: the project's folder of the commit (a project
   inside a bigger repository is just its folder), not the files on your disk. A Windows
   checkout, whose files have CRLF line ends, gives the same release as a Linux one.
2. **Refused what a release mustn't carry** (below): files not committed, a private key, code
   imported from outside the project, a symbolic link or a submodule.
3. **Unpacked them into a temporary folder** and worked out every interface's
   [code version](deployments.md#the-code-version) there, loading the code as an engine's start
   does. The versions describe exactly what the release carries, never what happens to be on
   your disk.
4. **Ran the tests there** (`ductus test`, as a separate process, `--env dev` unless you say
   `--env`) and refused to make the release if one failed.
5. **Wrote the file**, `.ductus/releases/<project>-<id>.tar.gz` (`.ductus/` is in the
   `.gitignore` `ductus init` writes; `--out` writes it elsewhere).

| Option | What it does |
|---|---|
| `-m "why"` | The reason, carried in the release and shown by every check of it |
| `--uncommitted` | Carry files that aren't committed as they are in the folder, instead of refusing; the release lists them and every check says so with a ⚠ |
| `--no-tests` | Don't run the tests; the release says so, and the gate then runs every interface's |
| `--over RELEASE` | The release this one replaces (its whole id or its file): recorded for the deploy's "whose work would this undo" check when the project isn't in git |
| `--env NAME` | The environment the tests run in (default `dev`); the release is the same for every environment ([nuance](../nuances/releases.md#the-tests-at-release-time-run-in-dev-level-3)) |
| `--out PATH` | A folder, or the file to write |
| `--json` | The manifest as one JSON document, with the file's path, size and time: for CI |

Every option of both commands: [`ductus release`](../reference/cli/release.md).

In CI, let `ductus release` be the test step: it runs the tests on exactly what it carries, and
records that they passed, so each environment's gate then runs only the tests of what changes
there.

```text
ductus release -m "$CI_COMMIT_TITLE" --json --out dist/
```

(`--out` makes a folder that ends with `/` when it's missing.) **When not to use `--no-tests`:**
in a pipeline that ships. A release made without its tests makes every gate run every
interface's tests, and says so wherever it goes.

### What a release refuses

Each refusal names the files and says what to do. None of them writes anything.

| Refused | Why | What to do |
|---|---|---|
| A file changed, deleted, added or new and not ignored, not in HEAD | The release is the commit: a file only on your disk would ship code nobody can find in git ([nuance](../nuances/releases.md#a-release-is-the-commit-not-the-folder-level-1)) | Commit it; add it to `.gitignore` if it's yours alone; or `--uncommitted` to try a release out |
| A private key (a PEM `PRIVATE KEY`, or a `.key`, `.p12`, `.pfx`, `.jks` file) | A release is copied to CI and servers: the key would be readable by everyone handling it ([nuance](../nuances/releases.md#a-committed-private-key-refuses-the-release-level-1)) | `git rm --cached <file>`, keep keys out of the project, name them in resources.yaml by a path on the server |
| Code imported from outside the project | The release can't carry it; each server would run whatever it has at that path ([nuance](../nuances/releases.md#code-imported-from-outside-the-project-refuses-the-release-level-2)) | Move it into the project (`lib/`) |
| A failing test | A release is something you'd ship | Fix it, or `--no-tests` (said in the release) |
| A symbolic link or a git submodule | A release carries files only | Commit the file itself |

A change not committed, as it reads:

```text
$ ductus release
Error: 1 file(s) in the project aren't committed:
  interfaces/adt-in/archive.ts
a release is HEAD's commit: commit them (or add them to .gitignore), or make it with `--uncommitted` to carry them as they are (the release then says so)
```

## The release id

The id is a SHA-256 hash of the files the release carries, shown as its first 12 hex characters
(`7427b4d0a2b8`). **The same commit always gives the same id**: made again tomorrow, made by
someone else, made on Windows from a CRLF checkout, made with a different `-m`. A change to any
file the release carries (a test, a fixture, the README) gives another id. That's what makes an
id worth comparing: "is prod running what test ran?" is "same id?"
([nuance](../nuances/releases.md#the-same-commit-gives-the-same-release-id-whatever-the-reason-or-the-machine-level-2)).

It's not the archive's checksum: the archive also holds who made it and when, so two archives of
one commit differ in bytes and share an id. To check one by hand, the id is over lines
`sha256sum` prints, every file under `project/` in path order:

```text
ductus release 1
<sha-256 of the file>  ductus.yaml
<sha-256 of the file>  interfaces/adt-in/archive.ts
…
```

Texts count as code versions count them: without a byte order mark, with `\n` line ends. A file
that isn't text (an image, a spreadsheet, a CSV in Latin-1) is carried and counted byte for byte
([nuance](../nuances/releases.md#texts-are-carried-normalized-other-files-byte-for-byte-level-3)).

## What's in the file

A gzipped tar any tool opens (`tar tzf`, 7-Zip, Windows' own `tar`), in this order:

| Entry | What |
|---|---|
| `release.json` | The manifest (below): always first, so a tool can read it without unpacking the rest |
| `ancestry` | The commit's whole history: 20 bytes per commit, sorted. A deploy uses it to tell a colleague's merged work from work your release would undo |
| `project/…` | The project's files, as committed |

`ductus init`'s project as a release (its interface renamed `release-demo-adt`, as the demo
does):

```text
$ tar tzf integrations-7427b4d0a2b8.tar.gz
release.json
ancestry
project/.gitignore
project/.vscode/launch.json
project/.vscode/settings.json
project/README.md
project/ductus.yaml
project/environments/dev.yaml
project/environments/prod.yaml
project/interfaces/adt-in/archive.test.ts
project/interfaces/adt-in/archive.ts
project/interfaces/adt-in/fixtures/sample.hl7
project/interfaces/adt-in/interface.yaml
project/resources.yaml
project/tsconfig.json
```

and its manifest, as `ductus release --json` prints it (which adds `file`, `size` and
`seconds` at the end):

```json
{
  "format": 1,
  "id": "7427b4d0a2b8278bf690c521e050e0537f82201819b758123d218bd45105e2bd",
  "project": "integrations",
  "made": {
    "at": "2026-11-10T16:02:57Z",
    "by": "todd",
    "host": "DESKTOP-GCT6E6V",
    "reason": "first release",
    "binary": "0.7.0-dev",
    "platform": "linux-x86_64"
  },
  "git": {
    "commit": "30b7d9233446833ef15bdf2aa38918cbe2189191",
    "commits": 1
  },
  "over": null,
  "project_version": {
    "version": "56cb5df5ea64df483e884224448ff268c2936d426689e8c7f5fdd8fd02c78392",
    "files": [
      "ductus.yaml",
      "environments/dev.yaml",
      "environments/prod.yaml",
      "resources.yaml"
    ]
  },
  "interfaces": {
    "release-demo-adt": {
      "version": "dc57f91f79d2e319005d9c733f8106f53079404c7b503fc01b14573358e175a7",
      "files": [
        "interfaces/adt-in/archive.ts",
        "interfaces/adt-in/interface.yaml"
      ],
      "parts": {
        "source": [],
        "destinations": {
          "archive": [
            "interfaces/adt-in/archive.ts"
          ]
        }
      }
    }
  },
  "tests": {
    "ran": true,
    "environment": "dev",
    "passed": 1,
    "failed": 0,
    "ms": 63
  },
  "files": 13,
  "bytes": 7798
}
```

The same id as the release above, made from another clone of the same files at another time:
that's the point of it.

| Field | What |
|---|---|
| `interfaces` | Each interface's [code version](deployments.md#the-code-version), its files, and which files each part (source, each destination) runs |
| `project_version` | One hash over the project's own files, which belong to no interface: `ductus.yaml`, `resources.yaml`, `docs-schema.yaml`, `environments/`, `secrets/` (encrypted; each environment's key stays on its servers). A release that only changes `resources.yaml` changes this and no code version |
| `git` | The commit, how many commits `ancestry` holds, and the project's folder in the repository when it isn't the top |
| `uncommitted` | Files carried as they were in the folder (`--uncommitted`) |
| `over` | The release it replaces (`--over`) |
| `binary` | Files carried byte for byte (not text) |
| `made` | Who (the account that ran it), on which machine, when, why, and with which Ductus |

## The gate: `ductus release check`

Before a release goes to an environment, check it there:

```text
$ ductus release check integrations-212583ebcc72.tar.gz --env prod
ductus release check: release 212583ebcc72 of integrations against environment prod (nothing is changed)

The release
✓ 13 file(s) match its id 212583ebcc729254a9bb2366361180b55b372e46a383f5f7c03eacfd4dcbbef2
✓ made by todd on DESKTOP-GCT6E6V at 2026-11-10T16:01:13Z with 0.7.0-dev: archive: MSH-6
✓ from commit 10e754ba0520 (2 commit(s) of history)
✓ its tests passed when it was made (1 in environment dev)

What it changes in prod
✓ 1 interface(s) change, 0 new there, 0 as they run now
note: changed:
    release-demo-adt: dc57f91f79d2 → bb37528fc711

validate
✓ valid
    ⚠ release-demo-adt: about.from: add the sending system's `application` or `vendor`
    ⚠ release-demo-adt: about.to: add the receiving system's `application` or `vendor`

Tests
✓ every interface's (each one changes there): 1 passed in 0.0 s

release 212583ebcc72 passes the gate for prod (checked in 0.9 s)
```

It runs where the environment's settings are: prod's `DUCTUS_DATABASE_URL` (a read-only login is
enough, `pg_read_all_data`) and `DUCTUS_SECRET_KEY` (so `validate` can check prod's secrets),
on a prod server or a CI runner given them. What it does:

1. **Reads the release and checks its files against its id.** A release damaged or edited
   after it was made is refused before anything runs:

   ```text
   $ ductus release check edited.tar.gz --env prod
   Error: edited.tar.gz: its files don't match its id (the manifest says 212583ebcc72, the files are 461c8a4db5bb): the file was damaged or changed after it was made
   ```

2. **Unpacks it into a temporary folder** (removed at the end, whatever happens).
3. **Compares its code versions with what runs** on that database: the versions the
   environment's engines recorded at their last start or handover
   ([nuance](../nuances/releases.md#what-runs-there-is-what-the-engines-recorded-not-what-the-folder-holds-level-3)).
   Each interface changes, is new there, or runs as it is.
4. **Runs `ductus validate --read-only`** for that environment against its database, as a
   separate process: the config, the code loading, the types, the resources, lanes that would
   strand waiting messages, retention changes.
5. **Runs the tests of the interfaces the release changes there** (and those new there), as a
   separate process, in that environment. A release that changes nothing there runs none:

   ```text
   Tests
   ✓ no interface changes what runs: no tests to run (--all-tests runs them all)
   ```

   Every interface's tests run instead when the release was made with `--no-tests`, when there's
   no database to compare with, or with `--all-tests`
   ([nuance](../nuances/releases.md#the-gate-tests-what-the-release-changes-there-by-code-version-level-2)).

Any ✗ exits 1; warnings (⚠) don't. `--json` prints one document (each check with its section and
level, the manifest, the changes, the interfaces tested, validate's whole output) for CI.

**It writes nothing**: not in your project, not in the database, no folder left behind (the demo
checks all three). `validate` and `test` run with `--read-only`, which keeps them from writing
editor files into the copy.

**Without a database** the gate still checks the files, validate (without its database checks)
and every test, and says what it couldn't do:

```text
What it changes in prod
⚠ no database (DUCTUS_DATABASE_URL): what runs there isn't known, so every interface's tests run, and validate checks without it
```

**When not to rely on it alone:** the gate checks the release against the environment's
configuration and code; it doesn't send a message anywhere. To compare what the new code decides
with what the running code decided on real traffic, run [`ductus replay`](../upgrade.md) as
well.

## Measured

On a 4-core (8-thread) Linux machine, against a local Postgres (`scripts/bench-release.sh`;
details in [architecture](../architecture.md#releases-07432-releasers-the-clis-releasers)):

| Project | `ductus release` (tests run) | Its file | `release check`, everything new | Nothing changes | A tenth changes |
|---|---|---|---|---|---|
| `ductus init`'s, 1 interface, 13 files | 0.1 s | 4.3 KB | 0.9 s | 0.8 s | 0.9 s |
| The guides' 6 interfaces, 38 files | 1.2–2.1 s | 15.7 KB | 1.2–1.4 s | | |
| 1,001 interfaces, 2,002 tests, 5,023 files | 4.1–4.3 s | 142 KB | 8.6–8.9 s | 4.7–5.6 s | 5.2–5.4 s |

What grows with the project is `validate`, which the gate always runs whole (about 5 s of the
gate at a thousand interfaces); the tests grow only with what the release changes. A release
made on Windows from a CRLF checkout has the same id as one made on Linux (measured natively).

## Try it

`scripts/demo-release.sh` does all of the above on a fresh `ductus init` project against your
database: a release, the same id from a CRLF checkout, a change not committed refused, the gate
without and with the database, nothing changed after a start, a commit's change tested, an edited
release refused, and nothing written.
