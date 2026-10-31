# Windows installer

The MSI installs the engine as a Windows service, `Ductus`, with a settings file, a folder for
the project and its logs, and an Event Log source. It installs silently for Group Policy, SCCM
or Intune, and upgrades in place.

**Use it** on a Windows server that runs Ductus for real. **Don't** where you can't run an
installer (then the [`.zip`](archive.md) has the program alone), or to try Ductus on your own
machine ([`ductus dev`](dev.md) needs no installer and no administrator).

> **Unsigned before v1.0.** The installer isn't signed with a code-signing certificate yet, so
> Windows SmartScreen says "Windows protected your PC" and "unknown publisher" when it's run from
> a download. Check the file before you run it: its SHA-256 against the release's `SHA256SUMS`
> (`Get-FileHash ductus-0.6.0-x64.msi`), and that GitHub built it from the tagged source
> (`gh attestation verify ductus-0.6.0-x64.msi -R thorst/ductus`). Then "More info" → "Run
> anyway", or unblock it first (`Unblock-File`).

## Install

As an administrator:

```powershell
msiexec /i ductus-0.6.0-x64.msi /qn          # silent; without /qn, the usual wizard
```

Before v0.6.0 is released, MSIs are development builds: `packaging/windows/build.ps1` in a clone
of the repository makes one (it needs Rust, the Visual Studio Build Tools and WiX 5: see
`packaging/README.md`).

The install registers the service (it starts automatically at boot) but **doesn't start it**:
nothing is configured yet, and without a database it would stop at once.

## What the installer puts where

| Path | What |
|---|---|
| `C:\Program Files\Ductus\ductus.exe` | The engine and its command line (the web UI is inside it). The folder is on the system `PATH`. |
| `C:\ProgramData\Ductus\ductus.env` | Settings (`NAME=value` lines): the database URL, the project, the environment, the secrets key. Written once as a template; never overwritten. |
| `C:\ProgramData\Ductus\project\` | The project (a Git clone, usually). |
| `C:\ProgramData\Ductus\logs\` | `ductus.<date>.log`, a file a day, the last 14 kept. |
| Service `Ductus` | "Ductus Integration Engine": `ductus.exe run --service --env-file C:\ProgramData\Ductus\ductus.env`, as `NT SERVICE\Ductus` unless you choose another account (below). |
| Event Log source `Ductus` | In the Application log: what operations teams watch for ([IDs](../reference/event-log.md)). |

`C:\ProgramData\Ductus` can be read by administrators, SYSTEM and the service only: `ductus.env`
holds the database password. Keep it that way.

## The service's account

The account is what SQL Server, file shares and anything else on the network see when the
engine logs in with Windows. Three choices
([nuance](../nuances.md#the-services-account-the-default-a-gmsa-or-a-named-account-level-2)):

| Account | How | When |
|---|---|---|
| `NT SERVICE\Ductus` (the default) | Nothing to do. A virtual account: no password; on the network it is the computer (`HOSPITAL\SERVER01$`). | The engine logs in to nothing with Windows, or a DBA grants the computer's account. |
| A gMSA (group managed service account) | `msiexec /i ductus-0.6.0-x64.msi /qn SERVICEACCOUNT=HOSPITAL\svc-ductus$` | The engine needs its own identity on the network (a SQL Server Windows login, a share). Windows keeps the password; upgrades keep the account. **The best choice when you need one.** |
| An account with a password | Install, then `ductus service account HOSPITAL\svc-ductus --start` (it asks for the password; `--password-stdin` in scripts) | Only where a gMSA isn't possible: **after every upgrade** the service waits for the password again ([why](../nuances.md#an-upgrade-cant-keep-an-accounts-password-the-service-waits-for-it-level-2)). |

The installer (and `ductus service account`) grants the account "Log on as a service". Where
Group Policy manages that right, ask for the account to be added to the policy, or the next
policy refresh takes it away ([nuance](../nuances.md#log-on-as-a-service-the-installer-grants-it-group-policy-can-take-it-away-level-3)).
`ductus service account` with no name says who the service runs as now.

**Folders outside `C:\ProgramData\Ductus`.** The service can touch only what you give it: a file
interface's inbox, archive or output folder needs a grant
([nuance](../nuances.md#the-windows-service-can-only-touch-what-you-give-it-level-2)):

```powershell
icacls "D:\Interfaces" /grant "NT SERVICE\Ductus:(OI)(CI)M"
```

That works whatever account runs the service (its own SID is in its token). For a network share,
grant the computer account (`HOSPITAL\SERVER01$`), or the gMSA.

## Configure

1. **Settings.** Edit `C:\ProgramData\Ductus\ductus.env` as an administrator (Notepad run as
   administrator works). At least the database:

   ```text
   DUCTUS_DATABASE_URL=postgres://ductus:PASSWORD@db1.hospital.org/ductus?sslmode=verify-full
   DUCTUS_PROJECT=C:\ProgramData\Ductus\project
   DUCTUS_ENV=prod
   DUCTUS_SECRET_KEY=…
   ```

   No quotes are needed, and backslashes are plain backslashes here. Every variable is in the
   [install overview](index.md#what-every-install-has-in-common); the database's login and TLS
   are on the [PostgreSQL](postgres.md) page. **Secrets are never installer properties** (they'd
   be written to the install log): the password lives in this file only.

2. **The project.** Clone it into `C:\ProgramData\Ductus\project` (or point `DUCTUS_PROJECT`
   elsewhere and grant the service read access there). In YAML, write Windows paths with their
   drive and in single quotes (`'D:\Interfaces\in'`): in double quotes a backslash starts an
   escape ([nuance](../nuances.md#certificate-files-write-the-drive-level-1)).

3. **Check before starting**, with the service's own settings:

   ```powershell
   ductus validate --env-file C:\ProgramData\Ductus\ductus.env --root C:\ProgramData\Ductus\project
   ductus doctor --env-file C:\ProgramData\Ductus\ductus.env --root C:\ProgramData\Ductus\project
   ```

   These run as you, not as the service's account: a folder or a SQL Server Windows login can
   pass here and fail for the service. The service's log says "access denied" then.

## Start, stop, look

```powershell
Start-Service Ductus           # after a change to ductus.env: Restart-Service Ductus
Get-Service Ductus
Get-ChildItem C:\ProgramData\Ductus\logs\ductus.*.log | Sort-Object LastWriteTime |
  Select-Object -Last 1 | Get-Content -Tail 50 -Wait      # the newest log, as it grows
