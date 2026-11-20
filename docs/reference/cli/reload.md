# `ductus reload`

Apply the project folder as it is now to the running engine (folder mode), in place: only the interfaces whose files or settings changed take the change (between two messages, or restarting alone; added ones start, removed ones stop), every other interface goes on untouched. What only a start reads (ductus.yaml, resources.yaml, docs-schema.yaml, the environment's secrets file, a new binary, ductus.env with `--with-environment`) is applied by the handover instead, said in the answer. Recorded as a deployment (`ductus history`). Asks over the control socket where there is one (Linux under the package, containers), else the engine's API with a `deploy` token (Windows, a plain `ductus run`). `systemctl reload ductus` runs it. In a project's folder whose `ductus dev` runs, with no token given, it reloads that engine (with the token `ductus dev` keeps in `.ductus/dev-token`); a dev engine applies in place only and says to restart it for the rest. Exit 0 once the engine runs the folder, 1 when it was refused (nothing changed: the reason is printed). Example: `git pull && sudo ductus reload -m "lab codes from LAB-142"`.

```text
ductus reload [OPTIONS]
```

**Options**

| | What it does | Default | Environment |
|---|---|---|---|
| `--control <CONTROL>` | The running engine's control socket (default: `$RUNTIME_DIRECTORY/control`, else /run/ductus/control when it's there; without one, the API is asked). |  | `DUCTUS_CONTROL` |
| `--api <API>` | The engine's API, asked when there's no control socket. | `http://127.0.0.1:7700` | `DUCTUS_API_URL` |
| `--token <TOKEN>` | A token with the `deploy` permission, for the API (`ductus token create reload --role operator --permission deploy`). |  | `DUCTUS_API_TOKEN` |
| `-m, --message <REASON>` | Why: recorded with the deployment, beside who asked. |  |  |
| `--in-place-only` | Refuse what only the handover applies, instead of handing over (nothing changes then). |  |  |
| `--with-environment` | Pass this command's environment along (the unit's reload: ductus.env read again): a `DUCTUS_*` value that differs from the running engine's is applied by the handover. Control socket only. |  |  |
| `--timeout <TIMEOUT>` | How long a handover's new engine may take to be ready. | `5m` |  |
| `--json` | Print the report as JSON (what `POST /engine/reload` answers). |  |  |

