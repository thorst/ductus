# `ductus types`

Write what an editor needs to help with the project, for the version installed: the SDK's typings (.ductus/types) and a tsconfig.json for the interfaces' code, and the configuration files' schemas (.ductus/schema) with VS Code's settings pointing at them, for completion, descriptions on hover and mistakes underlined in interface.yaml, resources.yaml and ductus.yaml. `ductus test` and `validate` do it too; run it after an upgrade.

```text
ductus types [OPTIONS]
```

**Options**

| | What it does | Default | Environment |
|---|---|---|---|
| `--root <ROOT>` | Project folder. | `.` |  |

