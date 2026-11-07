# What runs, recorded

Every time an engine starts or hands over (`systemctl reload`), it records what each interface
runs, from which git commit, who started it and why. This page is for whoever deploys and
whoever has to answer "what changed, and when?": which code a feed ran last Tuesday, who
deployed the change that broke it, whether what's running is what's in git.

Nothing to set up: a start records it on its own, from 0.7.3. It works with the way most sites
deploy today (`git pull` on the server, then `systemctl reload ductus`) and needs no release
process. What's recorded is in the database (the `deployments` table and its neighbours,
[schema](../schema.md#deployments)), on the Overview, on each interface's page and in the API.

## What's recorded

At every start and every handover, once the engine holds its interfaces:

| | What | Example |
|---|---|---|
| **Each interface's code version** | A hash of what it runs: its `interface.yaml` and every file its code imports ([below](#the-code-version)) | `adt-in 36792608ed03` |
| **What changed** | The interfaces whose version is new for them, with what they ran before | `adt-in 36792608ed03 → ab2dae752eb7` |
| **Who** | The account that started it, or the person who asked for the handover ([below](#who-and-why)) | `todd` |
| **Why** | What the person said (`-m`), when they said | `lab wants MSH-5 set` |
| **How** | `start` (someone ran it), `service` (systemd or the Windows service started it), `handover` | `handover` |
| **The git commit** | The project's HEAD, and the files the interfaces run that aren't that commit's | `commit 71be1444e8bc, 1 file not committed` |
| **The engine** | Its version, environment, host and run (as the audit log names it) | `0.7.0`, `prod`, `ductus-01` |

A start that changes nothing is recorded too, with `nothing changed`: restarts show beside
deploys. A record that can't be written is an error in the log, never a reason not to start
([nuance](../nuances/deployments.md#a-start-whose-deployment-cant-be-recorded-runs-anyway-level-3)). The files themselves are kept, once by content, so the code that ran can be read later
without git ([nuance](../nuances/deployments.md#interface-code-is-kept-in-the-database-level-2)).

Each record is a line in the engine's log right after its start line. A project of one interface
(`ductus init`'s) in git, started, then its `transform.ts` edited and handed over with a reason,
then committed and handed over again:

```text
INFO ductus_runtime: started interfaces=1 mllp=1 http=0 files=0 schedules=0 internal=0 shared=1 pool_threads=8 dedicated_threads=0 debugged=0 console=debug took=705.77886ms
INFO ductus_runtime::deployment: deployed: adt-in 36792608ed03 (new here) deployment=1 changed=1 interfaces=1 by=todd how="start" git=commit 71be1444e8bc took=10.066327ms
INFO ductus_runtime::deployment: deployed: adt-in 36792608ed03 → ab2dae752eb7 deployment=2 changed=1 interfaces=1 by=todd how="handover" reason="lab wants MSH-5 set" git=commit 71be1444e8bc, 1 file not committed: interfaces/adt-in/transform.ts took=19.695925ms
INFO ductus_runtime::deployment: deployed: nothing changed deployment=3 changed=0 interfaces=1 by=todd how="handover" git=commit 0e6f469b6e3d took=6.975417ms
```

The second deploy ran a file nobody had committed yet; once it was committed, the next handover
recorded the new commit with nothing uncommitted, and **the same version**: committing changes
nothing that runs. With many interfaces the line names the first ten changed and counts the rest.

## The code version

An interface's code version is a SHA-256 hash, shown as its first 12 hex characters
(`36792608ed03`), over:

- its `interface.yaml` as written (before `${...}` is filled in, so one version runs in every
  environment), and
- every project file its code imports: its `script:`, the files that imports, and so on through
  shared files (`lib/codes.ts`), including files imported only for their types and an
  `import("./x.ts")` that hasn't run yet (not one whose name is computed:
  [nuance](../nuances/deployments.md#import-with-a-computed-name-isnt-counted-level-4)).

Not counted: its tests and fixtures, a README, files nothing imports, the SDK (the engine's own
version is recorded beside every deployment; an upgrade then shows as an upgrade, not as every
interface changing), and environment values and secrets. Each file counts without a byte order
mark and with `\n` line ends, so a Windows checkout and a Linux one of the same commit give the
same versions
([nuance](../nuances/deployments.md#line-ends-and-a-byte-order-mark-dont-change-a-version-level-2)).

The engine reads exactly what it loads: the module loader hashes each file as it reads it for
the code to run, so the version can't disagree with the code that ran, even if a `git pull`
lands during a start.

**Why a computed version, not a number someone types:** a typed version drifts (someone forgets
to raise it, two changes share one); a hash changes exactly when what runs changes, and two
servers running the same files show the same version. **When it surprises:** a change to a
shared file changes the version of every interface that imports it
([nuance](../nuances/deployments.md#the-code-version-counts-what-the-code-imports-not-the-whole-folder-level-2)).

To check a version by hand, the hash is over lines `sha256sum` prints, in path order:

```text
ductus code version 1
interface adt-in
a62ffecddebf490aec72a1e6d737d9daa4f9977e4c211a90df77c384d32c6524  interfaces/adt-in/interface.yaml
3cfd63e477131380fcf86b5344be7424eb75b40e8482a442a0c4de3535d60f98  interfaces/adt-in/transform.ts
```

`code_versions.files` lists each version's files and their hashes; `code_files` holds the texts
([schema](../schema.md#code_versions)).

## Who and why

| Started by | Recorded as | Reason |
|---|---|---|
| `ductus run` in a terminal, or `ductus dev` | the account running it, `how` = `start` | none |
| systemd, or the Windows service (a start, a restart, a crash's restart) | the service's account (`ductus`), `how` = `service` | none |
| `systemctl reload ductus` | `systemd (systemctl reload)`, `how` = `handover` | none: systemd passes nothing on |
| **`sudo ductus handover -m "why"`** | **the person behind `sudo`**, `how` = `handover` | **what `-m` says** |

So on Linux, **deploy with `ductus handover` when you want the record to say who and why**
([nuance](../nuances/deployments.md#systemctl-reload-cant-say-who-deployed-level-1)). It's
the same handover `systemctl reload` runs (the new engine starts beside the old one on the same
sockets; nothing is refused, [high availability](high-availability.md#deploying-and-upgrading-without-a-gap)):

After a `git pull` on the server, `sudo ductus handover -m "lab wants MSH-5 set"`. From a lab
engine (a development build, hence its path; run as the engine's own account, so without `sudo`):

```text
$ ductus handover -m "lab wants MSH-5 set"
started /home/todd/target/release/ductus (pid 5470): it reads the project, migrates the database and loads every interface's code while this one runs
ductus 0.7.0-dev (pid 5470) is ready after 372 ms and receives beside this one: stopping this one
this engine stopped in 1 ms and let go of its interfaces: the new one takes them
handed over to ductus 0.7.0-dev (pid 5470): receiving never stopped, sending paused 31 ms
```

`sudo` is needed because only the engine's account and root may ask it for a handover; the
record names the person `sudo` ran for. [`ductus handover`](../reference/cli/handover.md) has the
options.

`-m` is optional; nothing refuses a deploy without one. Releases (0.7.4) will require a reason
outside dev. On Windows there's no handover: a restart is recorded as the service's start, with
no reason, until deploys through the API (0.7.4).

## Git

When the project folder is in a git repository (or one of the folders above it is), each
deployment records HEAD's commit and **the files the interfaces run that aren't that commit's**:
edited since it, or never committed. That is what "is this the code in git?" needs; files nothing
runs don't count, and neither do line ends
([nuance](../nuances/deployments.md#not-committed-means-the-files-differ-from-head-not-git-status-level-2)).
A deployment that ran uncommitted files says so in its log line and in `uncommitted`.

Ductus reads `.git` itself: no `git` needs to be installed on the server, and the engine's account
only needs to read the folder. Not in a repository: no commit is recorded, and nothing else
changes.

## Where to see it

- **The Overview**, under the title: when this engine was deployed, by whom, how many interfaces
  changed, why, its commit and how many files weren't committed.
- **An interface's page**: its **Code version**, and the deployment it came with (an earlier one
  when this start changed nothing for it).
- **The API**: [`GET /engine`](../reference/api/engine.md) `deployment` (this engine's), and
  `GET /interfaces/{name}` `code` (`version`, the full `hash`, and `since`).
- **SQL**, for reports: the last ten deployments of prod and what each changed:

  ```sql
  SELECT d.id, d.at, d.by_name, d.reason, d.changed, c.interface
  FROM deployments d LEFT JOIN deployment_changes c ON c.deployment_id = d.id
  WHERE d.environment = 'prod'
  ORDER BY d.id DESC LIMIT 10;
  ```

- **A timeline per interface**: `ductus history`, the interface's **History** tab and
  `GET /interfaces/{name}/history` ([below](#history-who-changed-what-and-why)).

## History: who changed what and why

From 0.7.3.3, one timeline per interface, newest first, from four sources:

| Source | What it adds | Example |
|---|---|---|
| **git** | Each commit that changed one of its files: its `interface.yaml`, its code and the shared files it imports, every version's ([nuance](../nuances/deployments.md#history-lists-the-commits-on-the-files-an-interfaces-versions-named-level-3)) | `committed 3c6d0d4dd062: adt-in: PV1-3 for the wards (interfaces/adt-in/transform.ts (modified))` |
| **Deployments** | Each start or handover that changed its code: from which version to which, by whom, why, from which commit, and a ⚠ when it may have undone someone else's change ([below](#two-people-one-server)) | `deployed 36792608ed03 → e5b6cc1310d6 by a handover: lab wants MSH-5 set (commit 8225dd3b602b)` |
| **The audit log** | What people (and the engine) did to it: pauses, resends, corrections, dismissals, retention changes, alerts acknowledged or silenced | `paused adt-in/lab: vendor maintenance` |
| **Its documentation** | Each change of its documentation fields (`about:`, `custom:`, its destinations' `custom:`), with the deployment that brought it | `documentation changed: custom.baa_signed: no → yes` |

**Why one timeline:** "it broke Tuesday afternoon" needs the commit, the deploy that ran it, and
what people did around it, side by side; each lives somewhere else (git, the engine's records, the
audit log), and reading them apart is how a cause gets missed.

### From the command line

`ductus history <interface>` reads the project folder for git, and the database (read-only, a
read-only login is enough) when `DUCTUS_DATABASE_URL` is set or `--database-url` is given. The
same lab as above, extended: Ann deploys a second change (`lab wants MSH-5 set`); Bob, whose
checkout doesn't have Ann's last commit, commits his own change and hands over:

```text
$ ductus history adt-in
adt-in
2026-11-06 01:46  deployed  bob  deployed e5b6cc1310d6 → 11de331fea89 by a handover: wards need PV1-3 (commit 3c6d0d4dd062) ⚠
                  ⚠ replaces e5b6cc1310d6 (deployed 2026-11-06 01:46 UTC by todd on DESKTOP-GCT6E6V): it came from commit 8225dd3b602b, which this project's history doesn't have, so their change may be undone (`ductus history adt-in`)
2026-11-06 01:46  commit    Bob Ray  committed 3c6d0d4dd062: adt-in: PV1-3 for the wards (interfaces/adt-in/transform.ts (modified))
2026-11-06 01:46  deployed  todd  deployed 36792608ed03 → e5b6cc1310d6 by a handover: lab wants MSH-5 set (commit 8225dd3b602b)
2026-11-06 01:46  deployed  todd  deployed 36792608ed03 (new here) (commit f07506d047cd)
2026-11-06 01:46  commit    Ann Lee  committed f07506d047cd: adt-in: relay ADT to the lab (interfaces/adt-in/interface.yaml (added), interfaces/adt-in/transform.ts (added))
```

Ann's "MSH-5 for the lab" commit isn't listed: it isn't in this checkout's history any more,
which is exactly what Bob's deploy undid. Her deployment of it is, with its commit.

| Option | What it does | Default |
|---|---|---|
| `--destination NAME` | Only what touches that destination ([below](#one-destination-or-the-source)) | the whole interface |
| `--source` | Only what touches its source | the whole interface |
| `--all` | Also the starts that ran it unchanged, payload reads and refused attempts ([nuance](../nuances/deployments.md#history-shows-unchanged-starts-reads-and-refusals-only-when-asked-for-everything-level-3)) | changes only |
| `--limit N` | How many entries | 50 |
| `--before KEY` | What's older than an entry: the last line of a full page prints the command | the newest |
| `--json` | One document: each entry's time, kind, who, what, and its details (the commit and its files, the deployment, the audit row, the documentation fields changed) | text |

Without a database it shows git's commits only, of the files the project runs now, and says so
(`note: no database read`). An interface no longer in the project is found by name in what was
recorded. [`ductus history`](../reference/cli/history.md) has every option.

### One destination, or the source

From 0.7.4.2 every entry says which parts of the interface it touches: its **source** (where
messages come from), which **destinations**, or neither (the interface as a whole: its owner, its
retention). The text ends each line with them; `--destination NAME` and `--source` show only one
part's. **Why:** the lab's analyst cares about what changed for the lab connection; billing's
port moving, or a change to the code only billing runs, is noise to them.

An interface whose `lab` and `billing` each have their own file (`lab.ts`, `billing.ts`), both
importing `lib/codes.ts`, and an `archive` without code. Ann changed the lab's code, Bob billing's
port, Ann the shared ward codes and the source's port, and someone paused the lab:

```text
$ ductus history adt-in
adt-in
2026-11-07 00:22  audit     docs-lab  paused adt-in/lab: lab system upgrade tonight  [lab]
2026-11-07 00:22  deployed  ann  deployed 5c44068cbf69 → 5bf5ee94dc38 by a handover: trimmed wards, new port (commit bff2bf5f7232)  [source, billing, lab]
2026-11-07 00:22  deployed  bob  deployed afa33226b3f7 → 5c44068cbf69 by a handover: billing's new server (commit 5ddd68044d58)  [billing]
2026-11-07 00:22  deployed  ann  deployed af7f053e2669 → afa33226b3f7 by a handover: lab wants the ward twice (commit 9dd295511ce8)  [lab]
2026-11-07 00:22  deployed  todd  deployed af7f053e2669 (new here) (commit c02c6689bc2b)
2026-11-06 22:27  commit    Ann Lee  committed bff2bf5f7232: adt-in: listen on 16662 (interfaces/adt-in/interface.yaml (modified))  [source]
2026-11-06 22:26  commit    Ann Lee  committed 898f2a90f36d: codes: trim ward codes (lib/codes.ts (modified))  [billing, lab]
2026-11-06 22:25  commit    Bob Ray  committed 5ddd68044d58: adt-in: billing moved to port 17012 (interfaces/adt-in/interface.yaml (modified))  [billing]
2026-11-06 22:24  commit    Ann Lee  committed 9dd295511ce8: adt-in: the ward in PV1-3.2 for the lab (interfaces/adt-in/lab.ts (modified))  [lab]
2026-11-06 22:23  commit    Ann Lee  committed c02c6689bc2b: adt-in: ADT to the lab and billing (interfaces/adt-in/billing.ts (added), interfaces/adt-in/interface.yaml (added), interfaces/adt-in/lab.ts (added), lib/codes.ts (added))  [every part]

$ ductus history adt-in --destination lab
adt-in, destination `lab`
2026-11-07 00:22  audit     docs-lab  paused adt-in/lab: lab system upgrade tonight
2026-11-07 00:22  deployed  ann  deployed 5c44068cbf69 → 5bf5ee94dc38 by a handover: trimmed wards, new port (commit bff2bf5f7232)
2026-11-07 00:22  deployed  ann  deployed af7f053e2669 → afa33226b3f7 by a handover: lab wants the ward twice (commit 9dd295511ce8)
2026-11-07 00:22  deployed  todd  deployed af7f053e2669 (new here) (commit c02c6689bc2b)
2026-11-06 22:26  commit    Ann Lee  committed 898f2a90f36d: codes: trim ward codes (lib/codes.ts (modified))
2026-11-06 22:24  commit    Ann Lee  committed 9dd295511ce8: adt-in: the ward in PV1-3.2 for the lab (interfaces/adt-in/lab.ts (modified))
2026-11-06 22:23  commit    Ann Lee  committed c02c6689bc2b: adt-in: ADT to the lab and billing (interfaces/adt-in/billing.ts (added), interfaces/adt-in/interface.yaml (added), interfaces/adt-in/lab.ts (added), lib/codes.ts (added))

$ ductus history adt-in --source
adt-in, the source
2026-11-07 00:22  deployed  ann  deployed 5c44068cbf69 → 5bf5ee94dc38 by a handover: trimmed wards, new port (commit bff2bf5f7232)
2026-11-07 00:22  deployed  todd  deployed af7f053e2669 (new here) (commit c02c6689bc2b)
2026-11-06 22:27  commit    Ann Lee  committed bff2bf5f7232: adt-in: listen on 16662 (interfaces/adt-in/interface.yaml (modified))
2026-11-06 22:23  commit    Ann Lee  committed c02c6689bc2b: adt-in: ADT to the lab and billing (interfaces/adt-in/billing.ts (added), interfaces/adt-in/interface.yaml (added), interfaces/adt-in/lab.ts (added), lib/codes.ts (added))

$ ductus history adt-in --destination labs
Error: `adt-in` has no destination `labs`: it has `archive`, `billing`, `lab`
```

The shared file's commit is both destinations'; Bob's port change and the source's port aren't
the lab's. A destination it no longer has (removed, renamed, or `default` from the singular
`destination:` before 0.7.4) can still be asked for: it's known from what the engine ran.

How each kind of entry is told apart:

| Entry | Tells its parts by | Example |
|---|---|---|
| A commit or deployment changing **interface.yaml** | Its blocks compared, as parsed (comments and spacing don't count): `destinations.<name>` is that destination's; `source`, `format`, `ordering`, `dedupe`, `durability`, `about.from` and `monitor`'s times the source's; `script`, `output`, `retry`, `rate_limit`, `monitor.behind` each destination's that has none of its own; `timeout`, `isolation`, `workers`, `dedicated_pools`, `about.to` every destination's; the owner, description, `custom`, `retention`, `quarantined`, `alert` the whole interface's only; `name` or a block it doesn't know every part's | Bob's `billing: { … port: 17012 }` → billing |
| A commit or deployment changing **code** | The files each destination's code loads: its `script:` (or the interface's) and everything that imports. A file several destinations load (`lib/`, the interface's script) is each one's ([nuance](../nuances/deployments.md#history-by-destination-counts-a-shared-file-as-each-destinations-that-loads-it-level-3)); a file none loads is the whole interface's. An HTTP `respond` interface's code is its source's | `lib/codes.ts` → billing, lab |
| **The audit log** | A pause, a resume or a destination's alert by its subject; a resend, correction or dismissal by the destinations it acted on; raised lanes (`ordering`) are the source's; retention, payload reads, an interface's own alerts are the whole interface's | `paused adt-in/lab` → lab |
| **Documentation** | `destinations.<name>.…` fields are that destination's (and only those are shown); the description, owner and `custom:` the whole interface's | `destinations.lab.custom.contact` → lab |
| The interface's first deployment, a start that ran it unchanged (`--all`) | Every part | `(new here)` |

**Recorded before 0.7.4.2:** a code version saved by an earlier engine doesn't say which files
each destination's code loads. A change of its code is then shown under every part, marked
``[may not be destination `lab`: which part its code is wasn't recorded before 0.7.4.2]``, and a
note says so; it's never guessed. interface.yaml changes are told exactly either way (both texts
are kept). The first start after the upgrade records the running version's files, so every change
from then on is exact
([nuance](../nuances/deployments.md#history-before-0742-cant-say-which-destination-a-code-change-was-level-2)).

### In the web UI and the API

- **The History tab** on an interface's page: the same timeline, a page of 50 at a time
  ("Older"), each commit's files and each documentation change's fields under it, the ⚠ in a
  box; "Everything" is `--all`. **Show** picks the whole interface, its source or one
  destination (those it has and had); each entry says what it touches ("Touches: billing, lab").
- **[`GET /interfaces/{name}/history`](../reference/api/engine.md)** (`before`, `limit` up to 200,
  `all`, `source=true` or `destination=NAME`): a page of `entries` (each with `touches`:
  `everything`, `source`, `destinations`, `unsure`), `next_before`, and `destinations` (the names
  to pick from). An unknown destination is a 400 naming those it has. Any signed-in person or
  token may read it; with `all=true`, payload reads and refused attempts are added only for those
  with the `audit` permission, as the audit log is theirs.

### Two people, one server

Folder mode (the code in a git checkout on the server, deployed with `git pull` and a handover)
has one trap: the last deploy wins. If Bob deploys from a checkout that doesn't have Ann's latest
commit, Ann's change stops running, and nothing said so. From 0.7.3.3 a start or handover warns
when an interface it changes runs a version **someone else** deployed **from a commit this
project's history doesn't have, or from files never committed**:

```text
$ sudo ductus handover -m "wards need PV1-3"
started /home/todd/target/release/ductus (pid 1208): it reads the project, migrates the database and loads every interface's code while this one runs
⚠ adt-in: replaces e5b6cc1310d6 (deployed 2026-11-06 01:46 UTC by todd on DESKTOP-GCT6E6V): it came from commit 8225dd3b602b, which this project's history doesn't have, so their change may be undone (`ductus history adt-in`)
ductus 0.7.0-dev (pid 1208) is ready after 291 ms and receives beside this one: stopping this one
this engine stopped in 1 ms and let go of its interfaces: the new one takes them
handed over to ductus 0.7.0-dev (pid 1208): receiving never stopped, sending paused 20 ms
```

The same text is a WARN line in the engine's log (`deployed over someone else's change`), and is
kept with the deployment (`deployment_changes.warning`), so `ductus history` and the History tab
show it from then on. It **warns and deploys all the same**: refusing needs releases (0.7.4),
which know what each deploy replaces
([nuance](../nuances/deployments.md#two-people-deploying-from-their-own-checkouts-get-a-warning-not-a-refusal-level-2)).
When you see it, read the history, get their commit into your checkout (or theirs pushed), and
deploy again.

It's worked out before the new engine says it's ready, while the old one still sends: reading
what each changed interface ran, and HEAD's history once (about 40 ms more before the new engine
is ready, at 1,001 interfaces and 1,000 commits), so the handover's sending pause doesn't grow
([what it costs](#what-it-costs)).

### The documentation's history

The company's documentation fields ([documentation fields](../configure/documentation-fields.md)) are kept as a
snapshot for SQL reports (`docs_snapshot`); from 0.7.3.3 each change of an entry is also a row of
`docs_history`, with the deployment that brought it ([schema](../schema.md#docs_history)). The
upgrade copies the snapshot as it is then, so each entry's history starts at the upgrade. The
snapshot is now saved with the deployment, just after the start
([nuance](../nuances/deployments.md#the-documentation-snapshot-is-saved-just-after-a-start-with-its-deployment-level-3)).
For a report of who changed a field when:

```sql
SELECT h.at, d.by_name, d.reason, h.docs->'custom'->>'baa_signed' AS baa_signed
FROM docs_history h LEFT JOIN deployments d ON d.id = h.deployment_id
WHERE h.kind = 'interface' AND h.name = 'adt-in'
ORDER BY h.id;
```

**Upgrading to 0.7.3.3** (migration 0028) adds the tables and columns above and four indexes,
one of them on the audit log (built once: what the audit log records waits meanwhile, a few
seconds per million rows; messages don't), and copies the documentation snapshot. Nothing waits
on `messages` or `deliveries`. `ductus upgrade --check` shows the tables' sizes first.

## Which code decided each delivery

From 0.7.3.2, every delivery records the code version that decided it: when a destination's
worker saves a message as sent, filtered or quarantined, it saves the version of the code that
made that decision with it. So "which code sent this?" has an answer per message and destination,
however many deploys happened since, and "deployed 14:02 by todd: fix PV1-3" comes with it.

| The delivery | Its code version |
|---|---|
| sent, filtered (`return null`, `kill()`), quarantined by the code (`quarantine()`, a thrown error, a runaway) | the version that ran |
| quarantined because the receiver refused it (AR, an HTTP 400, gave up after its retries) | the version that ran: it decided to send, and that's what was refused |
| dropped over its rate limit | the version running |
| pending (not decided yet, or sent again) | none |
| sent again (Resend, Replay) | none until it's decided again; the resend's row in its history keeps the version before |
| dismissed, or replaced by an edited copy | none: a person decided; the history keeps the version that decided it before |
| filtered because its destination was removed from interface.yaml | none: no code ran |
| decided by an engine before 0.7.3.2 | none |

([nuance](../nuances/deployments.md#a-delivery-names-the-code-that-decided-it-not-the-code-that-sent-it-again-level-2))

**Where to see it:**

- **A message's page**: the Destinations table's **Decided by** column, the version (its full
  hash when you point at it) and the deployment that brought it to the interface: when, by whom,
  why. A resend, dismissal or edit in its **History** says which version had decided the delivery
  before.
- **The API**: [`GET /messages/{id}`](../reference/api/messages.md) gives each delivery a
  `decided_by` (`version`, `hash`, `deployed` with `at`, `by`, `reason`), and each action a
  `previous_code_version`.
- **`ductus try --message ID`** names the version it tries on its first line, and under a
  destination whose record another version decided, says so: `decided by code version
  07cd7ba85180, not this one (ae5123af1d2c)` ([one message](../write/debugging.md#one-message-ductus-try)).
- **`ductus replay`** says per interface how many of the records it compares other code decided,
  and which versions: `note: 8 record(s) were decided by other code than this (…): a3f9c2e81b07
  (8); a difference from the record may be that change`. In `--json` that's each interface's
  `code_version` and `records_by_other_code`.
- **SQL**, for reports: what decided an interface's last day of deliveries, version by version:

  ```sql
  SELECT encode(substr(v.hash, 1, 6), 'hex') AS version, count(*)
  FROM deliveries d
  JOIN destinations t ON t.id = d.destination_id
  LEFT JOIN code_versions v ON v.id = d.code_version
  WHERE t.interface = 'adt-in' AND d.received_at > now() - interval '1 day'
  GROUP BY 1 ORDER BY 2 DESC;
  ```

**At a start**, workers begin on the messages already waiting as soon as their code has loaded;
the versions are saved right after (milliseconds, more the first time a project's files are
stored), and no decision is saved before its version is: a message decided in that moment waits
for it, never ingest
([nuance](../nuances/deployments.md#the-first-decisions-after-a-start-wait-for-the-code-versions-to-be-saved-level-4)).

**Upgrading to 0.7.3.2** adds the column (migration 0027): a change to the database's catalog,
not to its rows, done in milliseconds. It needs `deliveries` to itself for that moment, so it waits
at most 2 s for a query already reading it; past that the start stops, says what holds the table
and changes nothing (senders wait those 2 s at most, never refused), and a handover leaves the old
engine serving
([nuance](../nuances/deployments.md#upgrading-to-0732-waits-at-most-2-s-for-the-deliveries-table-level-3)).

## What it costs

Measured at 1,001 interfaces with code in a git repository (3,013 files, `scripts/bench-versions.sh`,
WSL): the start computes the versions in 14–16 ms; loading the code (which now hashes each
file as it reads it) and the whole start were within run-to-run noise of the version before. The
record is written beside the start, after it: 33–100 ms (the git comparison included), 389 ms the
first time, when every version and file is new (848 kB of file text). Nothing runs in the
background afterwards; one row per start or handover, plus the files that changed.

**The code version on each delivery** (0.7.3.2) is 4 bytes in a row (74 → 80 bytes), saved in
the statement that saves the decision anyway. Measured against the version before
(`scripts/bench-inspect.sh`: 20,000 HL7 messages through a transform over 4 connections, three
passes of three runs each, on WSL): ingest 3,879–7,214 messages/s before, 3,983–7,148 after;
everything delivered in 9.5–13.6 s before (one run of 32 s, just after a restart), 9.1–13.3 s
after: the same, within the runs' own spread (which was wide that day, for both). At 1,001
interfaces the whole start was 761–972 ms before and 756–927 ms after; the versions are saved
44–52 ms after they're computed when nothing changed (mostly one statement carrying a thousand
hashes), 212–267 ms the first time a project's files are stored. Only a decision that comes
before that waits for it (messages waiting at the start); nothing a sender waits for does.

**History** (0.7.3.3), measured with `scripts/bench-history.sh` on WSL: 1,001 interfaces with
code in a git repository of 1,001 commits (121 of them on the measured interface's files), 1,000
deployments that changed it (its documentation with each), and an audit log of a million rows
(50,000 naming it, 45,000 of those payload reads):

| | min / median / max of 7 |
|---|---|
| `ductus history v-0` (loads its code, walks git, reads the database) | 366 / 386 / 417 ms |
| the same with `--json` / `--all` | 373 / 384 / 411 ms / 382 / 398 / 507 ms |
| without a database (git only) | 251 / 299 / 343 ms |
| `GET /interfaces/v-0/history`, the first request (git walked) | 188 ms |
| the same, later (the walk kept until HEAD moves): first page / `all=true` / 800 entries deep | 9 / 11 / 12 ms, 8 / 9 / 10 ms, 7 / 8 / 10 ms |
| `git log` over the same five files, for comparison | 32 / 41 / 74 ms |

The database's part is 6–8 ms a page: each of its four reads takes under a millisecond through an
index (the audit log's through one that leaves payload reads out: without it, a page of changes
read through every read naming the interface). The git walk is most of a first read
([nuance](../nuances/deployments.md#the-first-history-read-after-a-deploy-walks-git-later-ones-dont-level-4)).

**The warning at a handover** (`scripts/bench-handover.sh` with `WARN=1`: 1,001 interfaces, a
repository of 1,000 commits, each handover by a different person after the last commit was
rewritten, so every one warned; nine handovers each, against 0.7.3.2's binary): sending paused
371–434 ms (median 398) with the warning, 347–473 ms (median 421) before it: the same. The check
runs before the new engine is ready: "ready after" was 579–643 ms, 541–613 ms before (+~40 ms,
while the old engine still sends). Nothing refused either way.
