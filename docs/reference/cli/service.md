# `ductus service`

The Windows service (as an administrator).

```text
ductus service [OPTIONS] <COMMAND>
```

## `ductus service account`

Show the account the service runs as; with NAME, run it as that account: grants it "Log on as a service", checks its password, and restarts the service if it was running. NAME is `DOMAIN\svc-ductus$` (a gMSA: no password), `DOMAIN\name` or `.\name` (asks for its password; give it again after every upgrade), or `NT SERVICE\Ductus` (the default).

```text
ductus service account [OPTIONS] [NAME]
```

**Arguments**

| | What it does | Default | Environment |
|---|---|---|---|
| `<NAME>` | The account: `DOMAIN\name`, a gMSA (`DOMAIN\name$`), or none to show the current one. |  |  |

**Options**

| | What it does | Default | Environment |
|---|---|---|---|
| `--password-stdin` | Read the password from stdin (one line) instead of asking. |  |  |
| `--start` | Start the service afterwards even if it was stopped. |  |  |

