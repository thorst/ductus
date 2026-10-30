# Session

Signing a browser in and out.

## `GET /api/v1/session`

Who is calling, and as what. Works while a password change is still required.

**Who may call it:** anyone signed in (a person or a token).

**Answers**

- `200` (application/json): [`Session`](#session)
- `401` (application/json): [`ErrorBody`](#errorbody)

## `POST /api/v1/session`

Sign a browser in with a username and password, or a token: answers with a session cookie (`ductus_session`), which then stands in for them. Needs the `X-Ductus: 1` header. After too many wrong passwords for one username the account refuses sign-in for a while (429).

**Who may call it:** anyone, without signing in.

**Body** (application/json): [`SignIn`](#signin)

**Answers**

- `200` (application/json): [`Session`](#session)
- `401` (application/json): [`ErrorBody`](#errorbody)
- `403` (application/json): [`ErrorBody`](#errorbody)
- `429` (application/json): [`ErrorBody`](#errorbody)

## `DELETE /api/v1/session`

Sign out: the session ends at once and the cookie is cleared.

**Who may call it:** anyone, without signing in.

**Answers**

- `204`

## Shapes

The JSON bodies the operations above take and return.

### `ErrorBody`

| Field | Type | |
|---|---|---|
| `error` | text | What went wrong, in plain words. |

### `Session`

| Field | Type | |
|---|---|---|
| `ends_at` (optional) | time (RFC 3339) or null | When the session ends at the latest (it also ends when unused for too long). None when the call came with a token instead of a session. |
| `kind` | text | `user` (signed in with a password) or `token`. |
| `must_change_password` | true or false | An admin chose this password: nothing else works until it is changed (`POST /me/password`). |
| `name` | text | The username, or the token's name. |
| `permissions` | list of text | What the role doesn't say: `payloads`, `audit`. Admins have both. |
| `role` | text | `viewer`, `operator` or `admin`. |

### `SignIn`

| Field | Type | |
|---|---|---|
| `password` (optional) | text or null |  |
| `token` (optional) | text or null | ...or an API token (`dux_...`), unless `auth.token_sign_in` is off. |
| `username` (optional) | text or null | A user's name (with `password`)... |

