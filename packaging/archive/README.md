# Ductus, the portable archive

The engine as one program, without an installer: for servers with no internet access, for
checking an upgrade before installing it, and for trying a version beside the installed one.
A development build: not for production before v1.0.

| File | What |
|---|---|
| `ductus` (`ductus.exe` on Windows) | the engine and its command line; the web UI is inside it |
| `LICENSE` | Apache-2.0 |
| `ductus.service`, `ductus.env` (Linux) | the systemd unit and settings file the packages install, as samples |

## Before an upgrade

Run the new version's program against the database and project the engines use now. It reads
only: nothing is changed, and a read-only database login is enough (`GRANT pg_read_all_data TO
ductus_check`).

```sh
DUCTUS_DATABASE_URL=postgres://ductus_check@db/ductus ./ductus upgrade --check --root /var/lib/ductus/project --env prod
```

On a server with the package installed, `--env-file /etc/ductus/ductus.env` (Windows:
`C:\ProgramData\Ductus\ductus.env`) gives it the same settings as the service.

It says which database changes the new version's first start makes (and how big the tables they
work through are now), which engines run on the database and on which versions, whether the
project validates against the database and passes its tests with the new version, and which
settings it deprecates. It exits 1 on anything that would refuse the start or fail the project.
`--json` gives one JSON document instead.

## Installing from the archive

Prefer the packages (`.deb`, `.rpm`) or the Windows installer (`.msi`): they set up the service,
its user and its folders, and upgrade in place. Without them (Linux):

```sh
sudo install -m 0755 ductus /usr/bin/ductus
sudo useradd --system --home-dir /var/lib/ductus --create-home --shell /usr/sbin/nologin ductus
sudo install -m 0640 -g ductus ductus.env /etc/ductus/ductus.env   # then edit it
sudo install -m 0644 ductus.service /usr/lib/systemd/system/ductus.service
sudo systemctl daemon-reload && sudo systemctl enable --now ductus
```

The program needs glibc 2.34 or later (RHEL 9 and its rebuilds, Ubuntu 22.04, Debian 12 and later)
on Linux, and 64-bit Windows.
