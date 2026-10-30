# `ductus dev`

Run the project with a Postgres of its own, for local development: nothing to install. The first run downloads Postgres (once per user); its data lives in `.ductus/postgres` and it stops with the engine. Prints the connection URL.

```text
ductus dev [OPTIONS]
```

**Options**

| | What it does | Default | Environment |
|---|---|---|---|
| `--root <ROOT>` | Project folder containing `interfaces/`. | `.` | `DUCTUS_PROJECT` |
| `--threads <THREADS>` | Threads in the shared worker pool (default: one per CPU core). |  | `DUCTUS_THREADS` |
| `--env <ENV>` | Environment: variables from environments/<env>.yaml, secrets from secrets/<env>.json. | `dev` | `DUCTUS_ENV` |
| `--api-listen <API_LISTEN>` | Where the REST API listens (`host:port`, or `off`). |  | `DUCTUS_API_LISTEN` |
| `--stop-timeout <STOP_TIMEOUT>` | On Ctrl-C, how long to finish the messages in hand before exiting anyway. | `30s` | `DUCTUS_STOP_TIMEOUT` |
| `--port <PORT>` | Postgres's port (default: the one kept from the last run, else a free one). |  |  |
| `--reset` | Delete this project's dev database and start again. |  |  |
| `--yes` | Don't ask before --reset deletes it. |  |  |
| `--database-only` | Start only the Postgres, without the engine, and print its URL. |  |  |

