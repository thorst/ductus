# Alerts

Alerts: what is wrong, acknowledging and silencing, and where notifications go.

## `GET /api/v1/alerts`

Alerts: open ones first, and what happened lately.

**Who may call it:** anyone signed in (a person or a token).

**Answers**

- `200` (application/json): [`AlertsOverview`](#alertsoverview)

## `GET /api/v1/alerts/channels`

Where alerts go, and how sending has gone lately. Secrets are never shown. Operator.

**Who may call it:** an operator or admin.

**Answers**

- `200` (application/json): [`ChannelsOverview`](#channelsoverview)
- `403` (application/json): [`ErrorBody`](#errorbody)

## `POST /api/v1/alerts/channels/{name}/test`

Send a test notification to a channel now (it says it is a test). Answers 502 with the reason when it can't be sent. Operator; logged.

**Who may call it:** an operator or admin.

**Parameters**

| | In | Type | |
|---|---|---|---|
| `name` (required) | path | text | Channel name. |

**Answers**

- `200` (application/json): [`TestResult`](#testresult)
- `403` (application/json): [`ErrorBody`](#errorbody)
- `404` (application/json): [`ErrorBody`](#errorbody)
- `502` (application/json): [`ErrorBody`](#errorbody)

## `GET /api/v1/alerts/{id}`

One alert and its history.

**Who may call it:** anyone signed in (a person or a token).

**Parameters**

| | In | Type | |
|---|---|---|---|
| `id` (required) | path | number | Alert id. |

**Answers**

- `200` (application/json): [`AlertDetail`](#alertdetail)
- `404` (application/json): [`ErrorBody`](#errorbody)

## `POST /api/v1/alerts/{id}/ack`

Acknowledge an open alert: someone is on it. No reminders until it gets worse or recovers. Operator.

**Who may call it:** an operator or admin.

**Parameters**

| | In | Type | |
|---|---|---|---|
| `id` (required) | path | number | Alert id. |

**Answers**

- `200` (application/json): [`AlertInfo`](#alertinfo)
- `403` (application/json): [`ErrorBody`](#errorbody)
- `404` (application/json): [`ErrorBody`](#errorbody)
- `409` (application/json): [`ErrorBody`](#errorbody)

## `POST /api/v1/alerts/{id}/silence`

Silence an alert: nothing is sent about it (fired, worse, reminders, recovery) until the time is up. It still shows. Works before anything fires (planned work). A silence is not a pause: messages still wait. Operator.

**Who may call it:** an operator or admin.

**Parameters**

| | In | Type | |
|---|---|---|---|
| `id` (required) | path | number | Alert id. |

**Body** (application/json): [`SilenceRequest`](#silencerequest)

**Answers**

- `200` (application/json): [`AlertInfo`](#alertinfo)
- `400` (application/json): [`ErrorBody`](#errorbody)
- `403` (application/json): [`ErrorBody`](#errorbody)
- `404` (application/json): [`ErrorBody`](#errorbody)

## `DELETE /api/v1/alerts/{id}/silence`

End a silence early. Operator.

**Who may call it:** an operator or admin.

**Parameters**

| | In | Type | |
|---|---|---|---|
| `id` (required) | path | number | Alert id. |

**Answers**

- `200` (application/json): [`AlertInfo`](#alertinfo)
- `403` (application/json): [`ErrorBody`](#errorbody)
- `404` (application/json): [`ErrorBody`](#errorbody)

## Shapes

The JSON bodies the operations above take and return.

### `AlertDetail`

| Field | Type | |
|---|---|---|
| `alert` | [`AlertInfo`](#alertinfo) |  |
| `events` | list of [`AlertEvent`](#alertevent) | Newest first (the last 100). |

### `AlertEvent`

| Field | Type | |
|---|---|---|
| `alert` | number |  |
| `at` | time (RFC 3339) |  |
| `by` (optional) | text or null | Who acknowledged or silenced it. |
| `failed` | number |  |
| `id` | number |  |
| `key` | text |  |
| `kind` | text | `fired`, `escalated`, `reminded`, `recovered`, `acknowledged`, `silenced`, `unsilenced`. |
| `sent` | number | Its notifications: sent, still to send (waiting for quiet hours to end, or being retried), and given up on. |
| `severity` (optional) | text or null |  |
| `state` (optional) | text or null | The state then (for `recovered`: what it recovered from). |
| `summary` (optional) | text or null |  |
| `waiting` | number |  |

### `AlertInfo`

| Field | Type | |
|---|---|---|
| `acknowledged_at` (optional) | time (RFC 3339) or null |  |
| `acknowledged_by` (optional) | text or null |  |
| `destination` (optional) | text or null | The destination; for a certificate, `host:port`. |
| `id` | number |  |
| `interface` | text | The interface; for a certificate, its resource. |
| `key` | text | `iface:<interface>`, `dest:<interface>/<destination>` or `cert:<resource>:<host>:<port>`. |
| `last_notified_at` (optional) | time (RFC 3339) or null | When people were last told (or will be, once quiet hours end). |
| `muted` | true or false | Its interface says `alert: { mute: true }`: nothing is ever sent about it. |
| `notifications` | number | Notifications sent since it fired. |
| `recovering_since` (optional) | time (RFC 3339) or null | Fine again since then, but not for `alerts.settle` yet (it may still reopen). |
| `severity` (optional) | text or null | `warning` or `critical` while open. |
| `silence_reason` (optional) | text or null |  |
| `silenced_by` (optional) | text or null |  |
| `silenced_until` (optional) | time (RFC 3339) or null | Set while silenced: nothing is sent until then. |
| `since` (optional) | time (RFC 3339) or null | Bad since. |
| `state` | text | `ok`, or what is wrong: `error`, `stalled`, `behind` (a destination), `stopped`, `silent`, `missed` (an interface), `certificate` (expiring or expired). |
| `summary` (optional) | text or null | The health view's words, as of the last check. |

### `AlertsOverview`

| Field | Type | |
|---|---|---|
| `alerts` | list of [`AlertInfo`](#alertinfo) | Every interface and destination, open alerts first (critical, then oldest). |
| `configured` | true or false | `alerts:` is in ductus.yaml. Without it nothing is judged or sent. |
| `recent` | list of [`AlertEvent`](#alertevent) | The latest events, newest first. |

### `ChannelCounts`

| Field | Type | |
|---|---|---|
| `failed` | number |  |
| `last_error` (optional) | text or null |  |
| `sent` | number |  |
| `waiting` | number |  |

### `ChannelInfo`

| Field | Type | |
|---|---|---|
| `email` | list of text |  |
| `failed` | number |  |
| `last_error` (optional) | text or null |  |
| `last_sent` (optional) | time (RFC 3339) or null |  |
| `name` | text |  |
| `sent` | number | Notifications in the last 7 days: sent, still to send, given up on. |
| `waiting` | number |  |
| `webhook` (optional) | text or null | Scheme and host only: the rest of a webhook URL is often its secret. |
| `webhook_format` (optional) | text or null | `json`, `slack` or `teams`. |

### `ChannelsOverview`

| Field | Type | |
|---|---|---|
| `channels` | list of [`ChannelInfo`](#channelinfo) |  |
| `configured` | true or false |  |
| `heartbeat` (optional) | text or null | The heartbeat URL (scheme and host) and how often. |
| `owner_notifications` (optional) | [`ChannelCounts`](#channelcounts) or null |  |
| `owners` | list of [`OwnerReach`](#ownerreach) | Owners named in interface.yaml, when a rule sends to `owner`. |
| `quiet` (optional) | text or null | `"22:00-06:00"` and which severities wait, if set. |
| `repeat` (optional) | text or null | Reminders: `4h`, or none. |
| `smtp` (optional) | [`SmtpInfo`](#smtpinfo) or null |  |

### `ErrorBody`

| Field | Type | |
|---|---|---|
| `error` | text | What went wrong, in plain words. |

### `OwnerReach`

| Field | Type | |
|---|---|---|
| `addresses` | list of text | Where alerts for this owner go; empty: nowhere (no user with that name and an email, not in `owners:`, not an address). |
| `owner` | text |  |

### `SilenceRequest`

| Field | Type | |
|---|---|---|
| `for` | text | How long: `30m`, `4h`, `2d` (at most 30 days). |
| `reason` (optional) | text or null | Why (planned work, a known outage): shown to everyone who sees the alert. |

### `SmtpInfo`

| Field | Type | |
|---|---|---|
| `from` | text |  |
| `host` | text |  |
| `port` | number |  |
| `tls` | text | `starttls`, `tls` or `none`. |
| `username` (optional) | text or null |  |

### `TestResult`

| Field | Type | |
|---|---|---|
| `channel` | text |  |
| `sent` | true or false |  |

