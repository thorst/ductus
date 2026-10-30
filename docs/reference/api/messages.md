# Messages

The message browser, the quarantine, and resending or correcting messages.

## `GET /api/v1/interfaces/{name}/messages`

An interface's messages, newest first, a page at a time.

**Who may call it:** anyone signed in (a person or a token).

**Parameters**

| | In | Type | |
|---|---|---|---|
| `name` (required) | path | text | Interface name. |
| `status` | query | text | Only messages with this overall status. |
| `before` | query | number | Only messages older than this id (the previous page's `next_before`). |
| `limit` | query | number | Page size, at most 500 (default 50). |

**Answers**

- `200` (application/json): [`MessagePage`](#messagepage)
- `400` (application/json): [`ErrorBody`](#errorbody)
- `404` (application/json): [`ErrorBody`](#errorbody)

## `POST /api/v1/messages/dismiss`

Dismiss many messages' quarantined deliveries at once (the quarantine screen's "dismiss selected"), in one transaction, with one reason. Operator.

**Who may call it:** an operator or admin.

**Body** (application/json): [`BulkDismissRequest`](#bulkdismissrequest)

**Answers**

- `200` (application/json): [`DismissResult`](#dismissresult)
- `400` (application/json): [`ErrorBody`](#errorbody)
- `403` (application/json): [`ErrorBody`](#errorbody)

## `POST /api/v1/messages/resend`

Send many messages again at once (the quarantine screen's "resend selected"): as `POST /messages/{id}/resend` for each, in one transaction. Operator.

**Who may call it:** an operator or admin.

**Body** (application/json): [`BulkResendRequest`](#bulkresendrequest)

**Answers**

- `200` (application/json): [`BulkResendResult`](#bulkresendresult)
- `400` (application/json): [`ErrorBody`](#errorbody)
- `403` (application/json): [`ErrorBody`](#errorbody)

## `GET /api/v1/messages/{id}`

One message: what was received, and what each destination did with it.

**Who may call it:** anyone signed in (a person or a token).

**Parameters**

| | In | Type | |
|---|---|---|---|
| `id` (required) | path | number | Message id. |

**Answers**

- `200` (application/json): [`MessageDetail`](#messagedetail)
- `404` (application/json): [`ErrorBody`](#errorbody)

## `POST /api/v1/messages/{id}/dismiss`

Dismiss a message's quarantined deliveries: nobody will fix or resend them, so they leave the quarantine (the delivery becomes `filtered`, "dismissed by NAME: reason"). Nothing is deleted: the message and what happened to it stay, and the dismissal is in the audit log with the reason. Works the same in the held area. Operator.

**Who may call it:** an operator or admin.

**Parameters**

| | In | Type | |
|---|---|---|---|
| `id` (required) | path | number | Message id. |

**Body** (application/json): [`DismissRequest`](#dismissrequest)

**Answers**

- `200` (application/json): [`DismissResult`](#dismissresult)
- `400` (application/json): [`ErrorBody`](#errorbody)
- `403` (application/json): [`ErrorBody`](#errorbody)
- `404` (application/json): [`ErrorBody`](#errorbody)
- `409` Nothing quarantined to dismiss (application/json): [`ErrorBody`](#errorbody)

## `POST /api/v1/messages/{id}/edit`

Queue a corrected copy of a message. The original stays as it was received; its quarantined deliveries to the copy's destinations are marked replaced. The copy is a new message (new id, so a new `meta.id`), never deduplicated, with the original's `origin`. Refused for a destination where the original is still pending (it may be being sent right now). Operator.

**Who may call it:** an operator or admin.

**Parameters**

| | In | Type | |
|---|---|---|---|
| `id` (required) | path | number | The message to correct. |

**Body** (application/json): [`EditRequest`](#editrequest)

**Answers**

- `201` (application/json): [`EditResult`](#editresult)
- `400` (application/json): [`ErrorBody`](#errorbody)
- `403` (application/json): [`ErrorBody`](#errorbody)
- `404` (application/json): [`ErrorBody`](#errorbody)
- `409` Still pending for a destination (application/json): [`ErrorBody`](#errorbody)

## `POST /api/v1/messages/{id}/resend`

Send a message again: its quarantined deliveries, or those to the destinations named (whatever they ended as: a replay). Each goes back to pending in place and is sent next in its lane. Pending deliveries are left alone. Operator.

**Who may call it:** an operator or admin.

**Parameters**

| | In | Type | |
|---|---|---|---|
| `id` (required) | path | number | Message id. |

**Body** (application/json): [`ResendRequest`](#resendrequest)

**Answers**

- `200` (application/json): [`ResentMessage`](#resentmessage)
- `400` (application/json): [`ErrorBody`](#errorbody)
- `403` (application/json): [`ErrorBody`](#errorbody)
- `404` (application/json): [`ErrorBody`](#errorbody)
- `409` Nothing to resend (application/json): [`ErrorBody`](#errorbody)

## `GET /api/v1/quarantine`

Quarantined deliveries across this engine's interfaces (or one interface or destination), newest message first: what needs a person. Counts per destination are in `GET /interfaces`.

**Who may call it:** anyone signed in (a person or a token).

**Parameters**

| | In | Type | |
|---|---|---|---|
| `interface` | query | text | Only this interface's. |
| `destination` | query | text | Only this destination's (with `interface`, that interface's; without, any interface's destination of that name). |
| `before` | query | text | The previous page's `next_before`. |
| `limit` | query | number | Page size, at most 500 (default 100). |

**Answers**

- `200` (application/json): [`QuarantinePage`](#quarantinepage)
- `400` (application/json): [`ErrorBody`](#errorbody)
- `404` (application/json): [`ErrorBody`](#errorbody)

## Shapes

The JSON bodies the operations above take and return.

### `ActionInfo`

| Field | Type | |
|---|---|---|
| `action` | text | `resend` (the delivery went back to pending), `edit` (a corrected copy was queued) or `dismiss` (a quarantined delivery was closed without sending). |
| `at` | time (RFC 3339) |  |
| `by` | text | The user or token that did it. |
| `copy` (optional) | number or null | `edit`: the copy's id. |
| `destination` | text |  |
| `previous_attempts` (optional) | number or null |  |
| `previous_error` (optional) | text or null |  |
| `previous_status` (optional) | text or null | The delivery before: `sent`, `filtered` or `quarantined` (none: there was none). |
| `reason` (optional) | text or null |  |

### `BulkDismissRequest`

| Field | Type | |
|---|---|---|
| `destination` (optional) | text or null | Only the destination with this name in each message's interface. Without it: all quarantined deliveries of each message. |
| `ids` | list of number | Message ids (at most 500), from any interfaces. |
| `reason` | text | Why. Required; kept with every dismissal and in the audit log. |

### `BulkResendRequest`

| Field | Type | |
|---|---|---|
| `destination` (optional) | text or null | Resend to the destination with this name in each message's interface, whatever its delivery ended as (a replay). Without it: every destination that quarantined each message. |
| `ids` | list of number | Message ids (at most 500), from any interfaces. |
| `quarantined_only` (optional) | true or false or null | Only deliveries still quarantined, even with `destination` named (what the quarantine screen sends: a delivery someone resolved meanwhile is skipped, not replayed). Default: true without `destination`, false with it. |
| `reason` (optional) | text or null | Why (kept with each action). |

### `BulkResendResult`

| Field | Type | |
|---|---|---|
| `resent` | list of [`ResentMessage`](#resentmessage) | The messages sent again, by id. |
| `skipped` | list of number | Ids with nothing to resend: not found, not run by this engine, a duplicate, or no delivery that matched (pending already, or not quarantined). |

### `DeliveryInfo`

| Field | Type | |
|---|---|---|
| `attempts` | number |  |
| `destination` | text |  |
| `last_error` (optional) | text or null |  |
| `output` (optional) | text or null | What was sent, for destinations with `save_output: true`. |
| `status` | text | `pending`, `sent`, `filtered` or `quarantined`. |

### `DismissRequest`

| Field | Type | |
|---|---|---|
| `destinations` (optional) | list of text or null | Dismiss only these destinations' quarantined deliveries. Without it: all of the message's. |
| `reason` | text | Why nobody will fix or resend these. Required: it is kept in the audit log. |

### `DismissResult`

| Field | Type | |
|---|---|---|
| `dismissed` | list of [`ResentMessage`](#resentmessage) | The messages with a delivery dismissed (`previous_status` is `quarantined`; `previous_error` is why it was). |
| `skipped` | list of number | Ids with nothing to dismiss: not found, not run by this engine, or no delivery still quarantined (someone resolved it meanwhile). |

### `EditRequest`

| Field | Type | |
|---|---|---|
| `destinations` (optional) | list of text or null | Where the copy goes. Without it: the destinations that quarantined the original. |
| `payload` | text | The corrected message, in full (HL7 segments separated by `\r`). |
| `reason` (optional) | text or null | Why (kept with the action). |

### `EditResult`

| Field | Type | |
|---|---|---|
| `destinations` | list of text | Where the copy goes. |
| `id` | number | The copy's id (a new message, queued at the back of its lane). |
| `interface` | text |  |
| `original` | number | The message it corrects. |
| `replaced` | list of text | Destinations whose quarantined delivery of the original is now filtered ("replaced by edited copy N"). |

### `ErrorBody`

| Field | Type | |
|---|---|---|
| `error` | text | What went wrong, in plain words. |

### `MessageDetail`

[`MessageSummary`](#messagesummary) or object

### `MessagePage`

| Field | Type | |
|---|---|---|
| `messages` | list of [`MessageSummary`](#messagesummary) | Newest first. |
| `next_before` (optional) | number or null | Pass as `before` for the next (older) page; absent on the last page. |

### `MessageSummary`

| Field | Type | |
|---|---|---|
| `control_id` (optional) | text or null | MSH-10 for HL7. |
| `id` | number |  |
| `interface` | text |  |
| `last_error` (optional) | text or null | Why it was quarantined or filtered (per destination when there are several). |
| `ordering_key` (optional) | text or null |  |
| `origin` (optional) | number or null | Routed from another interface: the id of the message there (`GET /messages/{origin}`). |
| `received_at` | time (RFC 3339) |  |
| `size` | number | Payload size in bytes. |
| `status` | text | `pending`, `sent`, `filtered`, `quarantined` or `duplicate`: the overall status (pending until every destination is done). |
| `updated_at` | time (RFC 3339) |  |

### `QuarantinePage`

| Field | Type | |
|---|---|---|
| `deliveries` | list of [`QuarantinedDelivery`](#quarantineddelivery) | Newest message first; a message quarantined by two destinations is listed twice. |
| `next_before` (optional) | text or null | Pass as `before` for the next page; absent on the last page. |

### `QuarantinedDelivery`

| Field | Type | |
|---|---|---|
| `attempts` | number |  |
| `control_id` (optional) | text or null | MSH-10 for HL7. |
| `destination` | text |  |
| `id` | number | The message's id. |
| `interface` | text |  |
| `last_error` (optional) | text or null | Why. |
| `ordering_key` (optional) | text or null |  |
| `quarantined_at` | time (RFC 3339) |  |
| `received_at` | time (RFC 3339) |  |
| `size` | number | Payload size in bytes. |

### `ResendRequest`

| Field | Type | |
|---|---|---|
| `destinations` (optional) | list of text or null | Resend to these destinations, whatever their delivery ended as (sent and filtered too: a replay). Without it: every destination that quarantined the message. |
| `reason` (optional) | text or null | Why (kept with the action). |

### `ResentDelivery`

| Field | Type | |
|---|---|---|
| `destination` | text |  |
| `previous_attempts` | number |  |
| `previous_error` (optional) | text or null |  |
| `previous_status` | text | What the delivery was before: `sent`, `filtered` or `quarantined`. |

### `ResentMessage`

| Field | Type | |
|---|---|---|
| `destinations` | list of [`ResentDelivery`](#resentdelivery) | The deliveries now pending again. |
| `id` | number |  |
| `interface` | text |  |

