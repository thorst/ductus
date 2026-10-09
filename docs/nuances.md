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
- **What:** delivery is *at least once*. If the engine stops after the destination accepted a message but before recording it as sent, the message is sent again on restart. Sent statuses are saved in batches of up to 50, so a crash can re-send up to that many per worker and destination.
- **Why:** the alternative (at most once) loses messages. Nothing that was ACKed is ever lost.
- **What to do:** make receivers tolerate repeats (most HL7 receivers dedupe on MSH-10), or use `dedupe` on the receiving interface.

### Your transform can run more than once for the same message (level 3)
- **What:** after a crash, a temporary failure (database down, `fetch()` getting a 503) or an interruption (see "Runaway scripts" below), the whole transform runs again from the start. Any `db().execute()` or `fetch()` POST it made the first time happens again.
- **What to do:** make writes idempotent: `INSERT ... ON CONFLICT DO NOTHING`, upserts keyed on the message's control ID, or an idempotency key header on API calls.

### Only the original is stored; outputs are recomputed (level 3)
- **What:** the engine stores each message exactly as received and never changes it. What was sent isn't stored by default; it's reproduced by running the same code on the original. That's exact only if the transform depends on nothing but the message and the code. Lookups (`db()`, `fetch()`), the current time and environment values can give a different result when re-run later.
- **Why:** transforms are fast, and storing every output would cost a database write per message.
- **What to do:** if you must be able to prove exactly what was sent, set `save_output: true` on that destination: what it was sent is kept with its delivery. Keep transforms that don't need lookups free of them.

### A "done" marker can't prevent duplicates (level 5)
- **What:** recording "this insert happened" in the engine's database doesn't make an insert in *another* database happen exactly once: the two writes can't share a transaction, so a crash between them still repeats the insert on re-run.
- **What to do:** put the guarantee where the data lands: a unique key or upsert in the target table, or an idempotency key the receiving API checks. Use the message's stable key `meta.id` (the second argument of `transform`: `transform(msg, meta)`) for it. It's unique within one engine database; add something per environment if several engines write to the same place.

## The store (Postgres)

### A query that holds the message tables for days can pause intake (level 4)
- **What:** each day's tables are created ahead of time, hourly. Creating one needs a brief exclusive lock on `messages`/`deliveries`, which has to wait for every open query on them. Maintenance gives up after 2 s and tries again in an hour (partitions exist 3 days ahead), so a long report never holds up ACKs. If tomorrow's partitions still don't exist (something held the tables through every try for two days), maintenance stops giving up and waits: new messages (and their ACKs) then wait as long as the blocking query runs, rather than fail at midnight. The engine logs a warning when that happens.
- **What to do:** run long reports against a replica, or keep them short; never leave a transaction open on the store. Watch for the warning "a partition needed soon is missing".

## Retention

### Retention is 1, 3 or 7 weeks, not any number of days (level 1)
- **What:** `retention:` in interface.yaml is `1w`, `3w` or `7w` (or the same in days: `7d`, `21d`, `49d`; default: `retention` in ductus.yaml, else 7w). Anything else is refused by `ductus validate`.
- **Why:** performance. Each window is a tier of partitions, and old messages go by dropping whole partitions, which costs nothing however many messages there are. Deleting rows one by one (the usual pruner) slows the busiest tables and needs vacuuming after. Each tier adds partitions that some queries have to plan for, so there are few, short tiers, keeping the count around 24.
- **What to do:** pick the window that covers how far back you need to look into problems. Older history: the source system, or an archive on cheaper disk (an export is planned). The engine isn't a data warehouse.

### Messages are kept at least their window, and gone within a period after (level 2)
- **What:** a partition covers a day (1w) or a week (3w, 7w), and it's dropped once all of it is older than the window. So a 1w message lives 7 to 8 days, a 7w one 49 to 56. Partitions are checked hourly.
- **What to do:** treat the window as "at least". If something must be gone sooner, use a shorter tier.

### Pending messages are never purged; past the window they're held (level 2)
- **What:** when a partition expires, the messages in it that someone still needs (pending for a destination, and quarantined ones) move to the **held area** with all their deliveries, and the partition drops as usual. Held messages stay in their destination's queue, in order, and in the message browser; they're delivered or resolved like any other. A month of the held area is dropped whole once it's over and nothing in it is pending or quarantined (its finished messages go with it). `ductus retention` shows the held area by month and interface, and `/metrics` has `ductus_held_messages` and `ductus_held_bytes` (both counted at the hourly retention run). A shorter retention never deletes pending or quarantined messages either.
- **Why:** a destination that was down for a month must not lose its backlog, and quarantined messages wait for a person. Before the held area, one waiting message kept its whole week for every interface in the tier.
- **What to do:** watch `ductus_held_messages` (alert on quarantined ones growing: each is a question nobody has answered). Moving a backlog writes each message once more, once, in the background: about 5,000 messages a second with two destinations each (300,000 in 58 s on a dev machine, busy half the time), so a day of a 1M-message feed held for one receiver takes 3 to 4 minutes to move, and 90M about 5 hours. Nothing to do unless the database is already at its limit at the top of the hour.

