# Nuances: order, retries and durability

What could surprise you here, why it is so, and what to do. Each entry has a level, 1 (day one) to 5 (expert): the [index](index.md) lists them all by level.

## Order is per key, and one stuck message holds up its key (level 3)
- **What:** messages with the same ordering key (e.g. MRN) are processed strictly in arrival order. A message that keeps failing temporarily holds up **every lane of its worker** for that destination, not just its own key, until it succeeds or is quarantined: a shared interface has one worker per destination, so one failing message holds up the whole destination; a dedicated one with `workers: 4` holds up about a quarter of the patients. With no `ordering.key`, the whole interface is one lane.
- **Why:** that's the price of ordering: patient B's update must not overtake patient A's. A worker handles its lanes one message at a time, in arrival order; it doesn't skip ahead past a message it's retrying.
- **What to do:** choose a key that's as narrow as correctness allows. Set `retry.max_attempts` on destinations where waiting forever is worse than quarantining. When a destination is stalled, `ductus hotspots <interface>` names the message, its key and its worker (see Monitoring).

## After a change of `ordering`, new messages of a key wait for its older ones (level 4)
- **What:** you can change `ordering.partitions` or `ordering.key` (add it, change it, remove it) whenever you need to, with messages waiting or not, by a restart, a handover, a release or a rollback. New messages are stored and acknowledged in the **new lanes** at once; the messages waiting in the **old lanes** are sent first; and a new lane that may hold the same keys as an old lane with messages waits (is **held**) for that destination until those are sent. So nothing is refused or held at the sender, nothing is sent twice, and no patient's messages pass each other. Once nothing waits in the old lanes, they're gone (`interface.lanes_settled` in the audit log, "lanes settled" in the log).
  - **Which new lanes wait:** only those sharing keys with old ones. From 64 to 32 lanes, new lane 8 waits for old lanes 8 and 40 only; from 8 to 64, new lane 13 waits for old lane 5. A changed or removed key shares nothing that can be worked out, so every new lane waits for every old one. In the shared pool (one worker per destination) nothing is held at all: a worker takes its lanes' messages oldest first, old and new together.
  - **One stuck message** in an old lane (a receiver refusing it, retried forever by default) holds the new lanes sharing its keys until it's sent, quarantined or dismissed: for a lane count change, about what its worker held before the change anyway; for a key change, every new message to that destination.
  - **Lane numbers:** new lanes are numbered after the old ones (64 lanes, then 32 more from 64), so the stored numbers no longer start at 0. `ductus hotspots`, the Hotspots page and the API name them as configured: "lane 8", "old lane 40".
  - **`workers` alone:** nothing to wait for (every lane still has one owner).
- **Why:** a message's lane is worked out once, from its key and the lane count, when it arrives, and stored with it. Moving the backlog to the new lanes would mean rewriting every waiting row before the engine could start; keeping the old lanes and reading them first costs nothing while nothing changes and nothing but the hold while it does. How long the old lanes take to empty is how long their backlog takes to send.
- **What to do:** change it when you need to. `ductus validate` with `DUCTUS_DATABASE_URL` set says what the next start will do (`DUC311`: "adt: lanes go from 64 to 32: the 412 messages waiting are sent first, …"). While it settles, the interface page says "changing from 64 to 32 lanes"; if a destination stays behind, `ductus hotspots <interface>` shows the held lanes ("waits for old lanes 8, 40") and the old message holding them. To give each of N workers 64 lanes, set `partitions: 64 * N`.

## A handover that fails after storing messages in new lanes leaves them for the next start (level 5)
- **What:** in a handover that changes `ordering`, the new engine receives beside the running one and stores what it receives in its new lanes. If the handover then fails (the new engine can't take over and the running one goes on), the running engine removes the new lanes again, unless messages were stored in them: those are safe in the database, but the running engine doesn't read their lanes, so they wait until the next start or handover, which sends them. The handover's answer says so ("the new engine stored messages in lanes this one doesn't read"), `ductus hotspots` shows them as "stored under another `ordering`", and meanwhile a message resent for that interface goes there too. For a patient with messages on both sides, the ones the running engine received after the failed handover can be sent before the ones the failed engine received.
- **Why:** an engine reads the lanes that were listed when it started; the failed engine's lanes were added after.
- **What to do:** after a handover fails, fix what it said and hand over again soon (or restart): the next engine sends what waits there. A handover that fails before it receives anything (a project that doesn't load, a port taken) leaves nothing behind.

## Without an ordering key, more workers don't help (level 3)
- **What:** an interface with no `ordering.key` is one lane, and a lane has exactly one owner. `isolation: dedicated` with `workers: 8` still processes it one message at a time per destination (the other seven workers have no lanes).
- **Why:** with no key, the only safe order is the order of arrival for everything.
- **What to do:** scale in this order: (1) set an ordering key (64 lanes by default), (2) `isolation: dedicated` (own threads, 4 workers by default), (3) raise `workers` up to about the number of cores, (4) `dedicated_pools` if it does database lookups. Most interfaces need none of it: one destination of a shared interface drained ~1,500 messages/s on a laptop.

## Temporary failures retry forever by default (level 3)
- **What:** a destination that's down, or a transform that hits a temporary error, is retried with back-off (1 s up to 60 s) until it works. `retry.max_attempts` limits delivery attempts; transform retries (database or API down) have no limit.
- **What to do:** set `monitor: { behind: ... }` (see Monitoring) rather than expecting failures to show up as quarantined messages. A destination retrying for a minute with messages waiting shows as *stalled* with the error it gets.

## Database login failures make messages wait, not fail (level 3)
- **What:** a wrong password or a missing database counts as *temporary*: messages wait and retry instead of being quarantined.
- **Why:** it's a config problem; quarantining thousands of messages would make the fix much harder.

## `durability: relaxed` can lose the last fraction of a second (level 4)
- **What:** ACKs don't wait for the disk flush. If the *database server* crashes or loses power, the last moments of ACKed messages can be lost. An engine crash alone never loses anything.

## Respond-mode HTTP requests aren't stored (level 3)
- **What:** in `mode: respond`, the transform's result is the HTTP response; there's no queue and no stored message. If something fails, the caller gets an error status and decides whether to retry.
