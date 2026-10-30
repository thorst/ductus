# `ductus drain`

Stop taking new messages and deliver what's stored (for upgrades, server moves, config changes that need an empty queue): MLLP listeners close, queued HTTP answers 503, file pickup and schedules pause. Through the running engine's REST API.

```text
ductus drain [OPTIONS]
```

**Options**

| | What it does | Default | Environment |
|---|---|---|---|
| `--stop` | Exit the engine once nothing is pending. |  |  |
| `--wait` | Wait until drained, showing what's left. |  |  |
| `--api <API>` | The engine's REST API. | `http://127.0.0.1:7700` | `DUCTUS_API_URL` |
| `--token <TOKEN>` | An operator token (`ductus token create <name> --role operator`). |  | `DUCTUS_API_TOKEN` |

