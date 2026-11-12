# `ductus validate`

Check configs, code loading and TypeScript types for an environment (exits 1 on problems).

```text
ductus validate [OPTIONS]
```

**Options**

| | What it does | Default | Environment |
|---|---|---|---|
| `--root <ROOT>` | The project folder: the one with `interfaces/` and ductus.yaml. | `.` |  |
| `--env <ENV>` | The environment: `${NAME}` values come from environments/<env>.yaml, `${secret:NAME}` from secrets/<env>.json. | `dev` | `DUCTUS_ENV` |
| `--database-url <DATABASE_URL>` | Also check the project against this database, reading only: lane changes the next start would make, retention changes still running or waiting for a confirmation. |  | `DUCTUS_DATABASE_URL` |
| `--json` | One JSON document for tools (an editor, CI) instead of the text: every finding with its severity, code (reference/validate.md), file, line and column, whether TypeScript or Ductus found it, and a fix where there is one. |  |  |

