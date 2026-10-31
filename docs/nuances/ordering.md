# Nuances: order, retries and durability

What could surprise you here, why it is so, and what to do. Each entry has a level, 1 (day one) to 5 (expert): the [index](index.md) lists them all by level.

## Order is per key, and one stuck message holds up its key (level 3)
- **What:** messages with the same ordering key (e.g. MRN) are processed strictly in arrival order. A message that keeps failing temporarily holds up **every lane of its worker** for that destination, not just its own key, until it succeeds or is quarantined: a shared interface has one worker per destination, so one failing message holds up the whole destination; a dedicated one with `workers: 4` holds up about a quarter of the patients. With no `ordering.key`, the whole interface is one lane.
- **Why:** that's the price of ordering: patient B's update must not overtake patient A's. A worker handles its lanes one message at a time, in arrival order; it doesn't skip ahead past a message it's retrying.
- **What to do:** choose a key that's as narrow as correctness allows. Set `retry.max_attempts` on destinations where waiting forever is worse than quarantining. When a destination is stalled, `ductus hotspots <interface>` names the message, its key and its worker (see Monitoring).

## Change `ordering.partitions` only with an empty queue (level 4)
- **What:** each message's lane is computed from its key and the interface's lanes when it arrives, and stored. The lanes are `partitions` with an `ordering.key`, and **one** without (removing the key is lowering them to 1). Change them while messages are waiting and:
  - **Lowered:** waiting messages in lanes at or above the new count would belong to no worker and never be sent. **The engine refuses to start** (exit 3), naming each destination, how many wait and in which lanes: "adt → lab: 412 messages waiting in lanes 32–63; adt now has 32 lanes (it had 64)". `ductus validate` with `DUCTUS_DATABASE_URL` set says the same before you deploy.
  - **Raised:** allowed, with a warning in the log and an `interface.partitions_raised` row in the audit log: a patient's waiting messages stay in their old lane while new ones go to another (maybe another worker), so until the backlog drains that patient's messages can be delivered out of order. Raising doesn't lose anything, so it isn't refused; whether that brief disorder matters is your call.
  - **`workers` alone:** safe (every lane still has one owner).
- **Why:** the engine records the lanes each interface last ran with (`interface_applied`) and, when they went down, counts what waits in the lanes being dropped (one pass over the queue index: ~20–60 ms for 1,001 interfaces with 300,000 waiting, measured on WSL).
- **What to do:** change the lanes only when the interface has nothing pending: keep the old value, `ductus drain --wait` (new messages wait at the sender, the engine sends what it has), then restart with the new value. Run `ductus validate` against the database before deploying. To give each of N workers 64 lanes, set `partitions: 64 * N`. Re-assigning waiting messages to the new lanes automatically is planned (v0.7).

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
