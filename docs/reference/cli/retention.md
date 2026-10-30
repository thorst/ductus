# `ductus retention`

How long each interface keeps its messages (`retention:` in interface.yaml), changes in progress, shorter values waiting for a confirmation, and expired partitions that had to stay. Through the running engine's REST API.

```text
ductus retention [OPTIONS] [COMMAND]
```

**Options**

| | What it does | Default | Environment |
|---|---|---|---|
| `--api <API>` | The engine's REST API. | `http://127.0.0.1:7700` | `DUCTUS_API_URL` |
| `--token <TOKEN>` | An operator token (`ductus token create <name> --role operator`). |  | `DUCTUS_API_TOKEN` |

## `ductus retention confirm`

Apply an interface's shorter retention: it deletes the messages past it (operator token).

```text
ductus retention confirm [OPTIONS] <INTERFACE>
```

**Arguments**

| | What it does | Default | Environment |
|---|---|---|---|
| `<INTERFACE>` | The interface whose shorter retention to apply. |  |  |

