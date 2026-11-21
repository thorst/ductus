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
  results (`ORC-2`). A message whose field is empty goes to the empty value's lane, shared by
  every keyless message; when most messages read empty (a path that doesn't match the sender's
  messages), the interface's health says **keyless** and names the path, and `ductus validate`
  warns when the key is empty in every fixture
  ([nuance](../nuances/ordering.md#an-empty-ordering-key-is-a-lane-of-its-own-level-2)).
- **More workers help only with a key**: with one lane, one worker does everything
  ([nuance](../nuances/ordering.md#without-an-ordering-key-more-workers-dont-help-level-3)).
- **Change `partitions` or `key` when you need to**, with messages waiting or not: new messages
  go to new lanes at once, the waiting ones are sent first, and a patient's new messages wait
  behind their older ones, so nothing is refused, sent twice or reordered. `ductus validate`
  against the database says what the next start will do
  ([nuance](../nuances/ordering.md#after-a-change-of-ordering-new-messages-of-a-key-wait-for-its-older-ones-level-4)).

### Key paths

A key (`ordering.key`, each of `dedupe.keys`) is a path in the interface's `format`, read by the
engine as each message arrives, before any code runs. Real feeds rarely put the value where a
plain path looks, so a path can pick a repetition by what it says:

| format | plain | picking by what it says |
|---|---|---|
| hl7 | `PID-3.1`, `PID-3[2].1`, `PID-3.4.1` | `PID-3[.5=MR].1`: the identifier whose type (component 5) is `MR`; `PID-3[.5=MR\|.4=HOSP].1`: that, else the one from `HOSP` |
| json | `patient.mrn`, `ids[0].value` | `identifier[system=urn:oid:1.2.3].value`: the identifier of that system (`[type.coding[0].code=MR]` works too) |
| xml | `Patient/MRN`, `Item[2]/@code` | `id[@root=2.16.840.1.113883.19]/@extension`, `Item[Code=GLU]/Qty`: by an attribute or a child's text |
| csv, fixed | a column's or field's name | |
| any, and text | | `{ regex: 'MRN=(\d+)' }`: the first group of a regular expression over the message's text |

```yaml
# interfaces/adt-from-two-hospitals/interface.yaml, in part
ordering:
  key: [MSH-4, "PID-3[.5=MR].1"]   # one patient of one facility: MRNs repeat across facilities
```

- **Why filters**: an ADT feed's PID-3 often carries an SSN, a medical record number and an
  account number in any order, and a FHIR Patient lists identifiers by system. `PID-3.1` is
  whichever came first; a filter names the one you mean, without code.
- **Alternatives** (`|`) are tried in order: the first repetition the first filter matches, else
  the first the second matches. A filter that matches nothing reads empty, like a missing field.
- **A filter compares exactly**, the decoded value (HL7 escapes like `\T\` turned back into `&`).
  Quote a value with `|`, `]` or quotes in it: `PID-3[.4='A|B'].1`.
- **Several parts together** (`key: [MSH-4, PID-3.1]`) are one key, its values joined by `/`
  (`EAST/123456`). It **reads empty when any part does**: a message without an MRN has no patient
  to keep in order, and the interface's *keyless* health then says so
  ([nuance](../nuances/ordering.md#a-key-with-several-parts-is-empty-when-any-part-is-a-filter-that-matches-nothing-reads-empty-level-2)).
- **XML names match by their local name**, whatever the namespace prefix (`Patient` matches
  `<hl7:Patient>` and `<v3:Patient>`), since a sender's prefixes can change. Set
  `format: { xml: { names: qualified } }` to match prefixes exactly
  ([reference](../reference/config/interface.md#formatxml)).
- **A regex** is for what has no fields (`format: text`, a vendor's layout) or no path that
  reaches it. It reads the whole message as text (an HL7 message's segments end with a carriage
  return), uses Rust's syntax (no look-around; linear time, so no message can make it slow) and
  takes the first group, or the whole match without one. Prefer a path when one exists: it
  reads the field, not text that may also appear elsewhere.
- **The code reads the same paths**: `msg.get("PID-3[.5=MR].1")`, `doc.get("id[@root=…]/@extension")`,
  and `getPath(value, path)` for any message, JSON included
  ([formats](../write/formats.md#reading-by-the-keys-paths)). A test that reads the key with them
  tests the key the engine orders by.
- **A malformed path is refused** by `ductus validate` and at the start, at its place in
  interface.yaml (`ordering.key.1` for a list's second part), with what's wrong and an example.

### A key function

When no path says it, the interface's own code does: a key part can be `{ script: key.ts }`, the
function `key` that file exports, or `{ script: keys.ts, function: trace }` for another name. It
gets the message decoded as a transform gets it and returns the key as text (a number is its
digits; `""`, `null` or nothing is an empty key). It works for `ordering.key` and `dedupe.keys`,
alone or as one part of a list, in every format.

```yaml
# interfaces/remits-in/interface.yaml, in part
dedupe:                             # a payer's resent file: its payment's trace number again
  keys: [{ script: keys.ts, function: trace }]
  window: 90d
```

```ts
// interfaces/remits-in/keys.ts
import { parseX12 } from "../../lib/x12.ts";

/**
 * The payment's trace number (TRN02): a payer that sends the same 835 again sends the same trace
 * number, so it's the file's dedupe key. Read with the separators the file declares in its ISA
 * segment, which a regex can't know. A file that isn't X12 is quarantined, with why.
 */
export function trace(text: string): string {
  return parseX12(text).find((seg) => seg.id === "TRN")?.e(2) ?? "";
}
```

- **When to use one**: a key that needs a decision (the MRN, else the account number with the
  facility before it), a value to normalize (case, leading zeros), or a format whose delimiters
  the message declares (X12). **Prefer a path** whenever one reads the value: it's checked
  against the fixtures by `validate` and needs no code.
- **It runs while the sender waits for its ACK**, on one thread the engine keeps for every
  interface's key functions (made when the first is loaded; an engine without any has none). So it
  must be **synchronous and quick**: no `async`, `await`, `db()`, `api()` or `fetch()`.
  `ductus validate` refuses those in its body (DUC208), and at the engine none of them is
  reachable. Look things up in a destination's code instead.
- **What it costs** (measured, release build): about 4 µs a message for a key that reads one HL7
  field (`msg.get("PID-3[.5=MR].1")`), plus about 60 µs when a message crosses to the key thread
  alone (an MLLP message; a file's messages cross together); a path takes under half a
  microsecond. Against a durable commit's ~3 ms that's nothing at a feed's pace: at 1,000 messages
  a second the senders' longest wait for an ACK was the same either way. At full speed it shows:
  one engine took 4,600–6,200 messages a second over 4 MLLP connections keyed by a function,
  6,900 by a path (`scripts/bench-keys.sh`).
- **A key function that throws** (or calls `quarantine("why")`) gets its message **stored
  quarantined, with why**, at every destination: `ordering key { script: key.ts } failed: Error: no
  MRN (interfaces/adt/key.ts:6:25)`. The sender gets its ACK (it was received; someone must look,
  not the sender), and the next message is keyed as usual. One that runs longer than **50 ms** is
  stopped and its message quarantined the same way; meanwhile other interfaces' key functions wait
  ([nuance](../nuances/ordering.md#a-key-function-runs-while-the-sender-waits-and-its-failure-quarantines-the-message-level-3)).
- **Test it like a transform**: import it in a `*.test.ts` and call it on a fixture
  ([testing](../write/testing.md)). `validate`'s check of keys against fixtures (DUC207) can't
  read a key that has a function in it.
- **A deploy of its code** loads it again, as a transform's code is: the next message is keyed by
  the new code.

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
