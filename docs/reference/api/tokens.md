# Tokens

API tokens (admin).

## `GET /api/v1/tokens`

Live tokens, everyone's (never their secrets). Admin.

**Who may call it:** an admin.

**Answers**

- `200` (application/json): list of [`TokenInfo`](#tokeninfo)
- `403` (application/json): [`ErrorBody`](#errorbody)

## `POST /api/v1/tokens`

Create a service token (it belongs to nobody). Its secret is in this answer only. Admin.

**Who may call it:** an admin.

**Body** (application/json): [`NewToken`](#newtoken)

**Answers**

- `200` (application/json): [`CreatedToken`](#createdtoken)
- `400` (application/json): [`ErrorBody`](#errorbody)
- `403` (application/json): [`ErrorBody`](#errorbody)

## `DELETE /api/v1/tokens/{name}`

Revoke a token: it stops working at once. Admin.

**Who may call it:** an admin.

**Parameters**

| | In | Type | |
|---|---|---|---|
| `name` (required) | path | text | Token name. |

**Answers**

- `204` Revoked
- `403` (application/json): [`ErrorBody`](#errorbody)
- `404` (application/json): [`ErrorBody`](#errorbody)

## Shapes

The JSON bodies the operations above take and return.

### `CreatedToken`

| Field | Type | |
|---|---|---|
| `secret` | text | The token itself: shown this once, never stored (only its hash is). |
| `token` | [`TokenInfo`](#tokeninfo) |  |

### `ErrorBody`

| Field | Type | |
|---|---|---|
| `error` | text | What went wrong, in plain words. |

### `NewToken`

| Field | Type | |
|---|---|---|
| `name` | text | Who or what uses it, e.g. `ci` or `monitoring`. |
| `permissions` (optional) | list of text or null | `payloads` and/or `audit`. None by default: a token can't read message payloads unless it is given the permission. |
| `role` (optional) | text or null | `viewer` (default), `operator` or `admin`. |

### `TokenInfo`

| Field | Type | |
|---|---|---|
| `created_at` | time (RFC 3339) |  |
| `last_used_at` (optional) | time (RFC 3339) or null | To the minute. |
| `name` | text |  |
| `owner` (optional) | text or null | The user it belongs to; none for a service token made by an admin. |
| `permissions` | list of text | `payloads`, `audit` (admins' tokens have all). |
| `role` | text | `viewer`, `operator` or `admin`. |

