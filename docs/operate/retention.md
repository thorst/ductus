# Retention and storage

How long messages stay, where the space goes, and what an operator does about it, which is
mostly nothing: old messages go by themselves, a partition at a time, with no purge job, no
vacuum to schedule and no slow delete in the middle of the day. The settings are in
[ductus.yaml](../configure/ductus-yaml.md#how-long-messages-are-kept) (the project's default) and
[interface.yaml](../configure/keeping-and-watching.md#how-long-retention-and-quarantined) (one
interface's).

## How it works

- **Three windows**: `1w`, `3w` or `7w` (the default). Each is a set of partitions of the
  message tables, a day each for `1w` and a week each for `3w` and `7w`
  ([nuance](../nuances/retention.md#retention-is-1-3-or-7-weeks-not-any-number-of-days-level-1)).
- **Old messages go by dropping a whole partition** once all of it is older than the window,
  checked hourly. Dropping a partition costs the same whether it held ten messages or ten
  million, and blocks nothing: no row-by-row delete, no table bloat, nothing to vacuum.
- **So a message is kept at least its window**, and gone within a day (`1w`) or a week (`3w`,
  `7w`) after: a `7w` message lives 49 to 56 days
  ([nuance](../nuances/retention.md#messages-are-kept-at-least-their-window-and-gone-within-a-period-after-level-2)).
  Treat the window as "at least".
- **Nothing anyone still needs is deleted.** When a partition expires, its messages still pending
  for a destination, and its quarantined ones (with `quarantined: keep`, the default), move to
  the **held area** first, with their deliveries, and stay in their queue and in the console
  ([nuance](../nuances/retention.md#pending-messages-are-never-purged-past-the-window-theyre-held-level-2)).
  A month of the held area is dropped once it's over and nothing in it is waiting.
- **The audit log is kept apart**, six years by default (`audit.keep`), and trimmed hourly.

## Looking at it

```text
$ ductus retention
adt-in                       7w
```

Each interface's retention; when there are any, also changes in progress (with what's left to
move), shorter values waiting for a confirmation (with how many messages they'd delete), and
what the held area keeps, by month and interface. The console's **Retention** page shows the
same. In Prometheus, `ductus_held_messages` (by interface and status) and `ductus_held_bytes`
are counted at each hourly run.

**When to act:** held *quarantined* messages growing is a question nobody has answered: look at
them ([resend, correct or dismiss](resend.md)). Held *pending* messages are a destination that's
been down past the window: they'll go when it's back, but the receiver's team should know.

## Changing retention

Change `retention:` in interface.yaml (or ductus.yaml) and deploy.

- **Longer** applies at once: everything not yet dropped is kept to the new window.
- **Shorter** deletes messages, so it **waits for a confirmation**. The engine logs a warning,
  keeps the old value, and `ductus retention` shows how many messages the new one would delete.
  `ductus retention confirm <interface>` (an operator token), or typing the interface's name on
  the Retention page, applies it: finished messages past the new window are deleted in the
  background, the rest move into the new window's partitions
  ([nuance](../nuances/retention.md#shortening-retention-deletes-messages-so-it-needs-a-confirmation-level-2)).
  Confirm on each database the project runs on.
- **One change at a time per interface**: a change works in batches, at most half the time busy
  (8,000 messages took 1 to 1.5 s on a dev machine), and a different value deployed meanwhile
  waits ([nuance](../nuances/retention.md#one-retention-change-at-a-time-per-interface-level-3)). Don't flip
  it back and forth.
- **Shortening frees disk later**, as the old partitions are dropped; to free it now, a DBA can
  `VACUUM FULL` the affected old partitions off-hours
  ([nuance](../nuances/retention.md#shortening-frees-disk-when-partitions-go-not-at-once-level-4)).

**Choose the window** your team looks back over when a problem surfaces: `1w` for busy feeds
nobody looks back on, `7w` where problems show up weeks later (billing, a monthly
reconciliation). Longer history belongs in the source system or an archive; the engine isn't a
data warehouse, and a long-term store with search is planned (v1.3).

## How much space

Roughly: **messages a day × their average size × the days kept**, plus a small row per
destination per message, the indexes, outputs kept with `save_output` (about 1.15 GB per million
deliveries, measured), and whatever is held. A 7w window keeps up to 56 days, so size for that.

Measure it on your own traffic after a week rather than estimating for long:

```sql
-- psql: the whole database, then the message tables by window (t7 = 1w, t21 = 3w, t49 = 7w)
SELECT pg_size_pretty(pg_database_size(current_database()));
SELECT substring(relname from '^(?:messages|deliveries)_(t\d+|held)_') AS tier,
       pg_size_pretty(sum(pg_total_relation_size(oid))) AS size
FROM pg_class
WHERE relkind = 'r' AND relname ~ '^(messages|deliveries)_(t\d+|held)_\d+$'
GROUP BY 1 ORDER BY 1;
```

On a development database it printed:

```text
 tier |  size
------+---------
 held | 8888 kB
 t21  | 54 MB
 t49  | 19 MB
 t7   | 832 kB
```

Space comes back as partitions are dropped, a day or a week at a time; between drops the database
grows by about a day's worth. Leave the disk room for the largest window's worth plus a held
backlog (a receiver down for a week keeps a week of its messages longer).

[Sizing the database](../install/postgres.md#sizing) has the rest: the commit time that decides
the speed, and keeping long queries off the message tables.

## What's not there

- **No archive or export** of old messages yet: past the window they're gone (planned, with
  long-term storage, v1.3).
- **No per-day windows** (30 days, 90 days): only the three, for the reason above.
- **No legal hold** on a single patient's messages: if one is needed, copy them out (the
  [schema](../schema.md) describes the tables) before their partition goes.
