# `ductus upgrade`

Before an upgrade, run with the new version's binary (from the portable archive, on the server or beside it), reading only: the migrations its start would apply and how big the tables they work through are, the engines on the database and their versions, the project validated against the database and its tests run with the new SDK, deprecated settings in use. Exits 1 on anything that would refuse the start or fail the project. A read-only login is enough (`pg_read_all_data`).

```text
ductus upgrade [OPTIONS] --check
```

**Options**

| | What it does | Default | Environment |
|---|---|---|---|
| `--check` | Check, change nothing (the only mode for now). |  |  |
| `--json` | One JSON document instead of the report. |  |  |
| `--root <ROOT>` | The project folder: the one with `interfaces/` and ductus.yaml. | `.` | `DUCTUS_PROJECT` |
| `--env <ENV>` | The environment: `${NAME}` values come from environments/<env>.yaml, `${secret:NAME}` from secrets/<env>.json. | `dev` | `DUCTUS_ENV` |
| `--database-url <DATABASE_URL>` | The engine's database, as a PostgreSQL URL: `postgres://ductus@db1/ductus?sslmode=verify-full`. |  | `DUCTUS_DATABASE_URL` |
| `--replay <DURATION>` | Also replay this much of the latest traffic (`1d`, `12h`) through the new version and compare it with the installed one (`ductus replay`). |  |  |

