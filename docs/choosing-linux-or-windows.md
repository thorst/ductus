# Choosing Linux or Windows

Ductus runs on both, from one code base and one project format: the same interfaces, the same
YAML, the same TypeScript, the same database. A project moves from one to the other with only
its file paths changed. Where they differ is in how each operating system starts, stops and
upgrades a service, how it logs in to other Windows systems, and how much the build has been
exercised. This page lists every difference we know of, each with the measurement or nuance
behind it.

**Our recommendation: Linux**, unless the engine must log in to SQL Server with Windows
accounts, or your team runs only Windows servers and would rather not keep one Linux server.
Both are supported, and both behave the same in everything not in this table.

## The differences

| | Linux (recommended) | Windows | What it means for you |
|---|---|---|---|
| **Install** | [`.deb` or `.rpm`](install/linux.md) (Ubuntu 22.04+, Debian 12+, RHEL/Rocky/Alma 9), or the [`.tar.gz`](install/archive.md) | The [MSI](install/windows.md), or the [`.zip`](install/archive.md) | Same engine and project files on both; a project moves between them, paths aside. |
| **Runs as** | A systemd service, user `ductus`, sandboxed by the unit: it can write only `/var/lib/ductus` ([the unit](install/linux.md#what-the-package-installs)) | A Windows service, `NT SERVICE\Ductus` by default; a gMSA, or a password account that needs one command after each upgrade ([nuance](nuances/windows.md#the-services-account-the-default-a-gmsa-or-a-named-account-level-2)) | On Windows the account is what SQL Server and file shares see; on both, the engine can touch only what you give it ([Windows](nuances/windows.md#the-windows-service-can-only-touch-what-you-give-it-level-2)). |
| **Restart, crash** | systemd keeps the listening sockets: **0 connections refused**; senders wait 0.2–0.3 s for a restart, ~5 s after a crash ([measured](nuances/upgrades.md#on-linux-a-restart-refuses-no-connection-senders-wait-for-it-level-2)) | `Restart-Service`: 0 refused, a few connections reset, longest wait ~1.1 s ([measured](nuances/upgrades.md#windows-and-containers-a-restart-is-a-short-gap-level-2)) | Senders resend on their own in both cases. |
| **Upgrade** | **The handover**: 0 refused, sending paused ~35 ms (1 interface) to ~0.6 s (1,001 interfaces), receiving never stops; a broken new version leaves the old one running ([measured](nuances/upgrades.md#systemctl-reload-hands-over-receiving-never-stops-sending-pauses-briefly-level-2), [broken version](nuances/upgrades.md#a-new-version-or-project-that-cant-start-leaves-the-running-one-serving-level-2)) | The MSI: the engine is down **3.1–3.5 s** (a few refused, nothing lost); where Group Policy turns Restart Manager off, msiexec first waits 100–160 s while the engine keeps running ([measured](nuances/upgrades.md#windows-and-containers-a-restart-is-a-short-gap-level-2), [why the wait](nuances/windows.md#what-goes-to-the-event-log-and-what-only-to-the-files-level-2)). Zero-downtime Windows upgrades are planned for v1.0 | Linux upgrades need no quiet moment; on Windows, pick one. |
| **Deploy project changes** | `systemctl reload ductus`: the same handover ([nuance](nuances/upgrades.md#deploying-project-changes-systemctl-reload-level-1)) | `Restart-Service Ductus`: the same short gap as a restart | As above. |
| **SQL Server Windows logins** | NTLM only, with a warning; no Kerberos before v1.0 ([nuance](nuances/sqlserver-windows-logins.md#from-linux-a-windows-login-is-always-ntlm-level-3)) | Integrated: Kerberos, with NTLM as Windows falls back; as a gMSA or a named account ([nuance](nuances/sqlserver-windows-logins.md#auth-windows-the-engines-own-account-or-a-named-one-level-1)) | A DBA who allows only Windows logins: prefer Windows. |
| **An outgoing connection refused** | Known at once | Takes ~1 s (Windows tries again before it says so); `localhost` may be `::1` first ([measured](nuances/windows.md#a-refused-connection-takes-about-a-second-on-windows-and-localhost-may-be-1-first-level-3)) | Write `127.0.0.1`, not `localhost`; keep an MLLP destination's `timeout_ms` above ~2 s on Windows. |
| **Logs** | The journal: `journalctl -u ductus` | Files in `C:\ProgramData\Ductus\logs` (14 days), and the Event Log for what operations watch: [IDs 1000–1100](reference/event-log.md), each kind at most once a minute ([nuance](nuances/windows.md#what-goes-to-the-event-log-and-what-only-to-the-files-level-2)) | SCOM, Splunk and Event Viewer read the Event Log on Windows; the journal feeds the same tools on Linux. |
| **Certificates** | The system's store; a private CA (for LDAP, Postgres, a partner) usually needs `ca:` ([nuance](nuances/tls-connecting.md#a-private-ca-is-trusted-only-where-you-name-it-level-2)) | The machine's store; the domain's CA is usually there already ([nuance](nuances/tls-postgres.md#a-ca-installed-on-this-server-counts-level-2)) | Name a private CA per connection on both; it always works. |
| **Paths** | `/etc/ductus`, `/var/lib/ductus` | `C:\ProgramData\Ductus`; a path without a drive is read from the project's drive; single-quote backslashes in YAML ([nuance](nuances/windows.md#certificate-files-write-the-drive-level-1)) | `ductus validate` warns about Linux paths on Windows. |
| **Containers** | The [Docker image](install/docker.md) (a development build, not published before v1.0) | None | Kubernetes is planned for v1.2. |
| **Tested on every push** | Every test, all the end-to-end demos, the packages under real systemd (255 and 252), the web UI in a browser | Every Rust test except those needing Linux helpers (a TLS Postgres, OpenLDAP, sending from 127.0.0.2), SQL Server Express with Windows logins, the MSI (install, service, Event Log, upgrades, uninstall) ([the workflow](https://github.com/thorst/ductus/blob/main/.github/workflows/ci.yml)) | Linux has more mileage. Neither has been tested against a real Active Directory yet: the first pilot with a domain does that. |

## Plan for these on either

- **Reboots** (patch nights): a reboot is a stop and a start. The engine stops cleanly (Windows
  gives the service its preshutdown time, 90 s; systemd sends SIGTERM and waits 45 s), and it
  starts with the server. While the server is down, senders are refused and retry, as with any
  engine; systemd's socket store doesn't survive a reboot. Reboot in a quiet moment, or move the
  interfaces to another engine first ([several engines](nuances/engines.md#an-interface-runs-on-one-engine-at-a-time-a-second-one-is-refused-level-2)).
- **Antivirus and endpoint protection** that scans every file write slows the database far more
  than the engine: the engine writes only its logs; Postgres writes every message. Ask for the
  usual exclusions for Postgres's data and WAL folders on its server (your DBA will know them),
  and for the engine's project folder if a scan on read slows its start.
- **Postgres** is the same on both: the DBA's server, on Linux or Windows, whatever the engine
  runs on ([PostgreSQL](install/postgres.md)).
- **Time zone:** cron schedules, quiet hours and `monitor:` times are in the server's local time
  on both. A server (or container) that runs on UTC reads `"06:00-22:00"` as UTC.

## How the two are kept the same

One code base and one project format; the whole Rust test suite runs on both on every push;
Windows-only behaviour is written down as a [nuance](nuances/windows.md) the day it's found (the
Windows section, and the Windows rows in other sections). When a difference is fixed (a resident
launcher for zero-downtime Windows upgrades is planned for v1.0, a Kerberos keytab on Linux too),
its row here changes in the same commit.
