# `ductus secret`

Manage encrypted secrets (secrets/<env>.json; key in DUCTUS_SECRET_KEY).

```text
ductus secret [OPTIONS] <COMMAND>
```

## `ductus secret init`

Print a new random key to put in DUCTUS_SECRET_KEY (keep it out of Git).

```text
ductus secret init [OPTIONS]
```

## `ductus secret set`

Create or update a secret. The value is read from stdin if not given.

```text
ductus secret set [OPTIONS] <NAME> [VALUE]
```

**Arguments**

| | What it does | Default | Environment |
|---|---|---|---|
| `<NAME>` | The secret's name, as config files use it: `${secret:NAME}`. |  |  |
| `<VALUE>` | The value (read from stdin when not given, which keeps it out of the shell's history). |  |  |

**Options**

| | What it does | Default | Environment |
|---|---|---|---|
| `--root <ROOT>` | The project folder: the one with `interfaces/` and ductus.yaml. | `.` |  |
| `--env <ENV>` | The environment: `${NAME}` values come from environments/<env>.yaml, `${secret:NAME}` from secrets/<env>.json. | `dev` | `DUCTUS_ENV` |

## `ductus secret list`

List secret names (never values).

```text
ductus secret list [OPTIONS]
```

**Options**

| | What it does | Default | Environment |
|---|---|---|---|
| `--root <ROOT>` | The project folder: the one with `interfaces/` and ductus.yaml. | `.` |  |
| `--env <ENV>` | The environment: `${NAME}` values come from environments/<env>.yaml, `${secret:NAME}` from secrets/<env>.json. | `dev` | `DUCTUS_ENV` |

