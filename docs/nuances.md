# Nuances: what to know before you rely on it

Every engine has behaviour that surprises people the first time. Ductus can't remove all of it, but it can tell you about it up front. This page is the running list: what happens, why, and how to write your interfaces and set expectations with it in mind.

Each entry has a **level** (1 = everyone needs it on day one, 5 = expert). The levels match the planned course tracks, from first interface to expert.

**For engine developers:** whenever a change adds or reveals a nuance, add it here in the same commit.

| Level | Who it's for |
|---|---|
| 1 Foundations | Writing a first interface |
| 2 Practitioner | Building and testing interfaces day to day |
| 3 Professional | Databases, APIs, files, schedules, ordering, retries |
| 4 Advanced | Isolation, performance, operations |
| 5 Expert | Delivery guarantees and failure semantics under crashes and runaways |

---

## Messages and delivery

### A message can be delivered twice (level 3)
- **What:** delivery is *at least once*. If the engine stops after the destination accepted a message but before recording it as sent, the message is sent again on restart. Sent statuses are saved in batches of up to 50, so a crash can re-send up to that many per worker.
- **Why:** the alternative (at most once) loses messages. Nothing that was ACKed is ever lost.
- **What to do:** make receivers tolerate repeats (most HL7 receivers dedupe on MSH-10), or use `dedupe` on the receiving interface.

### Your transform can run more than once for the same message (level 3)
- **What:** after a crash, a temporary failure (database down, `fetch()` getting a 503) or an interruption (see "Runaway scripts" below), the whole transform runs again from the start. Any `db().execute()` or `fetch()` POST it made the first time happens again.
- **What to do:** make writes idempotent: `INSERT ... ON CONFLICT DO NOTHING`, upserts keyed on the message's control ID, or an idempotency key header on API calls.

### Order is per key, and one stuck message holds up its key (level 3)
- **What:** messages with the same ordering key (e.g. MRN) are processed strictly in arrival order. A message that keeps failing temporarily blocks the other messages in its lane until it succeeds or is quarantined. With no `ordering.key`, the whole interface is one lane.
- **Why:** that's the price of ordering: patient B's update must not overtake patient A's.
- **What to do:** choose a key that's as narrow as correctness allows. Set `retry.max_attempts` on destinations where waiting forever is worse than quarantining.

### Temporary failures retry forever by default (level 3)
- **What:** a destination that's down, or a transform that hits a temporary error, is retried with back-off (1 s up to 60 s) until it works. `retry.max_attempts` limits delivery attempts; transform retries (database or API down) have no limit.
- **What to do:** monitor queue depth ("behind-by" comes in v0.5) rather than expecting failures to show up as quarantined messages.

### Database login failures make messages wait, not fail (level 3)
- **What:** a wrong password or a missing database counts as *temporary*: messages wait and retry instead of being quarantined.
- **Why:** it's a config problem; quarantining thousands of messages would make the fix much harder.

### `durability: relaxed` can lose the last fraction of a second (level 4)
- **What:** ACKs don't wait for the disk flush. If the *database server* crashes or loses power, the last moments of ACKed messages can be lost. An engine crash alone never loses anything.

### Respond-mode HTTP requests aren't stored (level 3)
- **What:** in `mode: respond`, the transform's result is the HTTP response; there's no queue and no stored message. If something fails, the caller gets an error status and decides whether to retry.

## Writing transforms

### Missing fields read as "" and never throw (level 1)
- **What:** `msg.PID[30][1]` on a message without it is `""`. This avoids null checks everywhere, but a typo in a position also quietly gives `""`.
- **What to do:** test with real sample messages (`ductus test`) and assert on the fields you map.

### `return null` filters; throwing quarantines (level 1)
- **What:** `null` → filtered (kept for audit, never delivered); `kill("reason")` → filtered with a reason; a thrown error or `quarantine("reason")` → quarantined for a person to look at.

### Imports used only for their side effects are removed (level 2)
- **What:** TypeScript drops `import { x } from "./m.ts"` if `x` is never used, so code that runs when `m.ts` loads won't run.
- **What to do:** write `import "./m.ts";` when you need a module only for what it does on load.

## Shared pool (see also [shared-pool.md](shared-pool.md))

### Interfaces share a global scope (level 2)
- **What:** in the shared pool many interfaces run in one JavaScript engine. Writing to `globalThis` or changing built-in prototypes affects them all. `ductus validate` warns (DX1, DX2).

