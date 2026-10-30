# `ductus status`

Show message counts by interface and status (from the database, also while the engine is down); with a token, also each interface's and destination's health from the running engine (silent, behind, stalled, paused, catching up...).

```text
ductus status [OPTIONS]
```

**Options**

| | What it does | Default | Environment |
|---|---|---|---|
| `--database-url <DATABASE_URL>` | The engine's database, as a PostgreSQL URL: `postgres://ductus@db1/ductus?sslmode=verify-full`. |  | `DUCTUS_DATABASE_URL` |
| `--api <API>` | The engine's REST API. | `http://127.0.0.1:7700` | `DUCTUS_API_URL` |
| `--token <TOKEN>` | An operator token (`ductus token create <name> --role operator`). |  | `DUCTUS_API_TOKEN` |

