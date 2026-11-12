# Releases and the gate

A **release** is one file holding exactly what a commit of your project has, with every
interface's code version, who made it, why, from which commit, and whether its tests passed. The
**gate** (`ductus release check`) checks a release against an environment (its values, secrets
and database) before anything deploys it, and changes nothing. This page is for whoever ships
changes: a developer, a release manager, a CI pipeline.

Releases are new in 0.7.4.3. You make them, check them from a shell or CI,
[store them on an engine](#store-a-release-on-an-engine) and have it check them against its own
environment, and (0.7.4.4) [deploy them](#deploy-a-release) to an engine in release mode on
Linux. Promoting one from test to prod, rolling back, and deploys on Windows and in containers come
next. An engine in folder mode deploys as before (`git pull` and `systemctl reload ductus`,
[what runs, recorded](deployments.md)).

**When not to bother:** a project you deploy only to your own machine with `ductus dev`. A
release earns its place once code goes through review, or to more than one environment.

## Make a release

In the project's folder, with everything committed:

```text
$ ductus release -m "first release"
running the tests on what the release carries…
release 7427b4d0a2b8 of integrations
  1 interface(s), 13 file(s), 7.6 KB unpacked, 4.3 KB as a release
  from commit 154175bd8a1c on branch main (1 commit(s) of history carried)
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
   separate process: the config, the code loading, the types, the resources, lane changes (a
   note: never a refusal), retention changes.
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

## Store a release on an engine

An engine keeps releases in its database, so every engine on that database (and, soon, the web
UI's Releases page) sees them, and deploying or rolling back will be choosing one. CI stores the
release it made, then asks the engine to check it there:

| Call | What it does | Who |
|---|---|---|
| `POST /api/v1/releases` | Stores the release file (the body, as `ductus release` wrote it) | `deploy` permission |
| `GET /api/v1/releases` | The releases stored, newest first, each with its last check per environment | anyone signed in |
| `GET /api/v1/releases/{id}` | One release (its id or first 12 characters): its manifest and every check, with reports | anyone signed in |
| `POST /api/v1/releases/{id}/check` | The gate, run by the engine against its own environment and database | `deploy` permission |

Every option and answer: the [REST API reference](../reference/api/releases.md).

**The `deploy` permission** is for operators and admins, given like `payloads`
([people](../configure/people.md#roles-and-permissions)): a CI token that ships changes, the
people who release. A viewer can't have it:

```text
$ ductus token create ci-release --role viewer --permission deploy
Error: `deploy` is for operators and admins, not viewers: make it an operator, or leave `deploy` out
```

(The demo's token names are its own; the outputs are what it printed.)

### Store it

```text
$ curl -sS -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/gzip" --data-binary @.ductus/releases/integrations-212583ebcc72.tar.gz "$DUCTUS_URL/api/v1/releases" | python3 -m json.tool
{
    "new": true,
    "id": "212583ebcc729254a9bb2366361180b55b372e46a383f5f7c03eacfd4dcbbef2",
    "short": "212583ebcc72",
    "project": "integrations",
    "made_at": "2026-11-10T22:41:41Z",
    "made_by": "todd",
    "reason": "archive: MSH-6",
    "commit": "d68fbc34cb4188c2c777b57d799de7a9bb24739c",
    "interfaces": 1,
    "files": 13,
    "bytes": 7829,
    "stored_at": "2026-11-10T22:41:44.079061Z",
    "stored_by": "release-demo-deploy-4574",
    "checks": [],
    "new_folders": 4,
    "new_files": 11
}
```

The engine reads the release as `ductus release check` does: its files must match its id, or it's
refused (400) before anything is stored; a release carrying a private key is refused too. It
stores only what the database doesn't have yet: each text once (shared with the
[code versions](deployments.md#the-code-version) engines record: 2 of these 13 files were there
already, from the engine's start), each folder's list of files once. A release that changes ten
interfaces of a thousand adds about ten folders and their changed files.

**The same release again** (the same commit, made again with another reason) is the same id:
stored once, said so (200, `"new": false`), nothing changed. It keeps the first one's manifest
([nuance](../nuances/releases.md#the-same-release-stored-twice-is-stored-once-with-its-first-manifest-level-2)):

```text
$ curl -sS -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/gzip" --data-binary @/home/ann/again.tar.gz "$DUCTUS_URL/api/v1/releases" | python3 -m json.tool | head -4
{
    "new": false,
    "id": "212583ebcc729254a9bb2366361180b55b372e46a383f5f7c03eacfd4dcbbef2",
    "short": "212583ebcc72",
```

Without the permission:

```text
$ curl -sS -H "Authorization: Bearer $VIEWER" -H "Content-Type: application/gzip" --data-binary @.ductus/releases/integrations-212583ebcc72.tar.gz "$DUCTUS_URL/api/v1/releases"
{"error":"`release-demo-viewer-4574` doesn't have the `deploy` permission: ask an admin for it"}
```

### Check it there

`POST /releases/{id}/check` runs [the gate](#the-gate-ductus-release-check) on the engine's
server, with the engine's environment, its secrets and its database: what `ductus release
check --env prod` does on a prod server, without anyone logging in to one. `?all_tests=true` is
`--all-tests`. It answers when the check ends, with the report `ductus release check --json`
prints (here the engine already ran that commit, so nothing changes; `all_tests` ran its test
anyway):

```text
$ curl -sS -X POST -H "Authorization: Bearer $TOKEN" "$DUCTUS_URL/api/v1/releases/212583ebcc72/check?all_tests=true" | python3 -m json.tool
{
    "id": 6,
    "at": "2026-11-10T23:54:24.899551328Z",
    "environment": "prod",
    "instance": "18e6d43dc5f3ff0f-433b",
    "host": "DESKTOP-GCT6E6V",
    "by": "release-demo-deploy-16736",
    "ok": true,
    "problems": 0,
    "seconds": 0.93,
    "report": {
        "release": "212583ebcc729254a9bb2366361180b55b372e46a383f5f7c03eacfd4dcbbef2",
        "environment": "prod",
        "ok": true,
        "problems": 0,
        "seconds": 0.92,
        "checks": [
            {
                "section": "release",
                "level": "ok",
                "text": "13 file(s) match its id 212583ebcc729254a9bb2366361180b55b372e46a383f5f7c03eacfd4dcbbef2",
                "more": []
            },
            {
                "section": "release",
                "level": "ok",
                "text": "made by todd on DESKTOP-GCT6E6V at 2026-11-10T23:54:21Z with 0.7.0-dev: archive: MSH-6",
                "more": []
            },
            {
                "section": "release",
                "level": "ok",
                "text": "from commit 40fba69d6f06 (2 commit(s) of history)",
                "more": []
            },
            {
                "section": "release",
                "level": "ok",
                "text": "its tests passed when it was made (1 in environment dev)",
                "more": []
            },
            {
                "section": "environment",
                "level": "ok",
                "text": "0 interface(s) change, 0 new there, 1 as they run now",
                "more": []
            },
            {
                "section": "validate",
                "level": "ok",
                "text": "valid",
                "more": [
                    "\u26a0 release-demo-adt: about.from: add the sending system's `application` or `vendor`",
                    "\u26a0 release-demo-adt: about.to: add the receiving system's `application` or `vendor`"
                ]
            },
            {
                "section": "tests",
                "level": "ok",
                "text": "every interface's (--all-tests): 1 passed in 0.0 s",
                "more": []
            }
        ],
        "tested": [
            "release-demo-adt"
        ],
        "changes": {
            "changed": [],
            "new": [],
            "unchanged": 1
        },
        "validate": {
            "ok": true,
            "output": "environment: prod\n\u2713 release-demo-adt: config and code load\n\u26a0 release-demo-adt: about.from: add the sending system's `application` or `vendor`\n\u26a0 release-demo-adt: about.to: add the receiving system's `application` or `vendor`\n  (warnings: `ductus impact` can't find what isn't named; `require_about: true` in ductus.yaml makes them errors)\n\u2713 database: TLS 1.3, not verified (sslmode=prefer): the server's certificate isn't checked (on this machine)\n\u2713 database: every interface's lanes reach the messages waiting for it\n\u2713 types: no errors\n\nvalid\n"
        }
    }
}
```

`ok` is what CI decides on. The check is kept with the release (`GET /releases` shows the last one
per environment, `GET /releases/{id}` every one with its report) and is in the audit log
(`release.check`), as is storing one (`release.store`).

- **It runs the release's code** on that server with that environment's secrets: hence the
  `deploy` permission
  ([nuance](../nuances/releases.md#checking-a-release-on-an-engine-runs-its-code-there-with-that-environments-secrets-level-2)).
- **It runs one at a time** per engine: a second check while one runs is refused with 409.

> **⚠ A check slows the engine while it runs.** It shares the engine's server: CPUs, memory,
> disk and the database. What it does, at a thousand interfaces:
>
> | Part | Time | What it uses |
> |---|---|---|
> | The release read back from the database, its id checked | a few tenths of a second | one CPU, a few database reads |
> | `validate`: every interface's code loaded, the type check, the database checks | ~5 s, every check | half the CPUs, ~290 MB, read-only queries |
> | The tests of what the release changes there | none to ~4 s | every CPU |
> | The report recorded | milliseconds | one row, one audit row |
>
> Measured on a 4-core machine: one interface's intake fell from ~800 to ~650 messages a second
> over a run overlapping one check, and to about half while checks ran back to back; ACKs waited
> up to ~0.1 s instead of ~0.03 s; nothing was refused or lost. Speed is back the moment the check
> ends. The check runs at a lower CPU priority, which made no measurable difference there
> ([nuance](../nuances/releases.md#a-check-on-the-engine-slows-its-intake-while-it-runs-and-runs-one-at-a-time-level-3)).
>
> **This is true of anything you add to a running engine**, not only checks: reports, exports,
> replays, connection checks, frequent API polling by a monitoring tool. Each one is a trade: what
> it tells you against what it costs the messages flowing meanwhile. Decide how often each is
> worth running, as you would for a monitoring schedule (a check every 15 minutes means a problem
> waits 7 minutes on average to be seen; every minute costs fifteen times as much). For checks:
> run one per release you mean to ship, not on every commit; on an engine that runs near its
> limits, check against a test engine sharing prod's configuration, or run `ductus release check`
> on another machine given prod's database (read-only) and secret key.
- **It changes nothing** but its own record: the engine's database is read-only to it, as for
  `ductus release check`.
- **Stored releases are kept**
  ([nuance](../nuances/releases.md#stored-releases-are-kept-level-3)).

In CI: `ductus release --json --out dist/` (its `id` and `file` are in the JSON), the two calls
above with the environment's URL and a token that has `deploy`, then fail the job when the
check's `ok` is false. Retry after a pause on 409 (another check is running there).

## Deploy a release

`ductus deploy` sends a release to an engine and deploys it there: the engine holds it against
what runs (would it undo someone else's work?), runs the gate against its own environment (or
reuses a recent passing check), then hands over to a new engine running it. Receiving never
stops; sending pauses while the two engines swap (tens of milliseconds for one interface, under
half a second at a thousand). Every deploy is recorded: who, why, which release, what changed
([what runs, recorded](deployments.md)), and in the audit log.

**When to use it:** an engine whose project should only ever be a release that passed the gate:
test and prod, once code goes through review. **When not:** your own machine (`ductus dev`), or
an engine where people still edit the project on the server: that's folder mode, and stays as it
was.

### Run the engine in release mode

An engine runs **releases** instead of a project folder when it's started with `--releases`
(DUCTUS_RELEASES) in place of `--root` (DUCTUS_PROJECT). In the package's
`/etc/ductus/ductus.env`, comment out `DUCTUS_PROJECT` and set:

```sh
DUCTUS_RELEASES=/var/lib/ductus/releases
```

then `sudo systemctl restart ductus`. The engine runs the release last deployed to its
environment for its project on its database, unpacked from the database into that folder (each
release in a folder named by its id; the running one and the two used last are kept). Before the
first deploy there is none: it starts with no interfaces and says so
([nuance](../nuances/releases.md#an-engine-in-release-mode-runs-nothing-until-a-release-is-deployed-and-its-api-starts-on-its-defaults-level-1)):

```text
release mode: nothing is deployed to this environment yet; the engine runs no interface until a release is (`ductus deploy`)
```

| Setting | What | Default |
|---|---|---|
| `DUCTUS_RELEASES` (`--releases`) | Release mode, and where releases are unpacked | off (folder mode) |
| `DUCTUS_RELEASE_PROJECT` (`--release-project`) | The project this engine runs, when several projects are deployed to its environment on one database | the only one |
| `DUCTUS_DEPLOY_REUSE_CHECK` (`--deploy-reuse-check`) | How long a deploy may reuse a passing check of the same release instead of running the gate again; `0`: never | `1h` |
| `DUCTUS_DEPLOY_REQUIRES` (`--deploy-requires`) | Environments a release must have been deployed to before this one takes it (`test`, `test,uat`) ([promote](#promote-and-roll-back)) | none |
| `DUCTUS_DEPLOY_BRANCHES` (`--deploy-branches`) | Branches a release may be made on to be deployed here (`main`, `main,release/*`) ([only main reaches prod](#only-main-reaches-prod)) | any |

On Windows, the same two lines in `C:\ProgramData\Ductus\ductus.env` (`DUCTUS_RELEASES=C:\ProgramData\Ductus\releases`,
`DUCTUS_PROJECT` commented out), then `Restart-Service Ductus`. In a container, `-e
DUCTUS_RELEASES=/tmp/ductus-releases`: no volume needed, the engine unpacks the release from the
database at every start ([Docker](../install/docker.md)).

Deploys apply by the [handover](high-availability.md#deploying-and-upgrading-without-a-gap):
under the package's systemd unit the new engine is a new process; on Windows, in a container
and under a plain `ductus run` it runs in the engine's own process, so the service stays Running
and the container never restarts
([nuance](../nuances/releases.md#a-deploy-applies-by-a-handover-to-a-new-process-under-systemd-in-the-engines-own-process-elsewhere-level-1)).

### Deploy

A token with the `deploy` permission (`ductus token create ci --role operator --permission
deploy`) in DUCTUS_API_TOKEN, the engine's API in DUCTUS_API_URL (or `--to`), and a reason
(`-m`, required everywhere but an environment named `dev`):

```text
$ ductus deploy .ductus/releases/integrations-7427b4d0a2b8.tar.gz -m "first release"
stored release 7427b4d0a2b8 on the engine
deploying: the gate runs (or a recent passing check is reused), then the handover…
release 7427b4d0a2b8 (project integrations) to environment `prod`
gate: passed in 0.91 s (no passing check of it here by Ductus 0.7.0-dev in the last 1 h, with nothing deployed to `prod` since)
  started /home/todd/target/release/ductus (pid 4598): it reads the project, migrates the database and loads every interface's code while this one runs
  ductus 0.7.0-dev (pid 4598) is ready after 438 ms and receives beside this one: stopping this one
  this engine stopped in 0 ms and let go of its interfaces: the new one takes them
✓ handed over to ductus 0.7.0-dev (pid 4598): receiving never stopped, sending paused 51 ms
changed 1 interface:
  release-demo-adt new here dc57f91f79d2
```

The release can be a file (stored first when the engine hasn't got it) or the id of one stored
there. It exits 1 when nothing was deployed; `--json` prints the engine's report.

| Option | What |
|---|---|
| `-m`, `--message` | Why: recorded with the deployment, in its history and in the audit log |
| `--to URL` | The engine's API (DUCTUS_API_URL; default `http://127.0.0.1:7700`) |
| `--force` | Deploy over someone else's work the "replaces" check names ([below](#it-wont-undo-someone-elses-work)); never skips the environment's rules |
| `--from URL` | Promote: fetch the release (by its id) from that engine with its record of where it went, then store and deploy it on `--to`'s ([promote](#promote-and-roll-back)) |
| `--from-token` | The token for `--from` (DUCTUS_FROM_TOKEN; default: `--token`'s) |
| `--rollback [ID]` | Go back to a release that ran there before ([roll back](#roll-back)) |
| `--all-tests` | The gate runs every interface's tests, not only those of what changes there (never reuses a check) |
| `--json` | The engine's report, as JSON |
| `--timeout` | How long to wait for the answer (default 40 min: the gate, then the handover) |

**The gate, or a check reused.** Checked on the engine first (`POST /releases/{id}/check`, as
when you want the answer before the change window), a deploy within the hour reuses that check
instead of running the gate again, as long as nothing at all was deployed to that environment
since, by the same Ductus version
([nuance](../nuances/releases.md#a-deploy-reuses-a-recent-passing-check-only-when-nothing-was-deployed-there-since-level-2)):

```text
$ ductus deploy 212583ebcc72 -m "archive: MSH-6"
deploying: the gate runs (or a recent passing check is reused), then the handover…
release 212583ebcc72 (project integrations) to environment `prod`
gate: reused check #24 (2026-11-11T17:06:08.209069Z by release-demo-deploy-3870: passed; nothing deployed here since)
  started /home/todd/target/release/ductus (pid 4685): it reads the project, migrates the database and loads every interface's code while this one runs
  ductus 0.7.0-dev (pid 4685) is ready after 412 ms and receives beside this one: stopping this one
  this engine stopped in 2 ms and let go of its interfaces: the new one takes them
✓ handed over to ductus 0.7.0-dev (pid 4685): receiving never stopped, sending paused 62 ms
changed 1 interface:
  release-demo-adt dc57f91f79d2 → bb37528fc711
```

A gate that finds problems refuses the deploy (exit 1, the problems listed, the check kept with
the release). A new engine that can't take over leaves the running one as it was, as any
handover does. For the rare failure after the old engine let go, see
[Troubleshooting](../troubleshooting.md#the-engine-stopped-during-a-deploy-or-reload).

**On Windows, in a container, under a plain `ductus run`**, the same deploy hands over inside the
engine's own process: the steps say "in this process" instead of a pid, and the service or the
container goes on as it was (here the engine is a container's PID 1, `scripts/demo-docker.sh`):

```text
$ ductus deploy /project/.ductus/releases/project-c85ec701a9c3.tar.gz --to http://127.0.0.1:7701 -m "first deploy"
stored release c85ec701a9c3 on the engine
deploying: the gate runs (or a recent passing check is reused), then the handover…
release c85ec701a9c3 (project project) to environment `dev`
gate: passed in 0.62 s (no passing check of it here by Ductus 0.7.0-dev in the last 1 h, with nothing deployed to `dev` since)
  started a new engine in this process: it reads the project, migrates the database and loads every interface's code while this one runs
  the new engine (ductus 0.7.0-dev, in this process) is ready after 151 ms and receives beside this one: stopping this one
  this engine stopped in 0 ms and let go of its interfaces: the new one takes them
✓ handed over to a new engine in this process (ductus 0.7.0-dev): receiving never stopped, sending paused 27 ms
no interface's code changed (the project's own files may have)
```

### It won't undo someone else's work

For each interface a release changes, the version running there must be yours, or come from a
commit in the release's history (a colleague's merged work), and not from files never committed;
the project's own files (`ductus.yaml`, `resources.yaml`, `environments/`) are held to the same
rule. Otherwise the deploy is refused before the gate runs, saying whose work and why:

```text
$ DUCTUS_API_TOKEN=$OPS ductus deploy 7427b4d0a2b8 -m "back to the first"
deploying: the gate runs (or a recent passing check is reused), then the handover…
release 7427b4d0a2b8 (project integrations) to environment `prod`
✗ would undo release-demo-adt: replaces bb37528fc711 (deployed 2026-11-11 17:06 UTC by release-demo-deploy-3870 on DESKTOP-GCT6E6V): it came from commit a324857afd23, which this release's history doesn't have, so their change may be undone (`ductus history release-demo-adt`)
✗ refused: it would undo someone else's work (release-demo-adt): merge their commit and release again, or deploy with `force` (`ductus deploy --force`) and a reason to undo it on purpose
```

Merge their commit and release again: what's in `main` is what ships. To undo their work on
purpose, `ductus deploy --force -m "why"`: the audit log (`release.force`) names each interface
and whose work, and the deployment's history keeps the warning. A release is the whole project,
so a colleague's deployed but unmerged work on *another* interface refuses yours too
([nuance](../nuances/releases.md#a-deploy-is-refused-when-it-would-undo-someone-elses-work-and-a-release-is-the-whole-project-level-2)).

### Through the API

`POST /api/v1/deployments` with `{"release": "<id>", "reason": "…", "force": false,
"all_tests": false, "rollback": false}` (the `deploy` permission; `release` may be left out of a
rollback). It answers when the new engine sends: 200 with the report (`ok`, `outcome`, `branch`,
`rules`, `gate`, `replaces`, `steps`, `changes`, and `place`: this deployment's record, as a
release file carries it), 409 when refused before anything ran (folder mode, another deploy or
check running, the release already runs, the environment's rules, the "replaces" check, a
rollback to a release that never ran there), 422 when the gate found problems, 500 when the new
engine couldn't take over (the running one goes on). One deploy or check at a time per engine.
Every attempt is in the audit log (`release.deploy`, or `release.rollback`, with `detail.ok` and
`detail.outcome`).

`GET /api/v1/releases/{id}/file` (the `deploy` permission, audited as `release.download`) gives
the release's file with its record brought up to date; `GET /api/v1/releases/{id}` says where it
was deployed (`deployed`) and its `branch`.

In release mode, the files on the server are the database's copy: an edit there is never
deployed ([nuance](../nuances/releases.md#in-release-mode-the-files-on-the-server-are-the-databases-copy-level-2)).

## Promote and roll back

A release moves from one environment to the next as the same file: the same id, checked by each
environment's own gate, with that environment's values and secrets. Each environment decides
what it takes (its engine's settings, not the project's: a release can't loosen its own rules),
and a deploy that went wrong is undone by going back to the release that ran before.

**When to use it:** test and prod have engines in release mode, and code reaches prod through
test. **When not:** a single environment (deploy, and roll back if needed, without the rules).

### Require test first

On prod's engine, in its ductus.env:

```sh
DUCTUS_DEPLOY_REQUIRES=test
```

A release that wasn't deployed to `test` is refused before anything runs (409, nothing changed):

```text
$ DUCTUS_API_TOKEN=$PROD_TOKEN ductus deploy .ductus/releases/integrations-7fc9677466dd.tar.gz --to $PROD -m "the lab feed"
stored release 7fc9677466dd on the engine
deploying: the gate runs (or a recent passing check is reused), then the handover…
release 7fc9677466dd (project integrations, branch main) to environment `prod`
✗ this environment takes only releases deployed to test first (DUCTUS_DEPLOY_REQUIRES), and it hasn't been deployed anywhere that this engine knows of: deploy it there, then bring it here with its record (`ductus deploy 7fc9677466dd --from <test's engine> --to <this engine>`)
✗ refused by this environment's rules (`rules` says why)
```

Test and prod have separate databases, so prod learns where a release went from **the record the
release carries**: each deploy of a file writes into it where it went (its id doesn't change: the
record isn't part of it):

```text
$ DUCTUS_API_TOKEN=$TEST_TOKEN ductus deploy .ductus/releases/integrations-7fc9677466dd.tar.gz --to $TEST -m "the lab feed"
stored release 7fc9677466dd on the engine (1 record of where it went added)
deploying: the gate runs (or a recent passing check is reused), then the handover…
release 7fc9677466dd (project integrations, branch main) to environment `test`
gate: passed in 0.99 s (no passing check of it here by Ductus 0.7.0-dev in the last 1 h, with nothing deployed to `test` since)
  started a new engine in this process: it reads the project, migrates the database and loads every interface's code while this one runs
  the new engine (ductus 0.7.0-dev, in this process) is ready after 362 ms and receives beside this one: stopping this one
  this engine stopped in 0 ms and let go of its interfaces: the new one takes them
✓ handed over to a new engine in this process (ductus 0.7.0-dev): receiving never stopped, sending paused 29 ms
changed 1 interface:
  promote-demo new here 0db7a1e1454e
recorded in .ductus/releases/integrations-7fc9677466dd.tar.gz: deployed to test
```

The record is unsigned: anyone holding the file can edit it
([nuance](../nuances/releases.md#where-a-release-went-and-its-branch-are-labels-in-its-file-not-proof-level-2)).
Several environments: `DUCTUS_DEPLOY_REQUIRES=test,uat`. `--force` never skips it, and a rollback
is never held to it
([nuance](../nuances/releases.md#an-environments-rules-arent-skipped-by---force-a-rollback-isnt-held-to-them-level-2)).

### Promote: `--from`

`ductus deploy ID --from <test> --to <prod>` fetches the release from test's engine with its
record brought up to date (`GET /releases/{id}/file`), stores it on prod's and deploys it there.
Each engine has its own tokens: `DUCTUS_FROM_TOKEN` (or `--from-token`) for the one it's fetched
from, DUCTUS_API_TOKEN for the one it goes to:

```text
$ DUCTUS_FROM_TOKEN=$TEST_TOKEN DUCTUS_API_TOKEN=$PROD_TOKEN ductus deploy 7fc9677466dd --from $TEST --to $PROD -m "tested in test"
fetched release 7fc9677466dd from http://127.0.0.1:17793 (1.3 KB, deployed to: dev, test)
release 7fc9677466dd was stored on the engine before (2 records of where it went added)
deploying: the gate runs (or a recent passing check is reused), then the handover…
release 7fc9677466dd (project integrations, branch main) to environment `prod`
gate: passed in 0.87 s (no passing check of it here by Ductus 0.7.0-dev in the last 1 h, with nothing deployed to `prod` since)
  started a new engine in this process: it reads the project, migrates the database and loads every interface's code while this one runs
  the new engine (ductus 0.7.0-dev, in this process) is ready after 365 ms and receives beside this one: stopping this one
  this engine stopped in 0 ms and let go of its interfaces: the new one takes them
✓ handed over to a new engine in this process (ductus 0.7.0-dev): receiving never stopped, sending paused 33 ms
changed 1 interface:
  promote-demo new here 0db7a1e1454e
```

Prod had stored it already (the refused deploy above): a release stored before keeps its first
manifest, and the records a file brings are added all the same. Prod's gate runs against prod:
a check in test says nothing about prod's database and secrets.

### Only main reaches prod

An engine in release mode never reads git: it runs the release deployed to it. A release is the
commit it was made from (`ductus release` refuses what isn't committed), and it records **the
branch** it was made on. So work on a feature branch reaches prod only if someone makes a release
there and deploys it. Two locks, the second one optional:

1. **Who can deploy.** CI builds releases from `main` and holds the only tokens with the `deploy`
   permission on test and prod; people get tokens without it there. This is the one that holds
   against someone determined.
2. **`DUCTUS_DEPLOY_BRANCHES=main`** on prod (a trailing `*` matches a start: `main,release/*`)
   refuses a release made on any other branch, made on a detached HEAD without a name, or made
   without git, with or without `--force`:

```text
$ DUCTUS_API_TOKEN=$PROD_TOKEN ductus deploy .ductus/releases/integrations-d74871d958b0.tar.gz --to $PROD --force -m "just this once"
stored release d74871d958b0 on the engine
deploying: the gate runs (or a recent passing check is reused), then the handover…
release d74871d958b0 (project integrations, branch feature/lab-codes) to environment `prod`
✗ this environment takes only releases deployed to test first (DUCTUS_DEPLOY_REQUIRES), and it hasn't been deployed anywhere that this engine knows of: deploy it there, then bring it here with its record (`ductus deploy d74871d958b0 --from <test's engine> --to <this engine>`)
✗ this environment takes only releases made on main (DUCTUS_DEPLOY_BRANCHES), and this one was made on `feature/lab-codes`: merge it, then release from there
✗ refused by this environment's rules (`rules` says why)
```

The branch is a label in the release, unsigned like the record. The same files made on a feature
branch and again on `main` (after a fast-forward merge) are one release, made on both: any of its
branches may match. In CI, a checkout of a tag or a pull request has no branch (a detached HEAD):
`ductus release --branch "$GITHUB_REF_NAME"` names it
([nuance](../nuances/releases.md#a-release-made-on-a-detached-head-records-no-branch-level-2)).
An engine in folder mode runs whatever its folder holds; it records the commit and any uncommitted
file and warns, but never refuses: one more reason for release mode on test and prod.

### Roll back

`ductus deploy --rollback -m "why"` goes back to the release that ran before the current one in
that environment (or `--rollback ID`, any release that ran there). It isn't held to the
environment's rules (the release ran there), it names whose newer work it undoes without
refusing (undoing is the point), and the gate always runs (the database may have changed since):

```text
$ DUCTUS_API_TOKEN=$OPS_PROD ductus deploy --rollback --to $PROD -m "LAB3 broke the lab feed"
rolling back: the gate runs, then the handover…
rollback to release e16ad8ac6cdb (project integrations, branch main) to environment `prod`
· a rollback isn't held to this environment's rules (DUCTUS_DEPLOY_REQUIRES, DUCTUS_DEPLOY_BRANCHES): release e16ad8ac6cdb ran here before
⚠ undoes promote-demo: replaces 1df6c3410060 (deployed 2026-11-12 16:36 UTC by ci on DESKTOP-GCT6E6V): it came from commit bc7b61dc0197, which this release's history doesn't have, so their change may be undone (`ductus history promote-demo`)
gate: passed in 0.81 s (no passing check of it here by Ductus 0.7.0-dev in the last 1 h, with nothing deployed to `prod` since)
  started a new engine in this process: it reads the project, migrates the database and loads every interface's code while this one runs
  ⚠ promote-demo: replaces 1df6c3410060 (deployed 2026-11-12 16:36 UTC by ci on DESKTOP-GCT6E6V): it came from commit bc7b61dc0197, which this release's history doesn't have, so their change may be undone (`ductus history promote-demo`)
  the new engine (ductus 0.7.0-dev, in this process) is ready after 328 ms and receives beside this one: stopping this one
  this engine stopped in 1 ms and let go of its interfaces: the new one takes them
✓ handed over to a new engine in this process (ductus 0.7.0-dev): receiving never stopped, sending paused 33 ms
changed 1 interface:
  promote-demo 1df6c3410060 → 9242851858df
```

The audit log has a `release.rollback` row (`detail.from`, the release it left; `detail.undoes`),
and the deployment is marked as a rollback. **A second `--rollback` doesn't go back to what the
first one left**: it skips any release a rollback left and goes further back (here, to the first
release); to return to a release a rollback left, deploy it by its id
([nuance](../nuances/releases.md#the-default-rollback-skips-releases-a-rollback-left-level-2)).
A release that never ran in that environment is refused as a rollback ("deploy it").

**What a rollback brings back:** the whole project as that release has it, every interface's code
and settings, `ductus.yaml`, `resources.yaml`, `environments/`, and the encrypted secrets as they
were then (a rotated secret comes back old). **Not:** the engine's own settings (ductus.env), the
binary, or the database: migrations come with the binary, never with a release, so a rollback
runs none, and what the later release did stays done (messages it sent; deliveries it filtered
for a destination it removed)
([nuance](../nuances/releases.md#a-rollback-brings-back-the-whole-project-secrets-included-but-not-the-database-level-2),
[nuance](../nuances/releases.md#a-destination-a-rollback-brings-back-doesnt-get-what-waited-while-it-was-gone-level-3)).

### Changing lanes

A release that changes an interface's lanes (`ordering.partitions` up or down, or `ordering.key`),
a rollback past one that changed them included, deploys like any other, with messages waiting or
not: the new engine stores new messages in new lanes at once, the messages waiting in the old
lanes are sent first, and a patient's new messages wait behind their older ones
([nuance](../nuances/ordering.md#after-a-change-of-ordering-new-messages-of-a-key-wait-for-its-older-ones-level-4)).
Here prod is rolled back from 8 lanes to 2 while a sender sends and the receiver is down
(`scripts/demo-promote.sh`, step 11):

<<DEMO-OUTPUT>>

The gate's `validate` notes the change (`DUC311`), never refuses it. Once the receiver is back,
every message arrives once and each patient's in order, and the old lanes settle
(`interface.lanes_settled` in the audit log). Should the handover fail after the new engine
stored messages in its new lanes, they wait for the next start or handover
([nuance](../nuances/ordering.md#a-handover-that-fails-after-storing-messages-in-new-lanes-leaves-them-for-the-next-start-level-5)).

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

Stored on an engine, the 1,001-interface project (the engine running the next commit):

| | Time | The database grew by |
|---|---|---|
| `POST /releases`, the first release | 0.34 s | 1.3 MB |
| `POST /releases`, the next commit (a shared file changed under 101 interfaces) | 0.17 s | 131 KB |
| `POST /releases`, one stored before | 0.04 s | nothing |
| `POST /releases/{id}/check`, nothing changes / a tenth changes / `all_tests` | 5.9–6.1 s / 6.3–6.5 s / 9.8–10.1 s | a check's row |

The engine's intake on one interface (20,000 HL7 messages over 4 connections, the same machine
and Postgres): 799–809 messages a second alone, 641–655 over a run during which one `all_tests`
check ran; nothing refused or lost.

Deployed to an engine in release mode, the same 1,001-interface project (`ductus deploy`, timed
from the command to its answer):

| | Time | Sending paused |
|---|---|---|
| A release deployed, the gate run (4.6–4.7 s of it) | 5.7–6.0 s | 62–159 ms |
| A release deployed, a passing check reused | 0.95–1.0 s | 48–85 ms |
| The new engine unpacking a release from the database (5,023 files, 1.1 MB) | 0.18–0.24 s, before it's ready | none (the old engine still sends) |
| A restart with the release's folder there: finding the release | 9–27 ms | (a restart's) |
| An engine in release mode with nothing deployed, to `started` | 0.37 s | |

The gate is what a deploy waits for; check the release on the engine before the change window
and the deploy itself takes about a second.

Promoted and rolled back, the same project (0.7.4.4.3, `scripts/bench-release.sh`, three rounds):

| | Time | Sending paused |
|---|---|---|
| `GET /releases/{id}/file` (what `deploy --from` fetches; 142 KB) | 0.16–0.17 s | |
| The fetched file stored again, its records merged | 0.03–0.05 s | |
| A rollback, the gate run (4.7–4.9 s of it), the release's folder there | 5.8–6.0 s | 51–98 ms |
| A rollback, the release unpacked again | 6.2 s | 177 ms |
| Deployed forward again after it (the gate run) | 5.9–6.3 s | 93–184 ms |

A rollback costs what a deploy with its gate costs: the gate is most of it, and it always runs.

The handover itself at 1,001 interfaces (`IN_PROCESS=1 scripts/bench-handover.sh`, asked by
SIGHUP, a sender on 4 connections at 200 messages a second), in the engine's own process against
a new process (the package's way):

| | In the engine's process | A new process |
|---|---|---|
| The new engine ready | 586–709 ms | 682–786 ms |
| Sending paused | 508–627 ms | 540–642 ms |
| The sender | 0 refused, longest wait 142–168 ms | 0 refused, longest wait 141 ms |
| Memory | 95 MB before; 138–167 MB at the peak (both engines); 120–136 MB after ten handovers | 96 MB each, two processes while they overlap |

## Try it

`scripts/demo-promote.sh` runs the promotion and rollback above: three engines in release mode,
each with its own database (dev, test, prod; prod with `DUCTUS_DEPLOY_REQUIRES=test` and
`DUCTUS_DEPLOY_BRANCHES=main`), a release refused by prod then deployed to dev and test and
promoted with `--from`, a feature branch's release refused even forced, a colleague's work
refused then forced in test, two rollbacks, the release's file saying where it went, and the
lanes limit.

`scripts/demo-release.sh` does all of the above on a fresh `ductus init` project against your
database: a release, the same id from a CRLF checkout, a change not committed refused, the gate
without and with the database, nothing changed after a start, a commit's change tested, an edited
release refused, and nothing written; then, on an engine, `deploy` refused to a viewer, the
release stored, stored again (the same id), refused without the permission, checked by the engine,
and listed; then an engine in release mode: nothing deployed, the first release deployed (the
gate run), the next one checked and deployed (the check reused), and the first one again by
someone else refused.
