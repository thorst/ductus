# Nuances: several destinations

What could surprise you here, why it is so, and what to do. Each entry has a level, 1 (day one) to 5 (expert): the [index](index.md) lists them all by level.

## Each destination runs its own function on the original message (level 2)
- **What:** with `destinations:`, every destination calls its function (`transform` unless it names another with `transform: toBilling`) on the message as received, not on another destination's result. Three destinations using `transform` run it three times, and each run makes its own `db()` and `fetch()` calls.
- **Why:** each destination works through its own queue at its own pace (one can be hours behind), and outputs aren't stored, so nothing can be handed from one to the other.
- **What to do:** put shared preparation in a helper the functions call. Keep lookups in the functions that need them. Module-level state is shared by all of an interface's functions (one module), so a bounded cache can save repeated lookups.

## One destination down holds up only itself (level 2)
- **What:** each destination has its own queue, order and retries. While the lab is down, billing and the archive keep receiving; the lab's messages wait, in order, and go once it's back. A message's overall status stays `pending` until every destination is done with it. Then it's `quarantined` if any destination quarantined it, `sent` if any destination got it, and `filtered` if all of them filtered it. The status per destination says which (`ductus status` lists them).
- **What to do:** watch each destination's queue, not just the interface's.

## Pausing a destination: at once, no questions, and it holds (level 2)
- **What:** `ductus pause <interface> [destination]` (or the API) stops sending to that destination at once: no new sends and no retries. A send already under way finishes. Messages keep arriving and wait, in order. No reason is needed to pause. Add one when there's time by running it again with `--reason "..."` (until then it shows "no reason yet"). The pause is stored, so a restart or a failover keeps it. A paused destination doesn't hold up a drain. `ductus resume <interface> [destination]` sends the backlog in order. Without arguments, `ductus resume` ends a drain.
- **Why:** in an emergency (a receiver breached, a vendor sending garbage back) you stop the damage first and explain after. A destination that's merely down already waits and retries by itself, so it needs no pause.
- **What to do:**
  - Down for minutes to hours: do nothing; it retries.
  - Cut off on purpose, or down for days to weeks: pause it, so it stops retrying and shows as *paused*, not *stalled*. Add the reason.
  - Longer than the interface's retention window: the waiting messages move to the held area as their partitions expire, and the rest of the history goes on schedule, so the disk holds the normal window plus the backlog (not the whole period it spans). Watch the held area's size (`ductus retention`). Agree a release plan with the vendor before they're back: the backlog goes in order, at the destination's rate limit (90M at 200/s is about 5 days).

## Filtering and quarantine are per destination (level 2)
- **What:** `return null`, `kill()` and `quarantine()` in a destination's function apply to that destination only. The message still goes to the others.

## Adding, removing or renaming a destination (level 3)
- **What:** at startup the engine matches stored deliveries to `interface.yaml`. A **new** destination also gets the messages still in flight (pending for any destination), not older ones. A **removed** destination's waiting messages are marked filtered ("destination `x` was removed from interface.yaml") and logged. A **rename** is both: the new name gets everything in flight, including what the old name had waiting, and may re-send some the old name had already sent.
- **What to do:** rename only when the destination is caught up. Removing a destination by mistake can't be undone by putting it back: its waiting messages were already filtered.

## Many destinations on one interface: when to split (level 3)
- **What:** every destination adds a delivery row per message, written in the same statement as the message and updated when it's sent. So a message with 25 destinations costs the database about 25 small writes on the way in and 25 on the way out, and senders wait for the first part before their ACK.
- **Measured** (dev laptop under WSL, strict durability, 16 sender connections, file destinations, steady-state partitions; `scripts/bench-fanout.sh`): 1 and 5 destinations ingest ~2,700–3,400 messages/s; 25 destinations ~1,150–1,300/s; 50 destinations ~500–700/s. Delivery tops out around 15,000–20,000 deliveries/s in total, whatever the fan-out: at 50 destinations, 6,000 messages took ~16 s to reach everyone (one run 27 s). For scale, 1M ADT messages a day averages ~12/s, with peaks of a few hundred.
- **What to do:**
  - Up to ~10 destinations per interface: nothing to plan, at any realistic volume.
  - More than that: multiply your peak messages/s by the number of destinations and compare with the deliveries/s your server measures (~15,000 on a laptop; `ductus bench` will measure it, v0.6). Under a third of it is comfortable: that leaves room for a burst of 3x and for catching up after a receiver's outage. Over that, a burst builds a backlog that clears only as fast as the spare capacity allows.
  - If it doesn't fit: split the feed. Route to a few internal interfaces (`destination: { interface: adt_out_a }`) that each fan out to a share of the receivers, or give low-priority receivers (archives, analytics) their own interface fed from a route. The sender's ACK then waits for a few delivery rows instead of dozens; in exchange, each hop stores one more copy of the message, so total writes grow a little.

## A runaway is blamed on the interface, not the destination (level 5)
- **What:** when code runs away after an `await`, the engine knows which interface's code it was, not which function. Every call of that interface in flight on the thread is quarantined, so another destination's message can be quarantined with it. Each one counts towards stopping the interface.
