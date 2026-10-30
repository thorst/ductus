# `ductus handover`

Hand the running engine over to the binary now on disk and the project as it is now, refusing no connection (Linux: the package's `systemctl reload ductus` runs it): an upgrade or a project deploy. The new engine starts beside the running one on the same listening sockets; once it's ready, the running one stops and lets go. Exit 0 once the new engine sends; 1 when it couldn't take over (the running engine goes on: the reason is printed).

```text
ductus handover [OPTIONS]
```

**Options**

| | What it does | Default | Environment |
|---|---|---|---|
| `--control <CONTROL>` | The running engine's control socket (default: `$RUNTIME_DIRECTORY/control`, which systemd sets for the service and its reload, else /run/ductus/control). |  | `DUCTUS_CONTROL` |
| `--timeout <TIMEOUT>` | How long the new engine may take to be ready: it migrates the database first. | `5m` |  |
| `--with-environment` | Start the new engine with this command's environment rather than the running engine's: under systemd, ductus.env as it is now (the unit's reload passes this). |  |  |

