# Install

Ductus is one program, `ductus` (`ductus.exe` on Windows): the engine, its command line and the
web UI, all in one file of about 140 MB. It needs a PostgreSQL database, which is never bundled:
in production that is your DBA's Postgres, on the same server or another. Everything the engine
maintains in it (tables, daily partitions, retention, cleanup) it does by itself, while it runs:
there are no cron jobs or scheduled tasks to set up.

## Which way to install

| Way | For | Page |
|---|---|---|
| `.deb` / `.rpm` | A Linux server: the service, its user, its folders, upgrades in place with no connection refused | [Linux packages](linux.md) |
| `.msi` | A Windows server: the service, its account, the Event Log, upgrades in place | [Windows installer](windows.md) |
| `.tar.gz` / `.zip` | Checking an upgrade before installing it; servers where the packages can't be used | [The portable archive](archive.md) |
| Docker image | Trying it with Compose; containers in development | [Docker](docker.md) |
| `ductus dev` | Your own laptop: the engine with a Postgres of its own, nothing else to install | [On your laptop](dev.md) |

Not sure between Linux and Windows? [Choosing Linux or Windows](../choosing-linux-or-windows.md)
lists every difference.

## What you need first

- **A server**: 64-bit x86 (`x86_64`/`amd64`; no ARM builds yet), Linux with glibc 2.34 or later
  (RHEL 9 and its rebuilds, Ubuntu 22.04, Debian 12 and later; RHEL 8 can't run it), or 64-bit
  Windows (tested on every push on GitHub's current Windows Server runner, and on Windows 11).
- **Memory**: the engine itself is small (measured: 1,001 interfaces loaded and idle in 111 MB);
  what grows is what interface code holds, up to
  [`isolates.heap_mb`](../reference/config/ductus.md#isolates) (512 MB) per worker thread at
  most, which most interfaces never come near. Watch the process once real traffic runs
  (`systemctl status ductus` shows its memory; Task Manager on Windows), and see
  [memory for interface code](../configure/ductus-yaml.md#memory-for-interface-code) for what
  makes it grow. During a Linux upgrade two engines run for a few seconds, so leave room for a
  second copy.
- **CPU**: one worker thread per core by default. A feed of a million HL7 messages a day
  averages 12 a second; one core runs a realistic HL7 transform at about 14,800 a second
  ([measured](../architecture.md#performance-notes-measured-wsl-25-ms-fsync-disk)), so CPU is
  rarely what limits it: the database's disk is.
- **PostgreSQL** 14 or later (18 recommended), with a login the engine owns its tables with:
  [PostgreSQL](postgres.md).
- **The ports** your senders connect to, and the ones the engine connects out on, opened in the
  firewall: [Ports and firewalls](../ports.md).
- **A project**: the folder of interfaces the engine runs, usually a Git repository. A new one
  starts with `ductus init`; [Your first interface](../first-interface.md) walks through it.

## What every install has in common

The engine is configured by a few environment variables, the same on every platform (the
packages read them from a settings file: `/etc/ductus/ductus.env` on Linux,
`C:\ProgramData\Ductus\ductus.env` on Windows):

| Variable | What it says | Default |
|---|---|---|
| `DUCTUS_DATABASE_URL` | The engine's database, as a PostgreSQL URL: `postgres://ductus:PASSWORD@db1.hospital.org/ductus?sslmode=verify-full`. The engine creates its tables at its first start. | None: `ductus run` refuses to start without it, and says so |
| `DUCTUS_PROJECT` | The project folder (`interfaces/`, `ductus.yaml`, `environments/`). | The current folder |
| `DUCTUS_ENV` | Which environment's variables and secrets to use ([environments](../configure/environments.md)). | `dev` |
| `DUCTUS_SECRET_KEY` | The key that opens `secrets/<env>.json`. | None (no secrets) |
| `DUCTUS_API_LISTEN` | Where the REST API and web UI listen, in place of [`api.listen`](../reference/config/ductus.md#api) in ductus.yaml. | `127.0.0.1:7700` (this machine only) |
| `DUCTUS_THREADS` | Worker threads for interface code. | One per CPU core |
| `DUCTUS_DB_CONNECTIONS` | The most connections the engine opens to its own database (plus one it holds for its lock). | `32` |
| `DUCTUS_STOP_TIMEOUT` | How long a stop may take to finish the messages in hand. | `30s` |
| `RUST_LOG` | How much the log says: `info`, or `info,ductus_runtime=debug` for one part in detail. | `info` |

Every `ductus` command takes `--env-file FILE` to read a file of these (`NAME=value` lines)
before anything else: `ductus doctor --env-file /etc/ductus/ductus.env` runs with exactly the
service's settings. Variables already set win over the file.

## After installing

1. Give it a database: [PostgreSQL](postgres.md).
2. Put the project in place and set `DUCTUS_PROJECT` and `DUCTUS_ENV`.
3. Run `ductus doctor --env-file …` on the server: it checks every connection the project makes,
   from where the engine will make it, and says in plain words what a failure means.
4. Start the service, and make the first admin account:
   `ductus user create you --role admin --env-file …` ([people](../configure/people.md)).
5. Open the web UI: `http://127.0.0.1:7700` on the server itself, or set up
   [`api.tls`](../configure/tls.md#the-web-ui-and-the-api) to use it from your own computer.
