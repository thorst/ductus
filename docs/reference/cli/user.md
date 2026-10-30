# `ductus user`

Manage the people who sign in (in the engine's database; that is how the first admin is made). Roles: viewer reads, operator also drains, resends and runs connection checks, admin also manages users and tokens. Permissions beyond a role: `payloads` (read message payloads), `audit`.

```text
ductus user [OPTIONS] <COMMAND>
```

## `ductus user create`

Make an account. The password is asked for twice (or read from stdin with --password-stdin).

```text
ductus user create [OPTIONS] <USERNAME>
```

**Arguments**

| | What it does | Default | Environment |
|---|---|---|---|
| `<USERNAME>` | The name they sign in with. |  |  |

**Options**

| | What it does | Default | Environment |
|---|---|---|---|
| `--role <ROLE>` | viewer, operator or admin. | `viewer` |  |
| `--display-name <DISPLAY_NAME>` | Their name as others see it (in the web UI and the audit log). |  |  |
| `--email <EMAIL>` | Their email address: how owner texts (`about.owner`) and alerts reach them. |  |  |
| `--permission <PERMISSIONS>` | Beyond the role: `payloads`, `audit`. Repeat for both. |  |  |
| `--password-stdin` | Read the password from stdin (one line) instead of asking. |  |  |
| `--must-change` | The person must choose their own password at their first sign-in. |  |  |

## `ductus user list`

List users.

```text
ductus user list [OPTIONS]
```

## `ductus user set`

Change a user's role and permissions (all of them are set to what is given).

```text
ductus user set [OPTIONS] <USERNAME>
```

**Arguments**

| | What it does | Default | Environment |
|---|---|---|---|
| `<USERNAME>` | The account's sign-in name. |  |  |

**Options**

| | What it does | Default | Environment |
|---|---|---|---|
| `--role <ROLE>` | The new role: viewer, operator or admin (none given: leave it). |  |  |
| `--permission <PERMISSIONS>` | The permissions the user has afterwards (none given: leave them as they are). |  |  |

## `ductus user password`

Give a user a new temporary password (they must change it at their next sign-in).

```text
ductus user password [OPTIONS] <USERNAME>
```

**Arguments**

| | What it does | Default | Environment |
|---|---|---|---|
| `<USERNAME>` | The account's sign-in name. |  |  |

**Options**

| | What it does | Default | Environment |
|---|---|---|---|
| `--password-stdin` | Read the password from stdin (one line) instead of asking. |  |  |

## `ductus user disable`

Stop a user signing in; their sessions and tokens stop at once.

```text
ductus user disable [OPTIONS] <USERNAME>
```

**Arguments**

| | What it does | Default | Environment |
|---|---|---|---|
| `<USERNAME>` | The account's sign-in name. |  |  |

## `ductus user enable`

Let a disabled user sign in again.

```text
ductus user enable [OPTIONS] <USERNAME>
```

**Arguments**

| | What it does | Default | Environment |
|---|---|---|---|
| `<USERNAME>` | The account's sign-in name. |  |  |

## `ductus user unlock`

Let a user locked out by wrong passwords try again.

```text
ductus user unlock [OPTIONS] <USERNAME>
```

**Arguments**

| | What it does | Default | Environment |
|---|---|---|---|
| `<USERNAME>` | The account's sign-in name. |  |  |

