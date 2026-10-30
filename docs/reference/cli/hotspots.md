# `ductus hotspots`

Where each destination's backlog sits: the lanes messages wait in (the ordering key's hash), the first message of each (key, age, attempts, last error), and which worker a failing message holds up. Through the running engine's REST API (viewer token).

```text
ductus hotspots [OPTIONS] [INTERFACE]
```

**Arguments**

| | What it does | Default | Environment |
|---|---|---|---|
| `<INTERFACE>` | Only this interface. |  |  |

**Options**

| | What it does | Default | Environment |
|---|---|---|---|
| `--lanes <LANES>` | Lanes to list per destination, most waiting first (default 10). |  |  |
| `--api <API>` | The engine's REST API. | `http://127.0.0.1:7700` | `DUCTUS_API_URL` |
| `--token <TOKEN>` | An operator token (`ductus token create <name> --role operator`). |  | `DUCTUS_API_TOKEN` |