### Quarantined messages are kept until resolved, unless you say `purge` (level 2)
- **What:** `quarantined: keep` (the default) holds quarantined messages past the retention window until someone resolves them. `quarantined: purge` (in interface.yaml, or ductus.yaml for every interface) lets them go with their partition, like the finished ones. There's no second time limit.
- **Why:** in most engines unresolved quarantines pile up for years; keeping them is the safe default, and purging is a decision someone makes on purpose for a feed where a missed message is resent anyway.
- **What to do:** keep the default unless the feed's quarantines are routinely ignored and resent by the sender; then `purge`, and say why in `about:`.

### Shortening retention deletes messages, so it needs a confirmation (level 2)
- **What:** deploying a shorter `retention:` doesn't apply it. The engine logs a warning and keeps the old one, and `ductus retention` shows how many messages the new value would delete. `ductus retention confirm <interface>` (an operator token) applies it: its finished messages past the new window are deleted in the background, the others move into the new tier. Lengthening needs no confirmation and keeps everything not yet dropped.
- **Why:** a typo (7d for 70d, say) shouldn't delete a quarter of the history.
- **What to do:** confirm after deploying, on each engine (it's per database).

### One retention change at a time per interface (level 3)
- **What:** a change moves (or deletes) the interface's older messages in batches, at most half the time busy (the demo's 8,000 messages took 1 to 1.5 s on a dev machine: a day of a 1M-message feed would take a few minutes). A different value deployed meanwhile waits until it's done; values in between are skipped. A change also waits for the interface's pending messages to be delivered before it finishes (they move once delivered).
- **What to do:** watch progress with `ductus retention` (messages left). Don't flip retention back and forth.

### Shortening frees disk when partitions go, not at once (level 4)
- **What:** deleted messages leave space that Postgres reuses only within the same partition, and old partitions get no new messages. The disk space comes back when their partitions are dropped, at most the old window later.
- **What to do:** to free space now, shorten and then, off-hours, `VACUUM FULL` the affected old partitions yourself (it locks each while it runs). A built-in compaction is planned.

## Several destinations

### Each destination runs its own function on the original message (level 2)
- **What:** with `destinations:`, every destination calls its function (`transform` unless it names another with `transform: toBilling`) on the message as received, not on another destination's result. Three destinations using `transform` run it three times, and each run makes its own `db()` and `fetch()` calls.
- **Why:** each destination works through its own queue at its own pace (one can be hours behind), and outputs aren't stored, so nothing can be handed from one to the other.
- **What to do:** put shared preparation in a helper the functions call. Keep lookups in the functions that need them. Module-level state is shared by all of an interface's functions (one module), so a bounded cache can save repeated lookups.

### One destination down holds up only itself (level 2)
- **What:** each destination has its own queue, order and retries. While the lab is down, billing and the archive keep receiving; the lab's messages wait, in order, and go once it's back. A message's overall status stays `pending` until every destination is done with it. Then it's `quarantined` if any destination quarantined it, `sent` if any destination got it, and `filtered` if all of them filtered it. The status per destination says which (`ductus status` lists them).
- **What to do:** watch each destination's queue, not just the interface's.

### Pausing a destination: at once, no questions, and it holds (level 2)
- **What:** `ductus pause <interface> [destination]` (or the API) stops sending to that destination at once: no new sends and no retries. A send already under way finishes. Messages keep arriving and wait, in order. No reason is needed to pause. Add one when there's time by running it again with `--reason "..."` (until then it shows "no reason yet"). The pause is stored, so a restart or a failover keeps it. A paused destination doesn't hold up a drain. `ductus resume <interface> [destination]` sends the backlog in order. Without arguments, `ductus resume` ends a drain.
- **Why:** in an emergency (a receiver breached, a vendor sending garbage back) you stop the damage first and explain after. A destination that's merely down already waits and retries by itself, so it needs no pause.
- **What to do:**
  - Down for minutes to hours: do nothing; it retries.
  - Cut off on purpose, or down for days to weeks: pause it, so it stops retrying and shows as *paused*, not *stalled*. Add the reason.
  - Longer than the interface's retention window: the waiting messages move to the held area as their partitions expire, and the rest of the history goes on schedule, so the disk holds the normal window plus the backlog (not the whole period it spans). Watch the held area's size (`ductus retention`). Agree a release plan with the vendor before they're back: the backlog goes in order, at the destination's rate limit (90M at 200/s is about 5 days).

