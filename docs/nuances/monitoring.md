# Nuances: monitoring

What could surprise you here, why it is so, and what to do. Each entry has a level, 1 (day one) to 5 (expert): the [index](index.md) lists them all by level.

## Counts and timings start at zero when the engine starts (level 3)
- **What:** `GET /api/v1/metrics` (Prometheus) and `since_start` in `GET /api/v1/interfaces` count messages received, sent, filtered, quarantined and retried, and time transforms, sends, database/API calls and the whole journey from receipt to sent. They're kept in memory and start again at zero with the engine. Queue sizes (`pending`, `quarantined`) come from the database and are always current.
- **Why:** counting in memory costs nothing per message; writing every count to the database would cost a write per message. Prometheus expects counters to reset and its `rate()` handles it.
- **What to do:** graph rates (`rate(ductus_deliveries_total[5m])`), not raw totals. For "how many today", use the message browser or the database, which keep everything for the retention window.

## Timings are estimates to a bucket, averages are exact (level 4)
- **What:** p50/p95 (half / 95% of the messages were faster than this) are estimated from histogram buckets (1, 2.5 and 5 per decade from 0.5 ms to 10 s, then 1 min, 5 min, 1 h), so they're good to about a bucket's width: a p50 of "3.5 ms" means somewhere between 2.5 and 5 ms. `mean_ms` is exact (the total time divided by the count).
- **What to do:** use p95 to spot slow outliers and changes over time, the mean to compare against a measurement.

## Prometheus series grow with destinations (level 4)
- **What:** each destination has 3 histograms (receipt to sent, send, transform) of 20 series each (18 buckets, a sum and a count), plus its counts and queue sizes: about 65 series per destination. 1,000 destinations ≈ 65,000 series, comfortable for Prometheus but a cost on hosted services that bill per series.
- **What to do:** on a hosted service, drop what you don't graph with `metric_relabel_configs` (for example keep `ductus_delivery_latency_seconds` and drop `ductus_transform_seconds`).

## In respond mode, calls run one after another unless you await them together (level 2)
- **What:** each `await` waits for its call before the next starts. Measured in the v0.4 demo (a patient lookup API: 2 database queries and 1 EMPI call): 2.1–3.0 ms in the engine on average, of which ~1.5–2.1 ms is the three calls one after another (~0.5–0.7 ms each on a local database; a remote one takes longer) and ~0.6 ms the engine itself.
- **What to do:** when calls don't depend on each other, start them together: `const [patient, empi] = await Promise.all([db("patient_db").one(...), fetch(...)])`. The response then takes about as long as the slowest call instead of the sum. `GET /api/v1/engine` (`resources`) shows each database's and API's call times, to see which one to work on.

## Nothing is expected until you say so (level 2)
- **What:** `monitor:` in interface.yaml says what operations expects: `active_within: 30m` (with optional `between: "06:00-22:00"`), `active_by: "Mon-Fri 09:00"` (with optional `from:`), `behind: 500`, `behind: 10m` or `behind: { messages: 500, age: 10m }` (a destination can set its own `behind`). Without it, an interface is *ok* however quiet it is, and a destination is *behind* only past a limit you set. Times are the server's local time.
- **Why:** how quiet is too quiet, and how far behind is too far, depends on the feed: a lab feed that sleeps at night, a nightly file, a 1M/day ADT feed. A default would alert on the wrong things.
- **What to do:** set `behind` with both a count and an age on every feed someone would call about (count alone misses an overnight stall when little arrives; age alone misses a daytime pile-up that still moves). Set `active_within` on steady feeds and `active_by` on scheduled ones. The engine alerts on these states itself once `alerts:` is in ductus.yaml (see Alerts below); Prometheus can too (`ductus_destination_state > 0`, `ductus_interface_state > 0`).

## What each state means, and when it shows (level 2)
- **What:** a destination is *ok*, *behind* (past its `behind` limit), *error* (has quarantined messages), *stalled* (messages waiting over a minute and nothing finished for a minute: retrying, a blocked lane, or a stopped interface; `ductus hotspots` shows which message) or *paused*, from best to worst (it shows the worst that applies; `summary` lists them all); says it in words ("behind 1,240 (oldest 12 min), catching up ~6 min"). An interface is *ok*, *silent*, *missed* (its `active_by` deadline) or *stopped*.
- **Why:** "it's backing up but still sending" and "it's not sending at all" need different calls: the first is the vendor's capacity, the second is down.
- **What to do:** treat *stalled* as the urgent one. *error* stays until the quarantined messages are dealt with, so clear them (or they hide the next real error). A backlog under a minute old isn't diagnosed at all: a flowing queue always has a few messages in it.

