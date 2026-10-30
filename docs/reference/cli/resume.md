# `ductus resume`

Take new messages again after `ductus drain`; or, with an interface, resume its paused destination (it sends its backlog in order).

```text
ductus resume [OPTIONS] [INTERFACE] [DESTINATION]
```

**Arguments**

| | What it does | Default | Environment |
|---|---|---|---|
| `<INTERFACE>` | Resume this interface's paused destination (instead of ending a drain). |  |  |
| `<DESTINATION>` | Which destination (needed when the interface has several). |  |  |

**Options**

| | What it does | Default | Environment |
|---|---|---|---|
| `--api <API>` | The engine's REST API. | `http://127.0.0.1:7700` | `DUCTUS_API_URL` |
| `--token <TOKEN>` | An operator token (`ductus token create <name> --role operator`). |  | `DUCTUS_API_TOKEN` |

