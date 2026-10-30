# `ductus pause`

Pause a destination at once: nothing is sent or retried until `ductus resume <interface> [destination]`; messages keep queueing. Act first, explain after: the reason is optional, and running it again with --reason adds one. Through the running engine's REST API.

```text
ductus pause [OPTIONS] <INTERFACE> [DESTINATION]
```

**Arguments**

| | What it does | Default | Environment |
|---|---|---|---|
| `<INTERFACE>` | The interface whose destination to pause. |  |  |
| `<DESTINATION>` | Which destination (needed when the interface has several). |  |  |

**Options**

| | What it does | Default | Environment |
|---|---|---|---|
| `--reason <REASON>` | Why (can be added later). |  |  |
| `--api <API>` | The engine's REST API. | `http://127.0.0.1:7700` | `DUCTUS_API_URL` |
| `--token <TOKEN>` | An operator token (`ductus token create <name> --role operator`). |  | `DUCTUS_API_TOKEN` |

