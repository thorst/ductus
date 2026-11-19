# `ductus deploy`

Deploy a release to an engine in release mode (`ductus run --releases`): the engine holds it against what runs there (the "replaces" check: refused when it would undo someone else's work), runs the gate (`ductus release check`) against its environment unless a recent passing check can be reused, then applies it **in place** when only interfaces' code, destinations and destination settings change, or interfaces are added or removed (those interfaces take their new code, start or stop between two messages; the others never pause), else hands over to an engine running it (receiving never stops; the answer says which change needed it). Exits 1 when it wasn't deployed. Needs a token with the `deploy` permission. A file deployed gets the record of where it went written into it (its id unchanged), which an environment requiring this one first reads (DUCTUS_DEPLOY_REQUIRES). Promote from one engine to the next with `--from`; go back with `--rollback`. Example: `ductus deploy .ductus/releases/integrations-0a1b2c3d4e5f.tar.gz --to https://ductus-test:7700 -m "lab wants PV1-3 mapped"`, then `ductus deploy 0a1b2c3d4e5f --from https://ductus-test:7700 --to https://ductus-prod:7700 -m "tested in test"`.

```text
ductus deploy [OPTIONS] [RELEASE]
```

**Arguments**

| | What it does | Default | Environment |
|---|---|---|---|
| `<RELEASE>` | The release: a file `ductus release` wrote (stored on the engine first when it isn't), or the id of a release stored there (its first 12 characters are enough). With `--rollback`, optional: the last release run there before the current one. |  |  |

**Options**

| | What it does | Default | Environment |
|---|---|---|---|
| `--to <TO>` | The engine's REST API. | `http://127.0.0.1:7700` | `DUCTUS_API_URL` |
| `--token <TOKEN>` | A token with the `deploy` permission (`ductus token create ci --role operator --permission deploy`). |  | `DUCTUS_API_TOKEN` |
| `--from <URL>` | Promote: fetch the release (by its id) from this engine's API with its record of where it was deployed (`GET /releases/{id}/file`), store it on `--to`'s engine, and deploy it there. Example: `--from https://ductus-test:7700`. |  |  |
| `--from-token <FROM_TOKEN>` | The token for `--from` (its engine's: a token with the `deploy` permission there); default: `--token`'s. |  | `DUCTUS_FROM_TOKEN` |
| `--rollback` | Roll back to a release that ran on that engine's environment before: the one named, or the last one before the current run (skipping any a rollback left). Not held to the environment's DUCTUS_DEPLOY_REQUIRES or DUCTUS_DEPLOY_BRANCHES; names whose newer work it undoes without refusing; the gate runs. Needs `-m`, except to `dev`. |  |  |
| `-m, --message <REASON>` | Why: required, except to an environment named `dev`; recorded with the deployment, in its history and in the audit log. |  |  |
| `--force` | Deploy over someone else's work the "replaces" check names: their change is undone (recorded in the audit log as `release.force`, and in the history). Needs `-m`. Doesn't skip the environment's DUCTUS_DEPLOY_REQUIRES or DUCTUS_DEPLOY_BRANCHES. |  |  |
| `--all-tests` | Run every interface's tests in the gate, not only those of what the release changes there (never reuses a check). |  |  |
| `--in-place-only` | Only in place: refuse (before the gate runs) when the release changes something only a handover applies (how messages are stored, lanes, the project's own files, a port moving from one interface to another), and when applying in place fails, instead of handing over. For sites that schedule handovers. |  |  |
| `--interface <NAME>` | Deploy only this interface's work from the release (repeat for several): the engine makes a release of its own from what runs there with this interface's folder and the shared code it loads taken from the release named, and deploys that (an interface the release lacks is removed, one only it has is added). Every other interface, and the project's own files, stay as they run. Refused when a shared file it needs differs and other interfaces run it (named: take them too). Example: `ductus deploy 0a1b2c3d4e5f --interface adt-to-lab -m "lab's fix, without billing's change"`. |  |  |
| `--json` | The engine's report as JSON. |  |  |
| `--timeout <TIMEOUT>` | How long to wait for the engine's answer (its turn behind other deploys, up to 5 minutes; the gate, up to 30; then applying it). | `40m` |  |

