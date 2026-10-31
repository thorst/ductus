# On your laptop: `ductus dev`

`ductus dev` runs a project with a PostgreSQL of its own: no database to install, nobody to ask,
nothing to configure. It's how you write and try interfaces on your own machine, on Linux or
Windows.

**Use it** for development, demos and learning. **Don't** use it to run anything that matters:
it uses Postgres's default settings, nobody backs its data up, and it has no service, no
handover and no restart on failure
([nuance](../nuances.md#ductus-dev-is-a-development-tool-not-a-way-to-run-in-production-level-1)).
Everywhere else, use `ductus run` (or a [package](index.md)) with your DBA's Postgres.

## Get the program

Download the [portable archive](archive.md) for your system and put `ductus` (`ductus.exe`) on
your `PATH`, or build it from source (`cargo build --release` in a clone; see the repository's
README). No administrator rights are needed on either system.

## Run it

```sh
ductus init hospital      # a new project, with one interface to start from
cd hospital
ductus dev
```

The first run downloads Postgres 18.6 once for your user (about 30 MB, from Maven Central), then
prints where everything is:

```text
Postgres 18.6.0 is running for this project:
  postgresql://postgres:…@127.0.0.1:42731/ductus
  (data in ./.ductus/postgres/data; stops with the engine)
Web UI login: user `dev`, password `…` (kept in ./.ductus/dev-login)
… INFO ductus_runtime::api: REST API listening (/api/v1) host=127.0.0.1 port=7700 https=false
… INFO ductus_runtime: started interfaces=1 mllp=1 http=0 files=0 schedules=0 …
```

- **The web UI** is at `http://127.0.0.1:7700`, with the `dev` login it printed.
- **The database URL** works with any SQL tool (psql, DBeaver, DataGrip) while the engine runs,
  and with every command that needs a database: `export DUCTUS_DATABASE_URL=…` (PowerShell:
  `$env:DUCTUS_DATABASE_URL = "…"`), then `ductus status`, `ductus user create`, `ductus token
  create` and the rest talk to it.
- **Ctrl-C** stops the engine cleanly, then its Postgres.
- **The data stays** in `.ductus/postgres` between runs, and so does its port. `ductus init`'s
  `.gitignore` leaves `.ductus/` out of Git.

| Option | What it does |
|---|---|
| `--reset` | Throws this project's dev database away and starts with an empty one (asks first; `--yes` doesn't). |
| `--database-only` | Starts only the Postgres and prints its URL: for tests, or to run `ductus run` yourself against it. |
| `--port N` | Postgres's port, instead of the one kept from the last run (or a free one the first time). |
| `--env NAME` | The environment's variables and secrets, as with `run` ([environments](../configure/environments.md)). |
| `--api-listen HOST:PORT` | Where the web UI and API listen (`off` for neither). |

All of them are in the [reference](../reference/cli/dev.md).

## Without internet access

The download is the only thing `ductus dev` fetches, and it keeps it in a per-user folder:
`~/.cache/ductus/postgres` on Linux (`$XDG_CACHE_HOME` when set), `%LOCALAPPDATA%\ductus\postgres`
on Windows. Where the download is blocked, copy that folder to the same place from a machine of
the same system that has run `ductus dev` once; or unpack Postgres's binaries yourself into a
folder holding `bin/`, and point `DUCTUS_POSTGRES_DIR` at it. Or skip `dev` and run `ductus run` against any
Postgres you have, with `DUCTUS_DATABASE_URL`.

## What's different from a server

- One engine per project folder at a time: a second `ductus dev` in the same folder says it's
  already running.
- No handover: `systemctl reload` is the packages'. After a change to the project, stop and start
  `ductus dev` again (it starts in well under a second).
- `DUCTUS_DATABASE_URL` is ignored: `dev` always uses its own Postgres.
