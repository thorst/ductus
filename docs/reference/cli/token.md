# `ductus token`

Manage REST API tokens (in the engine's database). Roles: viewer reads, operator also drains and runs connection checks, admin also manages users and tokens. A token can't read message payloads unless it is given `--permission payloads`.

```text
ductus token [OPTIONS] <COMMAND>
```

## `ductus token create`

Create a token and print it (only this once: it's stored as a hash).

```text
ductus token create [OPTIONS] <NAME>
```

**Arguments**

| | What it does | Default | Environment |
|---|---|---|---|
| `<NAME>` | Who or what uses it, e.g. `ci` or `monitoring`. |  |  |

**Options**

| | What it does | Default | Environment |
|---|---|---|---|
| `--role <ROLE>` | viewer, operator or admin. | `viewer` |  |
| `--permission <PERMISSIONS>` | Beyond the role: `payloads` (read message payloads), `audit`. Repeat for both. |  |  |

## `ductus token list`

List tokens (never their secrets).

```text
ductus token list [OPTIONS]
```

**Options**

| | What it does | Default | Environment |
|---|---|---|---|
| `--revoked` | Include revoked tokens. |  |  |

## `ductus token revoke`

Revoke a token: it stops working at once.

```text
ductus token revoke [OPTIONS] <NAME>
```

**Arguments**

| | What it does | Default | Environment |
|---|---|---|---|
| `<NAME>` | The token's name, as `ductus token list` shows it. |  |  |

