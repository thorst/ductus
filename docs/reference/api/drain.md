# Drain

Stop taking messages and deliver what's stored.

## `GET /api/v1/drain`

Drain mode: accepting, draining or drained, and what's still pending.

**Who may call it:** anyone signed in (a person or a token).

**Answers**

- `200` (application/json): [`DrainStatus`](#drainstatus)

## `POST /api/v1/drain`

Start draining: stop taking new messages (MLLP listeners close, queued HTTP answers 503, file pickup and schedules pause) and keep delivering what's stored. Operator.

**Who may call it:** an operator or admin.

**Body** (application/json): [`DrainRequest`](#drainrequest)

**Answers**

- `200` (application/json): [`DrainStatus`](#drainstatus)
- `403` (application/json): [`ErrorBody`](#errorbody)

## `DELETE /api/v1/drain`

Stop draining: take messages again. Operator.

**Who may call it:** an operator or admin.

**Answers**

- `200` (application/json): [`DrainStatus`](#drainstatus)
- `403` (application/json): [`ErrorBody`](#errorbody)

## Shapes

The JSON bodies the operations above take and return.

### `DrainRequest`

| Field | Type | |
|---|---|---|
| `stop` (optional) | true or false | Exit the engine once drained (for an upgrade or a server move). |

### `DrainStatus`

| Field | Type | |
|---|---|---|
| `paused` | list of text | Paused destinations (`interface/destination`): they keep their messages and don't hold the drain up. |
| `pending` | number | Deliveries still to go, over all interfaces. |
| `since` (optional) | time (RFC 3339) or null | When draining started, or when it finished. |
| `state` | text | `accepting`, `draining` or `drained`. |
| `stop_when_drained` | true or false | The engine exits once drained. |
| `waiting_for` | list of text | Interfaces still holding pending deliveries (a destination that's down, or a stopped interface, keeps a drain from finishing). |

### `ErrorBody`

| Field | Type | |
|---|---|---|
| `error` | text | What went wrong, in plain words. |

