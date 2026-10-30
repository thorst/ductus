# Configuration files

A project is a folder of plain files, kept in Git: what the engine runs is what's in them. These pages list every setting each file takes, with what it does, why and when you'd use it, its default and an example.

| File | What it holds |
|---|---|
| [`interface.yaml`](interface.md) | `interface.yaml`: one interface's settings. Where its messages come from (`source`), how they're read (`format`), where they go (`destination`, or `destinations` for several), and how they're delivered, kept, watched and documented. Its code is in the `script` beside it. |
| [`resources.yaml`](resources.md) | `resources.yaml`: what many interfaces share, each defined once by a name: databases, SFTP servers, HTTP APIs, directories for signing in, and certificates. |
| [`ductus.yaml`](ductus.md) | `ductus.yaml`: the project's settings, for every interface in it and the engine that runs them: how long messages are kept, the REST API and web UI, how people sign in, the audit log, alerts, and the engine's own connection checks. |
| [`environments/*.yaml`](environment.md) | `environments/<name>.yaml`: one environment's values (`dev.yaml`, `test.yaml`, `prod.yaml`). The engine reads the one it's started with (`--env prod`, or `DUCTUS_ENV`), and every `${NAME}` in interface.yaml, resources.yaml and ductus.yaml's `alerts:` takes its value from here. The same project then runs anywhere, with only this file differing. |
| [`docs-schema.yaml`](docs-schema.md) | `docs-schema.yaml`: the documentation fields your company keeps about its interfaces, destinations and resources, beyond what Ductus asks for (a BAA reference, a change ticket, a runbook link, the analyst). Values go in `custom:` beside each one; `ductus validate` checks them, and the web UI shows and filters by them. The engine itself ignores them. |

**Values from the environment.** In interface.yaml, resources.yaml and the `alerts:` section of ductus.yaml, any value may be `${NAME}` (from `environments/<env>.yaml`) or `${secret:NAME}` (from the encrypted `secrets/<env>.json`), filled in for the environment the engine runs in. A value that is only a reference keeps the variable's type: `port: ${EPIC_PORT}` is a number.

**In your editor.** `ductus types` (and `ductus test`, `ductus validate`) writes these files' JSON Schemas into the project (`.ductus/schema/`) and tells VS Code where they are, so with the YAML extension (Red Hat's) you get completion, these descriptions on hover, and mistakes underlined as you type. The schemas come with the engine you have installed, so they match it; run `ductus types` again after an upgrade. They're also in the repository, `docs/schema/`.

**Durations** are written with their unit: `500ms`, `90s`, `15m`, `24h`, `7d`.
