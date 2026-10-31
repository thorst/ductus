# Keeping and watching

How long an interface's messages are kept, what operations expects of it, and who hears when it
doesn't happen.

## How long: `retention` and `quarantined`

[`retention`](../reference/config/interface.md#retention) on an interface overrides the project's
(ductus.yaml, `7w` by default) for that interface: `1w`, `3w` or `7w`.

```yaml
# interfaces/adt-in/interface.yaml, in part
retention: 1w          # a busy feed nobody looks back on
quarantined: keep      # the default: until a person resolves them
```

The rules are the project's: whole partitions dropped, kept at least the window, nothing waiting
ever deleted, a shorter window waiting for `ductus retention confirm adt-in`
([how long messages are kept](ductus-yaml.md#how-long-messages-are-kept)). Change it per interface
when its feed is far busier or far more looked-back-on than the rest. One change at a time per
interface ([nuance](../nuances.md#one-retention-change-at-a-time-per-interface-level-3)).

[`quarantined`](../reference/config/interface.md#quarantined) is the same choice as the project's,
for this interface's quarantined messages past the window: `keep` them in the held area until
someone resends, corrects or dismisses them, or `purge` them with the rest
([nuance](../nuances.md#quarantined-messages-are-kept-until-resolved-unless-you-say-purge-level-2)).

Neither applies to an HTTP source in `respond` mode: it keeps nothing.

## What's expected: `monitor`

The engine can't know that a quiet feed is a problem: ADT from an EHR that stops for half an hour
in the afternoon is an outage, a lab's nightly file that doesn't come by 07:00 is one too, and a
test feed that's quiet all week is fine. So **nothing is expected until you say so**
([nuance](../nuances.md#nothing-is-expected-until-you-say-so-level-2)), in the terms the receiving
team uses, with [`monitor`](../reference/config/interface.md#monitor):

```yaml
# interfaces/adt-in/interface.yaml, in part
monitor:
  active_within: 30m              # something received every 30 minutes...
  between: "06:00-22:00"          # ...during these hours (the server's local time)
  behind: { messages: 500, age: 10m }
```

```yaml
# interfaces/quest-results/interface.yaml, in part
monitor:
  active_by: "Mon-Fri 07:00"      # the nightly file, by 07:00 on weekdays
  from: "18:00"                   # counting what arrived since 18:00 the evening before
```

| Setting | Breaking it is | For |
|---|---|---|
| `active_within` (with `between`) | **silent** | Feeds that never stop in normal operation: set it a little above the longest normal gap. |
| `active_by` (with `from`) | **missed** | Feeds that come once a day or week: a nightly file, a Monday batch. |
| `behind` (messages, age, or both) | **behind** | Queues: when the receiving department would call it late. A count alone misses an overnight stall; an age alone misses a daytime pile-up that still moves; give both for busy feeds. |

Only receiving counts for `active_within` and `active_by`. A destination can set its own `behind`.
Besides these, some states need no setting: a destination with messages waiting that has sent
nothing for a minute is **stalled**, one with quarantined messages is in **error**, an interface
whose code was stopped is **stopped**, and a paused destination is **paused** (never an alert).
The web UI, `ductus status` and Prometheus show them all
([what each state means](../nuances.md#what-each-state-means-and-when-it-shows-level-2)).

## Who hears: `alert`

States become notifications only through the alert rules in ductus.yaml
([Alerts and owners](alerts.md)): a rule says which states alert, how badly, and to which
channels. [`alert`](../reference/config/interface.md#alert) on an interface adds recipients on top
of the rule's, or silences the interface:

```yaml
# interfaces/adt-in/interface.yaml, in part
alert:
  to: [lab-team, owner]     # channels from ductus.yaml, and/or whoever owns it
```

```yaml
# interfaces/new-feed/interface.yaml, in part
alert:
  mute: true                # half-built: alerts open and show, but nobody is told
```

`to: [owner]` reaches whoever the interface's (or the destination's) `owner` names
([who "owner" is](../nuances.md#who-owner-is-level-2)). `mute` sends nothing about this interface;
its alerts still open and show in the web UI, so nothing is hidden from people who look.
