# The portable archive

The engine as one program, without an installer: `ductus-0.6.0-linux-x64.tar.gz` and
`ductus-0.6.0-windows-x64.zip`, each unpacking into a folder `ductus-0.6.0`. Unpack it anywhere
and run it; nothing is registered, and nothing is installed.

**Use it** to check an upgrade before installing it (the main reason it exists), to try a new
version beside the installed one, to run the command line on a workstation (`doctor`, `replay`,
`user`), and on servers that can't take packages. **Don't** use it as the way to run production
when a package will do: the packages set up the service, its user, its sandbox and its folders,
and upgrade in place with no connection refused.

| File | What |
|---|---|
| `ductus` (`ductus.exe`) | The engine and its command line; the web UI is inside it. |
| `LICENSE` | Apache-2.0. |
| `README.md` | What this page says, in short. |
| `ductus.service`, `ductus.env` (Linux only) | The systemd unit and settings file the packages install, as samples. |

## Before an upgrade

Run the **new** version's program against the database and project your engines use now,
before the change window:

```sh
tar xzf ductus-0.6.1-linux-x64.tar.gz && cd ductus-0.6.1
./ductus upgrade --check --env-file /etc/ductus/ductus.env --root /var/lib/ductus/project
```

```powershell
Expand-Archive ductus-0.6.1-windows-x64.zip -DestinationPath C:\Temp     # makes C:\Temp\ductus-0.6.1
C:\Temp\ductus-0.6.1\ductus.exe upgrade --check --env-file C:\ProgramData\Ductus\ductus.env --root C:\ProgramData\Ductus\project
```

It reads only, changing nothing in the database or the project, and says what the new version's
first start would do: the database changes it makes (and how big the tables they work through
are now), which engines run on the database and on which versions, whether the project
validates and passes its tests with the new version, and which settings it deprecates. It exits
1 on anything that would refuse the start or fail the project; `--json` gives one document to
attach to a change ticket ([nuance](../nuances.md#run-ductus-upgrade---check-with-the-new-version-before-every-upgrade-level-1)).

A read-only database login is enough, and safer for a program you're trying out:

```sql
CREATE ROLE ductus_check LOGIN PASSWORD '…';
GRANT pg_read_all_data TO ductus_check;
```

Then `DUCTUS_DATABASE_URL=postgres://ductus_check:…@db1.hospital.org/ductus?sslmode=verify-full`
in place of `--env-file` (keep `--env prod` and the secrets key, which the check needs to read
the project as the engine does). The same login suits `ductus replay`, which runs a day's
traffic through the new version and compares
([nuance](../nuances.md#ductus-replay-shows-what-a-change-would-do-to-real-traffic-before-you-make-it-level-1)).

## Installing from the archive

Prefer the packages. Without them, on Linux, the archive's samples give the same setup by hand:

```sh
sudo install -m 0755 ductus /usr/bin/ductus
sudo useradd --system --home-dir /var/lib/ductus --create-home --shell /usr/sbin/nologin ductus
sudo install -d -m 0755 /etc/ductus
sudo install -m 0640 -g ductus ductus.env /etc/ductus/ductus.env   # then edit it
sudo install -m 0644 ductus.service /usr/lib/systemd/system/ductus.service
sudo systemctl daemon-reload && sudo systemctl enable --now ductus
```

Everything on [Linux packages](linux.md) from "What the package installs" on then applies,
except that upgrades are yours: replace `/usr/bin/ductus` and run `sudo systemctl reload ductus`
(the handover, as the package does). On Windows, the `.zip` has no service setup: use the MSI
for a service; `ductus.exe` from the zip is for the command line and for trying a version.

The program needs glibc 2.34 or later on Linux (RHEL 9 and its rebuilds, Ubuntu 22.04, Debian 12
and later), and 64-bit Windows.
