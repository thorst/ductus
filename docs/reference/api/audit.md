# Audit

Who did what: the audit log (the `audit` permission) and your own activity.

## `GET /api/v1/audit`

The audit log, filtered. Needs the `audit` permission.

**Who may call it:** anyone with the `audit` permission.

**Parameters**

| | In | Type | |
|---|---|---|---|
| `actor` | query | text | A user's or token's name (a user's tokens count as theirs). |
| `action` | query | text | An action (`message.dismiss`) or a group of them (`message`, `user`, `session`). |
| `interface` | query | text |  |
| `message` | query | number | A message id. |
| `from` | query | time (RFC 3339) | From this time (inclusive). |
| `to` | query | time (RFC 3339) | Until this time (exclusive). |
| `before` | query | number | Entries older than this id (the previous page's `next_before`). |
| `after` | query | number | Entries newer than this id, oldest first (what an export follows). |
| `format` | query | text | `json` (default: a page, newest first) or `ndjson` (one entry per line, oldest first, up to `limit`, default 10,000, at most 100,000: continue with `after` the last id). An export is itself logged. |
| `limit` | query | number | Page size, at most 500 (default 100). |

**Answers**

- `200` With `format=ndjson`: `application/x-ndjson`, one entry (as in `entries`) per line (application/json): [`AuditPage`](#auditpage)
- `400` (application/json): [`ErrorBody`](#errorbody)
- `403` (application/json): [`ErrorBody`](#errorbody)

## Shapes

The JSON bodies the operations above take and return.

### `AuditEntry`

| Field | Type | |
|---|---|---|
| `action` | text | `message.payload.read`, `message.dismiss`, `user.create`, ... |
| `actor` | text | The user's or token's name when it happened. |
| `actor_kind` | text | `user`, `token`, `system` (the engine, or someone at the server's command line) or `anonymous` (a sign-in as a name nobody has). |
| `at` | time (RFC 3339) |  |
| `detail` (optional) | object or null | More about it, by action (never a password, token or payload). |
| `id` | number |  |
| `interface` (optional) | text or null |  |
| `ip` (optional) | text or null | The address the request came from (a proxy's, if there is one). |
| `message` (optional) | number or null |  |
| `outcome` | text | `ok`, `denied` or `failed`. |
| `subject` (optional) | text or null | What it was done to: a username, a token, `interface/destination`. |
| `via` (optional) | text or null | `session` (the web UI), `token`, `cli` or `engine`; none for entries from before the log. |

### `AuditPage`

| Field | Type | |
|---|---|---|
| `entries` | list of [`AuditEntry`](#auditentry) | Newest first. |
| `next_before` (optional) | number or null | Pass as `before` for the next (older) page; absent on the last page. |

### `ErrorBody`

| Field | Type | |
|---|---|---|
| `error` | text | What went wrong, in plain words. |

