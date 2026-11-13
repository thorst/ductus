# Nuances: releases

What could surprise you here, why it is so, and what to do. Each entry has a level, 1 (day one) to 5 (expert): the [index](index.md) lists them all by level.

## A release is the commit, not the folder (level 1)
- **What:** `ductus release` carries HEAD's files (the project's folder of the commit), read from git's own store, not what's in the folder. A file changed and not committed, deleted, added with `git add` but not committed, or new and not ignored by `.gitignore` refuses the release, named. `--uncommitted` carries such files as they are in the folder instead (a deleted one is left out), and the release lists them; `ductus release check` then says so with a ⚠.
- **Why:** a release must be something anyone can find again: the same commit, the same release. A file only on your disk would deploy code nobody can read in git.
- **What to do:** commit, or add the file to `.gitignore` if it's yours alone (`out/`, notes). Keep `--uncommitted` for trying a release out on a test engine. `.ductus/` is never looked at: it's where releases are written.

## The same commit gives the same release id, whatever the reason or the machine (level 2)
- **What:** the release id is a hash of the files the release carries (each text without a byte order mark and with `\n` line ends), not of the archive. Two releases of one commit have one id even with different `-m` reasons, made on Linux and on Windows (where Git checks files out with CRLF), on different days. Their archives may differ (the manifest says who, when and why), and so may their files' sizes on disk.
- **Why:** the id answers "is this the same code?", which is what promotion and rollback need; who made it and why are recorded beside it, not in it.
- **What to do:** compare releases by id, never by the archive's checksum. To recompute an id by hand: `sha256sum` each file under `project/` in path order, as `ductus release 1` lines ([how](../operate/releases.md#the-release-id)).

