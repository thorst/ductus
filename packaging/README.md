# Packaging

How Ductus is installed: a container image, Linux packages with a systemd service, and (coming)
a Windows MSI with a Windows service. Every one is a development build until v1.0.

The engine is one binary (`ductus`, the web UI inside it) plus PostgreSQL, which is never
bundled: it's the DBA's, on the same server or another, and so are its backups (pgBackRest or WAL
archiving). Everything the engine maintains (partitions, retention, cleanup) runs inside it: no
cron jobs.

Settings come from environment variables, the same everywhere:

| Variable | What |
|---|---|
| `DUCTUS_DATABASE_URL` | `postgres://user:password@host/database`; the engine creates its tables on first start. There is no default: without it `ductus run` says so |
| `DUCTUS_PROJECT` | the project folder (`interfaces/`, `ductus.yaml`, `environments/`), usually a Git clone |
| `DUCTUS_ENV` | which environment's variables and secrets to use (`dev`, `prod`, ...) |
| `DUCTUS_SECRET_KEY` | opens `secrets/<env>.json` |
| `DUCTUS_API_LISTEN` | the REST API and web UI (`127.0.0.1:7700` by default) |
| `DUCTUS_STOP_TIMEOUT` | how long a stop may take to finish the messages in hand (30 s) |

## Local development without a database

`ductus dev` runs a project with a Postgres of its own: nothing to install, nothing to configure.
The first run downloads Postgres 18.6 once per user (about 30 MB, Maven Central); the cluster
lives in `<project>/.ductus/postgres` (`ductus init` gitignores it), keeps its port between runs
and prints its URL, so any SQL tool can look inside. It stops with the engine. Without internet
access, point `DUCTUS_POSTGRES_DIR` at an unpacked copy of the binaries. `--reset` starts over.
It is for development: production uses the DBA's Postgres through `DUCTUS_DATABASE_URL`.

## Stopping and upgrading

A stop (SIGTERM from systemd or Docker, Ctrl-C, or `ductus drain --stop`) is clean: no new
messages, the ones in hand finished and saved, exit code 0 (`docs/nuances.md`, "Stopping the
engine"). Service managers are set to wait longer than the engine's own limit, so they never
cut a stop short.

An upgrade replaces the binary and restarts the service. The new version updates the database at
startup; updates only ever add (expand-only migrations), so the previous version still runs on
the updated database: rolling back is installing the previous package. Zero-downtime upgrades
(two engines handing over) and `ductus upgrade --check` come later in v0.6.

## Container image

```sh
docker build -t ductus:dev .
PROJECT=$PWD/examples/adt-relay docker compose -f packaging/docker/compose.yaml up
```

- Debian 13 slim, the binary at `/usr/local/bin/ductus` (the same one as in the packages), CA
  certificates; runs as user `ductus` (uid 10001), never root.
- The project is mounted read-only at `/project`. From v1.0, production images will be built
  `FROM` the engine image with the project inside (an image per release of your interfaces).
- The API listens on `0.0.0.0:7700` inside the container; publish it to `127.0.0.1` unless a TLS
  proxy is in front. Publish each interface's listener port.
- `stop_grace_period: 45s` in compose (`docker stop --time 45`): Docker's default 10 s can be
  shorter than a stop needs.
- Images aren't published to a registry before v1.0.
- Test: `scripts/demo-docker.sh`.

## Linux packages (.deb, .rpm)

```sh
docker build --target packages --build-arg VERSION=0.5.0-dev.20261009 --output dist .
```

Built with [nfpm](https://nfpm.goreleaser.com) from `packaging/linux/nfpm.yaml`. One Dockerfile
builds the binary once, on Rocky Linux 9, for both the packages and the image: it runs where
glibc is 2.34 or later (RHEL 9 and its rebuilds, Ubuntu 22.04, Debian 12 and later). Versions are semver; a development build `0.5.0-dev.20261009`
becomes `0.5.0~dev.20261009`, which sorts before `0.5.0`.

| Path | What |
|---|---|
| `/usr/bin/ductus` | the engine and CLI |
| `/usr/lib/systemd/system/ductus.service` | the service (runs as `ductus`, hardened: writes only `/var/lib/ductus`) |
| `/etc/ductus/ductus.env` | settings: root and the `ductus` group only; an upgrade never replaces it |
| `/var/lib/ductus` | the service's home; the project goes in `/var/lib/ductus/project` |

- **Install:** creates the `ductus` user; doesn't start or enable the service (nothing is
  configured yet) and prints the next steps: edit `ductus.env`, clone the project,
  `systemctl enable --now ductus`, `journalctl -u ductus -f`.
- **Upgrade:** a running service is restarted on the new version (a clean stop first); a stopped
  one stays stopped.
- **Remove:** stops and disables the service; keeps the user, the project and the settings.
- Logs go to the journal (`journalctl -u ductus`).
- Test: `scripts/demo-packages.sh` (systemd in containers: install, configure, upgrade under
  traffic, stop, remove; Ubuntu and Rocky).

## Windows (coming in 0.6.1)

An MSI (WiX) installing a Windows service: silent installs (`msiexec /i ductus.msi /qn`) for
GPO, SCCM and Intune, upgrades in place.
