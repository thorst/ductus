# Upgrade

How to move a running Ductus to a new version: what the version numbers promise, how to find out
what the new version will do before you install it, the steps for each way of installing, what
happens to the database, and how to go back. For deploying a change to your own project (new
interface code, a new interface), the same tools apply: see
[Deploy a change](install/linux.md#deploy-a-change-upgrade).

The short version, for a server installed from the Linux package:

1. Unpack the new version's [portable archive](install/archive.md) anywhere on the server.
2. `./ductus upgrade --check --replay 1d --env-file /etc/ductus/ductus.env --root /var/lib/ductus/project`:
   it changes nothing and exits 1 when something would go wrong.
3. Install the new package. It hands the running engine over to the new version: receiving never
   stops.
4. Look: `systemctl status ductus`, then the console's Overview ("took over from …").

## Versions and what's supported

**What:** a version is `MAJOR.MINOR.PATCH` ([semver](https://semver.org)): `0.6.0`, `0.6.1`,
`1.2.0`. `ductus --version` prints it; the console's Overview, `GET /api/v1/engine` and
`ductus upgrade --check` show the version of every engine on the database.

**What each kind of version promises:**

| | Before 1.0 (now) | From 1.0 |
|---|---|---|
| Configuration keys, SDK functions, CLI options | May change in any minor version; the release notes say what | A minor may *deprecate* one: it keeps working, `ductus validate` warns and names the replacement, VS Code strikes it through. Only a major removes it |
| The database | Every version only adds (new tables and columns); older versions run on what newer ones added | The same in every minor. A major may also remove what an earlier minor stopped using |
| Upgrading across versions | From any version to any later one | Within a major, from any version to any later one. A major only from the latest minor of the one before (5.0 from 4.latest): the engine will refuse to skip, and `upgrade --check` will print the path (built with the first major that needs it) |
| Going back | To the version you came from | To the previous minor; never back across a major |

**Why:** a hospital upgrades rarely and needs to know an upgrade won't strand a project or a
database. Deprecating in minors and removing only at majors means a project that validates
without warnings on 4.9 starts on 5.0. A database that only grows within a major is what makes
going back a matter of installing the older program.

**What's supported today:** Ductus is pre-release. There are no released versions yet, only
development builds of the latest commit, and only that is supported
([SECURITY.md](https://github.com/thorst/ductus/blob/main/SECURITY.md)). Long-term-support
versions, for sites that stay on one version for a year or more, are planned once clustering
(v1.2) is stable; until then, follow the latest version.

**Several engines on one database** may run different versions while you upgrade them one at a
time: each runs its own interfaces, and the older ones keep working on what the newer one added.
Don't leave them on different versions for longer than the upgrade: a fix you upgraded for
isn't on the ones you haven't.

## Before every upgrade: check, then replay

### `ductus upgrade --check`

**What:** the *new* version's program reads the database and the project and says what its first
start would do. It changes nothing: not the database, not the project folder.

**Why:** otherwise the first place you learn that a database change works through a 50 GB table,
or that your tests fail under the new SDK, is the restart itself, in the change window.

**When:** before every upgrade, from the new version's [portable archive](install/archive.md#before-an-upgrade)
(no install needed), on the server or any machine that reaches the database, with the service's
settings. A read-only login is enough (`GRANT pg_read_all_data TO ductus_check`). Run it again
on the day: the database and the project may have changed since.

**Default:** it checks everything below; `--replay 1d` adds a replay of a day's traffic; `--json`
gives one document (for a change ticket). Exit 0: nothing in the way. Exit 1: something would
refuse the start or fail the project.

From a lab engine (`DESKTOP-GCT6E6V` is the machine it ran on):

```text
$ ./ductus upgrade --check
ductus upgrade --check: what version 0.6.0-dev would do here (nothing is changed)

The database
✓ migrations: nothing to apply

Engines on this database
✓ 18e39e5085ef2289-3ee on DESKTOP-GCT6E6V (pid 1006): version 0.6.0-dev, environment dev, since 2026-10-31 12:52 UTC, runs 1 interface(s)

The project (., environment dev)
✓ validate: valid
✓ test: 1 passed, 0 failed

Between versions
✓ required stops: none (versions before 1.0 upgrade to any later one)
✓ deprecated settings: none used

ready to upgrade to 0.6.0-dev (checked in 0.6 s)
```

What each section means, and what to do when it isn't ✓:

| Section | It says | When it isn't ✓ |
|---|---|---|
| The database | The changes (migrations) the first start applies, each with what it does; for one that works through an existing table, the table and its size now (⚠) | ⚠ heavy: plan the window by the table's size, and time it on a restored copy first ([below](#database-changes-migrations)). ✗ failed or edited: every start would refuse; don't upgrade, ask for help |
| Engines on this database | Every engine running on it: host, process, version, environment, how many interfaces | Engines you didn't expect: find out whose they are before you go on. Engines before 0.6.2 aren't recorded: look on each server |
| The project | `ductus validate` and `ductus test`, run with the new version's SDK and type checker | Fix the project (or the new version broke it: then don't upgrade yet). The full output is under the line |
| Between versions | Versions you must install on the way (none before 1.0), and settings the new version deprecates | Replace each deprecated setting with the one named; it still works until the next major |
| The last … replayed | With `--replay`: the period's traffic through the new version, compared ([below](#ductus-replay-what-it-does-to-real-traffic)) | Read the differences before going on |

A failing check, here a test the new version fails, ends with the count and exits 1:

```text
$ ./ductus upgrade --check --replay 1h
…
The project (., environment dev)
✓ validate: valid
✗ test: 0 passed, 1 failed
    adt-in
      ✗ sets the receiving application and keeps the patient
          expected "RECEIVER", got "LAB"
…
The last 1h replayed through 0.6.0-dev (nothing is sent)
✓ replay against the record and kept outputs: 300 message(s), 600 comparison(s), 600 identical, 0 different
    note: no installed version found (/usr/bin/ductus): outputs compared with kept outputs only; name a binary with --against

1 problem(s): fix them before upgrading (checked in 0.7 s)
```

On a database no version has run on yet, the first section says so instead: "a new database: the
first start creates every table (25 migrations)".

`--json` gives the same as one document: `ok` and `problems`, every check with its `section`,
`level` and `text`, and the details (`migrations.pending` with each one's tables, `engines`,
the project's `validate` and `test` output):

```text
$ ./ductus upgrade --check --json
{
    "version": "0.6.0-dev",
    "ok": true,
    "problems": 0,
    "seconds": 0.61,
    "checks": [
        {
            "section": "database",
            "level": "ok",
            "text": "migrations: nothing to apply",
            "more": []
        },
…
    "migrations": {
        "new_database": false,
        "pending": [],
        "newer": [],
        "edited": [],
        "failed": []
    },
…
```

### `ductus replay`: what it does to real traffic

**What:** `ductus replay --since 24h` runs a period's stored messages through the new version's
engine and your project's code, sends nothing and writes nothing, and compares what each
destination's code decides (sent and exactly what, filtered and why, quarantined and why) with
the installed version run beside it on the same messages, with what was recorded, and with kept
outputs. `ductus upgrade --check --replay 1d` runs it as part of the check.

**Why:** "what will change?" is the question a change board asks, and the usual evidence is weeks
of testing by hand on a test system. A replay of production's own traffic answers it in minutes,
message by message, field by field.

**When not:** it isn't a load test (one thread, one message at a time:
[measured](nuances/upgrades.md#what-a-replay-costs-and-where-level-3)), and it can't compare code
that writes (`db().execute`, a `POST`): those messages are reported as not comparable
([why](nuances/upgrades.md#a-replay-doesnt-write-those-messages-cant-be-compared-level-3)).

**What it compares with, by default:** the version installed on the server (`/usr/bin/ductus`,
`C:\Program Files\Ductus\ductus.exe`), run beside it; that needs the installed one to be 0.6.6.4
or later ([why](nuances/upgrades.md#a-replay-compares-outputs-with-the-installed-version-from-0664-on-level-2)).
**Without an installed version, outputs are compared only where they were kept**
(`save_output: true`): every other destination is compared by its decision alone (sent, filtered,
quarantined), so "identical" there means the same decision, not the same bytes. The report says
so on its `note:` line. On a machine without the installed version, name it: `--against
/path/to/old/ductus`.

A difference the installed version shares with the new one isn't the upgrade's: the data it looks
up changed since the message was sent. The report calls it **drift** and doesn't fail for it
([nuance](nuances/upgrades.md#a-difference-from-the-record-can-be-the-data-not-the-code-drift-level-3)).

For a change board, `--report replay.html` writes a page with the versions, the period, the counts
and a SHA-256 checksum. Examples in it quote field values from real messages: keep it as you keep
patient data, or make it with `--hide-values`
([nuance](nuances/upgrades.md#a-replays-report-holds-patient-data-unless-you-hide-it-level-2)).
The same tool compares a *code* change with `--baseline <git revision>`:

```text
$ ductus replay --since 1h --baseline HEAD --report replay.html
adt-in: 300 message(s) replayed in 0.1 s
ductus replay: version 0.6.0-dev against the project at HEAD, 2026-10-31 11:52 to 2026-10-31 12:52 UTC, environment dev (nothing was sent)

adt-in: 300 message(s) in 0.1 s
  ✗ archive: 300 compared, 300 different
      vs the project at HEAD: MSH-5 differs in 300 message(s) (e.g. message 1: "RECEIVER" → "LAB")
  ✗ lab: 300 compared, 300 different
      vs the project at HEAD: MSH-5 differs in 300 message(s) (e.g. message 1: "RECEIVER" → "LAB")

300 message(s), 600 comparison(s): 0 identical, 600 different, 0 drift, 0 not comparable, 0 waiting (0.2 s)
report: …/replay.html (sha256:36f4f1782108ad30cd539435c598924e090fe5b6806e377c3dfe014c28532a99)
```

More on replay, with what the clock and `Math.random` do in it:
[Replay a period before a change](operate/resend.md#replay-a-period-before-a-change).

## Upgrading, by how you installed

### Linux packages: the handover

```sh
sudo apt install ./ductus_0.6.1_amd64.deb          # Ubuntu, Debian
sudo dnf install ./ductus-0.6.1-1.x86_64.rpm       # RHEL, Rocky, Alma
```

**What happens:** the package replaces the program and runs `systemctl reload ductus`. The new
version starts *beside* the running engine, on the same listening sockets: it reads the project,
applies the database changes and loads every interface's code while the old one carries on. Once
it's ready, the old engine stops cleanly, lets go of its interfaces, and the new one takes them
and sends. **Receiving never stops** and sending pauses for a moment (measured under real
systemd, 4 senders at 200 messages/s: 0 refused, longest wait for an ACK 0.17–0.22 s, nothing
lost: [nuance](nuances/upgrades.md#systemctl-reload-hands-over-receiving-never-stops-sending-pauses-briefly-level-2)).

The handover's own lines, from an engine handing over to the program now on disk (`ductus
handover` is what `systemctl reload` runs, with the service's control socket; these are the lines
`journalctl -u ductus` shows):

```text
$ ductus handover --control …/control
started …/ductus (pid 1312): it reads the project, migrates the database and loads every interface's code while this one runs
ductus 0.6.0-dev (pid 1312) is ready after 104 ms and receives beside this one: stopping this one
this engine stopped in 1 ms and let go of its interfaces: the new one takes them
handed over to ductus 0.6.0-dev (pid 1312): receiving never stopped, sending paused 41 ms
```

- **A new version that can't start** (a broken project, a failed database change, another engine
  owning an interface) leaves the old one serving: the install says so, `journalctl -u ductus`
  says why, and senders notice nothing. Fix what it names and `sudo systemctl reload ductus`
  ([nuance](nuances/upgrades.md#a-new-version-or-project-that-cant-start-leaves-the-running-one-serving-level-2)).
- **A start longer than 5 minutes** (a large database change) is given up by the handover:
  `sudo ductus handover --timeout 30m` instead of the reload.
- **For a few seconds two engines run**, so the server needs memory for both.
- **From a development build before 0.6.6.3** (no released version is), the package restarts
  instead of handing over; from one before 0.6.6.2 that restart refuses connections for a moment
  ([nuance](nuances/upgrades.md#on-linux-a-restart-refuses-no-connection-senders-wait-for-it-level-2)).
- **A drain refuses a handover**: resume first, or stop and start.

### Windows installer

```powershell
msiexec /i ductus-0.6.1-x64.msi /qn          # as an administrator; no need to uninstall first
```

**What happens:** the installer stops the service cleanly, replaces the program and starts it
again; `C:\ProgramData\Ductus` (settings, project, logs) is never touched. The engine is down for
a few seconds (an MSI upgrade measured **3.1–3.5 s**: a few connections refused or reset,
nothing lost), so choose a quiet moment
([nuance](nuances/upgrades.md#windows-and-containers-a-restart-is-a-short-gap-level-2)). A
handover like Linux's is planned for v1.0. Details (a service stopped on purpose, a password
account, Restart Manager): [Windows installer](install/windows.md#upgrade).

### The portable archive

Replace the program and hand over, as the package would:

```sh
sudo install -m 0755 ductus-0.6.1/ductus /usr/bin/ductus
sudo systemctl reload ductus
```

Without the package's service (`ductus run` under your own supervisor), stop and start it with
the new program; the senders see a short gap. To hand over there too, start the engine with
`DUCTUS_CONTROL=/some/dir/control` and run `ductus handover --control /some/dir/control` after
replacing the program ([nuance](nuances/upgrades.md#deploying-project-changes-systemctl-reload-level-1)).

### Docker

Build the new version's image (`docker build -t ductus:dev .` in the new version's source, as in
[Docker](install/docker.md)), then `docker compose -f packaging/docker/compose.yaml up -d`:
Compose sees the new image and replaces the container. Its
ports close while the new one starts, so senders are refused for a moment. Docker is for trying
and developing until v1.0.

### `ductus dev` on a laptop

Replace the program and start `ductus dev` again. Its Postgres (in `.ductus/postgres`) is kept and
updated by the new version like any other database; `ductus dev --reset` starts from an empty one.

## Database changes (migrations)

**What:** a new version may change the database: a new table, a new column, a new index. Each
change is a numbered *migration*, applied once, in order, by the first start of the version that
carries it, and recorded in the database (`_sqlx_migrations`). `ductus upgrade --check` lists the
ones still to apply, each with a sentence saying what it does, for example 0025: "keeps the names
accounts had before, and the directory's id for each directory account".

**They only add.** Within a major version, a migration never renames or removes anything the
version before uses: the old program keeps working on the new tables. That's what makes going
back safe, and what lets several engines on one database be upgraded one at a time. A store test,
on every push, starts the engine on a database that has a migration it doesn't know, as an older
program meets a newer one's database.

**Heavy ones.** Most take milliseconds. One that works through an existing table row by row (an
index built, a column filled) is marked ⚠ with the table and its size now: while it runs, the
start waits, and storing or sending waits as it says
([nuance](nuances/upgrades.md#a-migration-marked-heavy-holds-up-a-table-while-it-runs-level-2)).
Under the Linux package's service senders wait in the queue rather than being refused, for as
long as it takes. Plan the window by the size shown, and time it on a restored backup first.
There have been none since v0.5 (0011, the queue index, was the last).

**When one fails** part-way, the start refuses, and so does every later start, until it's
repaired, and `upgrade --check` shows it ✗ with its number. Don't edit
`_sqlx_migrations` by hand: [ask for help](https://github.com/thorst/ductus/issues). The version
before still runs on the database as it is: going back keeps you running meanwhile.

**Backups are the DBA's.** The engine needs no backup of its own before an upgrade, but your
usual point-in-time recovery (pgBackRest, WAL archiving) is what undoes anything at all
([PostgreSQL](install/postgres.md#backups-are-the-dbas)).

## Going back (rollback)

**What:** installing the previous version again. The database stays as the new version left it;
the previous version runs on it, ignoring what it doesn't know.

**When:** the new version misbehaves in a way `upgrade --check` and `replay` didn't show. Decide
by what you see: a project problem is fixed in the project and deployed (`systemctl reload`); a
problem in Ductus itself is a rollback, and an [issue](https://github.com/thorst/ductus/issues).

| Installed with | Going back |
|---|---|
| Linux package | `sudo apt install --allow-downgrades ./ductus_0.6.0_amd64.deb`, or `sudo dnf downgrade ./ductus-0.6.0-1.x86_64.rpm`: the package hands over to the older program as an upgrade does (when the older one is 0.6.6.3 or later; before that, it restarts) |
| Windows installer | The installer refuses to install an older version over a newer one ("A newer version of Ductus is already installed. Uninstall it first to go back."). Uninstall (`msiexec /x`), then install the older MSI: `C:\ProgramData\Ductus` and the database stay. Give `SERVICEACCOUNT=` again if the service ran as another account (a gMSA): the install sets the account afresh |
| Portable archive | Put the older program back and `sudo systemctl reload ductus` |
| Docker | Run the older image |

**What going back doesn't undo:** whatever the new version wrote stays in the database: tables
and columns the older one doesn't read. Messages received meanwhile are kept and sent as usual;
features only the new version had stop until you upgrade again. Going back across a major version
isn't possible (from 1.0 on: the major may have removed what the older one needs).

Package downgrades aren't part of the end-to-end demos yet (the database side is: an older
program on a newer database is tested on every push); the v0.6 demo adds them.

## After the upgrade: confirm

- `systemctl status ductus` shows "running N interfaces" (Windows: `Get-Service Ductus` is
  Running, and the Event Log has the start, ID 1000).
- The console's Overview says the version, and "took over from v…" after a handover; the audit log
  has an `engine.handover` entry ([audit actions](reference/audit.md)).
- `ductus status --token …` shows every interface ok within a minute; `ductus doctor` checks every
  connection from the server.
- If you kept the replay's report, file it with the change: it's the record of what the upgrade
  was expected to change.
