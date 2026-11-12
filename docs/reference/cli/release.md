# `ductus release`

Make a release of the project: one file (a .tar.gz) holding exactly what HEAD's commit has (the project's folder of it), every interface's code version, the project version, who made it, why, from which commit (with its whole history) and the tests' result. Its id is a hash of the files it carries, so the same commit gives the same id anywhere. Refuses files that aren't committed (unless `--uncommitted`), a private key, and code imported from outside the project; runs the tests first (on what the release carries) and refuses when one fails. Writes `.ductus/releases/<project>-<id>.tar.gz`. `ductus release check` is the gate before a deploy.

```text
ductus release [OPTIONS]
       ductus release <COMMAND>
```

**Options**

| | What it does | Default | Environment |
|---|---|---|---|
| `-m, --message <WHY>` | Why: a sentence the release carries, shown wherever it goes (`-m "lab feed: new OBX mapping"`). |  |  |
| `--uncommitted` | Carry files that aren't committed as they are in the folder (changed, added, new and not ignored; deleted ones left out), instead of refusing. The release lists them, and every check of it says so: for trying a release out, not for production. |  |  |
| `--over <RELEASE>` | The release this one replaces, by its whole id or its file: recorded, so a deploy can tell whose work it would undo when the project isn't in git (in git, the commit's history says). |  |  |
| `--branch <NAME>` | The branch to record when HEAD is detached (CI checking out a tag or a pull request; on GitHub Actions `--branch "$GITHUB_REF_NAME"`). An engine with DUCTUS_DEPLOY_BRANCHES refuses releases made on other branches. When HEAD is on a branch, it's that one (another name here is refused). |  |  |
| `--no-tests` | Make it without running the tests; the release says they weren't run, and `ductus release check` then runs every interface's. |  |  |
| `--out <PATH>` | Where to write it: a folder (made when missing if it ends with `/`), or the file's path (default `.ductus/releases/` in the project). |  |  |
| `--json` | One JSON document: the manifest, with the file written, its size and how long it took. |  |  |
| `--root <ROOT>` | The project folder: the one with `interfaces/` and ductus.yaml. | `.` |  |
| `--env <ENV>` | The environment the tests run in and interface.yaml is read in (the release itself is the same for every environment). | `dev` | `DUCTUS_ENV` |

## `ductus release check`

The gate: check a release against an environment before deploying it, changing nothing. Unpacks it into a temporary folder (removed after), checks its files against its id, then runs `ductus validate` for the environment against its database (reading only) and the tests of the interfaces whose code differs from what runs there, each as a child process. Exits 1 on any problem. A read-only login is enough (`pg_read_all_data`).

```text
ductus release check [OPTIONS] --env <ENV> <BUNDLE>
```

**Arguments**

| | What it does | Default | Environment |
|---|---|---|---|
| `<BUNDLE>` | The release's file (`ductus release` printed it). |  |  |

**Options**

| | What it does | Default | Environment |
|---|---|---|---|
| `--env <ENV>` | The environment to check it for: its values, secrets (DUCTUS_SECRET_KEY) and database. |  | `DUCTUS_ENV` |
| `--all-tests` | Run every interface's tests, not only those of what the release changes. |  |  |
| `--database-url <DATABASE_URL>` | The environment's database (read-only): what runs there now, and validate's checks against it. Without it every test runs and validate checks without a database (said). |  | `DUCTUS_DATABASE_URL` |
| `--json` | One JSON document instead of the report. |  |  |

