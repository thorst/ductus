# Linux packages

The `.deb` (Ubuntu, Debian) and `.rpm` (RHEL, Rocky, Alma) install the engine as a systemd
service with its own user, a settings file and a folder for the project. Upgrades hand the
running engine over to the new version with no connection refused.

**Use them** on any Linux server that runs Ductus for real. **Don't** where you can't install
packages (then the [portable archive](archive.md) has the same unit and settings file as
samples), or in a container (the [image](docker.md)).

Both packages hold the same binary, built once on Rocky Linux 9: it runs wherever glibc is 2.34
or later (RHEL 9 and its rebuilds, Ubuntu 22.04, Debian 12 and later). RHEL 8 (glibc 2.28) can't
run it.

## Install

```sh
# Ubuntu, Debian
sudo apt install ./ductus_0.6.0_amd64.deb
# RHEL, Rocky, Alma
sudo dnf install ./ductus-0.6.0-1.x86_64.rpm
```

Before v0.6.0 is released, packages are development builds: `docker build --target packages
--build-arg VERSION=0.6.0-dev.1 --output dist .` in a clone of the repository makes both.

The install creates the `ductus` user and group, and doesn't start anything: nothing is
configured yet. It prints what to do next, which is the rest of this page.

## What the package installs

| Path | What |
|---|---|
| `/usr/bin/ductus` | The engine and its command line (the web UI is inside it). |
| `/usr/lib/systemd/system/ductus.service` | The service: runs as `ductus`, sandboxed (below). |
| `/etc/ductus/ductus.env` | Settings: the database URL, the project, the environment, the secrets key. Readable by root and the `ductus` group only. An upgrade never replaces it. |
| `/var/lib/ductus` | The service's home and the only place it can write. The project goes in `/var/lib/ductus/project`. |

**The sandbox.** The unit lets the engine use the network and its own folder, and nothing else:
the rest of the file system is read-only to it (`ProtectSystem=strict`), home folders are
hidden (`ProtectHome=yes`), and it has no capabilities. That's on purpose: one bad interface
can't write anywhere on the server. It means a [file destination](../reference/config/interface.md#destinationfile)
or a file source that moves files must work in a folder you give it:

```sh
sudo systemctl edit ductus
# in the editor that opens:
#   [Service]
#   ReadWritePaths=/data/outbound /data/inbound
sudo systemctl restart ductus
```

