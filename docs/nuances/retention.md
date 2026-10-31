# Nuances: retention

What could surprise you here, why it is so, and what to do. Each entry has a level, 1 (day one) to 5 (expert): the [index](index.md) lists them all by level.

## Retention is 1, 3 or 7 weeks, not any number of days (level 1)
- **What:** `retention:` in interface.yaml is `1w`, `3w` or `7w` (or the same in days: `7d`, `21d`, `49d`; default: `retention` in ductus.yaml, else 7w). Anything else is refused by `ductus validate`.
- **Why:** performance. Each window is a tier of partitions, and old messages go by dropping whole partitions, which costs nothing however many messages there are. Deleting rows one by one (the usual pruner) slows the busiest tables and needs vacuuming after. Each tier adds partitions that some queries have to plan for, so there are few, short tiers, keeping the count around 24.
- **What to do:** pick the window that covers how far back you need to look into problems. Older history: the source system, or an archive on cheaper disk (an export is planned). The engine isn't a data warehouse.

## Messages are kept at least their window, and gone within a period after (level 2)
- **What:** a partition covers a day (1w) or a week (3w, 7w), and it's dropped once all of it is older than the window. So a 1w message lives 7 to 8 days, a 7w one 49 to 56. Partitions are checked hourly.
- **What to do:** treat the window as "at least". If something must be gone sooner, use a shorter tier.

## Pending messages are never purged; past the window they're held (level 2)
- **What:** when a partition expires, the messages in it that someone still needs (pending for a destination, and quarantined ones) move to the **held area** with all their deliveries, and the partition drops as usual. Held messages stay in their destination's queue, in order, and in the message browser; they're delivered or resolved like any other. A month of the held area is dropped whole once it's over and nothing in it is pending or quarantined (its finished messages go with it). `ductus retention` shows the held area by month and interface, and `/metrics` has `ductus_held_messages` and `ductus_held_bytes` (both counted at the hourly retention run). A shorter retention never deletes pending or quarantined messages either.
- **Why:** a destination that was down for a month must not lose its backlog, and quarantined messages wait for a person. Before the held area, one waiting message kept its whole week for every interface in the tier.
- **What to do:** watch `ductus_held_messages` (alert on quarantined ones growing: each is a question nobody has answered). Moving a backlog writes each message once more, once, in the background: about 5,000 messages a second with two destinations each (300,000 in 58 s on a dev machine, busy half the time), so a day of a 1M-message feed held for one receiver takes 3 to 4 minutes to move, and 90M about 5 hours. Nothing to do unless the database is already at its limit at the top of the hour.

## Quarantined messages are kept until resolved, unless you say `purge` (level 2)
- **What:** `quarantined: keep` (the default) holds quarantined messages past the retention window until someone resolves them. `quarantined: purge` (in interface.yaml, or ductus.yaml for every interface) lets them go with their partition, like the finished ones. There's no second time limit.
- **Why:** in most engines unresolved quarantines pile up for years; keeping them is the safe default, and purging is a decision someone makes on purpose for a feed where a missed message is resent anyway.
- **What to do:** keep the default unless the feed's quarantines are routinely ignored and resent by the sender; then `purge`, and say why in `about:`.

## Shortening retention deletes messages, so it needs a confirmation (level 2)
- **What:** deploying a shorter `retention:` doesn't apply it. The engine logs a warning and keeps the old one, and `ductus retention` shows how many messages the new value would delete. `ductus retention confirm <interface>` (an operator token) applies it: its finished messages past the new window are deleted in the background, the others move into the new tier. Lengthening needs no confirmation and keeps everything not yet dropped.
- **Why:** a typo (7d for 70d, say) shouldn't delete a quarter of the history.
- **What to do:** confirm after deploying, on each engine (it's per database).

## One retention change at a time per interface (level 3)
- **What:** a change moves (or deletes) the interface's older messages in batches, at most half the time busy (the demo's 8,000 messages took 1 to 1.5 s on a dev machine: a day of a 1M-message feed would take a few minutes). A different value deployed meanwhile waits until it's done; values in between are skipped. A change also waits for the interface's pending messages to be delivered before it finishes (they move once delivered).
- **What to do:** watch progress with `ductus retention` (messages left). Don't flip retention back and forth.

## Shortening frees disk when partitions go, not at once (level 4)
- **What:** deleted messages leave space that Postgres reuses only within the same partition, and old partitions get no new messages. The disk space comes back when their partitions are dropped, at most the old window later.
- **What to do:** to free space now, shorten and then, off-hours, `VACUUM FULL` the affected old partitions yourself (it locks each while it runs). A built-in compaction is planned.