## Texts are carried normalized; other files byte for byte (level 3)
- **What:** a file that is UTF-8 without a NUL byte is a text: carried without a byte order mark and with CRLF pairs as `\n` (an HL7 fixture's lone `\r` segment ends are kept). Anything else (an image, a spreadsheet, a file in Latin-1) is carried exactly as committed and listed in the manifest's `binary`.
- **Why:** the same as for code versions, so a Windows checkout and a Linux one are the same release; a file that isn't text can't be "normalized" without breaking it.
- **What to do:** nothing usually. A CSV fixture saved in Latin-1 is carried byte for byte (still the same on every machine); save it as UTF-8 if you want it treated as text.

## A committed private key refuses the release (level 1)
- **What:** a committed file holding a PEM private key (`-----BEGIN … PRIVATE KEY-----`) or named like a key store (`.key`, `.p12`, `.pfx`, `.jks`) refuses the release, named. Certificates (public) are fine.
- **Why:** a release is copied to CI, to servers and (from 0.7.4.3.3) into the database: a key in it is readable by everyone who handles it.
- **What to do:** `git rm --cached <file>` and commit; keep keys out of the project (`*.key` is in the `.gitignore` `ductus init` writes) and name them in resources.yaml by a path on the server. The key stays in git's history: if the repository was shared, replace the key.

## Code imported from outside the project refuses the release (level 2)
- **What:** an interface whose code imports a file outside the project folder (an absolute path, or `../` reaching above the project) can't be released: the release can only carry the project. An absolute import is named in the refusal; a relative one that reaches out fails to load from the unpacked copy, with the path it looked for.
- **Why:** the release must run the same code wherever it goes; a file outside it would be whatever that server has at that path.
- **What to do:** move shared code into the project (`lib/` is the place) and import it from there.

## The gate tests what the release changes there, by code version (level 2)
- **What:** `ductus release check --env prod` runs the tests of the interfaces whose code version differs from what runs on prod's database (and those new there). An interface whose code is unchanged isn't tested again, even if its tests changed: they ran when the release was made, on the same code. A change only to the project's own files (resources.yaml, environments, secrets) selects no tests: `validate` checks those. Without the database, or for a release made with `--no-tests`, every test runs.
- **Why:** at a thousand interfaces every test takes seconds more than the few that matter, and a test of unchanged code in an unchanged binary can't give a different answer.
- **What to do:** `--all-tests` when you want them all anyway (a new Ductus version on the server, say). Give the gate the environment's database, read-only (`DUCTUS_DATABASE_URL`): without it the gate can't say what changes.

## "What runs there" is what the engines recorded, not what the folder holds (level 3)
- **What:** the gate compares the release with the versions the environment's engines recorded at their last start or handover (0.7.3), not with files on the server. An interface removed from the project still has its last record there; one that never ran there is "new".
- **Why:** the record is what's known to have run; a folder may have been changed since without a start.
- **What to do:** if the gate says nothing changes but you expected it to, check what the engines run (`GET /interfaces/{name}` `code`, or the interface's History tab).

## The tests at release time run in `dev` (level 3)
- **What:** `ductus release` runs the tests with `--env dev` unless you say `--env`: `${...}` in interface.yaml are filled from `environments/dev.yaml` for them. The release itself is the same for every environment (code versions count interface.yaml as written).
- **Why:** tests run without databases and connections; the environment only fills values in.
- **What to do:** when a test depends on an environment's values, make the release with that `--env`, or rely on the gate, which runs the changed interfaces' tests in the environment it checks.

## Checking a release on an engine runs its code there, with that environment's secrets (level 2)
- **What:** `POST /releases/{id}/check` runs `ductus release check` on the engine's server, as the engine's account, with its environment's values, its secret key and its database (read-only): `validate` loads every interface's code and the tests of what changes run it. It needs the `deploy` permission, which only operators and admins can have (giving it to a viewer is refused; a directory group can't give it to one either).
- **Why:** a check is only worth something where the environment is: its secrets, its certificates, its database. That also means whoever may check a release may run code with prod's secrets, which is what deploying will be.
- **What to do:** give `deploy` to the people and the CI token that ship changes, as carefully as `payloads`. Every check is in the audit log (`release.check`) and kept with the release.

## A check on the engine slows its intake while it runs, and runs one at a time (level 3)
- **What:** the check runs on the engine's server as a separate process, spread over every CPU as `validate` and `test` are, for seconds (at a thousand interfaces: 6 s, 10 s with every test). Measured on a 4-core machine, one interface's intake fell from ~800 to ~650 messages a second over a run that overlapped one check, and to about half while checks ran back to back; ACKs waited up to ~0.1 s instead of ~0.03 s. Nothing was refused or lost. The process runs at a lower CPU priority (nice 10 on Linux, below normal on Windows), which made no measurable difference there. A second check while one runs on the same engine is refused with 409 ("try again when it ends"). The call answers when the check ends.
- **Why:** a check is only worth something where the environment is, and `validate` at scale needs the CPUs it uses. Half of ~800 a second is still far above what a busy feed sends (a large hospital's ADT feed: tens a second at its peak), so messages wait milliseconds, not minutes.
- **What to do:** check once per release you mean to ship to that environment, not on every commit: like any work added to a running engine (reports, exports, replays, frequent polling), its value has to be worth what it takes from the messages meanwhile. On an engine that runs near its limits, check against a test engine sharing prod's configuration, or run `ductus release check` on another machine given prod's database (read-only) and secret key. In CI, retry on 409 after a pause, or check one environment at a time.

## The same release stored twice is stored once, with its first manifest (level 2)
- **What:** `POST /releases` with a release whose id is stored already answers 200 with `"new": false`, who stored it and when, and changes nothing: a second archive of the same commit made with another `-m` reason keeps the first one's manifest (who made it, why).
- **Why:** the id is the files ([above](#the-same-commit-gives-the-same-release-id-whatever-the-reason-or-the-machine-level-2)); a release is what it carries, and storing it twice would give one release two stories.
- **What to do:** say why a change ships where it ships: in the deploy's reason (0.7.4.4), not in a second `ductus release -m`.

## Stored releases are kept (level 3)
- **What:** releases stored on a database stay there, with their checks: nothing deletes them. A release adds only what earlier ones and the engines' code versions don't already hold (each text once, each folder's file list once), plus its manifest (hundreds of KB at a thousand interfaces) and its commit history (20 KB per 1,000 commits).
- **Why:** a deploy, a rollback and "what did prod run in March" choose among stored releases; what's measured is small next to messages.
- **What to do:** nothing at the sizes measured ([releases](../operate/releases.md#measured)). A repository with a very long history (100,000 commits) adds 2 MB a release: tell us if that matters to you.

## An engine in release mode runs nothing until a release is deployed, and its API starts on its defaults (level 1)
- **What:** `ductus run --releases` (DUCTUS_RELEASES) runs the release last deployed to its environment for its project on its database. Before the first deploy there is none: the engine starts with no interfaces (`started interfaces=0`), says so in its log, and waits. Its REST API listens where `--api-listen` (DUCTUS_API_LISTEN) says, else on 127.0.0.1:7700, without TLS and without an allow list: those settings are in the project's `ductus.yaml`, which arrives with the first release.
- **Why:** in release mode the project is in the database, not in a folder on the server; there's nothing else to read them from yet.
- **What to do:** deploy the first release from the server itself (`ductus deploy … --to http://127.0.0.1:7700`), or set DUCTUS_API_LISTEN for that first deploy on a network you trust. From the first release on, `ductus.yaml`'s `api:` settings apply.

## A deploy is refused when it would undo someone else's work, and a release is the whole project (level 2)
- **What:** for each interface a release changes, the version running there must be the deployer's own, or come from a commit in the release's history (a colleague's merged work), and not from files never committed; the files outside every interface (`ductus.yaml`, `resources.yaml`, `environments/`, …) are held to the same rule. Otherwise the deploy is refused before the gate runs, naming the interface, whose work it was, when they deployed it and why it would be undone. Because a release is the whole project, a colleague's deployed but unmerged work on another interface refuses your release too: it would put that interface back. "The same person" is by name: the token's or account's that deployed. A release made without git is held against the release it names as its base (`ductus release --over`).
- **Why:** two people deploying from their own branches would otherwise undo each other's changes without either knowing; merging first is what makes a release carry both.
- **What to do:** merge their commit and release again (what's in `main` is assumed shippable). To undo their change on purpose, `ductus deploy --force -m "why"`: the audit log (`release.force`) names each interface and whose work, and the deployment's history keeps the warning.

## A deploy reuses a recent passing check only when nothing was deployed there since (level 2)
- **What:** a deploy runs the gate (`ductus release check` against that environment and database) unless a check of the same release passed there within DUCTUS_DEPLOY_REUSE_CHECK (default 1 h), run by the same Ductus version, with **no deployment of any kind recorded in that environment since**: a restart, a handover, another release (and never a check from before the engine you deploy to started). Then it reuses that check and says which (`gate: reused check #12 …`); otherwise it runs the gate and says why none could be reused. `--all-tests` never reuses one.
- **Why:** a check says the release fits the environment as it was at that moment; any start or deploy can change what it would find (another interface's version, the database's migrations).
- **What to do:** check a release on the engine (`POST /releases/{id}/check`) when you want to know before the change window, then deploy within the hour; set DUCTUS_DEPLOY_REUSE_CHECK=0 to run the gate at every deploy.

## In release mode the files on the server are the database's copy (level 2)
- **What:** each release is unpacked from the database into `<DUCTUS_RELEASES>/<release id>/` the first time an engine runs it there; the running release and the two used last are kept, older folders removed. A folder without its marker file (`.ductus-release`, written last) is unpacked again. An edit to those files on the server is never deployed and is replaced the next time the folder is unpacked.
- **Why:** the database holds what was released, gated and recorded; a folder only mirrors it so the engine can read it like a project.
- **What to do:** change the project, release, deploy. To look at what runs, read the folder, or the release itself (`GET /releases/{id}`).

## A deploy applies by a handover: to a new process under systemd, in the engine's own process elsewhere (level 1)
- **What:** a deploy applies by the handover: a new engine runs the release beside the old one, then takes over (receiving never stops). Under the package's systemd unit (a control socket) the new engine is a new process; on Windows, in a container and under a plain `ductus run` it runs in the engine's own process, so the Windows service stays Running and the container never restarts ([nuance](upgrades.md#without-a-control-socket-the-handover-runs-in-the-engines-own-process-level-2)). The deploy's answer comes from the engine being replaced, once the new one sends; it says which (`✓ handed over to ductus 0.7.0 (pid 4598)` or `✓ handed over to a new engine in this process`). Only `ductus dev` takes no deploys (409).
- **Why:** one way to apply a release everywhere, with the same promise: nothing refused, a release that can't start changes nothing.
- **What to do:** nothing differs for the person deploying. On Windows and in containers, leave room for a second engine's memory during the deploy, as on Linux.

## Where a release went, and its branch, are labels in its file, not proof (level 2)
- **What:** an environment with DUCTUS_DEPLOY_REQUIRES reads where a release was deployed from the record its file carries (written by `ductus deploy` into the file it deployed, and by `GET /releases/{id}/file`, which `ductus deploy --from` fetches) and from its own database's deployments. DUCTUS_DEPLOY_BRANCHES reads the branch the release records. Both are fields of the manifest, outside the release id and unsigned: anyone holding the file can edit them, and the release keeps its id. An engine whose environment happens to be named `test` writes a record like any other.
- **Why:** test and prod have separate databases, so prod can only learn about test from what it's given; signing releases is a later step.
- **What to do:** treat the rules as a guard against mistakes, not against someone determined. What keeps work that isn't in `main` out of production is who can deploy there: CI building releases from `main` and holding the only tokens with the `deploy` permission on test and prod. Give people tokens without `deploy` on those engines.

## An environment's rules aren't skipped by `--force`; a rollback isn't held to them (level 2)
- **What:** a deploy refused by DUCTUS_DEPLOY_REQUIRES or DUCTUS_DEPLOY_BRANCHES (409, before the gate runs) stays refused with `--force`, which only concerns the "replaces" check (someone else's work). A rollback (`ductus deploy --rollback`) is never held to them: the release ran in that environment before. The report and the audit row (`release.rollback`) say so.
- **Why:** the rules are what the operator decided an environment takes; a person deploying shouldn't be able to waive them in passing. Going back to what ran there yesterday mustn't wait for a test environment.
- **What to do:** to deploy outside the rules on purpose (an emergency fix straight to production), change the setting in ductus.env and reload (`sudo systemctl reload ductus`, `ductus handover` on Windows): the reload is recorded. Put it back after.

## A rollback brings back the whole project, secrets included, but not the database (level 2)
- **What:** a rollback deploys an earlier release, and a release is the whole project: every interface's code and interface.yaml, `ductus.yaml`, `resources.yaml`, `environments/`, and `secrets/*.json` **as they were then** (a secret rotated in the later release comes back with its old value). It doesn't change the engine's own settings (ductus.env, `DUCTUS_*`, the secret key), the binary, or the database: migrations come with the binary, never with a release, so a rollback runs none (the schema only grows, and the earlier release runs on it as the gate checked). What the later release did stays done: messages it sent stay sent.
- **Why:** a release is one exact, checked artifact; going back means running that artifact again, not reversing each change.
- **What to do:** after rotating a secret, release again with the new one rather than rolling back over it (or rotate it again after the rollback). A rollback that changes an interface's lanes (`ordering.partitions` went up in the later release) is fine with messages waiting: they're sent first ([nuance](ordering.md#after-a-change-of-ordering-new-messages-of-a-key-wait-for-its-older-ones-level-4)).

## A destination a rollback brings back doesn't get what waited while it was gone (level 3)
- **What:** when a release removed a destination, the deliveries waiting for it were filtered at that release's start ([nuance](destinations.md#adding-removing-or-renaming-a-destination-level-3)). Rolling back to a release that has the destination brings it back for new messages; the filtered deliveries stay filtered.
- **Why:** filtering is a decision recorded per delivery; a rollback doesn't rewrite what was decided while it wasn't there.
- **What to do:** find them in the message view (status filtered, that destination, the time between the two deploys) and resend them, or replay them.

## The default rollback skips releases a rollback left (level 2)
- **What:** `ductus deploy --rollback` without a release goes to the last release run in that environment before the current one, **skipping any release that a rollback left**. After A → B → rollback (back to A), the next `--rollback` goes to the release before A, not back to B. Only releases that ran in that environment can be rolled back to; any other is refused (409) with "deploy it". Restarts don't count as runs.
- **Why:** a rollback says "that release was bad"; a second rollback toggling back to it would undo the first.
- **What to do:** to go back to a release a rollback left (it was fine after all), deploy it by its id: `ductus deploy <id>` (held to the environment's rules, like any deploy).

## A release made on a detached HEAD records no branch (level 2)
- **What:** `ductus release` records the branch HEAD is on. CI often checks out a commit, not a branch (a tag, a pull request): HEAD is detached and the release records none, which DUCTUS_DEPLOY_BRANCHES refuses. `--branch NAME` names it then; on a branch, a different name is refused.
- **Why:** git doesn't know which branch a detached commit "belongs to" (it can be on several).
- **What to do:** in CI, `ductus release --branch "$GITHUB_REF_NAME"` (GitHub Actions; other CI systems have a variable for the branch built).
## A deploy from the Releases page waits for the gate and the handover (level 1)
- **What:** **Deploy** and **Roll back** on the Releases page wait for the engine's answer: the gate (seconds at a thousand interfaces, ~1 s when a check is reused), then the handover. The engine answering is the one being replaced; the new one takes the same port, so the page goes on (sessions are in the database: nobody is signed out), its live updates reconnect and every screen is fetched again from the new engine. One deploy or check at a time per engine: a second gets 409 until the first ends.
- **Why:** the page shows the engine's own report (deployed, refused and why, or the gate's problems) rather than "started"; at these times a wait is simpler and clearer than polling.
- **What to do:** leave the dialog open until it answers. Closing the tab doesn't stop a deploy that started: look at **What ran here** or the audit log (`release.deploy`) for how it ended.

## What a release changes here is worked out from code versions, not checked (level 2)
- **What:** a release's page says which interfaces it changes, adds or removes in this environment by comparing its code versions with what this engine runs, and whether the project's own files (`ductus.yaml`, `resources.yaml`, `environments/`, `secrets/`) differ from the running release's. Nothing runs to say it: the gate (**Check first**, **Check against prod**) is what tells whether it works here. A change to an environment's values outside the release (ductus.env) shows nowhere. In folder mode, or while the engine's start is still being recorded, the project files' line and the whole section may be missing.
- **Why:** it's free (no gate, microseconds), so the page can say it before anyone asks.
- **What to do:** read it as "what would move"; run the check before deploying anything that matters.

## The `download` permission reads every release's code and encrypted secrets (level 2)
- **What:** `download` lets a token fetch any stored release's file (`GET /releases/{id}/file`) and nothing else: no deploy, no check, no upload. Any role may hold it (it's meant for a viewer token that another engine pulls with, `DUCTUS_PROMOTE_FROM_TOKEN`). The file carries the project's code and every environment's secrets, encrypted with each environment's own key. Directory groups don't grant it.
- **Why:** prod pulling from test needs a key to test that can't change test; `deploy` would be more than it needs.
- **What to do:** keep the token where the database's password is (ductus.env, readable by the service only). Each download is in the audit log (`release.download`, with who). Don't give `download` to people: they get files from the Releases page with `deploy`.

## Pulling from another engine trusts the system's CAs only (level 2)
- **What:** prod's engine calls test's (`DUCTUS_PROMOTE_FROM`) over HTTPS verified against the server's trusted CAs, as `ductus deploy --from` does; resources.yaml's certificates aren't used (they're the release's, and the release is what's being fetched). Plain `http://` works and sends the token in the clear.
- **Why:** the engine's own settings shouldn't depend on what a release carries.
- **What to do:** with a private CA, add it to the server's trust store (Linux: `/usr/local/share/ca-certificates` and `update-ca-certificates`; Windows: the machine's Trusted Root store). Use `http://` only on a loopback or a network you'd send the database password over.
