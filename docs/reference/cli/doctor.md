# `ductus doctor`

Check every connection the project needs, from this server: DNS, TCP (with the address a firewall request needs), TLS (certificate, expiry), then each system's own login. Explains failures in plain words. Never sends anything to a destination. Exits 1 if something failed.

```text
ductus doctor [OPTIONS]
```

**Options**

| | What it does | Default | Environment |
|---|---|---|---|
| `--interface <INTERFACE>` | Only what these interfaces use (repeat for several); the engine's database always. |  |  |
| `--timeout <TIMEOUT>` | Longest wait per step, e.g. 5s or 500ms. | `5s` |  |
| `--report <REPORT>` | Also save the report (for a network team or vendor): JSON if the name ends in .json. |  |  |
| `--root <ROOT>` | The project folder: the one with `interfaces/` and ductus.yaml. | `.` |  |
| `--env <ENV>` | The environment: `${NAME}` values come from environments/<env>.yaml, `${secret:NAME}` from secrets/<env>.json. | `dev` | `DUCTUS_ENV` |
| `--database-url <DATABASE_URL>` | The engine's database, as a PostgreSQL URL: `postgres://ductus@db1/ductus?sslmode=verify-full`. |  | `DUCTUS_DATABASE_URL` |