### Filtering and quarantine are per destination (level 2)
- **What:** `return null`, `kill()` and `quarantine()` in a destination's function apply to that destination only. The message still goes to the others.

### Adding, removing or renaming a destination (level 3)
- **What:** at startup the engine matches stored deliveries to `interface.yaml`. A **new** destination also gets the messages still in flight (pending for any destination), not older ones. A **removed** destination's waiting messages are marked filtered ("destination `x` was removed from interface.yaml") and logged. A **rename** is both: the new name gets everything in flight, including what the old name had waiting, and may re-send some the old name had already sent.
- **What to do:** rename only when the destination is caught up. Removing a destination by mistake can't be undone by putting it back: its waiting messages were already filtered.

### Many destinations on one interface: when to split (level 3)
- **What:** every destination adds a delivery row per message, written in the same statement as the message and updated when it's sent. So a message with 25 destinations costs the database about 25 small writes on the way in and 25 on the way out, and senders wait for the first part before their ACK.
- **Measured** (dev laptop under WSL, strict durability, 16 sender connections, file destinations, steady-state partitions; `scripts/bench-fanout.sh`): 1 and 5 destinations ingest ~2,700–3,400 messages/s; 25 destinations ~1,150–1,300/s; 50 destinations ~500–700/s. Delivery tops out around 15,000–20,000 deliveries/s in total, whatever the fan-out: at 50 destinations, 6,000 messages took ~16 s to reach everyone (one run 27 s). For scale, 1M ADT messages a day averages ~12/s, with peaks of a few hundred.
- **What to do:**
  - Up to ~10 destinations per interface: nothing to plan, at any realistic volume.
  - More than that: multiply your peak messages/s by the number of destinations and compare with the deliveries/s your server measures (~15,000 on a laptop; `ductus bench` will measure it, v0.6). Under a third of it is comfortable: that leaves room for a burst of 3x and for catching up after a receiver's outage. Over that, a burst builds a backlog that clears only as fast as the spare capacity allows.
  - If it doesn't fit: split the feed. Route to a few internal interfaces (`destination: { interface: adt_out_a }`) that each fan out to a share of the receivers, or give low-priority receivers (archives, analytics) their own interface fed from a route. The sender's ACK then waits for a few delivery rows instead of dozens; in exchange, each hop stores one more copy of the message, so total writes grow a little.

### A runaway is blamed on the interface, not the destination (level 5)
- **What:** when code runs away after an `await`, the engine knows which interface's code it was, not which function. Every call of that interface in flight on the thread is quarantined, so another destination's message can be quarantined with it. Each one counts towards stopping the interface.

### Order is per key, and one stuck message holds up its key (level 3)
- **What:** messages with the same ordering key (e.g. MRN) are processed strictly in arrival order. A message that keeps failing temporarily holds up **every lane of its worker** for that destination, not just its own key, until it succeeds or is quarantined: a shared interface has one worker per destination, so one failing message holds up the whole destination; a dedicated one with `workers: 4` holds up about a quarter of the patients. With no `ordering.key`, the whole interface is one lane.
- **Why:** that's the price of ordering: patient B's update must not overtake patient A's. A worker handles its lanes one message at a time, in arrival order; it doesn't skip ahead past a message it's retrying.
- **What to do:** choose a key that's as narrow as correctness allows. Set `retry.max_attempts` on destinations where waiting forever is worse than quarantining. When a destination is stalled, `ductus hotspots <interface>` names the message, its key and its worker (see Monitoring).

### Change `ordering.partitions` only with an empty queue (level 4)
- **What:** each message's lane is computed from its key and `partitions` when it arrives, and stored. Change `partitions` while messages are waiting and two things can happen:
  - **Lowered:** waiting messages in lanes at or above the new count belong to no worker and are **never sent** (and `ductus drain` never finishes) until `partitions` is set back. `ductus hotspots` lists them as "a lane no worker reads".
  - **Raised:** a patient's waiting messages stay in their old lane while new ones go to a different lane (maybe another worker), so until the backlog drains that patient's messages can be delivered out of order.
  - **`workers` alone:** safe (every lane still has one owner).
- **What to do:** change `partitions` only when the interface has nothing pending: `ductus drain --wait` (or stop the sender and check `ductus status`), then restart with the new value. If you lowered it by mistake, set it back and restart: the stranded messages are sent, in order within their lanes (a patient who also got messages while it was lowered may get those out of order, as when raising it). To give each of N workers 64 lanes, set `partitions: 64 * N`. (The engine will refuse an unsafe change, then re-assign waiting messages itself, in later releases; until then, this is on you.)

