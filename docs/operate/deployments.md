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

A timeline per interface (`ductus history`, an interface's **History** tab) comes with the next
part of 0.7.3.

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
