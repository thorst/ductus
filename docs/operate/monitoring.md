# Monitoring and metrics

The engine judges its own health: every interface and destination has a state, worked out from
what it expects ([`monitor`](../configure/keeping-and-watching.md#whats-expected-monitor)) and what
it sees. The console, `ductus status`, the API and Prometheus all show the same states, and
[alerts](alerts.md) are made from them. This page is what each state means, how to look deeper,
and what to graph.

## Health: what each state means

| State | Of | Means | What to do |
|---|---|---|---|
| **ok** | both | nothing to report | |
| **behind** | destination | past its `monitor.behind` (a count of waiting messages, the oldest one's age, or both), but still sending | the receiver is slower than the feed, or a rate limit: usually wait; call the vendor if it doesn't catch up |
| **error** | destination | it has quarantined messages | look at them: [resend, correct or dismiss](resend.md) |
| **stalled** | destination | messages waiting over a minute and nothing finished for a minute | the urgent one: the receiver is down, or one message keeps failing. [Hotspots](#where-a-backlog-sits-hotspots) says which |
| **paused** | destination | paused on purpose, with who and why | resume it when the receiver is back |
| **silent** | interface | nothing received within `monitor.active_within` | the sender, or the network between |
| **missed** | interface | nothing received by `monitor.active_by` | the nightly file or batch didn't come |
| **stopped** | interface | its code ran away or ran out of memory three times in a row: it receives, but sends nothing until the engine restarts | the quarantined messages and the log name the message; fix the code |

A destination shows the worst state that applies, worded with the numbers ("behind 1,240
(oldest 12 min), catching up ~6 min"), and once a few weeks of history are in, why: a burst of
arrivals, or a receiver slower than usual for this hour of the week
([nuance](../nuances.md#burst-and-slow-receiver-need-a-few-weeks-of-history-level-3)).
**Nothing is expected until you say so**: without `monitor:`, a quiet interface is *ok*, and a
destination is *behind* only past a limit you set ([nuance](../nuances.md#nothing-is-expected-until-you-say-so-level-2)).

**Set `behind` with both a count and an age** on every feed someone would phone about: a count
alone misses an overnight stall when little arrives, an age alone misses a daytime pile-up that
still moves. Set `active_within` on steady feeds and `active_by` on scheduled ones.

## Where a backlog sits: hotspots

A count says *that* a queue is stuck; hotspots says *which* message or patient. Messages with an
ordering key wait in lanes (the key's hash), each read by one worker, so a backlog always sits
behind one message per worker.

```text
$ ductus hotspots adt-in --lanes 3
adt-in → lab: 30 waiting in 8 of 8 lanes; blocked by message 381 (key 102001, lane 1, 7 attempts, waiting 1 min): Connection refused (os error 111); all 29 behind it wait (one worker: order)
  lane   worker    waiting  first msg    key                     age attempts  last error
  1      0               6  381          102001                1 min        7  Connection refused (os error 111)
  6      0               6  388          102008                1 min        0
  0      0               3  382          102002                1 min        0
  (5 more lanes waiting: --lanes N lists more)
```

How to read the first line ([nuance](../nuances.md#finding-the-message-or-patient-that-holds-a-queue-up-level-3)):

- **"blocked by message M" while others were sent since it arrived**: the receiver is up and
  takes other messages; this one is the trouble (its content, or the receiver's data for that
  patient). Look at the message and its last error; set `retry.max_attempts` on that destination
  if such messages should be quarantined instead of holding their lane.
- **"every worker is retrying its first message"**, or blocked with a connection error as above:
  the receiver, or something the code calls, is down. Nobody's message is to blame; call the
  vendor, and `ductus pause` stops the retries meanwhile.
- **"lane L has 73% of the backlog"** or one worker with most of it: one patient (or a few)
  arriving faster than one worker sends. Order keeps a key on one worker, so more workers don't
  help that key. A hot key that clears by itself is a burst; one that's hot every day is a feed
  to discuss with the sender.
- **"a lane no worker reads"**: `ordering.partitions` was lowered while messages waited
  ([nuance](../nuances.md#change-orderingpartitions-only-with-an-empty-queue-level-4)).

After a receiver comes back, the first message is tried at its next retry: the wait between tries
doubles up to `retry.max_backoff_ms` (60 s by default), so a destination can show *stalled* for up
to a minute after the receiver is back, then drains in order
([nuance](../nuances.md#a-receiver-thats-back-can-stay-stalled-for-up-to-a-minute-level-2)).

## Prometheus

`GET /api/v1/metrics` is in Prometheus's format, for Prometheus, Grafana Agent, Datadog or
anything that scrapes. It needs a token, a viewer one:

```yaml
# prometheus.yml, in part
scrape_configs:
  - job_name: ductus
    metrics_path: /api/v1/metrics
    scheme: https                      # with api.tls; http on the engine's own server
    authorization: { credentials_file: /etc/prometheus/ductus-token }
    static_configs:
      - targets: ["ductus1.hospital.org:7700"]
```

(`ductus token create prometheus --role viewer` makes the token. List Prometheus's address in
`api.allow`.) What one destination's lines look like, from a real engine:

```text
ductus_messages_received_total{interface="adt-in"} 411
ductus_deliveries_total{interface="adt-in",destination="lab",outcome="sent"} 380
ductus_deliveries_total{interface="adt-in",destination="lab",outcome="quarantined"} 0
ductus_delivery_retries_total{interface="adt-in",destination="lab"} 7
ductus_delivery_latency_seconds_bucket{interface="adt-in",destination="lab",le="0.01"} 165
…
```

Every metric, with its labels: [Prometheus metrics](../reference/metrics.md).

### What to graph

| Question | Query |
|---|---|
| Messages in, per second | `rate(ductus_messages_received_total[5m])` |
| Sent, filtered, quarantined, per second | `rate(ductus_deliveries_total[5m])` by `outcome` |
| How long from receipt to sent (95% faster than) | `histogram_quantile(0.95, rate(ductus_delivery_latency_seconds_bucket[5m]))` |
| The receiver's time per send | `histogram_quantile(0.95, rate(ductus_send_seconds_bucket[5m]))` |
| Your code's time per message | `histogram_quantile(0.95, rate(ductus_transform_seconds_bucket[5m]))` |
| What's waiting, and for how long | `ductus_queue_pending`, `ductus_queue_oldest_seconds` |
| Retries (a receiver flapping) | `rate(ductus_delivery_retries_total[5m])` |
| Database and API calls from code | `ductus_resource_call_seconds`, `ductus_resource_call_errors_total` |

- **Counters start at zero when the engine starts**; `rate()` and `increase()` handle it. Graph
  rates, not totals ([nuance](../nuances.md#counts-and-timings-start-at-zero-when-the-engine-starts-level-3)).
  Queue sizes come from the database at each scrape and are always current.
- **Timings are histogram buckets**: a p95 is good to about a bucket's width
  ([nuance](../nuances.md#timings-are-estimates-to-a-bucket-averages-are-exact-level-4)).
- **About 65 series per destination**: on a hosted service that bills per series, drop what you
  don't graph with `metric_relabel_configs` ([nuance](../nuances.md#prometheus-series-grow-with-destinations-level-4)).

### Alerting from Prometheus

The engine alerts by itself ([alerts](alerts.md)), and that's the simpler place. If your
operations team alerts from Prometheus instead (one pager for everything), use the states the
engine has already judged rather than re-deriving them:

```yaml
# ductus-rules.yml, in part (a Prometheus rule file)
groups:
  - name: ductus
    rules:
      - alert: DuctusDestinationStalled
        expr: ductus_destination_state == 3
        for: 2m
      - alert: DuctusDown
        expr: up{job="ductus"} == 0
        for: 2m
```

`ductus_destination_state` is 0 ok, 1 behind, 2 error, 3 stalled, 4 paused;
`ductus_interface_state` is 0 ok, 1 silent, 2 missed, 3 stopped. **Watch the engine itself
too**: an engine that's down reports nothing, so `up == 0` (or the engine's
[heartbeat](alerts.md#an-engine-thats-down-tells-nobody-the-heartbeat)) is the alert that covers
it. And `ductus_alert_send_failures_total` going up means the engine's own alerts aren't
getting through.

## In the API

- `GET /api/v1/interfaces`: every interface and destination with its health, counts since the
  start, and timings (what the console's lists show).
- `GET /api/v1/engine`: the engine itself: version, start, database, other engines on the
  database, and each database's and API's call times (`resources`), to see which lookup is slow.
- `GET /api/v1/events`: the same changes as they happen (what keeps the console live).

[REST API reference](../reference/api/index.md).
