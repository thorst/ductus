# Delivery: order, retries, limits

How an interface's messages are delivered: in what order, how failures are tried again, what
counts as a repeat, how fast a receiver may be sent to, when a sender gets its ACK, and where the
code runs. The defaults are safe: strict order, retried until delivered, acknowledged only once
on disk, in the shared pool.

## Order: `ordering`

[`ordering`](../reference/config/interface.md#ordering) says which messages must be delivered in
the order they arrived: those with the same `key`.

```yaml
# interfaces/adt-to-lab/interface.yaml, in part
ordering:
  key: PID-3.1        # one patient's messages in order; different patients may overtake
```

- **Without a key** (the default), the whole interface is **one lane**: delivered strictly in
  arrival order, one message at a time per destination. Correct for anything, and the right
  start for a feed whose receiver cares about order across patients.
- **With a key**, messages with the same value (one patient's MRN) stay in order with each
  other, and different values move independently, in 64 lanes by default
  ([`partitions`](../reference/config/interface.md#ordering)). One patient's failing message then
  holds up only its own lane, not everyone
  ([nuance](../nuances/ordering.md#order-is-per-key-and-one-stuck-message-holds-up-its-key-level-3)).
- **Choose the narrowest key correctness allows**: by patient for ADT (`PID-3.1`), by order for
  results (`ORC-2`). A message whose field is empty goes to the empty value's lane.
- **More workers help only with a key**: with one lane, one worker does everything
  ([nuance](../nuances/ordering.md#without-an-ordering-key-more-workers-dont-help-level-3)).
- **Set `partitions` before the interface carries traffic.** Lowering it with messages waiting is
  refused at start (they'd be in lanes no worker reads); raising it can reorder a patient's
  waiting and new messages until the backlog drains. `ductus drain --wait` first
  ([nuance](../nuances/ordering.md#change-orderingpartitions-only-with-an-empty-queue-level-4)).

## Failures: `retry`

[`retry`](../reference/config/interface.md#retry) is how a delivery that failed for a reason that
may pass (a receiver down, a timeout, an AE, a database the code needs unreachable) is tried
again: waits doubling from `initial_backoff_ms` (1 s) to `max_backoff_ms` (60 s), **forever by
default**.

```yaml
# interfaces/orders-to-lab/interface.yaml, in part
retry:
  max_attempts: 10              # then quarantine: "gave up after 10 attempts: …"
  max_backoff_ms: 300000        # at most 5 minutes between tries
```

- **Forever is the default** because a receiver that comes back should get every message it
  missed, in order, with nobody resending by hand
  ([nuance](../nuances/ordering.md#temporary-failures-retry-forever-by-default-level-3)).
- **Set `max_attempts`** where a person looking beats waiting: a receiver that refuses some
  messages for good with errors that look temporary. A message being retried holds up its lane.
- `max_backoff_ms` is how soon after a receiver comes back its messages start moving, at worst.
- A failure that can't pass (an AR, a 4xx, the code throwing) isn't retried: it's quarantined at
  once. The code's own temporary errors (a database down) are always retried.
- A destination can set its own `retry`.

## Repeats: `dedupe`

[`dedupe`](../reference/config/interface.md#dedupe) recognizes a message sent again within a
window (a sender resending after a lost ACK, a file dropped twice) by the values of `keys`, and
doesn't deliver it twice. **Off by default.**

```yaml
# interfaces/adt-in/interface.yaml, in part
dedupe:
  keys: [MSH-10]      # the default: HL7's message control ID
  window: 24h         # the default
```

A repeat is stored with the status `duplicate`, so it can be found, but never queued. **Use it**
when the receiver can't tolerate repeats and the sender is known to resend. Most HL7 receivers
already ignore a repeated MSH-10, so it's often not needed. Choose keys a resend repeats and a
new message never does; cover the longest a sender may take to resend (after a weekend, `7d`).
Dedupe stops repeats from the sender; delivery itself is at least once, so the receiver may still
see one message twice after a crash ([nuance](../nuances/delivery.md#a-message-can-be-delivered-twice-level-3)).

`dedupe` protects the receiver of *this* interface. When your code writes to another system
(`db().execute`, an API `POST`), a "done" marker in a table of yours can't make that write happen
exactly once: the two writes can't share a transaction. Make the write itself idempotent (an
upsert keyed on the message's control id, an idempotency key header)
([nuance](../nuances/delivery.md#a-done-marker-cant-prevent-duplicates-level-5)).

## Speed: `rate_limit`

[`rate_limit`](../reference/config/interface.md#rate_limit) is the most messages a minute each
destination may be sent, for receivers that can't take bursts (a device, a vendor API with a
quota). **No limit by default.**

```yaml
# interfaces/results-to-portal/interface.yaml, in part
rate_limit:
  per_minute: 600       # 10 a second, on average
  burst: 20             # at most 20 at once after a quiet spell
  over_limit: wait      # the default: held, in order
```

`mode: hard` counts per calendar minute instead, for quotas counted that way. `over_limit: drop`
filters what's over the limit, with the reason: only for what is worthless late (frequent status
updates where the next replaces the last). Each destination has its own allowance, and can set
its own limit.

## When senders get their ACK: `durability`

[`durability`](../reference/config/interface.md#durability) decides whether a sender's ACK (or
HTTP 202) waits for the message to be on the database's disk.

- **`strict`** (the default): the ACK waits for the flush. Once a sender has it, the message
  survives a crash or power loss of any machine.
- **`relaxed`**: the ACK goes out as soon as the database has the message, before the flush. One
  sender connection can then go much faster than one message per disk flush (about 400 a second
  at 2.4 ms a flush). The cost: if the **database server** crashes or loses power, the last
  fraction of a second of acknowledged messages is lost, and senders won't resend them. An engine
  crash loses nothing in either mode
  ([nuance](../nuances/ordering.md#durability-relaxed-can-lose-the-last-fraction-of-a-second-level-4)).

**Use `relaxed` only** where one connection's peak is more than the disk allows and a lost message
can be recovered another way (a resend from the source, a nightly reconciliation). Try a faster
disk for Postgres's log, or more sender connections (they share flushes), first.

## Dedicated threads and connections

By default an interface's code runs in the **shared pool**: one thread per CPU core, each running
many interfaces, with one JavaScript engine (an isolate) per thread. Cheap, and right for most.
[`isolation`](../reference/config/interface.md#isolation) `dedicated` gives an interface threads
of its own instead.

```yaml
# interfaces/results-bulk/interface.yaml, in part
ordering: { key: PID-3.1 }
isolation: dedicated
workers: 8
dedicated_pools: { patient_db: 4 }
```

- **[`workers`](../reference/config/interface.md#workers)** (default 4, dedicated only): its
  threads, each with its own isolate, sharing the lanes. More help only with an ordering key,
  up to about the number of cores.
- **[`dedicated_pools`](../reference/config/interface.md#dedicated_pools)** (dedicated only):
  connections of its own to resources.yaml databases, so a busy interface can't take every
  connection of the shared pool from the others, nor theirs from it
  ([nuance](../nuances/databases-and-apis.md#dedicated-connection-pools-only-for-dedicated-interfaces-level-4)).
- **When:** a busy interface that needs more than one worker (with a key, so there are lanes to
  share), code that computes for seconds on purpose, or code you don't trust yet: its load and
  its runaways then touch no other interface
  ([shared pool](../nuances/runaway.md#neighbours-wait-while-runaway-code-runs-level-4)).
- **The cost:** each worker is a thread and up to `isolates.heap_mb` of memory
  ([memory](ductus-yaml.md#memory-for-interface-code)).

Most interfaces never need it: one destination of an interface in the shared pool drained about
1,500 messages a second in the measurements, against an average of 12 a second for a million
messages a day ([measured](../architecture.md#performance-notes-measured-wsl-25-ms-fsync-disk)).
When one does, in this order: an ordering key, then `dedicated`, then more `workers`, then
`dedicated_pools`.
