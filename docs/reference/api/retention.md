# Retention

How long messages are kept, and changes to it.

## `GET /api/v1/retention`

Every interface's retention, expired partitions that had to stay, and the held area.

**Who may call it:** anyone signed in (a person or a token).

**Answers**

- `200` (application/json): [`RetentionReport`](#retentionreport)

## `POST /api/v1/retention/{name}/confirm`

Confirm an interface's shorter retention, which deletes its messages past it. It's applied at once (or when the running change finishes). Operator.

**Who may call it:** an operator or admin.

**Parameters**

| | In | Type | |
|---|---|---|---|
| `name` (required) | path | text | Interface name. |

**Answers**

- `200` (application/json): [`InterfaceRetention`](#interfaceretention)
- `404` (application/json): [`ErrorBody`](#errorbody)
- `409` (application/json): [`ErrorBody`](#errorbody)

## Shapes

The JSON bodies the operations above take and return.

### `Change`

| Field | Type | |
|---|---|---|
| `from` | number |  |
| `rows_left` (optional) | number or null | Messages still to move or delete (an estimate). |
| `started_at` | time (RFC 3339) |  |
| `to` | number |  |

### `ErrorBody`

| Field | Type | |
|---|---|---|
| `error` | text | What went wrong, in plain words. |

### `Held`

| Field | Type | |
|---|---|---|
| `bytes` | number | Disk it uses, indexes included. |
| `interfaces` | list of [`HeldInterface`](#heldinterface) | Interfaces with unfinished messages in it. |
| `messages` | number | Messages in it, finished ones included (they go when their month drops). |
| `months` | list of [`HeldMonth`](#heldmonth) | By month: a month is dropped once nothing in it is unfinished. |
| `pending` | number |  |
| `quarantined` | number |  |

### `HeldInterface`

| Field | Type | |
|---|---|---|
| `name` | text |  |
| `pending` | number |  |
| `quarantined` | number |  |

### `HeldMonth`

| Field | Type | |
|---|---|---|
| `bytes` | number |  |
| `messages` | number |  |
| `month` | text | The month, `2026-09`. |
| `partition` | text |  |
| `pending` | number |  |
| `quarantined` | number |  |

### `InterfaceRetention`

| Field | Type | |
|---|---|---|
| `change` (optional) | [`Change`](#change) or null |  |
| `configured` | number | What interface.yaml (or ductus.yaml) says, in days. |
| `days` | number | The retention in force: its new messages are kept this many days. |
| `name` | text |  |
| `needs_confirmation` | true or false | The waiting value deletes messages: confirm it (`POST /retention/{name}/confirm`). |
| `waiting` (optional) | number or null | A configured value not applied yet: a change is running, or it's shorter and needs a confirmation. |
| `would_delete` (optional) | number or null | How many messages the waiting value would delete now, when it needs a confirmation. |

### `KeptPartition`

| Field | Type | |
|---|---|---|
| `kept_for` | list of text | Interfaces that now keep their messages longer, with messages still in it. |
| `partition` | text |  |
| `pending` | number |  |
| `quarantined` | number |  |
| `tier` | number | Its retention tier, in days. |

### `RetentionReport`

| Field | Type | |
|---|---|---|
| `held` | [`Held`](#held) | The held area as of the last drop: messages past their retention that someone still needs (pending for a destination, or quarantined and kept until resolved). |
| `interfaces` | list of [`InterfaceRetention`](#interfaceretention) |  |
| `kept` | list of [`KeptPartition`](#keptpartition) | Expired partitions kept at the last drop, and why. |
| `last_drop_at` (optional) | time (RFC 3339) or null | When expired partitions were last dropped (hourly). |