Without that the interface's messages fail with "Read-only file system", and wait to be tried
again ([nuance](../nuances.md#on-linux-the-service-can-write-only-varlibductus-until-you-add-a-folder-level-2)).
A folder for a source needs the same when the source moves or deletes what it read (the default,
`after: move`). A port below 1024 (an MLLP port of 104, say) needs a capability too; the unit
says which two lines to add with `systemctl edit`.

## Configure

1. **Settings.** Edit `/etc/ductus/ductus.env` (as root). At least the database:

   ```sh
   # /etc/ductus/ductus.env
   DUCTUS_DATABASE_URL=postgres://ductus:PASSWORD@db1.hospital.org/ductus?sslmode=verify-full
   DUCTUS_PROJECT=/var/lib/ductus/project
   DUCTUS_ENV=prod
   DUCTUS_SECRET_KEY=…
   ```

   The file's comments explain each line; every variable is in the
   [install overview](index.md#what-every-install-has-in-common). For the database login and
   TLS, see [PostgreSQL](postgres.md). `DUCTUS_SECRET_KEY` is the key your project's
   `secrets/prod.json` was encrypted with ([secrets](../configure/environments.md#secrets)).

2. **The project.** Clone it as the service's user, so the files are its own:

   ```sh
   sudo -u ductus git clone git@git.hospital.org:interfaces/ductus.git /var/lib/ductus/project
   ```

   The `ductus` user has no shell (`nologin`), but `sudo -u ductus` runs a command as it. For a
   repository that needs a login, give the user a deploy key that can only read
   (`/var/lib/ductus/.ssh/`), or copy the project in from your deployment tool. The engine only
   reads the project: it never writes there.

3. **Check before starting.** As root or the `ductus` user, with the service's own settings:

   ```sh
   sudo -u ductus ductus validate --env-file /etc/ductus/ductus.env --root /var/lib/ductus/project
   sudo -u ductus ductus doctor --env-file /etc/ductus/ductus.env --root /var/lib/ductus/project
   ```

   `validate` checks the project as the engine will read it, in the environment it will run in,
   and against the database (reading only): lane changes that would strand waiting messages,
   retention changes still running. `--root` is needed: unlike `run`, these commands don't read
   `DUCTUS_PROJECT`. `doctor` connects to everything the project connects to, from this
   server, and says in words what a failure means: a firewall, a certificate, a login.

## Start, stop, look

```sh
sudo systemctl enable --now ductus     # start now and at every boot
systemctl status ductus                # "running 12 interfaces" once every one is up
journalctl -u ductus -f                # the log; "started interfaces=12 …" when ready
```

- **`systemctl start` waits** until every interface is up (the engine tells systemd when it's
  ready). A start migrates the database first, so after an upgrade it can take longer; there's no
  time limit.
- **A stop is clean**: no new connections, the messages in hand finished and saved, exit 0
  ([what finishes](../nuances.md#stopping-the-engine-what-finishes-and-what-waits-level-2)).
  systemd waits 45 s, longer than the engine's own limit (`DUCTUS_STOP_TIMEOUT`, 30 s).
- **A restart or a crash refuses no connection**: systemd keeps the listening sockets and hands
  them to the next start; senders wait in the queue (0.2–0.3 s for a restart, ~5 s after a
  crash: [measured](../nuances.md#on-linux-a-restart-refuses-no-connection-senders-wait-for-it-level-2)).
  A stop, a failed start and a drain do close the ports, on purpose
  ([why](../nuances.md#a-stopped-service-a-failed-start-and-a-drain-refuse-connections-on-purpose-level-2)).
- **A refused start isn't retried.** Exit code 3 means another engine runs some of these
  interfaces on the same database, or a change to an interface's lanes would strand waiting
  messages. Trying again changes neither, so systemd doesn't (`RestartPreventExitStatus=3`): the
  log says what to do, then `systemctl start ductus`. Any other failure is retried every 5 s.
- **Logs** go to the journal, plain text with fields (`interface=adt-in`). `RUST_LOG` in
  ductus.env sets how much: `info` by default.

## Deploy a change, upgrade

**Project changes** (new code, a new interface): pull the project, then reload. The new project
starts beside the running engine and takes over once it's ready; nothing is refused, and a
project that can't start leaves the running one serving
([deploying](../nuances.md#deploying-project-changes-systemctl-reload-level-1)):

```sh
sudo -u ductus git -C /var/lib/ductus/project pull
sudo -u ductus ductus validate --env-file /etc/ductus/ductus.env --root /var/lib/ductus/project
sudo systemctl reload ductus
```

**Upgrades**: run the new version's `ductus upgrade --check` from its
[portable archive](archive.md#before-an-upgrade) first, then install the new package the same
way as the first. The package hands the running engine over to the new version
(`systemctl reload` again: 0 refused, sending paused for a moment:
[measured](../nuances.md#systemctl-reload-hands-over-receiving-never-stops-sending-pauses-briefly-level-2)).
A new version that can't start leaves the old one running, and the install says so. Rolling back
is installing the previous package: the database changes a version makes only ever add, so the
version before runs on them.

## Remove

`sudo apt remove ductus` (or `dnf remove`) stops and disables the service and removes the
program. The `ductus` user, `/etc/ductus/ductus.env` and `/var/lib/ductus` (the project) stay,
and so does everything in the database: removing Ductus never deletes messages.
