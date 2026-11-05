# Nuances: what runs, recorded

What could surprise you here, why it is so, and what to do. Each entry has a level, 1 (day one) to 5 (expert): the [index](index.md) lists them all by level.

## The code version counts what the code imports, not the whole folder (level 2)
- **What:** an interface's code version is a hash of its `interface.yaml` and every project file its code imports: its script, the files that imports, and so on through shared files (`lib/…`), including files imported only for their types and `import("./x.ts")` that hasn't run yet. Its tests, fixtures, a README in its folder and files nothing imports don't count; neither does the SDK (the binary's version is recorded beside each deployment instead). So a change to a shared file changes the version of **every interface that imports it**, and the next start or handover lists each of them as changed.
- **Why:** the version answers "does this interface run something different now?". A changed test doesn't change what runs; a changed shared library does, for everyone who imports it.
- **What to do:** read a long list of changed interfaces after a deploy as "a shared file changed", and check which (`code_versions.files` lists each version's files with their hashes). Keep shared code that few interfaces need out of files every interface imports.

## Line ends and a byte order mark don't change a version (level 2)
- **What:** each file is counted without a byte order mark and with `\n` line ends, so a Windows checkout of a commit (CRLF, Git's usual setting there) and a Linux one have the same versions. A file outside the project folder (an import reaching above it) is counted by its full path, so that interface's version differs between servers whose paths differ.
- **Why:** the same commit must give the same version wherever it runs, or every engine would look changed.
- **What to do:** keep interface code inside the project folder.

## `systemctl reload` can't say who deployed (level 1)
- **What:** a handover started by `systemctl reload ductus` is recorded as by `systemd (systemctl reload)`, with no reason: systemd runs the reload as the service and tells it nothing about who typed the command. A plain start under systemd or the Windows service is recorded as by the service's account (`ductus`), `how` = `service`.
- **Why:** nothing from the person reaches the engine through systemd.
- **What to do:** deploy with `sudo ductus handover -m "why"` instead: the same handover, recorded as by the person behind `sudo`, with the reason ([who and why](../operate/deployments.md#who-and-why)).

## "Not committed" means the files differ from HEAD, not `git status` (level 2)
- **What:** a deployment records the project's git commit (HEAD) and the files its interfaces run that aren't that commit's: edited since, or never committed. Files nothing runs (a log, an untracked note, a changed test) aren't counted, and neither are line ends. A project in no git repository records no commit; one in a repository without a commit yet records every file as not committed.
- **Why:** what matters for "is this the commit?" is the code that runs; `git status` also reports files that change nothing.
- **What to do:** a deployment with files not committed ran code nobody can find in git: commit them (the next start or handover records the commit with nothing uncommitted, and the versions don't change), or read the code that ran from `code_files`.

## Interface code is kept in the database (level 2)
- **What:** every file a code version counts is stored once, by its content, in `code_files`: the first start stores the whole project's code (about a megabyte of text for a thousand interfaces), later ones only what changed. It's kept, so the code that ran last month can be read without git. Whoever can read the engine's database can read your interface code.
- **Why:** to show, and run again, the exact code that decided a message (only originals are stored; an output is reconstructed by running the code that made it), on a server where git history isn't at hand.
- **What to do:** nothing usually: code holds no credentials (they're secrets and variables, which aren't stored). If code holds something it shouldn't, remove it from the code; rows already stored stay (ask your database administrator).

## A start whose deployment can't be recorded runs anyway (level 3)
- **What:** if recording the deployment fails (a database error), the engine logs an error ("this start's deployment … wasn't recorded; the engine runs all the same") and runs; the API then shows no `deployment`, and the interfaces no `code`. If the git repository can't be read, the deployment is recorded without its commit, with a warning.
- **Why:** the engine may be all that's left to move messages; a record must never stop it, as with the audit log.
- **What to do:** look at the error; the next start or handover records a deployment again.

## A delivery names the code that decided it, not the code that sent it again (level 2)
- **What:** each delivery records the code version that decided it (0.7.3.2): sent, filtered, or quarantined, by the code or because the receiver refused what the code decided to send. Sent again (Resend, Replay), it names none until a worker decides it again, then the version running then; the resend's row in the message's history keeps the version before. A dismissal and a replacement by an edited copy name none (a person decided), and neither do deliveries decided by an engine before 0.7.3.2 or filtered because their destination was removed.
- **Why:** the question is "which code made this decision?"; a resend is a new decision, possibly by newer code, and the old one is kept where the change is recorded.
- **What to do:** for a delivery's earlier decisions, read its history (the message page's History, `GET /messages/{id}` `actions[].previous_code_version`). A delivery that names no code was decided by a person or before 0.7.3.2: its `last_error` says which.

## The first decisions after a start wait for the code versions to be saved (level 4)
- **What:** at a start, workers decide the messages already waiting as soon as their interface's code has loaded, but save no decision before the interface's code version has been saved (right after they're computed). About 50 ms at a thousand interfaces; about a quarter of a second at a project's first start (every file is stored then), measured. Ingest never waits for it: messages are stored and ACKed as always. A handover saves them while the old engine still sends, so its new engine doesn't wait. If the database refuses for 30 s, decisions are saved without their version, and the log says so.
- **Why:** each delivery names the version that decided it, and that version's row must exist first; saving it in the start itself would delay every start.
- **What to do:** nothing; a delivery saved without its version in that case names none.

## Upgrading to 0.7.3.2 waits at most 2 s for the deliveries table (level 3)
- **What:** migration 0027 adds a column to `deliveries` and `message_actions` (no row is rewritten: milliseconds), but needs each table to itself for that moment, so it waits for every query already reading it, and every later statement waits behind it, inserts too. It waits at most 2 s per table; past that the start fails with "migration 0027 waited 2 s for a table's lock and gave up", the query to find what holds it, and nothing changed. Senders wait those 2 s at most; none is refused. A handover to the new version fails the same way and leaves the old engine serving.
- **Why:** without a limit, one long report on `deliveries` would hold every sender's ACK for as long as it ran.
- **What to do:** upgrade when no long report runs (`ductus upgrade --check` lists the migration); if the start says so, let the query finish or end it (`pg_cancel_backend`), then start again. Under systemd the service starts again by itself.

## `import()` with a computed name isn't counted (level 4)
- **What:** `import("./" + name + ".ts")` can't be followed without running the code, so the files it loads don't count in the version until they've been loaded at a start before the version is computed (they usually haven't).
- **Why:** the version is computed from the code as written, before any message runs.
- **What to do:** import with a literal (`import("./codes.ts")`), or import statically.
