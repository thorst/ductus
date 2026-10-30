# `ductus run`

Run every interface in a project.

```text
ductus run [OPTIONS]
```

**Options**

| | What it does | Default | Environment |
|---|---|---|---|
| `--root <ROOT>` | Project folder containing `interfaces/`. | `.` | `DUCTUS_PROJECT` |
| `--database-url <DATABASE_URL>` | The engine's database, as a PostgreSQL URL: `postgres://ductus@db1/ductus?sslmode=verify-full`. |  | `DUCTUS_DATABASE_URL` |
| `--threads <THREADS>` | Threads in the shared worker pool (default: one per CPU core). |  | `DUCTUS_THREADS` |
| `--db-connections <DB_CONNECTIONS>` | Maximum PostgreSQL connections. | `32` | `DUCTUS_DB_CONNECTIONS` |
| `--env <ENV>` | Environment: variables from environments/<env>.yaml, secrets from secrets/<env>.json. | `dev` | `DUCTUS_ENV` |
| `--api-listen <API_LISTEN>` | Where the REST API listens (`host:port`, or `off`), instead of `api.listen` in ductus.yaml (default 127.0.0.1:7700). |  | `DUCTUS_API_LISTEN` |
| `--stop-timeout <STOP_TIMEOUT>` | On Ctrl-C or SIGTERM, how long to finish the messages in hand before exiting anyway (they stay pending for the next start). Service managers should wait longer. | `30s` | `DUCTUS_STOP_TIMEOUT` |
| `--service` | Run as a Windows service, under the service manager (the installer sets this up). |  |  |

