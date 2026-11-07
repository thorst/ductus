# `ductus history`

One timeline of who changed an interface, what and why, newest first: the git commits that changed its files (its interface.yaml, its code and the shared files it imports, every version's), the deployments that changed its code (who, why, from which commit, and a ⚠ when one may have undone someone else's change), the audit log's entries naming it (pauses, resends, retention) and its documentation's changes. Each line ends with the parts it touches (`[source, lab]`); `--source` or `--destination NAME` shows only one part's. Reads the project and, with DUCTUS_DATABASE_URL, the database (read-only); without it, git only.

```text
ductus history [OPTIONS] <INTERFACE>
```

**Arguments**

| | What it does | Default | Environment |
|---|---|---|---|
| `<INTERFACE>` | The interface: its name, or a file or folder inside its folder. One no longer in the project is found by name in what was recorded. |  |  |

**Options**

| | What it does | Default | Environment |
|---|---|---|---|
| `--source` | Only what touches its source: `source:`, `format`, `ordering`, `dedupe`, `durability`, `about.from`, `monitor`'s times (and an HTTP `respond` interface's code). |  |  |
| `--destination <NAME>` | Only what touches this destination: its block in interface.yaml, the interface's settings it has none of its own for, the files its code loads (a shared file touches each destination that loads it), its pauses, resends and alerts, its documentation. |  |  |
| `--all` | Everything: also the starts that ran it unchanged, payload reads and refused attempts. |  |  |
| `--limit <LIMIT>` | How many entries, newest first. | `50` |  |
| `--before <BEFORE>` | Entries older than this one: the `--before` the previous page ends with. |  |  |
| `--json` | One JSON document: every entry with its time, kind, who, what, and its details (the commit and files, the deployment, the audit row, the documentation fields changed). |  |  |
| `--database-url <DATABASE_URL>` | The database (read-only) for deployments, the audit log and documentation; git only without it. |  | `DUCTUS_DATABASE_URL` |
| `--root <ROOT>` | The project folder: the one with `interfaces/` and ductus.yaml. | `.` |  |
| `--env <ENV>` | The environment its interface.yaml is read in. | `dev` | `DUCTUS_ENV` |

