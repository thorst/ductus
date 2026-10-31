# Alerts in practice

How to set alerts up is [Alerts and owners](../configure/alerts.md); this page is living with
them: what arrives, what to do with it, and how to keep alerts trusted, which is the only way they
keep working. An alert people learn to ignore is worse than none.

## What an alert is

The engine judges every interface and destination every 15 seconds. When one enters a state a
rule in ductus.yaml names, an **alert opens** and its channels are told **once**. While it stays
open and nobody has acknowledged it, they're **reminded** every `repeat` (4 h by default); if it
gets worse (a warning becomes critical) they're told again; once the state has been fine for
`settle` (2 min), it **recovers** and they're told that too
([nuance](../nuances.md#alerts-tell-you-once-then-remind-they-dont-repeat-every-check-level-2)).

What a webhook receives, from a real engine whose lab receiver went down (`format: json`; Teams
and Slack get the same as a card or message):

```json
{"kind":"fired","alert":3,"key":"dest:adt-in/lab","interface":"adt-in","destination":"lab","state":"stalled","severity":"critical","since":"2026-10-31T10:35:21.087109+00:00","at":"2026-10-31T10:35:21.093252+00:00","summary":"stalled: nothing sent for 1 min, 30 waiting; retrying: Connection refused (os error 111)","environment":"dev","title":"[Ductus dev] CRITICAL adt-in → lab is stalled"}
```

The title says the environment, so a test engine's alert can't be taken for production's.

## When one arrives

1. **Open it** in the console's **Alerts** page (or the interface it's about): the summary says
   what and how much ("30 waiting; retrying: Connection refused").
2. **Acknowledge** it once someone is on it: the reminders stop, and the others on the channel see
   who took it. Acknowledging isn't fixing: the alert stays open until the state is fine
   ([nuance](../nuances.md#silencing-is-not-pausing-and-acknowledging-is-not-fixing-level-1)).
3. **Find the cause**: `stalled` → [hotspots](monitoring.md#where-a-backlog-sits-hotspots) (the
   receiver, or one message); `error` → the [quarantine](resend.md); `behind` → the receiver's
   speed and the catch-up time; `silent`/`missed` → the sender; `stopped` → the code (the log and
   the quarantined messages say which message); `certificate` → renew it
   ([renewing](../configure/tls.md)).
4. **If it's planned downtime you forgot to announce**, pause the destination with the reason: a
   paused destination doesn't alert, and its open alert waits quietly until it's resumed
   ([nuance](../nuances.md#a-pause-never-alerts-and-holds-an-alert-open-level-2)).
5. **It recovers by itself** when the state is fine, and the channel is told. Nothing to close.

## Silence, pause, mute: which one

| | Does | For |
|---|---|---|
| **Acknowledge** | stops the reminders for this alert, until it worsens or recovers | "I'm on it" |
| **Silence** (for a time) | nobody is told about this alert until the time is up; it stays open and shows | a known problem someone else owns, while they fix it |
| **Pause** the destination | nothing is sent or retried; never alerts | planned downtime of the receiver |
| **`alert: { mute: true }`** on the interface | its alerts open and show, but nobody is told | a half-built interface, until it goes live |

Silencing hides; pausing stops. Pause what's down on purpose, silence only what someone is
already fixing, and never mute a live interface to stop noise: fix the rule instead.

## Keeping alerts trusted

- **Every alert should need a person.** If people get alerts they do nothing about, change the
  rule: a state that isn't worth a call at 3 a.m. is a `warning` (held during quiet hours), and a
  feed nobody watches doesn't need `monitor:`.
- **Tune `monitor:` from what happened.** A `silent` that fires every Sunday morning means
  `active_within` doesn't fit the feed's quiet hours: add `between:`. A `behind` that fires during
  every nightly batch means the limit is below a normal night.
- **Send to teams, not people**: a team's list or chat channel survives holidays and job changes
  ([nuance](../nuances.md#who-owner-is-level-2)).
- **Test every channel before you rely on it**, and after the mail team changes anything:

```text
$ ductus alerts test chat
sent a test notification to `chat`
```

  It sends a test notification straight from ductus.yaml, without an engine (the webhook got
  `"kind":"test"`, `"title":"[Ductus dev] test notification"`). The Alerts page's
  channel table also has **Test**, and shows each channel's sent, failed and last error: a
  channel that's failing can't tell you so itself
  ([nuance](../nuances.md#notifications-are-sent-at-least-once-and-a-failing-channel-only-shows-in-the-ui-level-3)).
  Prometheus's `ductus_alert_send_failures_total` is the other way to hear about it.
- **Quiet hours hold warnings, never critical alerts**
  ([nuance](../nuances.md#quiet-hours-hold-warnings-until-morning-critical-ones-never-wait-level-2)).
- **After a restart**, states the engine hasn't watched for a minute yet don't alert, so a restart
  doesn't raise a wave of alerts for things that were already known
  ([nuance](../nuances.md#a-restart-doesnt-alert-on-what-it-hasnt-watched-yet-level-3)).

## An engine that's down tells nobody: the heartbeat

Alerts come from the engine, so an engine that's stopped, crashed or cut off from its database
can't send one. Set [`alerts.heartbeat`](../reference/config/ductus.md#alertsheartbeat) in
production: the engine calls a URL every `every` (say `5m`) while it runs, and a service that
expects the calls (healthchecks.io, Better Stack, Uptime Kuma, your monitoring's own push check),
running somewhere else, raises the alarm when they stop. Give that service a grace period of about
twice `every` ([nuance](../nuances.md#an-engine-that-is-down-sends-nothing-use-the-heartbeat-level-2)).
