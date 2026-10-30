# `ductus ports`

List the ports and HTTP routes a project listens on, the hosts it connects to, and port conflicts, for an environment (exits 1 on conflicts).

```text
ductus ports [OPTIONS]
```

**Options**

| | What it does | Default | Environment |
|---|---|---|---|
| `--root <ROOT>` | The project folder: the one with `interfaces/` and ductus.yaml. | `.` |  |
| `--env <ENV>` | The environment: `${NAME}` values come from environments/<env>.yaml, `${secret:NAME}` from secrets/<env>.json. | `dev` | `DUCTUS_ENV` |

