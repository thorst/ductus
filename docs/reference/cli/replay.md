# `ductus replay`

Run a period's messages through this version's code again, sending nothing, and compare what each destination's code decides (sent and what, filtered, quarantined) with the installed version run beside it, with what was recorded, and with kept outputs (`save_output`): the evidence a change board asks for before an upgrade or a code change. Reads only (a read-only login or a replica is enough). Exits 1 when anything differs.

```text
ductus replay [OPTIONS]
```

**Options**

| | What it does | Default | Environment |
|---|---|---|---|
| `--root <ROOT>` | The project folder: the one with `interfaces/` and ductus.yaml. | `.` | `DUCTUS_PROJECT` |
| `--env <ENV>` | The environment: `${NAME}` values come from environments/<env>.yaml, `${secret:NAME}` from secrets/<env>.json. | `dev` | `DUCTUS_ENV` |
| `--database-url <DATABASE_URL>` | The engine's database, as a PostgreSQL URL: `postgres://ductus@db1/ductus?sslmode=verify-full`. |  | `DUCTUS_DATABASE_URL` |
| `--since <SINCE>` | The period's start: a duration back from its end (`24h`, `7d`) or a time (RFC 3339). | `24h` |  |
| `--until <UNTIL>` | The period's end, a time (RFC 3339); default now. |  |  |
| `--interface <INTERFACE>` | Only this interface (repeat it for more). |  |  |
| `--limit <LIMIT>` | At most this many messages per interface, the first in the period: a sample. |  |  |
| `--against <AGAINST>` | What it's compared with besides the record: `installed` (the version installed on this server, run beside this one: the default), `stored` (kept outputs only), or the path of another ductus binary. |  |  |
| `--baseline <BASELINE>` | Compare with another version of the project instead: a folder, or a Git revision of this one (`main`, `v12`, `HEAD~1`): the same report for a code change. It runs with this binary unless --against names one. |  |  |
| `--report <REPORT>` | Also write the report to this file: a page a change board can read and sign (.html), or JSON (.json). |  |  |
| `--json` | The report as JSON on stdout instead of text. |  |  |
| `--hide-values` | Leave field values out of the examples: the report then holds no patient data. |  |  |

