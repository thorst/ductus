# `ductus audit`

The audit log (who did what), straight from the engine's database.

```text
ductus audit [OPTIONS] <COMMAND>
```

## `ductus audit export`

Print the log as one JSON object per line, oldest first (for a SIEM or an auditor). The export is itself logged.

```text
ductus audit export [OPTIONS]
```

**Options**

| | What it does | Default | Environment |
|---|---|---|---|
| `--since <SINCE>` | From this day (`2026-01-31`, UTC) or time (`2026-01-31T08:00:00Z`). |  |  |
| `--until <UNTIL>` | Until this day or time (exclusive). |  |  |
| `--actor <ACTOR>` | A user's or token's name. |  |  |
| `--action <ACTION>` | An action (`message.dismiss`) or a group (`message`, `user`, `session`). |  |  |
| `--interface <INTERFACE>` | Only what concerns this interface. |  |  |
| `--message <MESSAGE>` | A message id. |  |  |

