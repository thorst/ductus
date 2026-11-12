# `ductus handover`

Hand the running engine over to a new one on the project (or release) as it is now, refusing no connection: a reload. The new engine starts beside the running one on the same listening sockets; once it's ready, the running one stops and lets go. Linux: the package's `systemctl reload ductus` runs it, and the new engine is the binary now on disk (an upgrade too); exit 0 once it sends, 1 when it couldn't take over (the running engine goes on: the reason is printed). Windows: asks the service (`sc.exe control Ductus paramchange`, from an elevated shell); the new engine runs in the service's process, with ductus.env read again, and the service's log and the Event Log say how it went.

```text
ductus handover [OPTIONS]
```

**Options**

| | What it does | Default | Environment |
|---|---|---|---|
| `--control <CONTROL>` | The running engine's control socket (default: `$RUNTIME_DIRECTORY/control`, which systemd sets for the service and its reload, else /run/ductus/control). |  | `DUCTUS_CONTROL` |
| `--timeout <TIMEOUT>` | How long the new engine may take to be ready: it migrates the database first. | `5m` |  |
| `--with-environment` | Start the new engine with this command's environment rather than the running engine's: under systemd, ductus.env as it is now (the unit's reload passes this). |  |  |
| `-m, --message <REASON>` | Why: recorded with the new engine's deployment, beside who asked (the person behind `sudo`, else this account; `systemctl reload` is recorded as systemd's, with no reason). Example: `sudo ductus handover -m "lab wants PV1-3 mapped"`. |  |  |

