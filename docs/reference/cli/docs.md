# `ductus docs`

The company-defined documentation fields (`docs-schema.yaml`) and their values (`custom:`) for every interface, destination and resource. Exits 1 if a required value is missing.

```text
ductus docs [OPTIONS]
```

**Options**

| | What it does | Default | Environment |
|---|---|---|---|
| `--missing` | Only what's missing. |  |  |
| `--json` | As JSON, for a report or another tool. |  |  |
| `--root <ROOT>` | The project folder: the one with `interfaces/` and ductus.yaml. | `.` |  |
| `--env <ENV>` | The environment: `${NAME}` values come from environments/<env>.yaml, `${secret:NAME}` from secrets/<env>.json. | `dev` | `DUCTUS_ENV` |

