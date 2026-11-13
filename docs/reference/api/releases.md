# Releases

Releases made by `ductus release`: stored here, and checked against this engine's environment (the `deploy` permission).

## `GET /api/v1/deployments`

The releases this engine's project ran in its environment, newest first, one entry per run: what runs now, what ran before, and which one a rollback without a release named goes back to. Empty in folder mode.

**Who may call it:** anyone signed in (a person or a token).

**Answers**

- `200` (application/json): list of [`RunInfo`](#runinfo)

## `POST /api/v1/deployments`

Deploy a stored release to this engine (release mode): this environment's rules, the "replaces" check, the gate (or a recent passing check reused), then the handover to an engine running it; or roll back (`rollback`) to a release that ran here before. Answers when the new engine sends, or when it couldn't (this one goes on): seconds, plus the gate's time when it runs. 200: deployed. 409: refused before anything ran (folder mode, another deploy or check running, the release already runs, this environment's rules, its "replaces" check names someone's work, a rollback to a release that never ran here), the report says why. 422: the gate found problems. 500: the new engine couldn't take over (this one goes on). Needs the `deploy` permission. One deploy or check at a time per engine.

**Who may call it:** an operator or admin with the `deploy` permission (admins have it).

**Body** (application/json): [`DeployRequest`](#deployrequest)

**Answers**

- `200` Deployed: the release runs here (application/json): [`DeployReport`](#deployreport)
- `400` No reason given, no release named, or no such release (application/json): [`ErrorBody`](#errorbody)
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

One release: what it is, where it went, what it would change here, and every check of it (newest first, with reports); its manifest with `?manifest=true`.

**Who may call it:** anyone signed in (a person or a token).

**Parameters**

| | In | Type | |
|---|---|---|---|
| `id` (required) | path | text | The release id, or its first 12 characters (at least 4). |
| `manifest` | query | true or false | Include the manifest (every interface's version and files: hundreds of KB at a thousand interfaces). |

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

## `GET /api/v1/releases/{id}/checks/{check}`

One check of a release, with its report (a release's page carries only each environment's newest report).

**Who may call it:** anyone signed in (a person or a token).

**Parameters**

| | In | Type | |
|---|---|---|---|
| `id` (required) | path | text | The release id, or its first 12 characters. |
| `check` (required) | path | number | The check's id (`checks[].id`). |

**Answers**

- `200` (application/json): [`CheckInfo`](#checkinfo)
- `404` (application/json): [`ErrorBody`](#errorbody)

## `GET /api/v1/releases/{id}/file`

A stored release's file, as `ductus release` wrote it (the same id), its record of where it was deployed brought up to date: this database's deployments of it and what earlier files said (`deployed`, oldest first). What `ductus deploy ID --from URL --to URL` fetches to promote it to another engine, record included, and what an engine pulling from this one fetches (`DUCTUS_PROMOTE_FROM`). Needs the `deploy` or `download` permission (it carries the code and every environment's encrypted secrets); audited as `release.download`.

**Who may call it:** anyone with the `download` permission, or an operator or admin with `deploy` (admins have both).

**Parameters**

| | In | Type | |
|---|---|---|---|
| `id` (required) | path | text | The release id, or its first 12 characters. |

**Answers**

- `200` The release file (application/gzip): [`ReleaseArchive`](#releasearchive)
- `403` (application/json): [`ErrorBody`](#errorbody)
- `404` (application/json): [`ErrorBody`](#errorbody)

## `GET /api/v1/upstream`

The engine this one pulls from (`DUCTUS_PROMOTE_FROM`) and its releases of this project, each with whether it's stored here. 404 when this engine pulls from none; 502 when that engine can't be reached or refuses the token.

**Who may call it:** anyone signed in (a person or a token).

**Answers**

- `200` (application/json): [`UpstreamInfo`](#upstreaminfo)
- `404` This engine pulls from no other (application/json): [`ErrorBody`](#errorbody)
- `502` That engine didn't answer, or refused the token (application/json): [`ErrorBody`](#errorbody)

## `POST /api/v1/upstream/releases/{id}`

Bring a release here from the engine this one pulls from: its file (with its record of where it went, brought up to date there) is fetched and stored, as `POST /releases` stores one; a release stored here before keeps its first manifest and gains the records it didn't have. Nothing is deployed. Needs the `deploy` permission here; audited as `release.pull` (when new).

**Who may call it:** an operator or admin with the `deploy` permission (admins have it).

**Parameters**

| | In | Type | |
|---|---|---|---|
| `id` (required) | path | text | The release id there, or its first 12 characters. |

**Answers**

- `200` Stored here before: only records of where it went were added (application/json): [`Pulled`](#pulled)
- `201` Brought here and stored (application/json): [`Pulled`](#pulled)
- `400` The file it sent isn't a release, or is damaged (application/json): [`ErrorBody`](#errorbody)
- `403` (application/json): [`ErrorBody`](#errorbody)
- `404` This engine pulls from no other (application/json): [`ErrorBody`](#errorbody)
- `502` That engine didn't answer, or refused (application/json): [`ErrorBody`](#errorbody)

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
| `report` | object | `ductus release check --json`'s document (without the manifest: it's the release's); null in lists, and for a release's older checks of an environment. |
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
| `branches` | list of text | The branches it was made on, as its files said (the same files made on two branches are one release); empty: a detached HEAD without `--branch`, or made without git. |
| `changes` | list of [`DeployChange`](#deploychange) |  |
| `deployment` (optional) | number or null | The deployment the new engine recorded and what it changed (absent if it wasn't recorded within seconds: `GET /engine` shows it then). |
| `environment` | text |  |
| `forced` | true or false | Deployed over the work `replaces` names. |
| `gate` (optional) | [`DeployGate`](#deploygate) or null |  |
| `ok` | true or false | The new release runs here. |
| `outcome` | text | What happened, in a sentence. |
| `place` (optional) | [`PlaceInfo`](#placeinfo) or null |  |
| `project` | text |  |
| `release` | text | The release, in hex, and its first 12 characters. |
| `replaces` | map of name to text | Each interface (and "the project's own files") whose running version is someone else's work this release would undo, with why. A deploy is refused when any is named, unless forced; a rollback names them and goes on. |
| `rollback` | true or false | A rollback: to a release that ran here before. |
| `rules` | list of text | What this environment's rules said (`DUCTUS_DEPLOY_REQUIRES`, `DUCTUS_DEPLOY_BRANCHES`): each reason a deploy is refused, or (a rollback) that they don't hold it. |
| `short` | text |  |
| `steps` | list of text | What the handover said as it went. |

### `DeployRequest`

| Field | Type | |
|---|---|---|
| `all_tests` (optional) | true or false | Run every interface's tests in the gate, not only those of what the release changes here (a passing check is reused only when it ran them all too: never, then). |
| `force` (optional) | true or false | Deploy over someone else's work the "replaces" check names: their change is undone. The interfaces and whose work go into the audit log (`release.force`) and the deployment's history. Doesn't skip this environment's rules (`DUCTUS_DEPLOY_REQUIRES`, `DUCTUS_DEPLOY_BRANCHES`). |
| `reason` (optional) | text or null | Why: required, except in an environment named `dev`. Recorded with the deployment, in its history and in the audit log. |
| `release` (optional) | text or null | The release: its id, or its first 12 characters. Store it first (`POST /releases`). Required, except for a rollback (default: the last release run here before the current one, skipping any a rollback left). |
| `rollback` (optional) | true or false | Roll back: go back to a release that ran in this environment before. Not held to this environment's rules; the "replaces" check names whose work it undoes without refusing; the gate runs. Audited as `release.rollback`. |

### `ErrorBody`

| Field | Type | |
|---|---|---|
| `error` | text | What went wrong, in plain words. |

### `Here`

What a release would change in this engine's environment, compared with what runs here now.

| Field | Type | |
|---|---|---|
| `changes` | list of [`HereChange`](#herechange) | Each interface it changes, adds or removes here, by name. |
| `environment` | text |  |
| `project_files_change` (optional) | true or false or null | The files outside every interface (`ductus.yaml`, `resources.yaml`, `environments/`, `secrets/`) differ from the running release's. None when this engine doesn't run a release (folder mode, or nothing deployed yet). |
| `ran_before` | true or false | It ran in this environment before (a rollback may go back to it). |
| `running` | true or false | It runs here now: deploying it changes nothing. |
| `unchanged` | number | Interfaces it runs as they run here now. |

### `HereChange`

| Field | Type | |
|---|---|---|
| `change` | text | `changed`, `new` (not run here now) or `removed` (runs here now, not in the release). |
| `from` (optional) | text or null | The code version running here (12 hex characters); none: new here. |
| `interface` | text |  |
| `to` (optional) | text or null | The release's (12 hex characters); none: removed. |

### `PlaceInfo`

Where a release was deployed: one entry of a release file's record (`deployed`).

| Field | Type | |
|---|---|---|
| `at` | time (RFC 3339) |  |
| `by` | text |  |
| `deployment` | number | The deployment's id on that engine's database. |
| `ductus` | text | The Ductus version that ran it. |
| `environment` | text |  |
| `here` (optional) | true or false | Recorded on this engine's database; false: a release file said so when it was stored here. |
| `host` | text | The engine's server. |

### `Pulled`

What a pull did.

[`Stored`](#stored) or object

### `ReleaseArchive`

A release made by `ductus release`: the `.tar.gz` file's bytes, as they are.

bytes

### `ReleaseDetail`

[`ReleaseInfo`](#releaseinfo) or object

### `ReleaseInfo`

| Field | Type | |
|---|---|---|
| `branches` | list of text | Every branch it was made on, as its files said when they were stored here (the same files made on two branches are one release); empty: a detached HEAD without `--branch`, or made without git. Labels, unsigned. |
| `bytes` | number | Its files' bytes, unpacked. |
| `checks` | list of [`CheckInfo`](#checkinfo) | In a list: the last check against each environment (without its report). In a release's own page: every check, newest first, each environment's newest with its report (the others' `GET /releases/{id}/checks/{check}` gives). |
| `commit` (optional) | text or null | The git commit it was made from (none: made without git). |
| `environments` | list of text | The environments it went to, by name: deployed there on this database, or a file carrying it said so (unsigned). Where and when: `GET /releases/{id}`'s `deployed`. |
| `files` | number |  |
| `id` | text | The release id (SHA-256 over its files), in hex. |
| `interfaces` | number |  |
| `made_at` (optional) | time (RFC 3339) or null | When and by whom it was made (`ductus release`), and why (`-m`). |
| `made_by` | text |  |
| `project` | text | The project's folder name. |
| `reason` (optional) | text or null |  |
| `running` | true or false | This engine runs it now (release mode). |
| `short` | text | Its first 12 characters, as `ductus release` shows it. |
| `stored_at` | time (RFC 3339) | When it was stored here, and by whom. |
| `stored_by` | text |  |

### `RunInfo`

One run of a release in this environment: a deploy (or rollback), and the restarts that ran the same release again.

| Field | Type | |
|---|---|---|
| `at` | time (RFC 3339) |  |
| `by` | text |  |
| `deployment` | number | Its first deployment in this run: when, by whom, why. |
| `reason` (optional) | text or null |  |
| `release` | text | The release, in hex, and its first 12 characters. |
| `rollback` | true or false | The run began with a rollback. |
| `rollback_target` | true or false | What `ductus deploy --rollback` (no release named) would go back to. |
| `running` | true or false | It runs now (the newest run). |
| `short` | text |  |

### `Stored`

[`ReleaseInfo`](#releaseinfo) or object

### `UpstreamCheck`

| Field | Type | |
|---|---|---|
| `at` | time (RFC 3339) |  |
| `environment` | text |  |
| `ok` | true or false |  |

### `UpstreamInfo`

The engine this one pulls from, and its releases of this engine's project.

| Field | Type | |
|---|---|---|
| `environment` | text | Its environment and Ductus version, as it says (`GET /engine` there). |
| `releases` | list of [`UpstreamRelease`](#upstreamrelease) | Its releases of this project, newest first (at most 50), as it lists them, each with whether it's stored here already. `stored_here` false: Bring here fetches it. |
| `url` | text | Its address (`DUCTUS_PROMOTE_FROM`). |
| `version` | text |  |

### `UpstreamRelease`

One of its releases, as it lists it (`GET /releases` there).

| Field | Type | |
|---|---|---|
| `branches` (optional) | list of text | The branches it was made on and the environments it went to, as that engine knows them (empty from an engine older than 0.7.4.5). |
| `checks` (optional) | list of [`UpstreamCheck`](#upstreamcheck) | Its last check there per environment. |
| `commit` (optional) | text or null |  |
| `environments` (optional) | list of text |  |
| `id` | text |  |
| `interfaces` (optional) | number |  |
| `made_at` (optional) | time (RFC 3339) or null |  |
| `made_by` (optional) | text |  |
| `project` | text |  |
| `reason` (optional) | text or null |  |
| `short` | text |  |
| `stored_at` | time (RFC 3339) |  |
| `stored_here` (optional) | true or false | Stored on this engine's database already (bringing it again adds only what its record says that this database doesn't know). |

