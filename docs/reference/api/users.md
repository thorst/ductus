# Users

People who sign in (admin).

## `GET /api/v1/users`

All users. Admin.

**Who may call it:** an admin.

**Answers**

- `200` (application/json): list of [`UserInfo`](#userinfo)
- `403` (application/json): [`ErrorBody`](#errorbody)

## `POST /api/v1/users`

Make an account with a temporary password (the person changes it at their first sign-in). Admin.

**Who may call it:** an admin.

**Body** (application/json): [`NewUserRequest`](#newuserrequest)

**Answers**

- `200` (application/json): [`UserInfo`](#userinfo)
- `400` (application/json): [`ErrorBody`](#errorbody)
- `403` (application/json): [`ErrorBody`](#errorbody)

## `PATCH /api/v1/users/{id}`

Change a user's details, role, permissions, or disable them. Refused when it would leave no admin who can sign in. Admin.

**Who may call it:** an admin.

**Parameters**

| | In | Type | |
|---|---|---|---|
| `id` (required) | path | number | User id. |

**Body** (application/json): [`UserUpdate`](#userupdate)

**Answers**

- `200` (application/json): [`UserInfo`](#userinfo)
- `400` (application/json): [`ErrorBody`](#errorbody)
- `403` (application/json): [`ErrorBody`](#errorbody)
- `404` (application/json): [`ErrorBody`](#errorbody)

## `POST /api/v1/users/{id}/password`

Give a user a new temporary password (their sessions end; they must change it when they sign in). Admin.

**Who may call it:** an admin.

**Parameters**

| | In | Type | |
|---|---|---|---|
| `id` (required) | path | number | User id. |

**Body** (application/json): [`PasswordReset`](#passwordreset)

**Answers**

- `204`
- `400` (application/json): [`ErrorBody`](#errorbody)
- `403` (application/json): [`ErrorBody`](#errorbody)
- `404` (application/json): [`ErrorBody`](#errorbody)

## `POST /api/v1/users/{id}/unlock`

Let a user who was locked out after wrong passwords try again. Admin.

**Who may call it:** an admin.

**Parameters**

| | In | Type | |
|---|---|---|---|
| `id` (required) | path | number | User id. |

**Answers**

- `204`
- `403` (application/json): [`ErrorBody`](#errorbody)
- `404` (application/json): [`ErrorBody`](#errorbody)

## Shapes

The JSON bodies the operations above take and return.

### `ErrorBody`

| Field | Type | |
|---|---|---|
| `error` | text | What went wrong, in plain words. |

### `NewUserRequest`

| Field | Type | |
|---|---|---|
| `display_name` (optional) | text or null |  |
| `email` (optional) | text or null |  |
| `password` | text | A temporary password: the person must change it at their first sign-in. |
| `permissions` (optional) | list of text or null | `payloads` and/or `audit`. |
| `role` (optional) | text or null | `viewer` (default), `operator` or `admin`. |
| `username` | text |  |

### `PasswordReset`

| Field | Type | |
|---|---|---|
| `password` | text | The temporary password; the person must change it at their next sign-in. |

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

### `UserUpdate`

| Field | Type | |
|---|---|---|
| `disabled` (optional) | true or false or null | Disabled users can't sign in, and their sessions and tokens stop at once. |
| `display_name` (optional) | text or null |  |
| `email` (optional) | text or null |  |
| `permissions` (optional) | list of text or null |  |
| `role` (optional) | text or null |  |

