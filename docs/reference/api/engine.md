# Engine

The engine, its interfaces and live updates.

## `GET /api/v1/engine`

The engine: version, environment, uptime, drain state.

**Who may call it:** anyone signed in (a person or a token).

**Answers**

- `200` (application/json): [`EngineInfo`](#engineinfo)

## `GET /api/v1/events`

Live updates (Server-Sent Events): an `update` event with every interface's queues, health and the drain state, sent at once and then whenever a count or a health state changes or a delivery finishes (checked every 2 s).

**Who may call it:** anyone signed in (a person or a token).

**Answers**

- `200` `text/event-stream` of `update` events (text/event-stream): [`LiveUpdate`](#liveupdate)

## `GET /api/v1/health`

Is the engine up? Needs no token (for load balancers and monitoring).

**Who may call it:** anyone, without signing in.

**Answers**

- `200` (application/json): [`Health`](#health)

## `GET /api/v1/hotspots`

Where each destination's backlog sits: the lanes messages wait in (the ordering key's hash), the first message of each, and which worker a failing message holds up. Only destinations with messages waiting are listed.

**Who may call it:** anyone signed in (a person or a token).

**Parameters**

| | In | Type | |
|---|---|---|---|
| `interface` | query | text | Only this interface's destinations. |
| `lanes` | query | number | Lanes to list per destination, most waiting first (default 10, at most 1000). Lanes whose first message is failing, and lanes no worker reads, are always listed. |

**Answers**

- `200` (application/json): list of [`DestinationHotspots`](#destinationhotspots)
- `404` (application/json): [`ErrorBody`](#errorbody)

## `GET /api/v1/interfaces`

Every interface with its state and each destination's queue.

**Who may call it:** anyone signed in (a person or a token).

**Answers**

- `200` (application/json): list of [`InterfaceStatus`](#interfacestatus)

## `GET /api/v1/interfaces/{name}`

One interface: its state, queues and what it connects.

**Who may call it:** anyone signed in (a person or a token).

**Parameters**

| | In | Type | |
|---|---|---|---|
| `name` (required) | path | text | Interface name. |

**Answers**

- `200` (application/json): [`InterfaceDetail`](#interfacedetail)
- `404` (application/json): [`ErrorBody`](#errorbody)

## `POST /api/v1/interfaces/{name}/destinations/{destination}/pause`

Pause a destination at once: nothing is sent or retried until it's resumed (a send under way finishes). Messages keep queueing. No reason needed; send one now or later (pausing again with a reason adds it). Survives restarts. Operator.

**Who may call it:** an operator or admin.

**Parameters**

| | In | Type | |
|---|---|---|---|
| `name` (required) | path | text | Interface name. |
| `destination` (required) | path | text | Destination name (`default` for one given with `destination:`). |

**Body** (application/json): [`PauseRequest`](#pauserequest)

**Answers**

- `200` (application/json): [`PauseStatus`](#pausestatus)
- `403` (application/json): [`ErrorBody`](#errorbody)
- `404` (application/json): [`ErrorBody`](#errorbody)

## `DELETE /api/v1/interfaces/{name}/destinations/{destination}/pause`

Resume a paused destination: it sends its backlog in order. Operator.

**Who may call it:** an operator or admin.

**Parameters**

| | In | Type | |
|---|---|---|---|
| `name` (required) | path | text | Interface name. |
| `destination` (required) | path | text | Destination name. |

**Answers**

- `200` (application/json): [`PauseStatus`](#pausestatus)
- `403` (application/json): [`ErrorBody`](#errorbody)
- `404` (application/json): [`ErrorBody`](#errorbody)

## `GET /api/v1/metrics`

Metrics in Prometheus's text format: messages received and delivered, retries, timings (receipt to sent, sends, transforms, respond-mode requests, database and API calls), queue sizes, stopped interfaces, drain state.

**Who may call it:** anyone signed in (a person or a token).

**Answers**

- `200` Prometheus text format (0.0.4) (text/plain): text

## Shapes

The JSON bodies the operations above take and return.

### `AllowedName`

| Field | Type | |
|---|---|---|
| `addresses` | list of text | The addresses let in for it: the last lookup that found some (kept when a later one fails). |
| `checked_at` (optional) | time (RFC 3339) or null | When it was last looked up. |
| `error` (optional) | text or null | Why the last lookup failed, if it did. |
| `name` | text |  |

### `DestinationCounts`

| Field | Type | |
|---|---|---|
| `filtered` | number |  |
| `latency` | [`Timing`](#timing) | From receipt to sent, waiting in the queue included. |
| `quarantined` | number |  |
| `retries` | number | Temporary failures (each retry counts). |
| `send` | [`Timing`](#timing) | Each successful send (the receiver's time). |
| `sent` | number |  |
| `transform` | [`Timing`](#timing) | The destination's function. |

### `DestinationHealth`

| Field | Type | |
|---|---|---|
| `behind` (optional) | text or null | Which `behind` limit is past: `messages` or `age`. |
| `catch_up_s` (optional) | number or null | Seconds to catch up at the last few minutes' rates (only while catching up). |
| `causes` | list of text | Why a backlog grows: `burst` (more arriving than normal for this hour of the week), `slow_receiver` (each send slower than normal). |
| `last_error` (optional) | [`LastError`](#lasterror) or null |  |
| `last_sent` (optional) | time (RFC 3339) or null | When it last delivered (kept across restarts). |
| `normal` | [`Rates`](#rates) | Normal for this hour of the week, learned from the hours watched (none until then). |
| `oldest_waiting_s` (optional) | number or null | Seconds the oldest waiting message has waited. |
| `recent` (optional) | [`Rates`](#rates) or null |  |
| `state` | text | `ok`, `behind` (past its `behind` limit), `error` (has quarantined messages), `stalled` (messages wait, nothing finished for a minute) or `paused` (best to worst). |
| `summary` | text | In operations' words: "behind 1,240 (oldest 12 min), catching up ~6 min; burst: 400/min received (normal 80)". |

### `DestinationHotspots`

| Field | Type | |
|---|---|---|
| `destination` | text |  |
| `findings` | list of [`FindingInfo`](#findinginfo) |  |
| `interface` | text |  |
| `lanes` | list of [`LaneInfo`](#laneinfo) | The lanes with the most waiting, plus any blocked or unread one. |
| `lanes_total` | number | Lanes the interface has (`ordering.partitions`; 1 without an ordering key). |
| `lanes_waiting` | number |  |
| `paused` | true or false |  |
| `summary` | text | In operations' words: "1,240 waiting in 3 of 64 lanes; worker 2 blocked by message ...". |
| `waiting` | number |  |
| `workers` | list of [`WorkerInfo`](#workerinfo) | Workers with something waiting. |
| `workers_total` | number | Workers each destination has (1 in the shared pool). |

### `DestinationStatus`

| Field | Type | |
|---|---|---|
| `connector` | text | Where it sends: `mllp lab.local:5000`, `http api billing /charges`, `file out`. |
| `health` | [`DestinationHealth`](#destinationhealth) | Its state and why: behind, stalled, errors, and how it compares with normal. |
| `name` | text |  |
| `owner` (optional) | text or null | Set when it isn't the interface's owner. |
| `paused` (optional) | [`PauseInfo`](#pauseinfo) or null |  |
| `pending` | number | Waiting to be delivered. |
| `quarantined` | number | Needing a person. |
| `since_start` | [`DestinationCounts`](#destinationcounts) | Counts and timings since the engine started. |

### `DrainStatus`

| Field | Type | |
|---|---|---|
| `paused` | list of text | Paused destinations (`interface/destination`): they keep their messages and don't hold the drain up. |
| `pending` | number | Deliveries still to go, over all interfaces. |
| `since` (optional) | time (RFC 3339) or null | When draining started, or when it finished. |
| `state` | text | `accepting`, `draining` or `drained`. |
| `stop_when_drained` | true or false | The engine exits once drained. |
| `waiting_for` | list of text | Interfaces still holding pending deliveries (a destination that's down, or a stopped interface, keeps a drain from finishing). |

### `EngineInfo`

| Field | Type | |
|---|---|---|
| `drain` | [`DrainStatus`](#drainstatus) |  |
| `environment` | text |  |
| `handover` (optional) | [`Handover`](#handover) or null |  |
| `holds_interfaces` | true or false | False while the engine has lost the database connection that holds its interfaces: sending, file pickup and schedules wait until it is back (receiving goes on). |
| `host` | text | The server's name. |
| `interfaces` | number |  |
| `others` | list of [`OtherEngine`](#otherengine) | Other engines running on this database, oldest first, and the interfaces each runs. An interface runs on one engine at a time. |
| `resources` | list of [`ResourceCalls`](#resourcecalls) | Calls from interface code to each database and API in resources.yaml (`fetch`: URLs called directly), since the engine started. |
| `started_at` | time (RFC 3339) |  |
| `stopped` | number | Interfaces stopped because their code ran away (their messages wait). |
| `uptime_seconds` | number |  |
| `version` | text |  |

### `ErrorBody`

| Field | Type | |
|---|---|---|
| `error` | text | What went wrong, in plain words. |

### `FindingInfo`

| Field | Type | |
|---|---|---|
| `kind` | text | `blocked` (a message keeps failing; its worker's lanes wait), `all_blocked` (every worker is retrying: the receiver looks down), `uneven` (one worker has most of the backlog), `hot_lane` (one lane has most of it), `unowned` (a lane no worker reads). |
| `lane` (optional) | number or null |  |
| `share` (optional) | number or null | Its share of the backlog (0–1), for `uneven` and `hot_lane`. |
| `text` | text |  |
| `worker` (optional) | number or null |  |

### `Handover`

| Field | Type | |
|---|---|---|
| `at` | time (RFC 3339) | When it held the interfaces. |
| `from_pid` | number |  |
| `from_version` | text | The version it took over from. |
| `sending_paused_ms` | number | From the old engine's stop until this one sent (receiving never stopped). |

### `HeadInfo`

The first message waiting in a lane: the next one its worker handles.

| Field | Type | |
|---|---|---|
| `attempts` | number | Failed attempts: above zero, its worker is retrying it and its lanes wait. |
| `id` | number | The message (`GET /messages/{id}`). |
| `last_attempt_at` (optional) | time (RFC 3339) or null |  |
| `last_error` (optional) | text or null |  |
| `ordering_key` (optional) | text or null |  |
| `received_at` | time (RFC 3339) |  |
| `waiting_s` | number |  |

### `Health`

| Field | Type | |
|---|---|---|
| `drain` | text | `accepting`, `draining` or `drained`: a load balancer can stop sending when it isn't `accepting`. |
| `status` | text | Always `ok` when the engine answers. |

### `InterfaceCounts`

| Field | Type | |
|---|---|---|
| `duplicates` | number | Stored as duplicates, not queued. |
| `received` | number | Stored and queued. |
| `request_errors` (optional) | number or null |  |
| `requests` (optional) | [`Timing`](#timing) or null |  |
| `store_failures` | number | Couldn't be stored (the sender was told to retry). |

### `InterfaceDetail`

[`InterfaceStatus`](#interfacestatus) or object

### `InterfaceHealth`

| Field | Type | |
|---|---|---|
| `last_received` (optional) | time (RFC 3339) or null | When something last arrived (kept across restarts). |
| `state` | text | `ok`, `silent` (nothing within `active_within`), `missed` (nothing by `active_by`) or `stopped` (best to worst). |
| `summary` | text | In operations' words: "silent: nothing received for 47 min". |

### `InterfaceStatus`

| Field | Type | |
|---|---|---|
| `accepting` | true or false | False while draining. |
| `description` (optional) | text or null |  |
| `destinations` | list of [`DestinationStatus`](#destinationstatus) |  |
| `health` | [`InterfaceHealth`](#interfacehealth) | What it should have received (`monitor:` in interface.yaml), checked now. |
| `isolation` | text | `shared` or `dedicated`. |
| `name` | text |  |
| `owner` (optional) | text or null |  |
| `retention_days` (optional) | number or null | Days its messages are kept, as configured (none in respond mode: it stores nothing). `GET /retention` has changes in progress. |
| `since_start` | [`InterfaceCounts`](#interfacecounts) | Counts since the engine started (they start at zero with it). |
| `source` | text | Where messages come from: `mllp 0.0.0.0:6661`, `http POST /orders on 0.0.0.0:8080 (queue)`. |
| `state` | text | `running`, or `stopped` (its code ran away: its messages wait until it's fixed and the engine restarted). |
| `stopped_reason` (optional) | text or null |  |
| `workers` | number | Parallel workers (dedicated) or 1 (shared). |

### `LaneInfo`

| Field | Type | |
|---|---|---|
| `head` | [`HeadInfo`](#headinfo) |  |
| `lane` | number |  |
| `waiting` | number |  |
| `worker` (optional) | number or null | None: no worker reads it (`ordering.partitions` was lowered while it had messages). |

### `LastError`

| Field | Type | |
|---|---|---|
| `at` | time (RFC 3339) |  |
| `error` | text |  |

### `LiveDestination`

| Field | Type | |
|---|---|---|
| `health` | [`LiveHealth`](#livehealth) | As in `GET /interfaces` (an update is sent when a state changes, not as a summary's minutes tick on). |
| `name` | text |  |
| `paused` | true or false |  |
| `pending` | number |  |
| `quarantined` | number |  |

### `LiveHealth`

| Field | Type | |
|---|---|---|
| `state` | text |  |
| `summary` | text |  |

### `LiveInterface`

| Field | Type | |
|---|---|---|
| `accepting` | true or false |  |
| `destinations` | list of [`LiveDestination`](#livedestination) |  |
| `health` | [`LiveHealth`](#livehealth) | What it receives, as in `GET /interfaces`. |
| `name` | text |  |
| `state` | text |  |

### `LiveUpdate`

| Field | Type | |
|---|---|---|
| `at` | time (RFC 3339) |  |
| `drain` | [`DrainStatus`](#drainstatus) |  |
| `interfaces` | list of [`LiveInterface`](#liveinterface) |  |

### `OtherEngine`

| Field | Type | |
|---|---|---|
| `environment` | text |  |
| `host` | text |  |
| `interfaces` | list of text | The interfaces it runs (respond-mode HTTP interfaces aren't claimed, so not listed). |
| `pid` | number |  |
| `started_at` | time (RFC 3339) |  |
| `version` | text |  |

### `PauseInfo`

| Field | Type | |
|---|---|---|
| `at` | time (RFC 3339) |  |
| `by` | text | The token that paused it. |
| `reason` (optional) | text or null | Why, once someone says (it can be added after the pause). |
| `reason_at` (optional) | time (RFC 3339) or null |  |
| `reason_by` (optional) | text or null |  |

### `PauseRequest`

| Field | Type | |
|---|---|---|
| `reason` (optional) | text or null | Why (optional: pause first, explain after; pausing again with a reason adds it). |

### `PauseStatus`

| Field | Type | |
|---|---|---|
| `destination` | text |  |
| `interface` | text |  |
| `paused` (optional) | [`PauseInfo`](#pauseinfo) or null |  |
| `pending` | number | Messages waiting for it. |

### `Rates`

| Field | Type | |
|---|---|---|
| `done_per_min` (optional) | number or null | Messages finished per minute (sent, filtered or quarantined). |
| `received_per_min` (optional) | number or null | Messages arriving per minute. |
| `send_ms` (optional) | number or null | A successful send's mean time. |

### `ResourceCalls`

| Field | Type | |
|---|---|---|
| `calls` | [`Timing`](#timing) |  |
| `errors` | number | Calls that failed (network, SQL, timeout; an HTTP error status is an answer, not a failure). |
| `kind` | text | `db`, `api` or `fetch`. |
| `name` | text |  |

### `SourceAllow`

A listener's `allow:` list and how it's doing.

| Field | Type | |
|---|---|---|
| `entries` | list of text | The list as written: addresses, subnets and host names. |
| `last_refused` (optional) | text or null | The last refused connection's address and time. |
| `last_refused_at` (optional) | time (RFC 3339) or null |  |
| `names` | list of [`AllowedName`](#allowedname) | Each host name in it with what it resolves to now. |
| `refused` | number | Connections refused since the engine started (0 when this engine doesn't hold the listener). |

### `SourceTls`

TLS on an interface's listener.

| Field | Type | |
|---|---|---|
| `address` | text | Where: `0.0.0.0:6661`, `POST /orders on 0.0.0.0:8443 (queue)`. |
| `certificate` | text | The `certificates:` pair it presents. |
| `clients_ca` (optional) | text or null | Senders need a certificate this CA (a `certificates:` entry) issued; none: any sender. |
| `protocol` | text | `MLLP over TLS` or `HTTPS`. |
| `subjects` | list of text | And one of these subjects (none: any the CA issued). |

### `SystemInfo`

| Field | Type | |
|---|---|---|
| `application` (optional) | text or null |  |
| `vendor` (optional) | text or null |  |

### `Timing`

How long something took, over every time since the engine started (estimated from histogram buckets, like Prometheus does: good to a bucket's width).

| Field | Type | |
|---|---|---|
| `count` | number |  |
| `mean_ms` (optional) | number or null |  |
| `p50_ms` (optional) | number or null |  |
| `p95_ms` (optional) | number or null |  |

### `WorkerInfo`

| Field | Type | |
|---|---|---|
| `blocked_by` (optional) | number or null | The lane whose first message it keeps retrying. |
| `lanes_waiting` | number |  |
| `waiting` | number |  |
| `worker` | number |  |

