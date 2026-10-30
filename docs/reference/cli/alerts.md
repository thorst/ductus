# `ductus alerts`

Alerts (`alerts:` in ductus.yaml): what is open, from the running engine's REST API (viewer token); or `ductus alerts test <channel>`, which sends a test notification straight from ductus.yaml (no engine needed: check the mail server or webhook before going live).

```text
ductus alerts [OPTIONS] [COMMAND]
```

**Options**

| | What it does | Default | Environment |
|---|---|---|---|
| `--api <API>` | The engine's REST API. | `http://127.0.0.1:7700` | `DUCTUS_API_URL` |
| `--token <TOKEN>` | An operator token (`ductus token create <name> --role operator`). |  | `DUCTUS_API_TOKEN` |

## `ductus alerts test`

Send a test notification to a channel now, from ductus.yaml in this environment.

```text
ductus alerts test [OPTIONS] <CHANNEL>
```

**Arguments**

| | What it does | Default | Environment |
|---|---|---|---|
| `<CHANNEL>` | A channel's name under `alerts.channels` in ductus.yaml. |  |  |

**Options**

| | What it does | Default | Environment |
|---|---|---|---|
| `--root <ROOT>` | The project folder: the one with `interfaces/` and ductus.yaml. | `.` |  |
| `--env <ENV>` | The environment: `${NAME}` values come from environments/<env>.yaml, `${secret:NAME}` from secrets/<env>.json. | `dev` | `DUCTUS_ENV` |