```

The log files are named by the date in UTC (`ductus.2026-10-30.log`), so near midnight the
newest one may carry tomorrow's or yesterday's date.

- **Starting and stopping report progress** to the service manager, so a long migration or a
  stop finishing the messages in hand isn't taken for a hang.
- **A stop is clean**, whether from the service manager, a Windows shutdown (the service takes
  its preshutdown time, 90 s) or `ductus drain --stop`: the messages in hand are finished and
  saved ([what finishes](../nuances.md#stopping-the-engine-what-finishes-and-what-waits-level-2)).
- **A restart is a short gap**: measured 0 refused, a few connections reset, the longest wait
  for an ACK ~1.1 s ([measured](../nuances.md#windows-and-containers-a-restart-is-a-short-gap-level-2)).
  Senders resend by themselves.
- **If it fails** it's started again after a minute, twice, then waits for a person. A start
  refused because another engine runs these interfaces isn't retried.
- **The Event Log** gets what operations watch for: started, stopped, couldn't start (with the
  reason), an interface stopped, a critical alert, and other errors at most once a minute per
  kind ([IDs](../reference/event-log.md), [nuance](../nuances.md#what-goes-to-the-event-log-and-what-only-to-the-files-level-2)).
  The detail is in the log files.

## Upgrade

Run the new version's `ductus upgrade --check` from its [`.zip`](archive.md#before-an-upgrade)
first, then the new MSI the same way as the first (`msiexec /i … /qn`; no need to uninstall).
The installer stops the service cleanly, replaces the program and starts it again; settings,
project and logs are never touched. The engine is down for **3.1–3.5 s** (measured: a few
connections refused, nothing lost), so pick a quiet moment
([measured](../nuances.md#windows-and-containers-a-restart-is-a-short-gap-level-2)).

- A service someone **stopped on purpose** stays stopped after the upgrade
  ([nuance](../nuances.md#an-upgrade-starts-the-service-again-unless-it-was-stopped-on-purpose-level-2)).
- A **password account** leaves the service stopped, waiting for
  `ductus service account NAME --start` (Event Log 1005 says so).
- Where Group Policy turns **Restart Manager** off, msiexec may wait 100–160 s before it stops the
  service (the engine keeps running meanwhile). Close Event Viewer first, or stop the service
  yourself first to save the wait ([why](../nuances.md#what-goes-to-the-event-log-and-what-only-to-the-files-level-2)).
- **Rolling back** is installing the previous MSI: the database changes a version makes only ever
  add, so the version before runs on them.

## Uninstall

"Apps" in Settings, or `msiexec /x ductus-0.6.0-x64.msi /qn`: removes the service, the program
and the Event Log source. `C:\ProgramData\Ductus` (settings, project, logs) stays, and so does
everything in the database.
