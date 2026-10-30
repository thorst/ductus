# Your account

The signed-in user's own account: password, sessions, tokens, appearance.

## `GET /api/v1/me`

The signed-in user's account.

**Who may call it:** a person signed in with a password.

**Answers**

- `200` (application/json): [`UserInfo`](#userinfo)
- `403` (application/json): [`ErrorBody`](#errorbody)

## `PATCH /api/v1/me`

Change your own name and email (a directory account's come from the directory).

**Who may call it:** a person signed in with a password.

**Body** (application/json): [`ProfileUpdate`](#profileupdate)

**Answers**

- `200` (application/json): [`UserInfo`](#userinfo)
- `403` (application/json): [`ErrorBody`](#errorbody)

## `GET /api/v1/me/activity`

What you did yourself (and what your tokens did): the same entries, only yours. Anyone signed in with a username and password.

**Who may call it:** a person signed in with a password.

**Parameters**

| | In | Type | |
|---|---|---|---|
| `before` | query | number | Entries older than this id. |
| `limit` | query | number | At most 500 (default 50). |

**Answers**

- `200` (application/json): [`AuditPage`](#auditpage)
- `403` (application/json): [`ErrorBody`](#errorbody)

## `POST /api/v1/me/password`

Change your own password. Your other sessions end. Works while a change is required.

**Who may call it:** a person signed in with a password.

**Body** (application/json): [`PasswordChange`](#passwordchange)

**Answers**

- `204`
- `400` (application/json): [`ErrorBody`](#errorbody)
- `403` (application/json): [`ErrorBody`](#errorbody)

## `GET /api/v1/me/sessions`

Your signed-in browsers.

**Who may call it:** a person signed in with a password.

**Answers**

- `200` (application/json): list of [`SessionInfo`](#sessioninfo)
- `403` (application/json): [`ErrorBody`](#errorbody)

## `DELETE /api/v1/me/sessions`

Sign out everywhere else: every session of yours but this one ends.

**Who may call it:** a person signed in with a password.

**Answers**

- `204`
- `403` (application/json): [`ErrorBody`](#errorbody)

## `DELETE /api/v1/me/sessions/{id}`

End one of your sessions.

**Who may call it:** a person signed in with a password.

**Parameters**

| | In | Type | |
|---|---|---|---|
| `id` (required) | path | number | Session id. |

**Answers**

- `204`
- `403` (application/json): [`ErrorBody`](#errorbody)
- `404` (application/json): [`ErrorBody`](#errorbody)

## `GET /api/v1/me/settings`

Your saved appearance (theme and custom colors), which follows you to any browser.

**Who may call it:** a person signed in with a password.

**Answers**

- `200` (application/json): [`Appearance`](#appearance)
- `403` (application/json): [`ErrorBody`](#errorbody)

## `PUT /api/v1/me/settings`

Save your appearance.

**Who may call it:** a person signed in with a password.

**Body** (application/json): [`Appearance`](#appearance)

**Answers**

- `200` (application/json): [`Appearance`](#appearance)
- `400` (application/json): [`ErrorBody`](#errorbody)
- `403` (application/json): [`ErrorBody`](#errorbody)

## `GET /api/v1/me/tokens`

Your own tokens (live ones).

**Who may call it:** a person signed in with a password.

**Answers**

- `200` (application/json): list of [`TokenInfo`](#tokeninfo)
- `403` (application/json): [`ErrorBody`](#errorbody)

## `POST /api/v1/me/tokens`

Make a token of your own, for scripts. It acts as you at most: never above your role or permissions, and it stops working if you are disabled. Its secret is in this answer only.

**Who may call it:** a person signed in with a password.

**Body** (application/json): [`NewMyToken`](#newmytoken)

**Answers**

- `200` (application/json): [`CreatedToken`](#createdtoken)
- `400` (application/json): [`ErrorBody`](#errorbody)
- `403` (application/json): [`ErrorBody`](#errorbody)

## `DELETE /api/v1/me/tokens/{name}`

Revoke one of your tokens (the name as listed).

**Who may call it:** a person signed in with a password.

**Parameters**

| | In | Type | |
|---|---|---|---|
| `name` (required) | path | text | Token name. |

**Answers**

- `204`
- `403` (application/json): [`ErrorBody`](#errorbody)
- `404` (application/json): [`ErrorBody`](#errorbody)

## Shapes

The JSON bodies the operations above take and return.

### `Appearance`

| Field | Type | |
|---|---|---|
| `colors` (optional) | object or null | The custom colors, as the UI saves them (a JSON object). |
| `saved` (optional) | true or false | Whether this was saved before: false means the UI may offer the browser's own choice. |
| `theme` | text | `system`, `light`, `dark`, `contrast` or `custom`. |

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

### `CreatedToken`

| Field | Type | |
|---|---|---|
| `secret` | text | The token itself: shown this once, never stored (only its hash is). |
| `token` | [`TokenInfo`](#tokeninfo) |  |

### `ErrorBody`

| Field | Type | |
|---|---|---|
| `error` | text | What went wrong, in plain words. |

### `NewMyToken`

| Field | Type | |
|---|---|---|
| `name` | text | What it is for, e.g. `deploy-script`; it is listed as `<you>/<name>`. |
| `permissions` (optional) | list of text or null | Defaults to none; never more than you have. |
| `role` (optional) | text or null | Defaults to your own role; never above it. |

### `PasswordChange`

| Field | Type | |
|---|---|---|
| `current` | text | The password you have now. |
| `new` | text |  |

### `ProfileUpdate`

| Field | Type | |
|---|---|---|
| `display_name` (optional) | text or null |  |
| `email` (optional) | text or null |  |

### `SessionInfo`

| Field | Type | |
|---|---|---|
| `created_at` | time (RFC 3339) |  |
| `current` | true or false | The one making this request. |
| `expires_at` | time (RFC 3339) |  |
| `id` | number |  |
| `last_seen_at` | time (RFC 3339) |  |

### `TokenInfo`

| Field | Type | |
|---|---|---|
| `created_at` | time (RFC 3339) |  |
| `last_used_at` (optional) | time (RFC 3339) or null | To the minute. |
| `name` | text |  |
| `owner` (optional) | text or null | The user it belongs to; none for a service token made by an admin. |
| `permissions` | list of text | `payloads`, `audit` (admins' tokens have all). |
| `role` | text | `viewer`, `operator` or `admin`. |

### `UserInfo`

| Field | Type | |
|---|---|---|
| `created_at` | time (RFC 3339) |  |
| `directory_checked_at` (optional) | time (RFC 3339) or null | A directory account: when the directory was last asked about them (sign-in or refresh). |
| `directory_refused` | true or false | A directory account the directory refused at its last check (no group, disabled or gone there): it can't sign in or act until the directory says yes again. |
| `disabled` | true or false |  |
| `display_name` | text |  |
| `email` | text |  |
| `id` | number |  |
| `last_sign_in_at` (optional) | time (RFC 3339) or null |  |
| `locked_until` (optional) | time (RFC 3339) or null | Refused after too many wrong passwords, until then. |
| `made_for_owner` | true or false | A directory account the engine made because an owner text names them, before their first sign-in (directory accounts are otherwise made at one). |
| `must_change_password` | true or false | An admin chose the password; the next sign-in can only change it. |
| `permissions` | list of text | `payloads`, `audit` (admins have both whatever is listed). |
| `role` | text | `viewer`, `operator` or `admin`. |
| `source` | text | Where the password is checked: `local`, or a directory. |
| `username` | text |  |

