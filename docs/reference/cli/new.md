# `ductus new`

Create something new in a project.

```text
ductus new [OPTIONS] <COMMAND>
```

## `ductus new interface`

A working interface (config, code, a test and a sample message) that passes `ductus test` and `ductus validate` as generated. MLLP and HTTP get the next free port from the ranges in ductus.yaml (default 6661-6699 and 8080-8099).

```text
ductus new interface [OPTIONS] <NAME>
```

**Arguments**

| | What it does | Default | Environment |
|---|---|---|---|
| `<NAME>` | Folder name and interface name, e.g. adt-to-lab. |  |  |

**Options**

| | What it does | Default | Environment |
|---|---|---|---|
| `--source <SOURCE>` | Where its messages come from. | `mllp` |  |
| `--format <FORMAT>` | Default: hl7 (json for http and schedule). |  |  |
| `--port <PORT>` | Listen on this port instead of the next free one. |  |  |
| `--server <SERVER>` | SFTP server from resources.yaml (needed when there are several). |  |  |
| `--root <ROOT>` | The project folder: the one with `interfaces/` and ductus.yaml. | `.` |  |

