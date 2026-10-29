# Packaging

How Ductus is installed: a container image, Linux packages with a systemd service, and a Windows
MSI with a Windows service. Every one is a development build until v1.0.

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

An upgrade replaces the binary; on Linux the package then hands the running engine over to it
(`systemctl reload ductus`: the new version starts beside the old one on the same listening
sockets and takes over once it's ready, so receiving never stops and sending pauses for a moment;
a new version that can't start leaves the old one running). Windows restarts the service. The new
version updates the database at startup; updates only ever add (expand-only migrations), so the
previous version still runs on the updated database: rolling back is installing the previous
package. On Linux a restart or a crash refuses no connection either: systemd keeps the listening
sockets for the next start (below). The same reload deploys project changes.

Before upgrading, run the new version's `ductus upgrade --check` from its portable archive (below):
it reads the database and the project, changes nothing, and says what the new version's first
start would do (migrations, and how big the tables they work through are), which engines run on
the database and on which versions, whether the project validates and passes its tests with the
new version, and which settings it deprecates. Exit 1 on anything that would refuse the start.

## Portable archive

`ductus-<version>-linux-x64.tar.gz` (the binary, `LICENSE`, the unit and `ductus.env` as samples)
and `ductus-<version>-windows-x64.zip` (`ductus.exe`, `LICENSE`): the program without an installer,
for `upgrade --check` before installing and for servers that can't use the packages. Its
`README.md` is `packaging/archive/README.md`. The `.tar.gz` comes out of the same Dockerfile build
as the packages (`--target packages`); the zip from `packaging/windows/archive.ps1 -Binary
ductus.exe -Version 0.6.0-dev.20261028`. CI builds both, runs each one's `upgrade --check`
(`demo-packages.sh`, the `windows` job) and keeps them as artifacts; releases attach them from
0.6.8.

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
docker build --target packages --build-arg VERSION=0.6.0-dev.20261028 --output dist .
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
- **The service is `Type=notify`:** the engine tells systemd when every interface is up
  (`systemctl start` waits for it; `systemctl status` shows "running N interfaces") and hands it
  each listening socket (`FileDescriptorStoreMax=`). systemd keeps them through a restart or a
  crash and gives them to the next start, so senders wait in the kernel's queue instead of being
  refused (measured: 0 refused, 0.2–0.3 s longest ACK wait for a restart; 5.4 s after a `kill -9`,
  which waits `RestartSec`). A stop, a failed start and a drain close the ports for real. No start
  timeout (`TimeoutStartSec=infinity`): a start migrates first, which can take long.
- **Upgrade:** a running service is handed over to the new version (`systemctl reload`:
  `ExecReload=ductus handover --with-environment`, asking the engine over `/run/ductus/control`;
  no connection refused, longest ACK wait 0.06–0.07 s for a reload and 0.17–0.22 s for an upgrade under real systemd 255 and 252); one that can't take over leaves the old version
  running and the install says so. A running version without the handover (before 0.6.6.3) is
  restarted instead; a stopped one stays stopped.
- **Remove:** stops and disables the service; keeps the user, the project and the settings.
- Logs go to the journal (`journalctl -u ductus`).
- Test: `scripts/demo-packages.sh` (systemd in containers: install, configure, a restart with and
  without the socket store and a `kill -9` under traffic, a reload under traffic and one with a
  broken project, upgrade under traffic (a handover), stop, remove;
  Ubuntu 24.04 with systemd 255 and Rocky 9 with 252).

## Windows (MSI and service)

```powershell
msiexec /i ductus-0.5.0-dev.20261012-x64.msi /qn      # silent: GPO, SCCM, Intune
notepad C:\ProgramData\Ductus\ductus.env             # DUCTUS_DATABASE_URL at least
Start-Service Ductus                                   # then Restart-Service after edits
```

Built by `packaging/windows/build.ps1` (WiX v5: `dotnet tool install --global wix --version 5.0.2`,
then `wix extension add --global WixToolset.Util.wixext/5.0.2`) from a release `ductus.exe`
(`cargo build --release -p ductus`, MSVC). Unsigned for now: SmartScreen warns on a downloaded
installer; a code-signing certificate is a later decision.

| Path | What |
|---|---|
| `C:\Program Files\Ductus\ductus.exe` | the engine and CLI; the folder is on the system PATH |
| `C:\ProgramData\Ductus\ductus.env` | settings (the variables above; `NAME=value` lines): the installer writes a template once and never overwrites it |
| `C:\ProgramData\Ductus\project\` | the project (a Git clone) |
| `C:\ProgramData\Ductus\logs\` | `ductus.<date>.log`, daily, the last 14 kept |
| service `Ductus` | "Ductus Integration Engine", `ductus.exe run --service --env-file C:\ProgramData\Ductus\ductus.env`, runs as the virtual account `NT SERVICE\Ductus` (or `SERVICEACCOUNT`), its own SID unrestricted |
| Event Log source `Ductus` | Application log; the text comes from a message table inside `ductus.exe` |

- **Secrets are never installer properties** (they would end up in the install log): the database
  password lives in `ductus.env`. `C:\ProgramData\Ductus` is readable by administrators, SYSTEM and
  the service only; the service has no rights anywhere else. Give it access to what interfaces
  use (a folder a file destination writes to, a share) with `icacls "D:\Out" /grant "NT SERVICE\Ductus:(OI)(CI)M"`.
- **The account:** `NT SERVICE\Ductus` by default. A gMSA: `msiexec /i ductus.msi /qn
  SERVICEACCOUNT=HOSPITAL\svc-ductus$` (no password; upgrades keep it). An account with a password:
  install, then `ductus service account HOSPITAL\svc-ductus --start` (asks for it;
  `--password-stdin` in scripts), and again after every upgrade (the installer keeps the name,
  leaves the service stopped and says so in the Event Log, event 1005). The installer and the
  command grant "Log on as a service"; the service's own SID is in its token whatever the account
  (`ServiceSidType` unrestricted), so the folder permissions above hold. An upgrade without
  `SERVICEACCOUNT` keeps the account the service has. Rules and Group Policy: `docs/nuances.md`,
  "Windows".
- **Install:** registers the service (automatic start) but doesn't start it: nothing is configured,
  and without a database it would stop at once. After editing `ductus.env`, `Start-Service Ductus`
  (or reboot).
- **Stop:** the service manager's Stop, a Windows shutdown (the service takes preshutdown, 90 s)
  and `ductus drain --stop` all stop cleanly, like SIGTERM on Linux. While starting and stopping
  it reports progress, so a long migration or a stop finishing the messages in hand isn't taken
  for a hang. If it fails it restarts after a minute, twice, then waits for a person.
- **Upgrade:** `msiexec /i` of a newer MSI (or another build of the same version: development
  builds replace each other; the installer's version is major.minor.patch) stops the service
  cleanly, replaces the program and, if `ductus.env` exists, starts it again, unless it was
  stopped on purpose (a Stop leaves `logs\stopped`, which the installer looks for; the next start
  removes it) or its account needs its password. Settings, project and logs are never touched.
  Installing the *same file* again is not an upgrade: Windows Installer only checks the installed
  product, removing and replacing nothing.
- **Uninstall:** removes the service, the program and the Event Log source; keeps `C:\ProgramData\Ductus`.
- `ductus dev` works on Windows too (its own Postgres), from a normal, non-elevated prompt.
- Test: `scripts/demo-windows.ps1 -Msi A -UpgradeMsi B` (elevated; two builds of the installer, so
  each upgrade is a real one; CI's `windows` job runs it on `windows-latest`): silent install,
  service account, SID type and settings, start, health, an MLLP round trip, the Event Log's text,
  clean stop, upgrade over a running install, an upgrade leaving a stopped service stopped, a named
  local account (logon right, process owner, a wrong password refused), an upgrade waiting for its
  password, uninstall, an install with `SERVICEACCOUNT`.
- Building on Windows: Rust (MSVC) and the VS Build Tools' C++ workload are enough (V8 comes
  prebuilt). rusty_v8's build script makes a symlink when the target folder and the cargo
  registry are on different drives, which needs Developer Mode or an elevated shell: keep
  `CARGO_HOME` and `CARGO_TARGET_DIR` on the same drive.
