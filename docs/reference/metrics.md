# Prometheus metrics

`GET /api/v1/metrics` serves these in Prometheus's text format, for Prometheus, Grafana Agent, Datadog or anything else that scrapes it. It needs a token like every other call: in Prometheus's scrape configuration, `metrics_path: /api/v1/metrics` and `authorization: { credentials: dux_… }` (a viewer token: `ductus token create prometheus --role viewer`).

Counters and histograms count from when the engine started (`rate()` and `increase()` handle restarts); queue sizes are read from the database at each scrape. Each destination adds a series per family, so very many destinations mean many series.

| Metric | Type | Labels | What it measures |
|---|---|---|---|
| `ductus_start_time_seconds` | gauge |  | When the engine started (Unix time). |
| `ductus_draining` | gauge |  | 1 while draining or drained (not taking new messages). |
| `ductus_alerts_open` | gauge | `severity` | Open alerts, by severity. Only with `alerts:` in ductus.yaml. |
| `ductus_alert_send_failures_total` | counter | `channel` | Alert notifications that couldn't be sent (each try counts), by channel (`owner`: owners' addresses). A failing channel can't alert about itself: watch this. |
| `ductus_certificate_expiry_seconds` | gauge | `side`, `resource`, `host`, `port` | Seconds until the earliest expiry in a certificate chain (negative once expired): a server's as of its last check (side remote), or one the engine presents (side ours: host is this server, port 0). |
| `ductus_interface_stopped` | gauge | `interface` | 1 if the interface was stopped because its code ran away. |
| `ductus_messages_received_total` | counter | `interface` | Messages stored and queued. |
| `ductus_messages_duplicate_total` | counter | `interface` | Messages stored as duplicates (not queued). |
| `ductus_messages_store_failures_total` | counter | `interface` | Messages that couldn't be stored (the sender was told to retry). |
| `ductus_deliveries_total` | counter | `interface`, `destination`, `outcome` | Messages a destination finished, by outcome (sent, filtered, quarantined). |
| `ductus_delivery_retries_total` | counter | `interface`, `destination` | Temporary failures (each retry counts). |
| `ductus_delivery_latency_seconds` | histogram | `interface`, `destination` | From receipt to sent, waiting in the queue included. |
| `ductus_send_seconds` | histogram | `interface`, `destination` | Each successful send: the receiver's time. |
| `ductus_transform_seconds` | histogram | `interface`, `destination` | The destination's function, per message. |
| `ductus_queue_pending` | gauge | `interface`, `destination` | Messages waiting for a destination. |
| `ductus_queue_quarantined` | gauge | `interface`, `destination` | Messages quarantined by a destination (needing a person). |
| `ductus_queue_oldest_seconds` | gauge | `interface`, `destination` | How long the oldest message waiting for a destination has waited. |
| `ductus_destination_state` | gauge | `interface`, `destination` | 0 ok, 1 behind (past its `behind` limit), 2 error (quarantined messages), 3 stalled, 4 paused. |
| `ductus_interface_state` | gauge | `interface` | 0 ok, 1 silent (nothing within `active_within`), 2 missed (nothing by `active_by`), 3 stopped. |
| `ductus_destination_paused` | gauge | `interface`, `destination` | 1 while a destination is paused on purpose (`ductus pause`). |
| `ductus_held_messages` | gauge | `interface`, `status` | Unfinished messages past their retention, in the held area (as of the hourly retention run). |
| `ductus_held_bytes` | gauge |  | Disk used by the held area (as of the hourly retention run). |
| `ductus_http_request_seconds` | histogram | `interface` | Respond-mode HTTP requests: time in the engine. |
| `ductus_http_request_errors_total` | counter | `interface` | Respond-mode HTTP requests that failed. |
| `ductus_resource_call_seconds` | histogram | `kind`, `resource` | Calls from interface code to a database or API. |
| `ductus_resource_call_errors_total` | counter | `kind`, `resource` | Calls that failed (network, SQL, timeout). |
