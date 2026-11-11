# `ductus deploy`

Deploy a release to an engine in release mode (`ductus run --releases`): the engine holds it against what runs there (the "replaces" check: refused when it would undo someone else's work), runs the gate (`ductus release check`) against its environment unless a recent passing check can be reused, then hands over to an engine running it: receiving never stops. Exits 1 when it wasn't deployed. Needs a token with the `deploy` permission. Example: `ductus deploy .ductus/releases/integrations-0a1b2c3d4e5f.tar.gz --to https://ductus-test:7700 -m "lab wants PV1-3 mapped"`.

```text
ductus deploy [OPTIONS] <RELEASE>
```

**Arguments**

| | What it does | Default | Environment |
|---|---|---|---|
| `<RELEASE>` | The release: a file `ductus release` wrote (stored on the engine first when it isn't), or the id of a release stored there (its first 12 characters are enough). |  |  |

**Options**

| | What it does | Default | Environment |
|---|---|---|---|
| `--to <TO>` | The engine's REST API. | `http://127.0.0.1:7700` | `DUCTUS_API_URL` |
| `--token <TOKEN>` | A token with the `deploy` permission (`ductus token create ci --role operator --permission deploy`). |  | `DUCTUS_API_TOKEN` |
| `-m, --message <REASON>` | Why: required, except to an environment named `dev`; recorded with the deployment, in its history and in the audit log. |  |  |
| `--force` | Deploy over someone else's work the "replaces" check names: their change is undone (recorded in the audit log as `release.force`, and in the history). Needs `-m`. |  |  |
| `--all-tests` | Run every interface's tests in the gate, not only those of what the release changes there (never reuses a check). |  |  |
| `--json` | The engine's report as JSON. |  |  |
| `--timeout <TIMEOUT>` | How long to wait for the engine's answer (the gate, up to 30 minutes, then the handover). | `40m` |  |

