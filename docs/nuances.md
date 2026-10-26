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
- **What:** delivery is *at least once*. If the engine crashes (or is killed) after the destination accepted a message but before recording it as sent, the message is sent again on restart. Sent statuses are saved in batches of up to 50, so a crash can re-send up to that many per worker and destination. A clean stop saves them first (see "Stopping the engine" below), so it re-sends nothing.
- **Why:** the alternative (at most once) loses messages. Nothing that was ACKed is ever lost.
- **What to do:** make receivers tolerate repeats (most HL7 receivers dedupe on MSH-10), or use `dedupe` on the receiving interface.

### Your transform can run more than once for the same message (level 3)
- **What:** after a crash, a temporary failure (database down, `fetch()` getting a 503) or an interruption (see "Runaway scripts" below), the whole transform runs again from the start. Any `db().execute()` or `fetch()` POST it made the first time happens again.
- **What to do:** make writes idempotent: `INSERT ... ON CONFLICT DO NOTHING`, upserts keyed on the message's control ID, or an idempotency key header on API calls.

### Only the original is stored; outputs are recomputed (level 3)
- **What:** the engine stores each message exactly as received and never changes it. What was sent isn't stored by default; it's reproduced by running the same code on the original. That's exact only if the transform depends on nothing but the message and the code. Lookups (`db()`, `fetch()`), the current time and environment values can give a different result when re-run later.
- **Why:** transforms are fast, and storing every output would cost a database write per message.
- **What to do:** if you must be able to prove exactly what was sent, set `save_output: true` on that destination: what it was sent is kept with its delivery. Keep transforms that don't need lookups free of them.

### Stopping the engine: what finishes and what waits (level 2)
- **What:** Ctrl-C, SIGTERM (`systemctl stop`, `docker stop`, Kubernetes) and `ductus drain --stop` all stop cleanly: listeners close, messages being stored get their ACK, each worker finishes the message in hand (a transform or a send under way completes) and saves what it finished, then the engine exits with code 0. A message waiting in a retry back-off or a rate-limit wait isn't sent: it stays first in its lane for the next start. A stop that takes longer than `--stop-timeout` (`DUCTUS_STOP_TIMEOUT`, 30 s) exits anyway, and so does a second Ctrl-C or SIGTERM; then the messages in hand are processed again at the next start, like after a crash. Measured (`scripts/demo-stop.sh`, 6,000 messages over 8 connections, stopped 1 s in; WSL): SIGTERM re-sent nothing in 6 runs and exited with code 0 within 27–42 ms (the stop itself ~15–21 ms); `kill -9` at the same point re-sent 5 messages in one run of 3 (the window is small: workers save after every read). With the receiver down, SIGTERM exited in ~40 ms instead of waiting out the back-off, leaving the 20 messages pending.
- **Why:** an upgrade or a server reboot shouldn't count as a crash.
- **What to do:** prefer `ductus drain --stop` before planned work (it also lets the queues empty first); a plain service stop is just as safe for messages. Keep a transform's `timeout` well under the stop limit, and give the service manager longer than the stop limit (the packages set 45 s; Docker's default is 10 s, so use `docker stop --time 45` or `stop_grace_period`).

### A "done" marker can't prevent duplicates (level 5)
- **What:** recording "this insert happened" in the engine's database doesn't make an insert in *another* database happen exactly once: the two writes can't share a transaction, so a crash between them still repeats the insert on re-run.
- **What to do:** put the guarantee where the data lands: a unique key or upsert in the target table, or an idempotency key the receiving API checks. Use the message's stable key `meta.id` (the second argument of `transform`: `transform(msg, meta)`) for it. It's unique within one engine database; add something per environment if several engines write to the same place.

## The store (Postgres)

### A query that holds the message tables for days can pause intake (level 4)
- **What:** each day's tables are created ahead of time, hourly. Creating one needs a brief exclusive lock on `messages`/`deliveries`, which has to wait for every open query on them. Maintenance gives up after 2 s and tries again in an hour (partitions exist 3 days ahead), so a long report never holds up ACKs. If tomorrow's partitions still don't exist (something held the tables through every try for two days), maintenance stops giving up and waits: new messages (and their ACKs) then wait as long as the blocking query runs, rather than fail at midnight. The engine logs a warning when that happens.
- **What to do:** run long reports against a replica, or keep them short; never leave a transaction open on the store. Watch for the warning "a partition needed soon is missing".

## Several engines on one database

### An interface runs on one engine at a time; a second one is refused (level 2)
- **What:** every engine holds a lock on the database while it runs and claims its interfaces when it starts. An engine started with an interface another live engine runs **refuses to start** (exit 3), whatever else it has: "another engine is running 2 of this engine's interfaces on this database: adt, orders; owned by: host-b, pid 4120, ductus 0.6.0, started …". The refusal is in the audit log (`engine.refused`). Engines with *different* interfaces share a database without trouble; each lists the others (Overview, `GET /engine` `others`). Respond-mode HTTP interfaces aren't claimed: they have no queue, so several engines may answer them behind a load balancer.
- **Why:** two engines running one interface both read its queue: both send each message (duplicates), and per-patient order breaks. A DR server booted with production's config, or a test VM pointed at the production database, did exactly that before v0.6.
- **What to do:** stop the other engine first (the message names its host and pid), or take the interfaces out of this one's project. A refused start isn't retried by the service manager (systemd: `RestartPreventExitStatus=3`; Windows restarts only after a crash): fix the cause, then start it.

### A dead engine's interfaces are free at once; a vanished host's within ~25 s (level 3)
- **What:** an engine's lock lives in its own database session. When the process ends in any way (`kill -9`, a crash), the operating system closes the connection and Postgres frees the lock: the next engine can claim its interfaces immediately (0.3 s after `kill -9` in `demo-one-owner.sh`). When the whole host vanishes (power loss, a network cut), nothing closes the connection; the engine asks Postgres to probe it (TCP keepalives on that session: 10 s idle, then 3 probes 5 s apart), so the lock goes within about 25 s.
- **Automatic standby is not this:** a second engine doesn't wait to take over; it refuses, and you start it when the first is gone. A standby that takes over by itself is v1.2's clustering.
- **What to do:** for DR, keep the standby engine stopped (or pointed elsewhere) and start it after the primary is down. Nothing else to configure.

### Losing the database connection pauses sending, not receiving (level 3)
- **What:** if the engine loses the connection that holds its lock (a database restart, a failover, a DBA ending the session), it pauses sending, file pickup and schedules within a few seconds (it checks every 2 s), keeps receiving (storing is safe from any number of engines), and takes the lock and its interfaces back as soon as it can; then it sends what came in meanwhile. If another engine took an interface over in the gap, this one stops (exit 3) rather than run it twice. The Overview says "Cut off from the database" while it lasts.
- **What to do:** nothing for restarts and failovers. Don't end the `ductus engine lock` session by hand unless you mean to pause that engine.

### PgBouncer in transaction mode can't hold the engine's lock (level 3)
- **What:** the lock is a session advisory lock: it needs one database session for the engine's whole life. A pooler in transaction (or statement) mode hands each transaction a different server connection, so the lock would be taken on one and lost at once.
- **What to do:** point the engine at Postgres directly, or through a pooler in session mode. The engine pools its own connections (`--db-connections` / `DUCTUS_DB_CONNECTIONS`, 32 by default) plus one for the lock.

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
- **What:** each message's lane is computed from its key and the interface's lanes when it arrives, and stored. The lanes are `partitions` with an `ordering.key`, and **one** without (removing the key is lowering them to 1). Change them while messages are waiting and:
  - **Lowered:** waiting messages in lanes at or above the new count would belong to no worker and never be sent. **The engine refuses to start** (exit 3), naming each destination, how many wait and in which lanes: "adt → lab: 412 messages waiting in lanes 32–63; adt now has 32 lanes (it had 64)". `ductus validate` with `DUCTUS_DATABASE_URL` set says the same before you deploy.
  - **Raised:** allowed, with a warning in the log and an `interface.partitions_raised` row in the audit log: a patient's waiting messages stay in their old lane while new ones go to another (maybe another worker), so until the backlog drains that patient's messages can be delivered out of order. Raising doesn't lose anything, so it isn't refused; whether that brief disorder matters is your call.
  - **`workers` alone:** safe (every lane still has one owner).
- **Why:** the engine records the lanes each interface last ran with (`interface_applied`) and, when they went down, counts what waits in the lanes being dropped (one pass over the queue index: ~20–60 ms for 1,001 interfaces with 300,000 waiting, measured on WSL).
- **What to do:** change the lanes only when the interface has nothing pending: keep the old value, `ductus drain --wait` (new messages wait at the sender, the engine sends what it has), then restart with the new value. Run `ductus validate` against the database before deploying. To give each of N workers 64 lanes, set `partitions: 64 * N`. Re-assigning waiting messages to the new lanes automatically is planned (v0.7).

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

### Copy what you change, not the whole thing through JSON (level 2)
- **What:** `JSON.parse(JSON.stringify(x))` builds the whole object twice, as text and again as objects, for every message. It also changes what it copies: dates become strings, `undefined` and functions disappear, a `Map` becomes `{}`.
- **What to do:** you rarely need a copy: each destination's function already gets the message as received (see "Each destination runs its own function on the original message"), so change it in place. For an object from `db()` or `fetch()`, copy only what you change: `{ ...row, name: row.name.trim() }`.

### One query for many values, not one query per value (level 3)
- **What:** each `db()` call is a round trip to the database and a step from your code into the engine and back. A lookup inside a loop (one query per OBX, per row of a file) adds one round trip per item, and the transform waits for each in turn.
- **What to do:** collect the values first and ask once. An array parameter arrives in Postgres as `jsonb`: `db("lab").query("SELECT code, name FROM tests WHERE code IN (SELECT jsonb_array_elements_text($1))", [codes])`, then look the answers up in a `Map`. Lookups that don't depend on each other can also run together (see "In respond mode, calls run one after another unless you await them together").

### Short-lived objects are cheap; what you keep is what costs (level 4)
- **What:** the JavaScript engine (V8) frees memory by itself, in two ways. Objects made while handling one message and dropped at the end are collected often and quickly. Objects still referenced later (a module-level array or `Map` that keeps growing, results kept "for next time") move to a second area that is collected less often, with longer pauses that delay every interface on that thread, and they count toward `isolates.heap_mb` until the engine restarts.
- **Why:** this is how V8 works, as Java's and .NET's collectors do. Rust code in the engine has no collector; only your TypeScript does.
- **What to do:** make what you need inside the transform and let it go. Don't avoid creating objects to "save memory": per-message garbage is the cheap kind. Keep anything that outlives a message bounded (see "Module-level caches live until the engine restarts").