## Burst and slow receiver need a few weeks of history (level 3)
- **What:** "normal" is learned per hour of the week (Monday 07:00 compares with past Mondays at 07:00), from each hour the engine watched for at least 15 minutes; recent weeks weigh more. Until an hour has been watched once, a backlog shows its trend (catching up, or not) but no cause; for send times, the other hours' average stands in. The last few minutes are compared with it: a burst is over twice the normal arrivals, a slow receiver over twice the normal send time (and at least 5 ms more). The rates and the catch-up time cover the last 5 minutes and start over when the engine does; "normal" is kept in the database.
- **What to do:** expect causes after the first week, and good ones after three or four. A feed whose traffic changed for good (a new hospital on it) becomes the new normal within a few weeks.

## Finding the message or patient that holds a queue up (level 3)
- **What:** `ductus hotspots [interface]` (`GET /api/v1/hotspots`) shows, for every destination with messages waiting, the lanes they wait in, the first message of each (id, key, age, attempts, last error) and how the backlog splits over the workers, and says what it sees:
  - *worker N blocked by message M*: that message keeps failing and everything on its worker waits behind it. "Other messages were sent since it arrived" means the receiver is up and takes other messages: the trouble is this message (its content, or the receiver's data for that patient).
  - *every worker is retrying its first message*: the receiver, or something the code calls, looks down. No single message is to blame.
  - *lane L has 73% of the backlog (key K at its head)* / *worker N has 80% of the backlog*: one patient (or a few) arrives faster than one worker can send. Ordering keeps a key on one worker, so more workers don't help that key.
  - *a lane no worker reads*: `ordering.partitions` was lowered while messages waited (see [Change `ordering.partitions` only with an empty queue](ordering.md#change-orderingpartitions-only-with-an-empty-queue-level-4)).
- **Why:** a backlog in an ordered queue sits behind one message per worker. The count alone says *that* it's stuck, not *which* patient to look at.
- **What to do:** for a blocked message, check its last error and the message (`GET /api/v1/messages/{id}`); fix the data or the receiver, or set `retry.max_attempts` so it's quarantined and the others go on (resending a single message comes with step 7). For a receiver that's down, call the vendor; `ductus pause` stops the retries meanwhile. A hot key under a few hundred messages clears by itself; one that stays hot every day is a feed to discuss with the sender (a batch load sent as one patient's updates?). Lanes with fewer than 100 waiting are never called hot: a flowing queue always has some.

## A receiver that's back can stay stalled for up to a minute (level 2)
- **What:** while a receiver is down, the wait between tries of the first waiting message doubles (1 s, 2 s, 4 s…) up to `retry.max_backoff_ms`, 60 s by default. When the receiver comes back, nothing is sent until that message's next try, so the destination can show *stalled* (and its alert stay open) for up to a minute after the receiver is up; then the backlog drains in order. Seen in a probe: the receiver back 8 s, still stalled, attempt 7 waiting its 60 s.
- **Why:** trying a receiver that's down every second for an hour helps nobody and fills its logs; doubling finds it again within one wait.
- **What to do:** nothing, usually. Where a receiver's comebacks must be noticed sooner, lower `max_backoff_ms` on that destination (`retry: { max_backoff_ms: 10000 }`).

## There's no search by message content yet (level 1)
- **What:** the console and the API list an interface's messages by status and time, a page at a time, and open one by its id. Finding "every message for MRN 123456 last Tuesday" isn't there: a search by patient and date is planned with long-term storage (v1.3).
- **What to do:** each message keeps its ordering key's value (the MRN, for `ordering.key: PID-3.1`) and MSH-10 in their own columns, so a read-only database login can find ids with `WHERE interface = … AND received_at` in a range `AND ordering_key = …` (the time range keeps Postgres to that day's partitions). Such a query bypasses the payload-read audit: give that login to the people your privacy officer would let read every message, and select ids, not payloads ([the message store](../operate/messages.md#searching-by-content-not-yet)).