### A shared library file is one copy per thread, not one per project (level 4)
- **What:** `lib/x.ts` imported by several interfaces is loaded once per isolate. Interfaces on the same pool thread share its top-level state (DX3); interfaces on different threads each see their own copy. A dedicated interface with `workers: 4` has four copies of its own module state too.
- **What to do:** never use module-level variables for data that must be consistent across the engine (counters, "last seen" values). Use the database.

### Module-level caches live until the engine restarts (level 2)
- **What:** a module-level `Map` keeps its contents across messages, and loses them on restart. It also counts against the isolate's heap limit.
- **What to do:** keep caches bounded, and make sure the code works with an empty cache.

## Runaway scripts

### `timeout` counts time without pausing, not total time (level 2)
- **What:** each run of your code may last `timeout` (default 5 s) without giving up the thread. Waiting on `db()` or `fetch()` doesn't count: a transform waiting 20 s on a slow database is fine; a loop running 6 s is not.
- **Why:** waiting doesn't hold up anyone; a loop holds up every interface on the thread.

### A runaway message is quarantined; three in a row stop the interface (level 2)
- **What:** code past its `timeout` is stopped and its message quarantined ("transform stopped: ..."), and the interface carries on: it might have been one bad message. Three timeouts in a row, or one heap overflow, **stop the interface**: new messages are still received and ACKed, and wait as pending. A stopped interface needs an engine restart once the code is fixed (a restart button comes with the admin API).
- **What to do:** guard loops that depend on message content (segment counts, repetition indexes) so they always end.

### Neighbours wait while runaway code runs (level 4)
- **What:** until the engine stops it, runaway code holds its thread for up to its `timeout`. Every other interface on that thread waits that long.
- **What to do:** keep `timeout` close to what the code really needs (`timeout: 500ms` is plenty for most HL7 transforms). Put interfaces with heavy or untrusted code in `isolation: dedicated`.

### The heap limit is shared by everything in an isolate (level 4)
- **What:** `isolates.heap_mb` (in `ductus.yaml`, default 512) applies to the whole isolate. In the shared pool, a heap overflow is blamed on whichever interface was running when memory ran out: usually the one allocating, but not necessarily the one that has been filling the heap slowly (e.g. an unbounded cache).
- **What to do:** bound caches; give memory-hungry interfaces their own isolate.

### A runaway after `await` interrupts every async transform on its thread (level 5)
- **What:** code that runs away *after an `await`* is stopped inside the JavaScript event loop. V8 then drops every queued continuation on that isolate, not just the runaway's. The engine fails every async transform in flight on that thread: the culprit is quarantined, the rest are **retried**. A retried transform may already have made some of its database or API calls, and an abandoned original may still finish a call that was in flight.
- **What to do:** this is one more reason to make writes idempotent (see above).

### A runaway in a shared library after `await` can't be blamed on anyone (level 5)
- **What:** to tell whose code ran away, the engine looks for an interface's files on the JavaScript call stack. Code in a shared library file, resumed after an `await`, has none, so nobody can be quarantined or stopped: every transform in flight on the thread is retried, and the problem can come back.
- **What to do:** keep loops over message content in the interface's own files, or call the library from the interface without awaiting in between.

## Databases and APIs

### SQL Server: one result set, one batch per call (level 3)
- **What:** only the first result set is returned. Each `query`/`execute` is its own batch, so a transaction must begin and end within one call. Text parameters are sent as `nvarchar`: comparing them with a `varchar` column can skip its index (cast the parameter).

### `fetch()` retries 5xx, 408 and 429 by default (level 3)
- **What:** those statuses become temporary errors (the message waits and retries). Pass `retry: false` to handle them yourself. Bodies are text only, up to 16 MB.

### Dedicated connection pools only for dedicated interfaces (level 4)
- **What:** `dedicated_pools` needs `isolation: dedicated`; shared-pool interfaces use the shared pools from `resources.yaml`.

## Files, SFTP and schedules

### A file is picked up one poll after it stops changing (level 3)
- **What:** a file is ready when its size and modification time haven't changed since the previous poll (or its `marker` file exists). Expect a delay of up to two poll intervals. Dot-files are skipped.

### One file at a time, oldest first; a failure stops the rest (level 3)
- **What:** files keep their order: if one fails, later files wait. Unsplit files over 256 MB are left in place with an error.

### Cron uses the server's local time; missed runs collapse to one (level 3)
- **What:** after downtime, a schedule runs once, not once per missed time. A run is skipped if the previous run's message is still pending.