### Without an ordering key, more workers don't help (level 3)
- **What:** an interface with no `ordering.key` is one lane, and a lane has exactly one owner. `isolation: dedicated` with `workers: 8` still processes it one message at a time per destination (the other seven workers have no lanes).
- **Why:** with no key, the only safe order is the order of arrival for everything.
- **What to do:** scale in this order: (1) set an ordering key (64 lanes by default), (2) `isolation: dedicated` (own threads, 4 workers by default), (3) raise `workers` up to about the number of cores, (4) `dedicated_pools` if it does database lookups. Most interfaces need none of it: one destination of a shared interface drained ~1,500 messages/s on a laptop.

### Temporary failures retry forever by default (level 3)
- **What:** a destination that's down, or a transform that hits a temporary error, is retried with back-off (1 s up to 60 s) until it works. `retry.max_attempts` limits delivery attempts; transform retries (database or API down) have no limit.
- **What to do:** set `monitor: { behind: ... }` (see Monitoring) rather than expecting failures to show up as quarantined messages. A destination retrying for a minute with messages waiting shows as *stalled* with the error it gets.

### Database login failures make messages wait, not fail (level 3)
- **What:** a wrong password or a missing database counts as *temporary*: messages wait and retry instead of being quarantined.
- **Why:** it's a config problem; quarantining thousands of messages would make the fix much harder.

### `durability: relaxed` can lose the last fraction of a second (level 4)
- **What:** ACKs don't wait for the disk flush. If the *database server* crashes or loses power, the last moments of ACKed messages can be lost. An engine crash alone never loses anything.

### Respond-mode HTTP requests aren't stored (level 3)
- **What:** in `mode: respond`, the transform's result is the HTTP response; there's no queue and no stored message. If something fails, the caller gets an error status and decides whether to retry.

## Resending and correcting messages

### Resend sends a message again in place: first in its lane, not last (level 2)
- **What:** resending (`POST /api/v1/messages/{id}/resend`, or many at once with `/messages/resend`) puts a destination's finished delivery back to pending. The message keeps its id, so its worker takes it **next in its key's lane**, ahead of the messages waiting there, but **after** the newer messages for the same key that were already sent. Without destinations named, only the quarantined deliveries are resent. Naming a destination resends to it whatever its delivery ended as, sent or filtered too (a replay: "the lab lost yesterday's results"). Pending deliveries are left alone: they're being sent or retried already. Each resend is recorded with who did it, why, and what the delivery was before (`GET /messages/{id}` shows them).
- **Why:** in place keeps the message's identity and history in one row; nothing is copied.
- **What to do:** for feeds that carry state (ADT updates, results corrections), look before resending an old message: if newer messages for the same patient were sent since, the receiver may apply the old one over newer data. The message's page shows its ordering key; the message browser shows what came after it.

### A replay is the same message; an edited copy is a new one (level 3)
- **What:** a resent message has the same `meta.id` and the same MSH-10, and the transform runs again with **today's** code and lookups (see "Only the original is stored"). A receiver that deduplicates on `meta.id` or the control id will drop a replay as a repeat. An edited copy (`POST /messages/{id}/edit`) is a new message: a new id (so a new `meta.id`), its own ordering key and control id read from what you typed, never deduplicated, at the back of its lane. The original stays exactly as received; its quarantined delivery to the copy's destinations becomes filtered ("replaced by edited copy N"), and the copy shows which message it corrects. A routed original's copy keeps its `origin`.
- **What to do:** to replay to a receiver that deduplicates, ask them to accept it (or change MSH-10 in an edited copy). Replaying to a route (`interface: x` destination) queues the message in `x` again, where `x`'s `dedupe` applies.

### A message still pending can't be replaced yet (level 3)
- **What:** an edit is refused for a destination where the original is still pending: its worker may be sending or retrying it at that moment, and both would go. A message that fails with a *temporary* error stays pending and retries forever by default, holding up its key's lane.
- **What to do:** for a destination whose receiver rejects bad data with a temporary error (5xx), set `retry.max_attempts` so such a message ends in quarantine, where it can be corrected. Pausing the destination doesn't make it editable (a send under way finishes, and the message stays pending). A "skip this message" action is planned.

## Routing between interfaces

### A route is exactly once inside the engine, not beyond it (level 3)
- **What:** `destination: { interface: lab }` stores the message in `lab` (which has `source: internal`) in the same database transaction that marks it sent here. A crash before that commit routes it again later; after it, never. Each routed message knows where it came from: `meta.origin` in code, `origin` in the API, `origin_id` in the table. What `lab` then sends to an outside system is at-least-once, as always.
- **Why:** both sides are rows in the same database, so one commit covers both. There's no network hop and no ACK to lose.
- **What to do:** nothing for the hop. Use `meta.id` (or `meta.origin`) as the idempotency key towards outside receivers, as usual.

