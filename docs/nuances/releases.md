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
