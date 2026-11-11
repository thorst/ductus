# Releases

Releases made by `ductus release`: stored here, and checked against this engine's environment (the `deploy` permission).

## `GET /api/v1/releases`

The releases stored on this database, newest first, each with its last check per environment.

**Who may call it:** anyone signed in (a person or a token).

**Parameters**

| | In | Type | |
|---|---|---|---|
| `before` | query | time (RFC 3339) | Releases stored before this time (the previous page's last `stored_at`). |
| `limit` | query | number | How many, newest first (default 50, at most 500). |

**Answers**

- `200` (application/json): list of [`ReleaseInfo`](#releaseinfo)

## `POST /api/v1/releases`

Store a release made by `ductus release` (the `.tar.gz` file as the body). Its id is recomputed from its files (a damaged or changed file is refused); one stored already is said so (200) and nothing changes; a new one is 201. Refused when it carries a private key. Needs the `deploy` permission.

**Who may call it:** an operator or admin with the `deploy` permission (admins have it).

**Body** (application/gzip): [`ReleaseArchive`](#releasearchive)

**Answers**

- `200` Stored before: nothing changed (application/json): [`Stored`](#stored)
- `201` Stored (application/json): [`Stored`](#stored)
- `400` Not a release, damaged, or carrying a private key (application/json): [`ErrorBody`](#errorbody)
- `403` (application/json): [`ErrorBody`](#errorbody)

## `GET /api/v1/releases/{id}`

One release: what it is, its manifest, and every check of it (newest first, with reports).

**Who may call it:** anyone signed in (a person or a token).

**Parameters**

| | In | Type | |
|---|---|---|---|
| `id` (required) | path | text | The release id, or its first 12 characters (at least 4). |

**Answers**

- `200` (application/json): [`ReleaseDetail`](#releasedetail)
- `400` (application/json): [`ErrorBody`](#errorbody)
- `404` (application/json): [`ErrorBody`](#errorbody)

## `POST /api/v1/releases/{id}/check`

Check a stored release against this engine's environment: `ductus release check` runs here, with this engine's settings, secrets and database (read-only), as a child process at a lower CPU priority. `validate`, then the tests of the interfaces the release changes here. Nothing is changed but the check's record (and an audit row). One check at a time per engine (409 while one runs). Answers when it ends (seconds at a thousand interfaces); `ok` says whether it passed. Needs the `deploy` permission: the release's code runs with this environment's secrets.

**Who may call it:** an operator or admin with the `deploy` permission (admins have it).

**Parameters**

| | In | Type | |
|---|---|---|---|
| `id` (required) | path | text | The release id, or its first 12 characters. |
| `all_tests` | query | true or false | Run every interface's tests, not only those of what the release changes here. |

**Answers**

- `200` The check ran: `ok` says whether it passed (application/json): [`CheckInfo`](#checkinfo)
- `403` (application/json): [`ErrorBody`](#errorbody)
- `404` (application/json): [`ErrorBody`](#errorbody)
- `409` Another check is running on this engine (application/json): [`ErrorBody`](#errorbody)

## Shapes

The JSON bodies the operations above take and return.

### `CheckInfo`

| Field | Type | |
|---|---|---|
| `at` | time (RFC 3339) |  |
| `by` | text |  |
| `environment` | text | The environment checked against, and the engine and server that ran it. |
| `host` | text |  |
| `id` | number |  |
| `instance` | text |  |
| `ok` | true or false | It passed the gate: no problem found. |
| `problems` | number |  |
| `report` | object | `ductus release check --json`'s document (without the manifest: it's the release's); null in lists. |
| `seconds` | number |  |

### `ErrorBody`

| Field | Type | |
|---|---|---|
| `error` | text | What went wrong, in plain words. |

### `ReleaseArchive`

A release made by `ductus release`: the `.tar.gz` file's bytes, as they are.

bytes

### `ReleaseDetail`

[`ReleaseInfo`](#releaseinfo) or object

### `ReleaseInfo`

| Field | Type | |
|---|---|---|
| `bytes` | number | Its files' bytes, unpacked. |
| `checks` | list of [`CheckInfo`](#checkinfo) | In a list: the last check against each environment (without its report). In a release's own page: every check, newest first, with their reports. |
| `commit` (optional) | text or null | The git commit it was made from (none: made without git). |
| `files` | number |  |
| `id` | text | The release id (SHA-256 over its files), in hex. |
| `interfaces` | number |  |
| `made_at` (optional) | time (RFC 3339) or null | When and by whom it was made (`ductus release`), and why (`-m`). |
| `made_by` | text |  |
| `project` | text | The project's folder name. |
| `reason` (optional) | text or null |  |
| `short` | text | Its first 12 characters, as `ductus release` shows it. |
| `stored_at` | time (RFC 3339) | When it was stored here, and by whom. |
| `stored_by` | text |  |

### `Stored`

[`ReleaseInfo`](#releaseinfo) or object