### The receiving interface's keys, dedupe and destinations apply (level 2)
- **What:** a routed message is a new message of the receiving interface: its `ordering.key`, `dedupe` and destinations are applied as it arrives, read from what the sending function produced. The sending destination's output format must be the receiving interface's `format` (`ductus validate` and startup check).
- **What to do:** if the receiving interface orders by MRN, the routed message must still carry the MRN where its `ordering.key` looks.

### Order holds across a route when the sender keeps it (level 4)
- **What:** messages leave a sending lane in order and are stored in the target in that order. Per-key order holds end to end when the sender orders by the same key, or has no key at all. If several interfaces route to one target, their messages interleave (each sender's own order is kept).
- **What to do:** give the sending interface the same `ordering.key` as the target when order matters.

### A routed message waits up to 20 ms before the next interface sees it (level 4)
- **What:** routes are stored in batches (up to 50 messages, or after 20 ms, or when the worker runs out of work), like sent statuses, so each commit carries many messages.
- **What to do:** nothing, unless a chain of many hops must answer in single milliseconds.

### Routes are fixed in interface.yaml, and loops are refused (level 2)
- **What:** where a destination routes to is configuration, not code. A route that would bring a message back to an interface it passed through (a → b → a) fails `ductus validate` and stops startup. An interface with `source: internal` that nothing routes to only gets a warning.
- **What to do:** to route by content, give the sender a destination per target and `return null` (or `kill()`) in the functions of the ones a message shouldn't go to. Rule-based routing without code comes in v1.1.

### Routes keep moving while draining (level 4)
- **What:** draining stops new messages from outside. Routed messages were already received, so they still flow to their targets, and the drain waits for the targets too.

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

### `fetch()` and `api()` retry 5xx, 408 and 429 by default (level 3)
- **What:** those statuses become temporary errors (the message waits and retries). Pass `retry: false` to handle them yourself. Bodies are text only, up to 16 MB.

### `api()` headers: the API's, then yours (level 2)
- **What:** an API in `resources.yaml` brings its URL, headers and timeout. A header your code (or an HTTP destination) sets with the same name replaces the API's. Paths go under the API's URL (`/patients/1` under `https://h/api/v2` is `https://h/api/v2/patients/1`); a full URL is refused.
- **Why:** the token lives in one place, per environment, and never passes through interface code; overriding is for the rare call that needs something else.
- **What to do:** keep credentials in the API's `headers` (with `${secret:...}`), not in code. Tests answer with `mock.api("name", ...)`; `mock.fetch` doesn't answer `api()` calls.

### Write resource names out: `db("patient_db")`, not `db(name)` (level 2)
- **What:** `ductus validate` checks that every database and API named in code exists in `resources.yaml`, and `ductus impact` uses the same names to know which interfaces use what. A name computed at run time can't be seen before the code runs: validate warns (DX4), and impact doesn't know that code uses it. The engine itself doesn't check names at startup: a wrong name quarantines messages at run time.
- **What to do:** write the name in the call. If a shared helper takes a name, have callers pass a literal through a small wrapper per resource, or accept the warning knowingly. Run `ductus validate` before deploying (in CI).

### Dedicated connection pools only for dedicated interfaces (level 4)
- **What:** `dedicated_pools` needs `isolation: dedicated`; shared-pool interfaces use the shared pools from `resources.yaml`.

## Impact and `about:`

### `ductus impact` only knows what's named (level 2)
- **What:** it finds interfaces through `about:` (applications, vendors, sender hosts), destinations' `application`/`vendor`, the resources.yaml entries they use, and the databases and APIs their code names. A side with no system named is invisible to `--vendor` and `--application`. Host names are compared as written unless you pass `--resolve` (DNS lookups, from where you run it).
- **What to do:** fill in `about:` when creating an interface (`ductus new` writes it); `validate` warns about gaps, and `require_about: true` in `ductus.yaml` makes them errors. Name the vendor on shared resources once in `resources.yaml`. List senders' subnets in `about.from.hosts`: the engine can't know where inbound connections come from.

### A destination's owner is its interface's unless it says otherwise (level 1)
- **What:** `about.owner` owns the interface and every destination; `owner:` on a destination overrides it for that destination only (e.g. its code is another team's). `ductus owners` lists destinations separately only when their owner differs. `ductus owners reassign --from A --to B` changes only `owner:` values that are written down: a destination that inherits moves with its interface.
- **What to do:** to hand one destination to someone else, add `owner:` to it by hand; to split a person's interfaces, run `reassign` once per group with `--interface`. It edits interface.yaml in place (comments and layout kept), so commit and deploy the change like any other.

### Moving ownership in the web UI is a preview, not a change (level 1)
- **What:** the Owners screen (and `POST /owners/reassign/preview`) shows what would change and gives a patch and the `ductus owners reassign` command; it writes nothing. The engine never edits its own project: interface.yaml lives in your repository, and a change made on the server would be undone by the next deploy. Owners shown stay as they are until the change is deployed.
- **What to do:** apply the patch in the project's repository (`git apply owners.patch`) or run the command there, review, commit and deploy. The preview reads the files on the engine's server, so preview against the engine running the version you'll change.

## Checking connections (`ductus doctor`)

### Doctor checks from where it runs (level 2)
- **What:** every result (DNS, routes, firewalls, the source address in a firewall request) is about the machine running `ductus doctor`. A laptop on the VPN and the engine's server can get opposite answers.
- **What to do:** run it on the engine's server, as the account the engine runs as (folder rights and trusted certificates are per account), with the environment it uses (`--env prod`).

### A pass means "can connect and log in", not "will accept your messages" (level 2)
- **What:** doctor never sends anything to a destination: MLLP and HTTP destinations are only connected to (and TLS checked). For an API it sends one `GET` to the API's base URL with its headers and reports the status: a 404 there is normal, a 401/403 means the token was refused. Databases get a login and a version query; SFTP a host key check, login and SFTP start. Code that writes to a table it has no rights to still fails at run time.
- **What to do:** use doctor for "can we reach it"; use the interface's tests and a test message for "does it accept what we send".

### One working address is enough to pass, with a warning (level 3)
- **What:** a name can resolve to several addresses (IPv4 and IPv6, or a cluster). Doctor tries each. If some fail and one works, the check warns: the engine (like any client) may try a failing address first and be slow, or fail now and then.
- **What to do:** ask for the DNS entry to list only working addresses, or for the others to be opened.

### The commit time is the store's disk, and the ceiling per strict sender (level 4)
- **What:** doctor times 20 durable commits against the engine's database. With `durability: strict`, each ACK waits for one, so one sender connection can't go faster than about 1 / commit time (2.4 ms ≈ 410 msg/s); more connections share commits. Doctor warns above 10 ms.
- **What to do:** if that's near your peak per connection, use a faster disk (SSD/NVMe, write cache with battery), more sender connections, or `durability: relaxed` where losing the last fraction of a second on a database crash is acceptable.

## Files, SFTP and schedules

### A file is picked up one poll after it stops changing (level 3)
- **What:** a file is ready when its size and modification time haven't changed since the previous poll (or its `marker` file exists). Expect a delay of up to two poll intervals. Dot-files are skipped.

### One file at a time, oldest first; a failure stops the rest (level 3)
- **What:** files keep their order: if one fails, later files wait. Unsplit files over 256 MB are left in place with an error.

### Cron uses the server's local time; missed runs collapse to one (level 3)
- **What:** after downtime, a schedule runs once, not once per missed time. A run is skipped if the previous run's message is still pending.

## Operations: drain mode and the REST API

### Draining closes MLLP listeners; it doesn't answer AE (level 4)
- **What:** `ductus drain` (or `POST /api/v1/drain`) stops taking new messages. MLLP listeners close: open connections are closed after the message being handled is ACKed, new ones are refused. A message that was half received, or that arrived just as the drain started, is dropped unanswered, never ACKed, so the sender sends it again later. Queued HTTP sources answer 503 with `Retry-After: 30`. File pickup pauses (a file being read stops between batches and resumes at its saved offset after `ductus resume` or a restart). A schedule that comes due while draining runs once on resume, however many times it came due.
- **Why:** to a sender, a closed port means "receiver down: keep the message and retry", which every sender handles. An AE can be configured to mean "skip it after N tries" on some engines, which would lose messages.
- **What to do:** expect senders to show the connection as down during a drain. Use it for upgrades and server moves: `ductus drain --wait --stop`, upgrade, start.

### Respond-mode HTTP keeps answering while draining (level 4)
- **What:** respond-mode interfaces (APIs that answer from a transform) keep answering until the engine stops. They store nothing, so there's nothing to drain.
- **What to do:** take the server out of your load balancer when you drain it. `GET /api/v1/health` reports `drain: draining`, which a load balancer can check.

### A drain finishes only when every destination has caught up (level 4)
- **What:** "drained" means nothing is pending anywhere. A destination that's down, or an interface stopped because its code ran away, keeps the drain waiting (`ductus drain --wait` and `GET /api/v1/drain` show which interfaces). Quarantined messages don't count: they wait for a person anyway.
- **What to do:** fix the destination, or resume and plan the maintenance for later. Stopping anyway is safe: what's pending stays stored and goes out after the restart.

### Drain mode isn't remembered across a restart (level 4)
- **What:** an engine that restarts takes messages again, even if it was draining.
- **Why:** the usual sequence is drain, stop, upgrade, start, and the start should take messages.
- **What to do:** to keep senders out across a restart, stop the senders, or drain again right after starting.

### The API listens on this machine only, until you open it (level 4)
- **What:** the REST API listens on `127.0.0.1:7700` by default (`api.listen` in ductus.yaml, `--api-listen`, or `off`). Every call except `/health` and `/openapi.json` needs a token. Tokens are made on the server with `ductus token create` and are stored only as hashes: a lost token can't be shown again; revoke it and make a new one.
- **What to do:** put a TLS reverse proxy in front before opening it to the network (the engine's own listeners have no TLS yet).

## The web UI

### The UI works over https, or at localhost on the engine's own server (level 4)
- **What:** the engine serves the web UI on the API's listener (`http://127.0.0.1:7700/` by default). Signing in trades a token for a session cookie marked `Secure`, which browsers keep only over https, or over plain http to `localhost`/`127.0.0.1`. Over plain http to another machine the browser drops it, and the sign-in page says so.
- **Why:** the cookie is as good as the token while it lasts; sent in clear text over a hospital network, anyone on the path could take it.
- **What to do:** to reach the UI from other machines, put a TLS reverse proxy in front of the engine (keep `api.listen` on `127.0.0.1` behind it), or use an SSH tunnel (`ssh -L 7700:127.0.0.1:7700 server`, then `http://localhost:7700`). The engine's own TLS comes in v0.6.

### A UI session is a token in disguise (level 2)
- **What:** a browser session acts with the role of the token it signed in with, and logs show that token's name. It ends after 12 hours unused, 7 days after sign-in, on sign-out, or when the token is revoked. Changes made with the session cookie (pausing, draining) must carry the `X-Ductus: 1` header, which the UI sends and other websites can't, so a page elsewhere can't act through someone's session. Scripts use the token itself and don't need the header.
- **What to do:** until users and logins arrive (v0.5 step 8), give each person their own token (`ductus token create jane --role operator`) so the log says who looked at which message or paused what; revoke it when they leave. The theme is kept per browser until then.

## Monitoring

### Counts and timings start at zero when the engine starts (level 3)
- **What:** `GET /api/v1/metrics` (Prometheus) and `since_start` in `GET /api/v1/interfaces` count messages received, sent, filtered, quarantined and retried, and time transforms, sends, database/API calls and the whole journey from receipt to sent. They're kept in memory and start again at zero with the engine. Queue sizes (`pending`, `quarantined`) come from the database and are always current.
- **Why:** counting in memory costs nothing per message; writing every count to the database would cost a write per message. Prometheus expects counters to reset and its `rate()` handles it.
- **What to do:** graph rates (`rate(ductus_deliveries_total[5m])`), not raw totals. For "how many today", use the message browser or the database, which keep everything for the retention window.

### Timings are estimates to a bucket, averages are exact (level 4)
- **What:** p50/p95 (half / 95% of the messages were faster than this) are estimated from histogram buckets (1, 2.5 and 5 per decade from 0.5 ms to 10 s, then 1 min, 5 min, 1 h), so they're good to about a bucket's width: a p50 of "3.5 ms" means somewhere between 2.5 and 5 ms. `mean_ms` is exact (the total time divided by the count).
- **What to do:** use p95 to spot slow outliers and changes over time, the mean to compare against a measurement.

### Prometheus series grow with destinations (level 4)
- **What:** each destination has 3 histograms (receipt to sent, send, transform) of 20 series each (18 buckets, a sum and a count), plus its counts and queue sizes: about 65 series per destination. 1,000 destinations ≈ 65,000 series, comfortable for Prometheus but a cost on hosted services that bill per series.
- **What to do:** on a hosted service, drop what you don't graph with `metric_relabel_configs` (for example keep `ductus_delivery_latency_seconds` and drop `ductus_transform_seconds`).

### In respond mode, calls run one after another unless you await them together (level 2)
- **What:** each `await` waits for its call before the next starts. Measured in the v0.4 demo (a patient lookup API: 2 database queries and 1 EMPI call): 2.1–3.0 ms in the engine on average, of which ~1.5–2.1 ms is the three calls one after another (~0.5–0.7 ms each on a local database; a remote one takes longer) and ~0.6 ms the engine itself.
- **What to do:** when calls don't depend on each other, start them together: `const [patient, empi] = await Promise.all([db("patient_db").one(...), fetch(...)])`. The response then takes about as long as the slowest call instead of the sum. `GET /api/v1/engine` (`resources`) shows each database's and API's call times, to see which one to work on.

### Nothing is expected until you say so (level 2)
- **What:** `monitor:` in interface.yaml says what operations expects: `active_within: 30m` (with optional `between: "06:00-22:00"`), `active_by: "Mon-Fri 09:00"` (with optional `from:`), `behind: 500`, `behind: 10m` or `behind: { messages: 500, age: 10m }` (a destination can set its own `behind`). Without it, an interface is *ok* however quiet it is, and a destination is *behind* only past a limit you set. Times are the server's local time.
- **Why:** how quiet is too quiet, and how far behind is too far, depends on the feed: a lab feed that sleeps at night, a nightly file, a 1M/day ADT feed. A default would alert on the wrong things.
- **What to do:** set `behind` with both a count and an age on every feed someone would call about (count alone misses an overnight stall when little arrives; age alone misses a daytime pile-up that still moves). Set `active_within` on steady feeds and `active_by` on scheduled ones. Alert on `ductus_destination_state > 0` and `ductus_interface_state > 0` in Prometheus (alerts in the engine come in step 8).

### What each state means, and when it shows (level 2)
- **What:** a destination is *ok*, *behind* (past its `behind` limit), *error* (has quarantined messages), *stalled* (messages waiting over a minute and nothing finished for a minute: retrying, a blocked lane, or a stopped interface; `ductus hotspots` shows which message) or *paused*, from best to worst (it shows the worst that applies; `summary` lists them all); says it in words ("behind 1,240 (oldest 12 min), catching up ~6 min"). An interface is *ok*, *silent*, *missed* (its `active_by` deadline) or *stopped*.
- **Why:** "it's backing up but still sending" and "it's not sending at all" need different calls: the first is the vendor's capacity, the second is down.
- **What to do:** treat *stalled* as the urgent one. *error* stays until the quarantined messages are dealt with, so clear them (or they hide the next real error). A backlog under a minute old isn't diagnosed at all: a flowing queue always has a few messages in it.

### Burst and slow receiver need a few weeks of history (level 3)
- **What:** "normal" is learned per hour of the week (Monday 07:00 compares with past Mondays at 07:00), from each hour the engine watched for at least 15 minutes; recent weeks weigh more. Until an hour has been watched once, a backlog shows its trend (catching up, or not) but no cause; for send times, the other hours' average stands in. The last few minutes are compared with it: a burst is over twice the normal arrivals, a slow receiver over twice the normal send time (and at least 5 ms more). The rates and the catch-up time cover the last 5 minutes and start over when the engine does; "normal" is kept in the database.
- **What to do:** expect causes after the first week, and good ones after three or four. A feed whose traffic changed for good (a new hospital on it) becomes the new normal within a few weeks.

### Finding the message or patient that holds a queue up (level 3)
- **What:** `ductus hotspots [interface]` (`GET /api/v1/hotspots`) shows, for every destination with messages waiting, the lanes they wait in, the first message of each (id, key, age, attempts, last error) and how the backlog splits over the workers, and says what it sees:
  - *worker N blocked by message M*: that message keeps failing and everything on its worker waits behind it. "Other messages were sent since it arrived" means the receiver is up and takes other messages: the trouble is this message (its content, or the receiver's data for that patient).
  - *every worker is retrying its first message*: the receiver, or something the code calls, looks down. No single message is to blame.
  - *lane L has 73% of the backlog (key K at its head)* / *worker N has 80% of the backlog*: one patient (or a few) arrives faster than one worker can send. Ordering keeps a key on one worker, so more workers don't help that key.
  - *a lane no worker reads*: `ordering.partitions` was lowered while messages waited (see "Change `ordering.partitions` only with an empty queue").
- **Why:** a backlog in an ordered queue sits behind one message per worker. The count alone says *that* it's stuck, not *which* patient to look at.
- **What to do:** for a blocked message, check its last error and the message (`GET /api/v1/messages/{id}`); fix the data or the receiver, or set `retry.max_attempts` so it's quarantined and the others go on (resending a single message comes with step 7). For a receiver that's down, call the vendor; `ductus pause` stops the retries meanwhile. A hot key under a few hundred messages clears by itself; one that stays hot every day is a feed to discuss with the sender (a batch load sent as one patient's updates?). Lanes with fewer than 100 waiting are never called hot: a flowing queue always has some.

### Anyone with a viewer token can read message payloads (level 4)
- **What:** `GET /api/v1/messages/{id}` (and the message page in the UI) returns the payload, which holds patient data. Each read is logged with the token's name; there are no finer permissions or audit log yet (they come with users and roles in v0.5 step 8).
- **What to do:** give tokens only to people and tools that may see patient data, and name them so the log says who looked.
