# `ductus owners`

Who owns which interface, destination and resources.yaml entry (`about.owner`, `owner` on a destination someone else owns, `owner` in resources.yaml), and, with a database, whether each owner is an account (username, email or display name), a team in `owners:` or nobody. `ductus owners reassign` moves ownership.

```text
ductus owners [OPTIONS]
       ductus owners <COMMAND>
```

**Options**

| | What it does | Default | Environment |
|---|---|---|---|
| `--check` | Fail (exit 1) if an owner is no account, team or address, a disabled account, or names several accounts. Needs the database. |  |  |
| `--root <ROOT>` | The project folder: the one with `interfaces/` and ductus.yaml. | `.` |  |
| `--env <ENV>` | The environment: `${NAME}` values come from environments/<env>.yaml, `${secret:NAME}` from secrets/<env>.json. | `dev` | `DUCTUS_ENV` |
| `--database-url <DATABASE_URL>` | Where the accounts are (without it, owners aren't matched to them). |  | `DUCTUS_DATABASE_URL` |

## `ductus owners reassign`

Change an owner in interface.yaml files and resources.yaml (someone left, or work is split up): only the `owner:` values change, comments and layout stay. To split, run it per group with --interface and --resource.

```text
ductus owners reassign [OPTIONS] --from <FROM> --to <TO>
```

**Options**

| | What it does | Default | Environment |
|---|---|---|---|
| `--from <FROM>` | The current owner (ignores case). |  |  |
| `--to <TO>` | The new owner. |  |  |
| `--interface <INTERFACE>` | Only these interfaces (repeat for several). With neither this nor --resource, everything of `--from` moves. |  |  |
| `--resource <RESOURCE>` | Only these resources.yaml entries (repeat for several). |  |  |
| `--dry-run` | Show what would change without writing. |  |  |
| `--root <ROOT>` | The project folder: the one with `interfaces/` and ductus.yaml. | `.` |  |
| `--env <ENV>` | The environment: `${NAME}` values come from environments/<env>.yaml, `${secret:NAME}` from secrets/<env>.json. | `dev` | `DUCTUS_ENV` |