### Loops over thousands of items: give objects one shape (level 4)
- **What:** V8 makes property access fast by remembering how objects of the same shape (the same properties, added in the same order) are laid out. Objects built in different ways, or with properties added later or `delete`d, take a slower path at the places that read them.
- **When it matters:** only in code that touches many objects per message: a file of 10,000 rows, a result with hundreds of OBX. A message's handful of objects never notices.
- **What to do:** build such objects in one place with every property, in the same order (`{ mrn, name, dob }` with `""` for missing ones), and set a property to `""` or `undefined` rather than `delete`.

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
- **What to do:** apply the patch in the project's repository (`git apply owners.patch`) or run the command there, review, commit and deploy. The preview reads the files on the engine's server, so preview against the engine running the version you'll change. The patch keeps each file's own line endings, which is what `git apply` needs: from an engine on Windows (CRLF files, Git for Windows's `core.autocrlf`) it applies to a Windows checkout; a patch made on Linux (LF) applies to a Linux checkout or one with `autocrlf` off.

### An owner is a person when the text names exactly one account (level 1)
- **What:** an `owner:` is free text. The engine matches it to an account ignoring case and surrounding spaces: a **username** first, then an **email**, then a **display name**; the first of those that matches anyone decides. If it matches two accounts at that level (two "Anna Smith"s), it names nobody: no link, no alert mail to either (a guess would send one person's alerts to the other). An owner that is an account shows as that person (name, email) on the Owners and interface pages, makes the interface theirs under Interfaces → Mine, and gets `to: owner` alerts. Otherwise it is a team in `owners:` (ductus.yaml), an email address, or **nobody**. A disabled account (or one the directory refused) still matches, but is flagged: its person probably left. `ductus validate` can't know this (accounts are in the database, not the project): `ductus owners` with `DUCTUS_DATABASE_URL` marks each owner, the Owners page says the same, and the engine logs owners it can't reach at start. A directory person's account exists only after their first sign-in: until then their name reaches nobody.
- **What to do:** write owners as a username or an email when you can: those are unique, a display name may not be. Name teams and mailing lists in `owners:`. Run `ductus owners --check` in the pipeline that deploys the project (it fails on an owner that reaches nobody, a disabled account, or one that names several). When someone leaves, move what they own with `ductus owners reassign` before disabling them.

### Moving an owner moves their resources.yaml entries too (level 2)
- **What:** `ductus owners reassign --from A --to B` (and the Owners page's preview) changes `owner: A` in every interface.yaml **and** in resources.yaml (databases, APIs, SFTP servers, directories), so certificate alerts follow the person. Naming `--interface x` or `--resource y` moves only what is named (naming interfaces leaves resources.yaml as it is). The preview says when the new owner isn't an account.
- **What to do:** to split someone's work, run it per group with `--interface`/`--resource` (or untick on the Owners page). Commit the resources.yaml change with the interface files.

### Operators start on their own interfaces (level 1)
- **What:** Interfaces opens on "Mine" for an operator who owns at least one interface (or a destination of one); everyone else, and an operator who owns nothing, starts on All. "Mine" goes by the owner matching above, for the person signed in (a token acts for its owner; a service token has no "Mine"). The choice is in the address (`?mine=false`), so a bookmark keeps it.
- **What to do:** nothing; if an operator sees an empty "Mine", their owner texts don't name their account (see above).

### Your own documentation fields are checked by `ductus validate`, not by the engine (level 2)
- **What:** `docs-schema.yaml` declares the fields your company wants on every interface, destination or resources.yaml entry (text, enum, date, url, person, list; required or not; where each applies); the values go in a flat `custom:` block beside the other keys (not under `about:`). A missing required value, a value outside an enum, a date that isn't `YYYY-MM-DD`, an undeclared name or a field used where it doesn't apply is an error in `ductus validate` (and `ductus docs --missing` exits 1) — but **the engine starts anyway** and shows what is there. Documentation must never be the reason a hospital's interfaces are down. In the web UI a missing required value reads "Missing"; a broken `docs-schema.yaml` is logged at startup and the fields are ignored until it is fixed. Enums match exactly, case included (`yes` is not `Yes`): that is the point, since it stops "Yes/yes/Y" from drifting into your reports.
- **What to do:** put `ductus validate` in the pipeline that deploys the project, so a gap is caught before the engine sees it. Declare a field you can't fill in yet as `required: false` first, then tighten it. Quote `"yes"` and `"no"` values in YAML so no tool reads them as booleans (the engine takes them as text).

### The documentation snapshot is as old as the last engine start (level 3)
- **What:** at every start the engine writes each interface's and resource's documentation as JSON, in your field names, to the `docs_snapshot` table, so reports and BI tools can ask `docs->'custom'->>'baa_signed' = 'no'` in SQL. The YAML in Git stays the source of truth: the table is made to match the project at start (entries you removed disappear) and is not updated while the engine runs. There is no history yet (it comes with versioned releases). The web UI and API read the files as loaded at start too: editing `custom:` on a running engine shows after a restart.
- **What to do:** deploy documentation changes like any other (restart or roll the engine); don't edit `docs_snapshot` by hand, it is rewritten. Several engines on one database each write their own project's rows, the last to start wins: give each project its own database, as you already do.

## Checking connections (`ductus doctor`)

### Doctor checks from where it runs (level 2)
- **What:** every result (DNS, routes, firewalls, the source address in a firewall request) is about the machine running `ductus doctor`. A laptop on the VPN and the engine's server can get opposite answers.
- **What to do:** run it on the engine's server, as the account the engine runs as (folder rights and trusted certificates are per account), with the environment it uses (`--env prod`).

### A pass means "can connect and log in", not "will accept your messages" (level 2)
- **What:** doctor never sends anything to a destination: MLLP and HTTP destinations are only connected to (and TLS checked). For an API it sends one `GET` to the API's base URL with its headers and reports the status: a 404 there is normal, a 401/403 means the token was refused. Databases get a login and a version query; SFTP a host key check, login and SFTP start. Code that writes to a table it has no rights to still fails at run time.
- **What to do:** use doctor for "can we reach it"; use the interface's tests and a test message for "does it accept what we send". The engine runs the certificate part of these checks by itself every few hours (see Certificates).

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

### A start logs one line, not one per listener (level 2)
- **What:** an engine's start ends with one line: `started interfaces=1001 mllp=1001 http=0 files=0 schedules=0 internal=0 shared=1001 pool_threads=8 dedicated_threads=0 took=712ms`. Since v0.6 the per-interface lines ("listening (MLLP)", "watching for files", "schedule started", and "MLLP listener closed" at a stop or drain) are debug lines: a thousand-interface engine wrote a thousand of each at every start, stop and drain. Measured with 1,001 interfaces (WSL): ready in 0.69–0.83 s warm (1.3 s with a cold file cache), down from ~5 s in v0.5.
- **What to do:** `ductus ports` lists what a project listens on. To see each listener in the log, `RUST_LOG=info,ductus_runtime=debug`; that also logs how long each step of the start took ("start: ...").

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
- **What to do:** before opening it to the network, give it `tls:` (see "The API on the network needs `tls`"), or put a TLS reverse proxy in front and keep it on `127.0.0.1`.

## The web UI

### The UI works over https, or at localhost on the engine's own server (level 4)
- **What:** the engine serves the web UI on the API's listener (`http://127.0.0.1:7700/` by default). Signing in trades a token for a session cookie marked `Secure`, which browsers keep only over https, or over plain http to `localhost`/`127.0.0.1`. Over plain http to another machine the browser drops it, and the sign-in page says so.
- **Why:** the cookie is as good as the token while it lasts; sent in clear text over a hospital network, anyone on the path could take it.
- **What to do:** to reach the UI from other machines, give the API `tls:` in ductus.yaml (`api: { listen: 0.0.0.0:7700, tls: { certificate: ductus-api } }`) and open `https://`; or put a TLS reverse proxy in front (keep `api.listen` on `127.0.0.1` behind it), or use an SSH tunnel (`ssh -L 7700:127.0.0.1:7700 server`, then `http://localhost:7700`).

### A UI session is a token in disguise (level 2)
- **What:** a browser session acts with the role of the token it signed in with, and logs show that token's name. It ends after 12 hours unused, 7 days after sign-in, on sign-out, or when the token is revoked. Changes made with the session cookie (pausing, draining) must carry the `X-Ductus: 1` header, which the UI sends and other websites can't, so a page elsewhere can't act through someone's session. Scripts use the token itself and don't need the header.
- **What to do:** until users and logins arrive (v0.5 step 8), give each person their own token (`ductus token create jane --role operator`) so the log says who looked at which message or paused what; revoke it when they leave. The theme is kept per browser until then.

### Your own colors can be unreadable on purpose (level 1)
- **What:** Appearance (user menu → Customize colors…) takes five colors (header, accent, page background, panel, text) and works the rest out from them. It checks every pair the screens draw against WCAG AA (4.5:1 for text, 3:1 for the focus ring) and marks the ones below it "Hard to read". It warns and does not stop you: **Use these colors** works either way, and the pages are then as hard to read as the check said. The built-in High contrast theme is the one that promises AAA (7:1).
- **Why:** it is your screen and your eyes; a rule that refuses a color nobody else sees would be in the way. But people spend all day in this, so the check is always shown.
- **What to do:** press **Fix it** on a row (or **Fix all**): it moves only the text or the accent, in lightness and just far enough, and shows the old and new values with Undo. It leaves the header, page background and panel alone, unless no text color could be read on them at all (a black page with a white panel): then it pulls the panel toward the page background and says so. The colors are kept in this browser, not on the server, until logins arrive (v0.5 step 8): another browser or machine starts from the system theme.

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
- **What to do:** set `behind` with both a count and an age on every feed someone would call about (count alone misses an overnight stall when little arrives; age alone misses a daytime pile-up that still moves). Set `active_within` on steady feeds and `active_by` on scheduled ones. The engine alerts on these states itself once `alerts:` is in ductus.yaml (see Alerts below); Prometheus can too (`ductus_destination_state > 0`, `ductus_interface_state > 0`).

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

## Alerts

### Alerts tell you once, then remind; they don't repeat every check (level 2)
- **What:** with `alerts:` in ductus.yaml, every interface and destination is judged every 15 s, whether or not anyone has the UI open. A state a rule names (`error`, `stalled`, `behind`, `stopped`, `silent`, `missed`; `certificate` has steps of its own, see Certificates) **fires** once, to the rule's channels. It **escalates** (people are told again) only when the severity rises, warning to critical. Getting better without being fine (stalled to behind) changes the alert quietly. While it stays open and nobody acknowledges it, a **reminder** goes every `repeat` (4 h by default; `0` = never). Once it has been fine for `settle` (2 min) it **recovers**, and the people who were told hear that too. Going bad again within `settle` reopens the same alert instead of firing a new one (a flapping receiver is one alert). More quarantined messages on a destination already in *error* are not a new alert.
- **Why:** an alert on every check is noise people learn to ignore. One per problem, a reminder while nobody owns it, and a word when it's over is what an on-call person can act on.
- **What to do:** acknowledge what you're working on (the Alerts page, or `POST /api/v1/alerts/{id}/ack`): reminders stop until it gets worse or recovers. Keep `repeat` unless the channel pages someone. Clear quarantines (resend, correct or dismiss): *error* stays open until the destination has none.

### A pause never alerts, and holds an alert open (level 2)
- **What:** a paused destination is paused on purpose, so it never fires. If it was already alerting, the alert stays open while paused: no reminders, and no recovery either (a pause isn't the receiver being fine). After the resume it's judged as usual.
- **What to do:** pause a destination you know is down (it stops the retries); the alert recovers once it delivers again.

### Silencing is not pausing, and acknowledging is not fixing (level 1)
- **What:** **silence** (up to 30 days, with a reason) sends nothing about an alert, not even its recovery, but it still shows, and you can silence a destination before planned work so nothing fires. **Pause** stops a destination sending. **Acknowledge** only says someone is on it. None of them touch messages: they keep waiting.
- **What to do:** for a planned outage, pause the destination and silence its alert, both with a reason. Both are in the audit log.

### An engine that is down sends nothing: use the heartbeat (level 2)
- **What:** alerts come from the engine. If it is stopped, crashed, or can't reach its database, nobody is told. `alerts.heartbeat: { url, every: 5m }` calls a URL every so often while the engine runs. A service that expects the calls (healthchecks.io, Better Stack, Uptime Kuma, your monitoring's own "push" check) raises its own alarm when they stop.
- **What to do:** set `heartbeat` in production, at a service that doesn't run on the engine's server, and set that service's grace period to about twice `every`.

### Notifications are sent at least once, and a failing channel only shows in the UI (level 3)
- **What:** a notification is stored with its alert (the outbox) before anything is sent, then sent by whichever engine claims it (two engines on one database send it once). A send that fails is retried: 30 s, then doubling, at most an hour apart, ten tries (about six hours), then counted as failed. A channel that fails never holds up another. If an engine dies after sending but before recording it, the notification goes out again after two minutes. A failing mail server or webhook can't alert about itself: failures show on the Alerts page (Channels), in the engine log, and in `ductus_alert_send_failures_total`.
- **What to do:** send critical rules to two channels of different kinds (email and a chat webhook). Watch `ductus_alert_send_failures_total` from Prometheus if you have it. Try a channel before going live with **Send test** or `ductus alerts test <channel> --env prod` (that one needs no engine).

### Quiet hours hold warnings until morning; critical ones never wait (level 2)
- **What:** `quiet: { between: "22:00-06:00", severities: [warning] }` holds notifications of those severities until the quiet hours end (server local time, like schedules). A warning that fires and recovers overnight is never sent: it's in the history. A critical one, or a warning that escalates to critical, goes at once.
- **What to do:** keep critical out of `severities`. Remember the server's time zone when the engine runs in UTC (containers often do).

### Who "owner" is (level 2)
- **What:** `to: [owner]` sends to whoever owns the destination (its own `owner:`, else its interface's `about.owner`). The owner text is matched, ignoring case, to a user's username, email or display name (a user with an email). Failing that it's looked up in `owners:` in ductus.yaml (a team's mailing list), and failing that it's used as is if it's an email address. An owner found nowhere gets nothing; the rule's other channels still do. `ductus validate` can't see users (they're in the database), so it only warns about owners that are neither in `owners:` nor addresses. The engine logs every owner that reaches nobody at start, and the Alerts page lists them.
- **What to do:** put teams in `owners:` with their list address, and give users an email. A test interface can say `alert: { mute: true }`; `alert: { to: [pager] }` adds recipients for one interface.

### A restart doesn't alert on what it hasn't watched yet (level 3)
- **What:** states that need watching (`stalled`, `behind`, `silent`, `missed`) are judged only after the engine has run a minute. `error` and `stopped` are facts in the database and alert at once. Open alerts are in the database too, so a restart neither repeats them nor forgets them.
- **What to do:** nothing. Expect the first alerts about a problem that was already there about a minute after a start.

### Webhook URLs are secrets (level 2)
- **What:** a chat webhook URL is its own password: anyone with it can post to the channel. The engine shows only its scheme and host (`https://hooks.slack.com/…`), in the UI, the API, logs and errors.
- **What to do:** write it as `${secret:CHAT_WEBHOOK}` (`ductus secret set`); `alerts:` may use `${...}` like resources.yaml, filled in per environment. Formats: `slack` (also Mattermost and Rocket.Chat), `teams` (a Workflows webhook: the old Office 365 connectors are being retired), `json` (everything, for your own tools).

### Judging alerts costs little, and runs all the time (level 4)
- **What:** every 15 s the engine counts its queues (as live updates do) and compares each state with its stored alert, writing only what changed. Measured with 1,001 interfaces (2,002 alerts, WSL): 35 ms a round, about 0.1% of one core in all, and 7 MB of memory.
- **What to do:** nothing at that size. Each round's time is in the log at debug level (`RUST_LOG=ductus_runtime::alerts=debug`, "alerts judged").
## TLS on what the engine listens on

### The API on the network needs `tls` (level 1)
- **What:** `api: { listen: 0.0.0.0:7700 }` without `tls:` serves the API and the web UI over plain HTTP: passwords and tokens cross the network readable, and browsers won't keep the session cookie (it is `Secure`), so the UI can't sign in. The engine and `ductus validate` warn about it.
- **What to do:** name a certificate (`api: { listen: 0.0.0.0:7700, tls: { certificate: ductus-api } }`, with `ductus-api` under `certificates:` in resources.yaml). Browsers that have seen it over HTTPS won't try plain HTTP to it again for a year (HSTS).

### A certificate the engine can't use stops the start, with the reason (level 2)
- **What:** every `certificates:` entry is loaded at start, before the database: a missing file, a file that isn't PEM, a key that isn't the certificate's, or an expired certificate stops the engine and says which entry and why. `ductus validate` checks the same, and shows each certificate's names and days left.
- **Why:** a typo should show at deploy, not as the first sender's refused handshake at 2 a.m.
- **What to do:** run `ductus validate` in the environment you deploy to (certificates from secrets differ per environment).

### Renewing: replace both files, no restart (level 2)
- **What:** a certificate given as `cert_file` and `key_file` is read again every minute (one task for the whole engine, comparing the files' contents; nothing touches the database). A changed pair that loads is used for new connections; connections already open keep theirs until they close. A changed pair that doesn't load (half copied, a new certificate with the old key, expired) is logged once as an error, and the engine keeps presenting the pair it had: a bad renewal never takes a listener down.
- **Why:** that's how certbot, win-acme, `step` and most PKI tooling deliver certificates: they replace files. Any ACME client works this way, with no engine feature needed.
- **What to do:** write the new files, then check the log for `certificate renewed` within a minute (or the "Ours" group on the Connections page; its "Check now" reads the files at once). Either order works: while only one of the two has changed, the old pair stays in use (a check that lands in between logs one error, which the next check clears). Certificates from secrets (`cert:`/`key:`) and CAs change with a deploy and a restart.

### Give the whole chain in `cert_file` (level 3)
- **What:** the engine presents exactly what `cert_file` (or `cert`) holds: the certificate first, then its intermediates. Browsers often fetch a missing intermediate themselves; Java and .NET senders and most integration engines don't, and refuse the handshake.
- **What to do:** put the CA's intermediate certificates after yours in the same file (the CA usually sends a "full chain" or "bundle" file). Doctor's full mode (the Connections page's "Run the checks", `ductus doctor` while the engine runs) connects to each TLS listener and warns when the chain stops short of a root (see "Doctor reads the chain our listeners serve").

### Senders on TLS 1.0 or 1.1 can't connect (level 3)
- **What:** listeners take TLS 1.2 and 1.3 only. Old engines and appliances that only speak 1.0/1.1 fail the handshake; the log says "no TLS version or cipher in common".
- **What to do:** upgrade the sender, or put a TLS-terminating proxy in front for it alone (and keep that hop on a trusted network).

### A client certificate says which system sent, not who (level 3)
- **What:** with `clients: { ca: hospital-ca, subjects: [...] }`, a listener takes only senders whose certificate the CA issued and whose subject matches one pattern (`CN=bridges*.hospital.org`; all parts of a pattern must match; `DNS=` matches the certificate's DNS names). It identifies the sending system, not a person: nothing about it reaches the audit log as an actor.
- **What to do:** keep `subjects` narrow: without it, every certificate that CA ever issued (often every machine in the hospital) is let in.

### A refused handshake is logged once a minute per sender (level 3)
- **What:** a sender that isn't speaking TLS, has no or the wrong client certificate, doesn't trust ours, or doesn't finish within 10 s is dropped without an answer (an MLLP sender sees the connection close, no ACK). The log says why, at most once a minute per listener and sender address: a misconfigured sender retries every few seconds, and the log isn't for counting them.
- **What to do:** after changing a sender, wait a minute for the next line, or watch the sender's own log. A check the running engine makes of its own listeners (the Connections page's full checks) isn't logged as a refusal; `ductus doctor` run on the same server is another program, so a listener that wants client certificates logs its check once, from this server's own address.

### A certificate from the hospital's CA: request, then show (level 2)
- **What:** `ductus cert request tls/mllp --dns mllp.hospital.org [--ip 10.1.2.3] [--subject "O=General Hospital, OU=Interfaces"]` writes a new private key (`tls/mllp.key`, readable only by its owner, never overwritten) and a certificate signing request (`tls/mllp.csr`) asking for a certificate a server and a client can both use. The key is RSA 2048 unless you ask for `--key ecdsa`: every CA takes RSA (Microsoft AD CS templates often still require it). What comes back goes in `tls/mllp.pem`, ours first, then the CA's intermediates. `ductus cert show tls/mllp.pem` (or the entry's name) says what it holds and judges it the way the engine and doctor do: names, issuer, days left (the chain's earliest), the chain's order and whether it reaches a root or a CA named in resources.yaml, and whether the key is the certificate's. It exits 1 when the engine would refuse the pair (wrong key, expired, not a certificate), and says when a file is a CSR, a key, or DER (binary) instead of PEM.
- **Why:** a key generated elsewhere and mailed around is a key someone else has; a chain that's incomplete or in the wrong order passes a browser test and fails Java and .NET senders.
- **What to do:** run `cert request` on the engine's server, send only the `.csr`, and run `cert show` on what comes back before naming it in resources.yaml. To renew: a new request under another name, then copy its `.pem` and `.key` over the two in use (see "Renewing").

### A CA's certificate as a server's (level 3)
- **What:** `openssl req -x509` makes a certificate marked as a CA's (`CA:TRUE`) by default, which is how many lab receivers get theirs. The engine's connections (MLLP and HTTP destinations, `apis:`, webhooks, the heartbeat, directories) accept a receiver presenting one **only when that exact certificate is itself trusted**: named in `ca:`, or installed as trusted on the engine's server. Its dates and the host name are still checked. That's the same on Linux and Windows, and what Windows, Java (Mirth) and .NET do. Trusting one exact certificate is at least as strict as trusting a CA. A CA:TRUE certificate that is only *issued* by a trusted CA is refused ("the receiver presents a CA's certificate (CA:TRUE) as its own"), on Windows too, though Windows alone would take it. A client certificate like that is refused on our listeners. **Exceptions:** three connections use a TLS stack of their own. Alert mail (SMTP) follows the OS: Linux refuses any CA:TRUE certificate, Windows takes both kinds. Postgres and SQL Server refuse any CA:TRUE certificate on both. Doctor's mail check says what the mail connection would. `ductus cert show` warns about any CA:TRUE pair.
- **What to do:** for a lab, naming the receiver's own certificate in `ca:` works. Anywhere else, ask for a server certificate (`ductus cert request` for the hospital's CA, or `ductus cert self-signed` for a lab of your own).

### What TLS costs (level 4)
- **What:** measured on one engine, TLS against plain (`scripts/bench-tls.sh`, WSL laptop, local Postgres with ~5 ms durable commits): strict MLLP ingest is the same within noise, 234–239 vs 238–245 ACKs/s on one connection and 2,690–2,780 vs 2,720–2,930 on 16 (the commit flush bounds both; the handshake is once per connection). A sender's connect to the end of its handshake takes ~0.56 ms p50 with an ECDSA certificate, ~0.96 ms with RSA 2048, ~0.66 ms when we also check its client certificate (plain TCP: 0.08 ms); TLS 1.3 resumption saves little (~0.54 ms: it still agrees a new key). An idle TLS connection holds ~23 kB of the engine's memory against ~15 kB plain (1,000 held open: +23 MB vs +15 MB). Respond mode over HTTPS: 0.29 ms p50 / 0.39 ms p95 on a kept-alive connection (HTTP: 0.27 / 0.36); with a new connection per request 1.2 ms / 3.0 ms (HTTP: 0.51 / 0.73).
- **What to do:** turn TLS on wherever the network isn't yours: for MLLP feeds it costs nothing you'll see at hospital volumes (1M ADT a day is ~12 msg/s on average). Senders that open a connection per message (some do) pay one handshake each, ~1 ms with RSA, on top of the ~4 ms a strict commit takes here: unnoticeable at a few messages a second. Many such senders at once, handshaking hundreds of times a second, weren't measured: ask a busy one to keep its connection open, as MLLP senders usually do. Callers of respond-mode APIs should keep connections alive (most HTTP clients do by default): a new HTTPS connection per call roughly quadruples the time on the wire. Memory is no reason to avoid TLS: 1,000 connections add ~23 MB.

### Post-quantum key exchange: on by default, never in the way (level 3)
- **What:** every TLS connection the engine makes or accepts (listeners, the API, MLLP and HTTP destinations, `api()` and `fetch()`, mail, webhooks, the directory) offers and prefers the hybrid key exchange X25519MLKEM768 (classic X25519 plus ML-KEM-768, the one browsers use). A peer that knows it gets it; one that doesn't gets plain X25519 with no extra round trip (both key shares go in the first message). It protects the session keys, so traffic recorded today can't be decrypted later by a quantum computer ("record now, decrypt later": health records stay sensitive for decades). Certificates and signatures are unchanged (no post-quantum certificates yet: not issued by CAs). Measured (`scripts/bench-tls.sh`): +0.15 ms per full handshake (ECDSA p50 0.69–0.73 ms vs 0.55–0.58 ms with X25519 alone), ~1.2 kB more in the first message; nothing on a connection kept open. Checked against OpenSSL 3.5 both ways: hybrid when both offer it, X25519 when one doesn't.
- **What to do:** nothing to turn on. Doctor's TLS step says what each server agreed ("TLS 1.3, post-quantum key exchange (X25519MLKEM768)" or "key exchange X25519"): the second kind are partners to ask about it (OpenSSL 3.5, Java 24+, recent Windows and Go have it).

### Testing TLS without the real systems (level 1)
- **What:** `ductus gen --tls` sends over TLS, trusting this server's roots plus `--ca FILE` and presenting `--cert FILE --key FILE`, verified the way the engine's destinations verify; it says why a handshake fails ("the receiver wants a client certificate (name one in --cert)"). `ductus sink --tls --cert FILE --key FILE [--clients-ca FILE --subject PATTERN]` takes MLLP over TLS only, as a listener with `tls:` does, and logs refusals the same way. Both read PEM files, the same as `certificates:` entries.
- **What to do:** try a feed's TLS settings in a lab with `ductus cert self-signed` pairs before the receiver's team is involved (`scripts/demo-tls.sh` does the whole path, with a renewal mid-stream).

### HTTP interfaces on one port share its `tls` (level 2)
- **What:** HTTP interfaces on the same port are one server, so they're all HTTPS with the same certificate, or all plain. `ductus validate` and the engine refuse a port whose interfaces disagree.
- **What to do:** give them the same `tls:`, or different ports.

## Who may connect (`allow:`)

### An allow list says who, which "one connection only" never did (level 2)
- **What:** `allow: [10.1.20.0/24, 10.1.30.15, epic-bridge01.hospital.org]` on an MLLP or HTTP source (or on `api` in ductus.yaml) closes a connection from any other address before the TLS handshake and before a byte is read; without `allow`, anyone who can reach the port may connect, as before. Listeners take any number of connections; there is no `connections: 1` setting.
- **Why:** sites limit a listener to one connection as a stand-in for "only the sending system", but it doesn't say which system: whoever connects first wins, and after a network blip the real sender's reconnect can be refused while its old half-dead connection holds the slot. An allow list names the senders, and a sender that opens several connections (common: one per thread, or a new one before the old has timed out) isn't refused for it.
- **What to do:** list the sending system's addresses (or its subnet), its failover or DR hosts, and anything that relays for it. `ductus validate` warns when `about.from.hosts` names a sender the list would refuse, and refuses a malformed entry (a bad subnet, a wildcard `*.hospital.org`, a range `10.1.20.1-10.1.20.50`, `allow: []`).

### It's defence in depth, next to the firewall (level 2)
- **What:** the check is in the engine, so it holds wherever the engine runs (a new server, a container, a firewall rule opened too wide), and its refusals are in the engine's log with the listener and address. But the connection has already reached the engine's server: the firewall still keeps everyone else off the network, and stops floods the engine would otherwise have to accept and close.
- **What to do:** keep the firewall rules; add `allow` so a mistake in either is caught by the other. Behind a proxy or load balancer the address seen is the proxy's: list the proxy (we don't trust `X-Forwarded-For`, which any caller can write).

### Host names are looked up at start and every 5 minutes (level 3)
- **What:** a host name in a list is resolved forwards (name → addresses: hosts file, then DNS) when the engine starts, every 5 minutes after, and at once after a refusal at a listener that lists names (at most once a minute per name, however many connections are refused). Connections never trigger a lookup: ten connections from `epic-bridge01` compare against addresses already in memory. A name that doesn't resolve at start lets no one in for it (the engine still starts, with a warning); a name that stops resolving keeps the addresses it last had, so a DNS outage doesn't cut off senders that were working. No reverse lookups: a sender controls the reverse record of its own address. Cost: one lookup per distinct name every 5 minutes, and nothing at all when no list names a host.
- **What to do:** prefer addresses or subnets for senders whose address never changes; use a name when the sending side moves (failover, a cloud host) and its DNS is kept current. After a sender moves, its first connection is refused and the next one, seconds later, gets in.

### Refusals are logged once a minute per listener and address (level 3)
- **What:** `connection refused: not on the allow list` with the listener, the address and the list (names with their addresses now), at most once a minute per listener and address, like refused TLS handshakes. The Ports card on the Connections page and `GET /ports` count refusals since the engine started and show the last address refused; the interface page shows the list. The sender sees its connection closed with nothing read: an MLLP sender gets no ACK and retries.
- **What to do:** a sender that says it can't connect: look for its address in the log or on the Ports card. The engine's own checks (Connections → Check now) are let in whatever the list says; `ductus doctor` run as its own program on the same server is refused unless 127.0.0.1 is listed, and it says so instead of checking the TLS it can't reach.

### List 127.0.0.1 on the API if you use it from its own server (level 2)
- **What:** `api.allow` applies to everyone, including `ductus status`, `drain` and a browser on the engine's own server. `ductus validate` warns when the list leaves out 127.0.0.1.
- **What to do:** `api: { listen: 0.0.0.0:7700, tls: ..., allow: [127.0.0.1, 10.1.5.0/24] }` (this machine and the operators' subnet).

### HTTP interfaces on one port share its `allow` (level 2)
- **What:** the list is checked before the request is read, so before the path is known: interfaces on one port share one list, and `ductus validate` and the engine refuse a port whose interfaces disagree.
- **What to do:** give them the same `allow:`, or put callers with different lists on different ports.

### Dead connections are noticed in about 30 seconds (level 3)
- **What:** every connection our listeners accept has TCP keepalive on: after 15 s of silence the server's kernel probes the sender every 5 s, and after 3 unanswered probes the connection is closed (the operating system's default would take about 2 hours). A sender that was powered off or cut off by a firewall no longer holds a connection open for hours, and quiet MLLP connections stay open through firewalls that drop idle ones. The kernel sends the probes; the engine does nothing.
- **What to do:** nothing; a firewall that forgets idle connections in under 15 s would still drop them (rare: most use minutes to an hour).

## TLS on what the engine connects to

### A private CA is trusted only where you name it (level 2)
- **What:** every connection the engine makes over TLS (MLLP destinations with `tls:`, HTTPS destinations and `apis:`, the directory, Postgres `databases:` entries, the alerts' mail server, webhooks and heartbeat) trusts what this server's system trusts, plus the CA its `ca:` names (a `certificates:` entry in resources.yaml), for that connection only. `tls: true` on an MLLP destination is the system's alone. The receiver's certificate must also cover the `host` (or URL host) connected to: a certificate for `hie.example.org` is refused at `10.1.2.3`, unless it lists that address.
- **Why:** a hospital CA added to the whole server would be trusted by every connection, and every program on it; naming it per connection keeps a partner's CA to that partner.
- **What to do:** ask the receiver's team for the CA that issued their certificate (not their certificate itself, which changes at every renewal), put it under `certificates:` (`ca_file:` or `ca: ${secret:...}`), and name it in `ca:`. Connect by the name on their certificate. `ductus validate` and the engine's start refuse a `ca:` that names a pair, a `certificate:` that names a CA, or a name that isn't there.

### A refused handshake is a temporary error: the message waits (level 2)
- **What:** when TLS fails (the receiver's certificate is from a CA we don't trust, doesn't cover the host, has expired; the receiver wants a client certificate we didn't present, or refuses ours), the delivery is a temporary failure: it retries with the usual back-off, nothing is quarantined, and the destination's error says why in words ("TLS: the receiver wants a client certificate (name one in `tls.certificate`)"). With TLS 1.3 the receiver judges our client certificate after our side has finished the handshake, so the refusal arrives as the answer to the first message: that message was not delivered, and is sent again.
- **What to do:** fix the configuration (or ask the receiver to); the waiting messages go as soon as the handshake works, in order. `tls:` pointed at a port that speaks plain MLLP gets no handshake and times out ("timed out waiting for ACK"): check with the receiver which port is TLS.

### Our client certificate: tell the receiver what to expect (level 2)
- **What:** `certificate:` (MLLP: `tls: { certificate: ... }`) presents a `certificates:` pair as our client certificate (mutual TLS). The receiver checks it against the CA it trusts for clients and often against its subject. A pair from files renews like ours (replace both files; new connections present the new one within a minute, no restart), as a listener's does; the mail server's connection picks it up at the next mail.
- **What to do:** give the receiver's team the certificate's subject and the CA that issued it, and renew it before it expires (the receiver refuses an expired one, and every message waits). One pair can serve as both a listener's certificate and a client certificate if its CA allows both uses.

### An HTTP destination through an API uses the API's TLS (level 2)
- **What:** `ca:` and `certificate:` go on the `apis:` entry, which `api()` calls and `http: { api: ... }` destinations share. On an HTTP destination they are only for one with its own `url:` (https://): `validate` refuses them next to `api:` or on an http:// URL.
- **What to do:** put TLS where the URL is.

### Doctor makes the engine's handshake (level 3)
- **What:** `ductus doctor`, the scheduled checks and the Connections page build each TLS connection from the same settings the engine uses (the named CA, the client certificate), so a check passes when the engine's connection would. After a TLS 1.3 handshake it waits up to half a second for the server to refuse the client certificate (TLS 1.3 says so only then; TLS 1.2 says so inside the handshake, so there's no wait), and says which. A certificate named in `ca:` or `certificate:` that doesn't load fails the checks that use it, with why, instead of stopping the run.
- **What to do:** after changing TLS settings, run `ductus doctor --interface <name>` before deploying; the MLLP check connects and shakes hands but sends nothing.

## TLS to Postgres

### Without `sslmode`, the database connection is TLS when offered, and nobody is checked (level 3)
- **What:** `DUCTUS_DATABASE_URL` means what it means in psql: without `sslmode` it is libpq's `prefer`, TLS when the server offers it (Ubuntu's Postgres and most packages do, with a self-signed certificate), plain when it doesn't, and the certificate is never checked. `require` insists on TLS and checks nothing either. Only `verify-full` checks that a trusted CA issued the server's certificate and that it is for the host connected to (`verify-ca`: the CA, not the name). resources.yaml `postgres` entries work the same with `tls:` (default `prefer`; `verify-full` when `ca:` names a CA). The start log, `ductus validate`, doctor and the Connections page say which ("TLS 1.3, not verified (sslmode=prefer)", "TLS 1.3, verified (sslmode=verify-full)", "plain, no TLS"), and warn when the database is on another machine and not verified.
- **Why:** a URL that works with psql must work here, and `verify-full` by default would refuse every install whose Postgres has its own self-signed certificate. But TLS that checks nothing stops only someone listening: anyone who can answer at the database's address (a DNS entry, a spoofed address on the way) gets the password and every query.
- **What to do:** for a database on another machine, `?sslmode=verify-full`, plus `&sslrootcert=/path/ca.pem` when this server doesn't already trust its CA (`sslrootcert=system`: the system's alone); in resources.yaml `tls: verify-full` and `ca:` (a `certificates:` entry). `ductus validate` says "verified" when it works. For one on this machine, a Unix socket (`postgres:///ductus?host=/var/run/postgresql`) needs no TLS and is the fastest.

### Cloud Postgres requires TLS (level 1)
- **What:** Azure Database for PostgreSQL and Amazon RDS are usually set to refuse plain connections. Azure's certificates chain to public roots this server already trusts; RDS's are issued by Amazon's own RDS CAs, which it doesn't.
- **What to do:** `sslmode=verify-full`; for RDS add `sslrootcert=` with Amazon's RDS CA bundle (downloaded from AWS) for the region. Connect by the endpoint's DNS name, the one on its certificate.

### A CA installed on this server counts (level 2)
- **What:** the database's certificate is checked against this server's trusted certificates (Linux: `update-ca-certificates`; Windows: the machine's store) plus `sslrootcert` (or `ca:`), as MLLP and HTTPS connections are. sqlx reads the system's certificates at every new connection (about 10 ms with `verify-full`), so a CA installed while the engine runs counts from the next one.
- **What to do:** install a hospital CA once on the server, or name it per connection; both work.

### The database's certificate is watched when the engine checks it (level 3)
- **What:** a Postgres connection with `verify-full` or `verify-ca` (the engine's own database or a resources.yaml entry) is in the engine's certificate checks: the 8-byte SSLRequest, then the handshake the engine would make, its dates kept and alerted at 30, 14 and 7 days, as "engine database" or the entry's name. Its expiry would stop the engine connecting. With `prefer` or `require`, doctor shows the certificate (and whether `verify-full` would accept it) but doesn't watch it: an expired one changes nothing for a connection that checks nothing.
- **What to do:** switch to `verify-full`, and it's watched.

### A client certificate renews at the next connection (level 3)
- **What:** `certificate:` on a Postgres entry logs in with a `certificates:` pair (pg_hba's `cert` method: no password; or with a password and `clientcert=verify-full`). Renewed from its files, the pool's next connection presents it; open ones keep theirs until they close (sqlx keeps a connection up to 30 minutes, an idle one 10). The engine's own database reads `sslcert=` and `sslkey=` at every new connection.
- **What to do:** renew before expiry: it counts down with "Ours" on the Connections page. Ask the DBA which CA they trust for clients and which name (the certificate's CN) maps to the database role.

### Opening a database connection over TLS costs ~60 ms, for now (level 4)
- **What:** sqlx 0.9.0 doesn't set `TCP_NODELAY`, and a TLS handshake followed by the login waits on the server's delayed ACKs: opening a connection took 64 ms (`prefer`) and 79 ms (`verify-full`) against 5 ms plain, on a local Postgres. Pools keep their connections, so messages rarely see it; a start does: 1,001 interfaces were ready in 0.91–1.06 s over TLS against 0.52–0.66 s plain. Strict ingest with 1 sender is within noise; with 16 senders it was ~7% lower (2,636 against 2,831 ACKs/s), ~2% with `TCP_NODELAY` set. Fixed in sqlx (transact-rs/sqlx#4336, after 0.9.0): with it, opening takes 8–11 ms (22 ms `verify-full`) and the start 0.63–0.72 s.
- **What to do:** nothing in most installs; the next sqlx release removes it. If start time or pool growth matters for a database on this machine, use a Unix socket. Don't give up verification for it.

## TLS to SQL Server

### SQL Server connections are verified, and `trust_server_certificate` is for labs (level 3)
- **What:** every SQL Server connection is TLS for the whole session, and the engine checks the server's certificate against this server's trusted certificates plus `ca:` (a `certificates:` entry), for the name in `host`. `trust_server_certificate: true` accepts any certificate: anyone who can answer at the server's address (a DNS entry, a spoofed address) gets the login and every query. The start log and `ductus validate` warn about it for a server on another machine, doctor says "not verified (trust_server_certificate)" with what verifying would make of the certificate, and the Connections page shows the lock open. It's refused next to `ca:` or `name_in_certificate:` (they'd never be checked). There's no `encrypt: false`: SQL Server would then encrypt only the login and send queries and results readable.
- **Why:** many SQL Servers present the certificate SQL Server generates for itself when none is configured, which nobody can verify: it's self-signed, an old X.509 v1 certificate (refused as such: "an old X.509 v1 certificate, which can't be verified"), and its name is always `SSL_Self_Signed_Fallback`, whatever the server is called (seen on 2022 and 2025). So drivers' "trust the server certificate" switch became a habit.
- **What to do:** ask the DBA for a certificate from your CA for the name you connect by (SQL Server Configuration Manager on Windows; mssql-conf `network.tlscert` and `network.tlskey` on Linux), name the CA in `ca:` unless this server already trusts it, and drop `trust_server_certificate`. Refusals say what's wrong: "not trusted: its issuer isn't one this server trusts", "the certificate isn't for 10.1.2.3", "has expired".

### Connecting by a name the certificate doesn't list: `name_in_certificate` (level 3)
- **What:** the certificate must be for the name in `host`. An availability group listener, a DNS alias or an IP address often isn't on it. `name_in_certificate: sql1.hospital.org` (ADO.NET's HostNameInCertificate) checks the certificate for that name instead, while connecting to `host`.
- **What to do:** use it rather than `trust_server_certificate`: the certificate is still checked, only for another of the server's names. Better still, ask for the listener's name on the certificate.

### `encrypt: strict` is TDS 8: TLS before anything (level 3)
- **What:** by default (`mandatory`), as every SQL Server client, the engine first exchanges PRELOGIN packets in the clear, then makes the TLS handshake inside them; this always negotiates TLS 1.2 (TDS 7 can't carry TLS 1.3). `encrypt: strict` makes the handshake first, asking for `tds/8.0`, with TLS 1.3 when the server allows it. Servers set to force strict accept nothing else. SQL Server accepts strict only with a certificate configured for its own host name, never with its generated one, and **SQL Server 2022 on Linux refuses strict whatever it's given** (tested with CU27); SQL Server 2025 on Linux accepts it. ADO.NET ignores `TrustServerCertificate` with strict and always verifies; here `trust_server_certificate` still turns verification off (for labs), warned about as without strict.
- **What to do:** leave `encrypt` out unless the DBA forces strict; then set `encrypt: strict`. A server that doesn't accept strict closes the connection during the handshake, and the error says to leave `encrypt: strict` out.

### SQL Server 2022 on Linux doesn't accept `encrypt: strict` (level 3)
- **What:** tested against SQL Server 2022 on Linux (CU27, Microsoft's container image): it refuses every TDS 8 strict connection with "A valid TLS certificate is not configured to accept strict (TDS 8.0 and above) connections", whatever it's given: its generated certificate, a CA's certificate for its own host name (CN and subjectAltName, server authentication, key encipherment), TLS 1.2 and 1.3 or 1.3 alone. SQL Server 2025 on Linux (CU10) accepts strict with the same certificate. SQL Server 2022 on Windows isn't tested here (Microsoft documents strict for it). The default (`mandatory`: TLS after PRELOGIN, verified) works with both.
- **Why:** not documented: Microsoft's page on encrypted connections for SQL Server on Linux doesn't mention strict for any version. Measured, not explained.
- **What to do:** against SQL Server 2022 on Linux, leave `encrypt` out: the connection is still TLS throughout and verified. Use `encrypt: strict` with 2025, or on Windows once tried there. The engine's own tests run against 2025 for this reason.

### SQL Server's certificate is read through PRELOGIN, and watched when verified (level 3)
- **What:** doctor and the engine's own check read a SQL Server's certificate as the engine's connection meets it: a PRELOGIN packet asking for encryption (its answer gives the server's version), then the handshake inside PRELOGIN packets (with `encrypt: strict`, plain TLS). Nothing logs in, in the scheduled check. A verified connection's certificate is kept and alerted at 30, 14 and 7 days under the entry's name; one with `trust_server_certificate` is described, not watched (an expired certificate doesn't stop a connection that checks nothing). A named instance without `port:` is found through the SQL Server Browser first, and its certificate is kept under the port it answered with. The check takes ~35 ms through PRELOGIN (TLS 1.2), ~0.55 s with strict (TLS 1.3: see "Doctor makes the engine's handshake").
- **What to do:** verify the connection, and the certificate is watched. A SQL Server reads its certificate at start: after the DBA renews it, the server's restart is what makes it count (and "Check now" shows it).

### Verification costs nothing measurable (level 4)
- **What:** opening a SQL Server connection (PRELOGIN, TLS, login, a first query) took ~120–140 ms in every mode against a local SQL Server 2025: `trust_server_certificate`, verified, and strict verified (`scripts/bench-mssqltls.sh`; differences within the noise between rounds). Pools keep their connections (closed after 5 idle minutes), so messages rarely wait for one.
- **What to do:** verify; there's no speed to gain by not.

## SQL Server Windows logins

### `auth: windows`: the engine's own account, or a named one (level 1)
- **What:** `auth: windows` with no `user` logs in as the account the engine runs as (integrated, like `Integrated Security=true`); with `user: 'HOSPITAL\svc-lab'` and its `password` it logs in as that account. `auth: sql` (the default) is a SQL login. The account the engine runs as exists only on Windows: on Linux, `auth: windows` without a `user` is refused when resources.yaml loads.
- **Why:** a SQL Server that allows only Windows logins (many hospital DBAs set it that way) needs one of the two; integrated keeps a password out of the configuration altogether.
- **What to do:** on Windows prefer integrated, with the service running as a gMSA (no password anywhere). Ask the DBA for a login for that account: `CREATE LOGIN [HOSPITAL\svc-ductus$] FROM WINDOWS`, then a user in the database.

### The service's own account is the computer on the network (level 3)
- **What:** the Windows service runs as `NT SERVICE\Ductus` by default. On the network that account is the computer's: `HOSPITAL\SERVER01$`. An integrated login from the default service is therefore a login for `HOSPITAL\SERVER01$`, and anything else on that server running as a service account gets the same identity.
- **Why:** a virtual service account has no network identity of its own; Windows uses the machine account for it.
- **What to do:** for a dedicated identity, run the service as a gMSA (`HOSPITAL\svc-ductus$`); the start log and doctor say which account SQL Server saw ("SQL Server logged us in as HOSPITAL\SERVER01$").

### Kerberos or NTLM: doctor and the start log say which (level 3)
- **What:** on Windows the login asks for Kerberos and falls back to NTLM by itself (SSPI's Negotiate, as ODBC and ADO.NET do). Doctor's login step and the start log (once per SQL Server, for Windows logins) say who the server took the login for and how: "(Windows, Kerberos)" or "(Windows, NTLM: no Kerberos, most likely no SPN for MSSQLSvc/sql1.hospital.org:1433)". NTLM to another machine is a warning in doctor; NTLM to a SQL Server on the same machine is usual and passes.
- **Why:** Kerberos needs the SQL Server's SPN (`MSSQLSvc/host:port`, or `:INSTANCE`) registered for the account SQL Server runs as, and the engine connecting by that name (not an IP address or an alias). Without it the login still works, as NTLM, and nothing else tells you.
- **What to do:** if doctor says NTLM, ask the DBA to register the SPN (`setspn -S MSSQLSvc/sql1.hospital.org:1433 HOSPITAL\sqlsvc`) and connect by the name in it. Kerberos itself has not been tested against a domain yet (no domain in our test setups): the first site with one confirms it.

### A refused Windows login: 18452 is Windows saying no, 18456 is SQL Server (level 2)
- **What:** when Windows doesn't accept the account, SQL Server answers error 18452, "the login is from an untrusted domain", whatever the cause: a wrong password, an account the server's Windows doesn't know, or NTLM blocked by the domain. When Windows accepts it but SQL Server has no login for it (or the login is disabled, or can't open its default database), the answer is 18456, "Login failed for user". The engine words both, and, as with a refused SQL login, messages wait (a temporary error) until it's fixed.
- **Why:** the server doesn't tell the client more; its own error log does.
- **What to do:** 18452: check the password secret for this environment and the account's name, then whether the domain restricts NTLM (or get Kerberos working: the SPN). 18456: ask the DBA for `CREATE LOGIN [DOMAIN\name] FROM WINDOWS` and a user in the database. A login given to a Windows group (SQL Server Express gives one to `BUILTIN\Users`, which every signed-in account is in) lets every member in: an account can get in with no login of its own.

### From Linux, a Windows login is always NTLM (level 3)
- **What:** on Linux, `auth: windows` needs a named account and logs in with NTLM, in pure Rust: never Kerberos. Validate and the start warn about it. Microsoft has deprecated NTLM (2024), many security teams audit it, and a domain that blocks NTLM refuses these logins (18452).
- **Why:** Kerberos from Linux needs a keytab and the system's GSSAPI library, which the binary would then need on every server whether it's used or not; it waits for the first site that needs it.
- **What to do:** from Linux prefer a SQL login, or run the engine on Windows for Windows logins. If you use NTLM, tell the security team, and expect it to stop working when the domain turns NTLM off.

### Extended Protection set to Required refuses NTLM logins (level 3)
- **What:** a SQL Server with Extended Protection for Authentication set to Required refuses an NTLM login that carries no channel binding, and the engine's carries none. Kerberos logins are not refused for it.
- **Why:** channel binding ties the login to the TLS connection; the library the engine uses doesn't send one yet (a possible later addition). Not tried here: our test server leaves Extended Protection off, as SQL Server does by default.
- **What to do:** use Kerberos (the SPN), Extended Protection set to Allowed, or a SQL login.

## Certificates

### The engine checks certificates by itself, and logs in to nothing (level 2)
- **What:** every `doctor.schedule` (6 h by default; the first a minute or two after start), the engine runs the connection checks in `certificates` mode: DNS, TCP and the TLS handshake of every connection with a certificate, nothing else. It keeps what it saw (the Connections page, `GET /api/v1/certificates`) and the run itself (the last 30). `mode: full` runs everything `ductus doctor` does, every database and SFTP login included, every few hours.
- **Why:** a full run logs in to every system four times a day. Hospital security teams notice service accounts that do that, and a password that changed locks the account out after a few runs.
- **What to do:** keep `certificates` unless someone asked for scheduled login checks; run full checks when you want them (the Connections page, `POST /api/v1/doctor`, `ductus doctor`). `doctor: { schedule: off }` turns the scheduled checks off (and with them the countdown's freshness: dates then come from the last check someone ran).

### What has a certificate the engine watches (level 3)
- **What:** HTTPS APIs (resources.yaml) and HTTPS destinations, MLLP destinations with `tls:`, the directory people sign in with (`ldap:` in resources.yaml, ldaps:// or StartTLS), Postgres with `verify-full` or `verify-ca` (the engine's own database and resources.yaml entries: see "The database's certificate is watched when the engine checks it"), SQL Server without `trust_server_certificate` (see "SQL Server's certificate is read through PRELOGIN"), and what alerts connect to: the mail server (`tls: starttls` or `tls`), HTTPS webhooks and the heartbeat. Not watched: databases the engine doesn't verify, SFTP (host keys, not certificates), MLLP destinations without `tls:` (plain TCP). Our own certificates are watched too, from what the engine loaded (see "Our certificates count down like the rest").
- **What to do:** verify every database on another machine, and its certificate is watched with it.

### The earliest expiry in the chain counts, in whole days (level 2)
- **What:** a server presents its certificate and usually one or more intermediates. Clients refuse the connection when any of them has expired, so the countdown runs to the earliest of them (the Connections page names an intermediate that ends first). Days are whole days in UTC dates: a certificate ending tomorrow at 00:30 UTC has 1 day left all of today, wherever the server is.
- **What to do:** when the intermediate is the one ending, the fix is the server's certificate bundle (the CA has a newer intermediate), not a new certificate.

### A certificate alert is told at 30, 14 and 7 days, and when it expires (level 2)
- **What:** with a rule naming `certificate` (`{ when: [certificate], severity: warning, to: [oncall] }`), a certificate alerts when it is 30 days from expiry, again at 14 and at 7 (escalated, even if someone acknowledged it), and as **critical** once expired, whatever the rule's severity. Each step is told once: no reminders before it expires (the steps are the reminders), the usual `repeat` after. Renewed past 30 days, it recovers (after `settle`); renewed with a certificate that is itself short, it moves to that step quietly. `alerts.certificates.warn_days` changes the steps. Without a rule naming `certificate`, nothing is sent (`ductus validate` says so) and the Connections page still shows them.
- **What to do:** send `certificate` to the people who renew certificates, not the on-call pager; warnings respect quiet hours, an expired one doesn't. `to: [owner]` reaches a destination's owner for a destination with its own URL, and a resources.yaml entry's `owner:` (an API, the directory) for its certificate; entries without one go only to the rule's channels.

### A check that can't connect keeps the last dates (level 2)
- **What:** a certificate is read even when this server doesn't trust it (a private CA, self-signed): the Connections page says "not trusted by this server", and the dates count all the same. When a check can't connect at all, the row shows the error and keeps the dates the last successful check read ("dates from ..."); the countdown and alerts go on from those. A server never reached shows "Not read yet" and can't alert.
- **What to do:** a certificate stuck on "Not read yet", or with an old "dates from", needs the connection fixed first (the same row's error says why).

### Behind a load balancer, you see the balancer's certificate (level 3)
- **What:** the check reads whatever certificate answers at the configured name and port: a load balancer or proxy that ends TLS presents its own, not the servers' behind it. If the balancer's backends present different certificates, each check may see another one; the alert follows what it sees (renewed, then not, is one alert held open by `settle`, but a backend left on an old certificate shows again at the next check).
- **What to do:** renew every backend, and ask the balancer's owner which certificate clients actually get.

### Our certificates count down like the rest, with no check (level 2)
- **What:** every `certificates:` pair the engine presents, on a TLS listener (an interface's or the API's) or as a client certificate, is one row in the Connections page's "Ours" group and in `GET /api/v1/certificates` (`side: ours`, on this server at port 0, with where it is presented). It is read from what the engine loaded, never over the network, and alerts like the servers' (30, 14, 7 days, expired) to its `owner:` in resources.yaml, or, without one, to the owner of the one interface it serves. Its row in the database is written at the start and again only when a renewal is picked up or can't be used: nothing runs for it while nothing changes (the minute's file check reads the files, not the database).
- **What to do:** give each pair an `owner:` (the people who renew it), and a rule naming `certificate` to someone. After replacing the files, "Check now" (or any connection check) reads them at once instead of within the minute.

### A renewal that can't be used shows on its row, not as an alert (level 2)
- **What:** files that changed but don't load (a new certificate with the old key, half copied, expired) keep the pair in use, log one error, and show on the pair's row ("the new files can't be used: ..."). The countdown and its alerts go on from the pair still presented; nothing new is sent about the broken files themselves.
- **What to do:** after a renewal, look at the "Ours" group (or the log's `certificate renewed`): an error there means senders still get the old certificate, and it still expires when it did.

### Doctor reads the chain our listeners serve, but not the name senders use (level 3)
- **What:** in full mode (the Connections page's "Run the checks", `POST /doctor` with `mode: full`, a scheduled `mode: full`), the engine connects to each of its TLS listeners, over loopback when it listens on every address (`0.0.0.0`, `::`), takes the chain it serves whatever it is, and judges it the way senders build a path: complete when it ends at a root, or at a certificate issued by one this server trusts or a `certificates:` CA; a warning when it stops short ("only our certificate is served, not the one that issued it"), since Java and .NET senders don't fetch a missing intermediate. `ductus doctor` does the same for a listener whose port is in use (a running engine). Certificates-only runs skip it (it reads nothing new about expiry).
- **Why it can't say more:** it doesn't know the name senders connect with (it shows the names the certificate covers), nor which roots they trust: a chain ending at your private CA is complete only for senders that trust that CA. A warning about a chain that ends at your private root can be right: name that root as a `certificates:` CA and doctor knows it.
- **What to do:** fix a warning by appending the intermediate to `cert_file`; ask senders which name they connect with and check it is among the names shown.

### A refused certificate is said once, not logged as an error each time (level 4)
- **What:** the TLS verifier the engine uses logs every certificate it refuses at ERROR. The engine turns that log off by default (`rustls_platform_verifier=off`): the refusal is already in the check's result and in a failed delivery's error, and scheduled checks of a private-CA server would otherwise log an error every few hours.
- **What to do:** to see the verifier's own lines, set `RUST_LOG=info,sqlx=warn,rustls_platform_verifier=debug`.

## People and access

### Reading payloads is a permission, not part of a role (level 4)
- **What:** `GET /api/v1/messages/{id}` (and the message page) returns the payload, and the saved output of each destination, only to admins and to users and tokens that were given the `payloads` permission. Without it the message page still shows its status, destinations and errors, and says the payload is hidden. Editing a message needs the permission too (correcting a message means reading it). Each read is in the audit log (next entries). Existing tokens were revoked when users came in (migration 0018); a new token has no permissions unless it is given them.
- **What it doesn't hide:** the ordering key (often an MRN), the control id, error texts (a quarantine's error can quote a field value) and sizes are still shown to anyone who can read the interface. The permission protects the message content, not every identifier the engine has seen. Write error messages in interface code the way you would write a log line that a support person may read.
- **What to do:** give `payloads` to the people who fix data problems and to nobody else; a monitoring token needs none. `ductus user create ... --permission payloads`, `ductus token create ... --permission payloads`.

### Passwords, lockout and what a disabled account loses (level 2)
- **What:** people sign in with a username (not case sensitive) and a password of at least 12 characters (`auth.password.min_length`; never below 8) that isn't the username, one character repeated, or one of a short list of common ones. Passwords are stored as argon2id hashes. Five wrong passwords for one username within 15 minutes lock that account for 15 minutes (`auth.lockout`); the right password doesn't help meanwhile, and an admin can unlock at once. A wrong username and a wrong password give the same answer in about the same time.
- **Why lock by username:** per username, not per address, because sign-ins arrive through a proxy. The price is that someone who knows a username can keep that person locked out; an admin unlocks, and locking was the safer side of that choice for patient data.
- **Disabling** a user ends their sessions at once and stops every token of theirs (their scripts get 401). A password an admin chose (a new account, a reset) is temporary: the person's first sign-in can do nothing but change it. There is always at least one admin who can sign in: the last one can't be disabled or demoted.
- **What to do:** make accounts for people, tokens for scripts; let people make their own tokens under Account (they stop with their owner). Turn `auth.token_sign_in` off once everyone has an account, so a leaked token can't also open the UI. If you lock yourself out, `ductus user` on the server (it works on the database directly) makes or unlocks an admin.

### The audit log records who did what, and what it leaves out (level 2)
- **What:** `GET /api/v1/audit` (the Audit page, `ductus audit export`) lists, newest first: sign-ins, failed sign-ins (and whether they locked the account), sign-outs, password changes and resets, user and token changes, payload reads, resends, replays, edits and dismissals of messages, pauses and resumes, drains, retention changes and confirmations, connection checks, exports of the log itself, and the engine's own starts, stops and stopped interfaces. Each row has who (a user, a token, `system` for the engine or someone at the server's command line, `anonymous` for a sign-in as a name nobody has), how (web UI, token, command line), the address, what it was done to, and a reason or details. Reading it needs the `audit` permission (admins have it); anyone signed in with a password sees their own activity under Account.
- **What it leaves out:** passwords, tokens and payloads are never in it. A failed sign-in for a name nobody has keeps no name: what people type in the username box is sometimes their password. Messages read through the database directly (psql) or by the engine's own code aren't API calls, so they aren't rows: the log covers the API, the web UI and the commands of `ductus`.
- **The address** is the connection's. Behind a TLS proxy (which the UI needs over a network) it's the proxy's address for everyone; the audit log can still say who, just not from where. A forwarded-for header is not trusted yet.
- **What to do:** give `audit` to the compliance or security person, not to operators. Ship the log to your SIEM with `ductus audit export --since <day>` (JSON lines, oldest first; the export is itself a row).

### Payload reads are logged before they are shown (level 3)
- **What:** a read of a payload (`GET /messages/{id}` by someone with `payloads`) writes its audit row first. If the database can't take the row, the answer is 503 and no payload is returned: an unlogged read of patient data is worse than a refused one. The same message read again by the same person within `audit.payload_read_window` (10 minutes) is **one row**: a screen that reloads itself (the UI refetches when the window regains focus) would otherwise write a row every few seconds and bury the log. `0s` logs every read. `audit.payload_reads: false` turns the rows off (the permission still applies).
- **What to do:** read "viewed message N between these times" as "at least once in a window", not as a count of looks. If you need every read, set the window to `0s` and size the log for it (a row is about 300 bytes).

### Dismissing a quarantined message is not deleting it (level 2)
- **What:** `POST /messages/{id}/dismiss` and `/messages/dismiss` (the Quarantine screen's "Dismiss selected") close a quarantined delivery nobody will fix or resend. The delivery becomes **filtered** with "dismissed by NAME: reason", leaves the quarantine, and nothing is sent; the message is settled again (sent, if another destination sent it). A **reason is required**. Only quarantined deliveries are touched: a pending one is being retried, a sent one isn't a problem. The message, its payload (while its retention lasts) and the dismissal stay visible on its page, and it can still be **replayed** later. In the held area a dismissal is how a held quarantine ends: the month's table drops once nothing in it is unfinished.
- **What to do:** write the reason for the next person ("lab retired the unit; these are the 37 results for it"), not for yourself. A dismissal is a decision about a patient's data not reaching a system; the audit log keeps who made it.

### Copies of payloads and the audit log are kept for different times (level 3)
- **What:** every resend, edit and dismissal keeps what the delivery was before, including the output it had sent (`message_actions.previous_output`, for destinations with `save_output: true`). That is a copy of patient data, so it goes when the message's own retention ends: an hourly job sets it to NULL for actions older than the interface's retention (1, 3 or 7 weeks; an interface that is no longer configured gets 7); the row (who, when, what it was) stays. The rows themselves, and the audit log, are deleted after `audit.keep`: **six years** unless the project says otherwise (at least one year; ask your compliance office what your organization needs). At about a thousand rows a day that is some two million rows over six years.
- **What to do:** set `audit.keep` in `ductus.yaml` to your documented retention period, then leave it; shortening it deletes history at the next hourly run. The engine's log shows what the job cleared and deleted.

### A crash shows in the audit log (level 3)
- **What:** each start writes `engine.start` (version, host, and how the previous run on this host ended); each clean stop writes `engine.stop` with the same instance. A start whose predecessor has no stop was killed, crashed or lost power; the new start's row says `previous: unclean` and the engine logs a warning. A stop that had to give up waiting (30 s) is written `forced`. With two engines on one database, each host is judged by its own rows.
- **What to do:** after an unclean start, nothing is lost (messages in hand are processed again, as the at-least-once rule says), but look at the receivers for duplicates around that time.

### Signing in with the directory: local names first, groups decide the role (level 2)
- **What:** with `auth.ldap` in ductus.yaml (and its directory in resources.yaml), people sign in on the same page with their directory username and password. A name a **local** account holds is checked here and only here, never against the directory: a directory account can't take over the break-glass admin's name, and a directory person whose name a local account already has can't sign in until one of them is renamed. Any other name goes to the directory: the service account finds the person (`user_filter`, with what they typed escaped, so `*` or `)(` match nothing), the engine binds as them with their password, then reads their groups. The highest role group they are in wins; `permissions` groups only add to a role; in no role group, they are told they have no access (the password was right, so that tells nobody anything new). Their account is made at the first sign-in and shows as a directory account; role, permissions, name and email are the directory's at every sign-in, read-only here. An admin can still disable one here: then it can't sign in, whatever the directory says.
- **What to do:** make Ductus groups in the directory (admins, operators, viewers, and PHI readers for `payloads`), not one per person. Keep one **local admin** (`ductus user create`) for when the directory is down, with a long password kept where the on-call can get it. Don't map a role to a group everyone is in (Active Directory's primary group, Domain Users, isn't in `member` and wouldn't match anyway).

### Write several group DNs one per line, not `[a, b]` (level 2)
- **What:** a group takes one DN or a list. In YAML's flow style (`payloads: [CN=PHI,OU=Groups,DC=h, CN=Night,OU=Groups,DC=h]`) every comma separates items, the commas inside each DN too, so the list becomes `CN=PHI`, `OU=Groups`, `DC=h`, ...: pieces that each look like a DN and match no group. The engine refuses to start (and `ductus validate` says) when a group "DN" has only one part, naming the piece.
- **What to do:** write lists in block style, one DN per line (`- CN=PHI,OU=Groups,DC=h`); a single DN can stay on the key's line.

### A person removed from the groups loses access within `refresh` (level 2)
- **What:** every `auth.ldap.refresh` (15 minutes by default, at least 1 minute) the engine reads again the groups and account of every directory person with a live session or token. Removed from every role group, disabled in Active Directory (`userAccountControl`), or deleted: their sessions end, their tokens stop, the account shows "Refused by the directory", and the audit log has a `user.directory_refused` row by the engine. Changed groups change the role and permissions the same way (`user.directory_change`). Put back, they sign in again (and their tokens work again at the next refresh). Between two refreshes, a session keeps the role it had: a removal takes up to `refresh`.
- **What to do:** for someone who must lose access now, disable the account on the Users page (at once), then remove them in the directory. A shorter `refresh` costs one search per person per group each time: 5 minutes is fine for a few hundred people.

### When the directory is down (level 2)
- **What:** a directory person's sign-in answers 503 "the directory can't be reached right now" (not "wrong password": they would retype it and lock themselves out). Local accounts keep working. Sessions already open **keep working**: a refresh that can't reach the directory changes nothing (an outage must not sign the whole hospital out), so an outage also delays a removal until the directory answers again. A wrong service account password (`bind_password`) looks the same as an outage: the engine logs it as an error, and `ductus doctor` (full) says which.
- **What to do:** watch the directory's certificate like the others (it is in the Connections table) and keep the break-glass admin. Point `url` at a name that fails over (a domain's DNS name that resolves to several controllers), not at one controller.

### Wrong passwords count twice: here and in the directory (level 3)
- **What:** a wrong directory password counts toward Ductus's lockout for that username (5 in 15 minutes, `auth.lockout`) once the person has signed in at least once (before that there is no account here to count on), **and** the directory counts it toward its own lockout policy. Active Directory answers a disabled, locked or expired account like a wrong password; the person sees "wrong username or password" either way.
- **What to do:** set Ductus's lockout at or below the directory's, so Ductus refuses first and a typo storm doesn't lock the person out of everything else they use. A person locked here is unlocked on the Users page; locked in the directory, by the directory's admins.

### Nested groups: Active Directory answers, other directories are walked (level 4)
- **What:** `nested: true` counts members of groups inside the configured groups. On **Active Directory** (seen in its root entry) one search per configured group uses its transitive-membership rule (`member:1.2.840.113556.1.4.1941:=`). Other directories (OpenLDAP) have no such rule: the engine walks up from the person's groups, at most 8 levels, searching `group_base` (default: the directory's naming context) for groups that contain each one. Without `nested`, only direct members count (`member`, or `uniqueMember` for groupOfUniqueNames).
- **Not tested here:** real Active Directory isn't available to the tests (OpenLDAP is); the transitive filter is unit-tested and is verified with the first pilot.
- **What to do:** prefer direct membership of the Ductus groups: it is one search per group and easy to audit. With deep or wide nesting on OpenLDAP, set `group_base` to the groups' OU so the walk searches less.

### The directory connection is TLS, verified like the rest (level 3)
- **What:** `url: ldaps://...` (TLS from the start, port 636) or `ldap://...` with `starttls: true`. The certificate is checked against this server's trusted certificates, the same as HTTPS; a directory signed by the hospital's own CA that this server doesn't trust is refused unless `ca:` names that CA (a `certificates:` entry: `hospital-ca: { ca: ${secret:HOSPITAL_CA} }` or `ca_file:`), which is then trusted for this directory only. Plain `ldap://` is refused unless `insecure: true`, which `ductus validate` and the engine warn about: people's passwords would cross the network readable.
- **What to do:** use ldaps:// or StartTLS everywhere but a test directory. On Windows servers joined to the domain the CA is usually installed already; on Linux servers it usually isn't (`ca:`).

### Theme and colors follow the account (level 1)
- **What:** a signed-in user's theme and custom colors are saved on the server and applied in any browser they sign in to. A copy stays in the browser so the right theme shows before the page has loaded; signing out clears it, so the next person starts from the default. A session made with a token has no account: its choice stays in that browser.
- **What to do:** nothing; a person who had picked colors before accounts existed has them sent to their account the first time they sign in.

## Local development

### `ductus dev` is a development tool, not a way to run in production (level 1)
- **What:** `ductus dev` starts a Postgres of its own for the project (data in `.ductus/postgres`, one engine at a time per project) and runs the engine against it. It ignores `DUCTUS_DATABASE_URL`, uses default Postgres settings, and nobody backs the data up. Postgres is downloaded once per user (about 30 MB).
- **Why:** to try an interface without asking anyone for a database. Production needs the DBA's Postgres (backups, monitoring, failover), and a message the engine has acknowledged must survive what the DBA protects against.
- **What to do:** use `ductus dev` on your own machine; everywhere else set `DUCTUS_DATABASE_URL` and use `ductus run`. Without a download possible, unpack the Postgres binaries yourself and set `DUCTUS_POSTGRES_DIR`. `ductus dev --reset` throws the dev data away.

## Windows

### The Windows service can only touch what you give it (level 2)
- **What:** the service runs as the virtual account `NT SERVICE\Ductus`, which can read `C:\ProgramData\Ductus` (settings, project) and write its logs there, and nothing else. A file or SFTP interface that moves, deletes or writes files outside it (an inbox on `D:\Interfaces`, an archive folder, an output share) fails with "access denied" until you allow it. Settings (`ductus.env`) hold the database password, so only administrators, SYSTEM and the service can read that folder.
- **Why:** a service that can write anywhere turns one bad interface into a problem for the whole server.
- **What to do:** grant the service the folders interfaces use, as narrowly as you can: `icacls "D:\Interfaces" /grant "NT SERVICE\Ductus:(OI)(CI)M"`. For a network share, grant the computer account (`DOMAIN\SERVER$`), since a virtual account authenticates as the machine. `ductus doctor` in a terminal runs as you, not as the service, so it can pass where the service fails: the service's log (`C:\ProgramData\Ductus\logs`) shows the "access denied".

### A refused connection takes about a second on Windows, and `localhost` may be `::1` first (level 3)
- **What:** when nothing listens at an address, Linux says "refused" at once; Windows sends the connection request again, half a second apart, before it says so (~2 s by default). The engine's MLLP destinations, SQL Server connections and doctor's checks ask Windows for two retries instead: refused in ~1 s, and a request lost on the way is still sent twice more (given up after ~7 s). Other connections (HTTP destinations, `fetch()`, `api()`, SFTP, LDAP, mail) keep Windows's ~2 s. A name with several addresses is tried one address at a time, and `localhost` is `::1` before `127.0.0.1`: a receiver listening on `127.0.0.1` only costs that second at every new MLLP connection (once per reconnect, since connections stay open).
- **Why:** Windows treats a refusal like a lost packet (a busy server may refuse and accept a moment later). No retries at all would make one lost packet a failed connection; measured on Windows 11: refused 2.03 s by default, ~1 s with two retries.
- **What to do:** name the receiver by the address it listens on (`127.0.0.1`, not `localhost`, for an IPv4-only receiver on the same server). Keep an MLLP destination's `timeout_ms` above ~2 s on Windows so a reconnect fits in it. Doctor tells a refused port ("refused: ask its owner whether it's up") from dropped traffic ("no answer: a firewall") as on Linux.

### Certificate files: write the drive (level 1)
- **What:** in resources.yaml, `cert_file`, `key_file` and `ca_file` with a drive or a share (`C:\certs\mllp.pem`, `\\vault\certs\mllp.key`) are read as written; anything else is relative to the project folder. A path written for Linux (`/etc/ductus/mllp.key`) has no drive, so Windows reads it from the project's drive (`C:\etc\ductus\mllp.key`): `ductus validate` warns and says where.
- **Why:** that is what a path without a drive means on Windows.
- **What to do:** write the whole path, or keep the files in the project and write them relative to it. In YAML, put Windows paths in single quotes (`'C:\certs\mllp.pem'`) or none: in double quotes a backslash starts an escape. The service account must be able to read them (above).
