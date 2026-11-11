# Releases

Releases made by `ductus release`: stored here, and checked against this engine's environment (the `deploy` permission).

## `POST /api/v1/deployments`

Deploy a stored release to this engine (release mode): the "replaces" check, the gate (or a recent passing check reused), then the handover to an engine running it. Answers when the new engine sends, or when it couldn't (this one goes on): seconds, plus the gate's time when it runs. 200: deployed. 409: refused before anything ran (folder mode, another deploy or check running, the release already runs, its "replaces" check names someone's work), the report says why. 422: the gate found problems. 500: the new engine couldn't take over (this one goes on). Needs the `deploy` permission. One deploy or check at a time per engine.

**Who may call it:** an operator or admin with the `deploy` permission (admins have it).

**Body** (application/json): [`DeployRequest`](#deployrequest)

**Answers**

- `200` Deployed: the release runs here (application/json): [`DeployReport`](#deployreport)
- `400` No reason given, or no such release (application/json): [`ErrorBody`](#errorbody)
- `403` (application/json): [`ErrorBody`](#errorbody)
- `409` Refused before anything ran: `outcome` says why (application/json): [`DeployReport`](#deployreport)
- `422` The gate found problems: `gate.check.report` (application/json): [`DeployReport`](#deployreport)
- `500` The new engine couldn't take over: this one goes on (application/json): [`DeployReport`](#deployreport)

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

### `DeployChange`

| Field | Type | |
|---|---|---|
| `from` (optional) | text or null | The code version before (12 hex characters); null: new here. |
| `interface` | text |  |
| `to` | text |  |
| `warning` (optional) | text or null | The "replaces" check's text, when deployed over someone else's work (`force`). |

### `DeployGate`

| Field | Type | |
|---|---|---|
| `check` | [`CheckInfo`](#checkinfo) |  |
| `reused` | true or false | A recent passing check reused instead of running the gate again (the pre-run). |
| `why_not_reused` (optional) | text or null | Why no check could be reused (when one ran). |

### `DeployReport`

What a deploy did, or why it didn't.

| Field | Type | |
|---|---|---|
| `changes` | list of [`DeployChange`](#deploychange) |  |
| `deployment` (optional) | number or null | The deployment the new engine recorded and what it changed (absent if it wasn't recorded within seconds: `GET /engine` shows it then). |
| `environment` | text |  |
| `forced` | true or false | Deployed over the work `replaces` names. |
| `gate` (optional) | [`DeployGate`](#deploygate) or null |  |
| `ok` | true or false | The new release runs here. |
| `outcome` | text | What happened, in a sentence. |
| `project` | text |  |
| `release` | text | The release, in hex, and its first 12 characters. |
| `replaces` | map of name to text | Each interface (and "the project's own files") whose running version is someone else's work this release would undo, with why. A deploy is refused when any is named, unless forced. |
| `short` | text |  |
| `steps` | list of text | What the handover said as it went. |

### `DeployRequest`

| Field | Type | |
|---|---|---|
| `all_tests` (optional) | true or false | Run every interface's tests in the gate, not only those of what the release changes here (a passing check is reused only when it ran them all too: never, then). |
| `force` (optional) | true or false | Deploy over someone else's work the "replaces" check names: their change is undone. The interfaces and whose work go into the audit log (`release.force`) and the deployment's history. |
| `reason` (optional) | text or null | Why: required, except in an environment named `dev`. Recorded with the deployment, in its history and in the audit log. |
| `release` | text | The release: its id, or its first 12 characters. Store it first (`POST /releases`). |

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

