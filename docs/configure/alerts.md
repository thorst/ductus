# Alerts and owners

The engine judges every interface and destination every 15 seconds. When one is in a state your
rules name (stalled, behind, silent…), it **tells people once**, reminds them while nobody has
acknowledged it, tells them again if it gets worse, and says when it's fine again. **Nothing
alerts until you write rules**: until then, states show in the web UI only.

All of it is the [`alerts`](../reference/config/ductus.md#alerts) section of ductus.yaml, the one
section read per environment (so test and production can mail different people, and webhook
URLs can be secrets):

```yaml
# ductus.yaml, in part
alerts:
  smtp:
    host: smtp.hospital.org
    from: ductus@hospital.org
    username: ductus@hospital.org
    password: ${secret:SMTP_PASSWORD}
  channels:
    oncall: { email: interfaces-oncall@hospital.org }
    chat: { webhook: { url: "${secret:TEAMS_WEBHOOK}", format: teams } }
  rules:
    - { when: [error, stalled, stopped], severity: critical, to: [oncall, chat] }
    - { when: [behind, silent, missed, certificate], severity: warning, to: [chat] }
  repeat: 4h
  quiet: { between: "22:00-06:00", severities: [warning] }
  heartbeat: { url: "${secret:HEARTBEAT_URL}", every: 5m }
```

## Channels: where alerts go

A **channel** is a name rules send to: an email list, a chat webhook, or both.

- **Send to teams, not people**: a team's mailing list or chat channel. People change jobs, go on
  leave and sleep; a team's list doesn't.
- **Email** needs `smtp`: your organization's relay or submission server, verified by its
  certificate (`tls: starttls` on 587 by default, `tls` on 465, `none` only for a relay on the
  same network segment). Ask the mail team to let `from` through their filters.
- **Webhooks**: `format: teams` (a Microsoft Teams Workflows webhook), `slack` (also Mattermost,
  Rocket.Chat), or `json` (every field, for your own incident tool). A webhook's URL is its own
  password, so keep it in a secret
  ([nuance](../nuances.md#webhook-urls-are-secrets-level-2)).
- **Try one** without an engine: `ductus alerts test oncall` sends a test notification now.

## Rules: what alerts, and how badly

Each rule names states (`when`), a `severity`, and channels (`to`). A state is matched by the
**first** rule that names it; a state no rule names doesn't alert, and `ductus validate` says
which.

| State | Means | Usually |
|---|---|---|
| `error` | A destination has quarantined messages someone must look at | critical |
| `stalled` | A destination has had messages waiting over a minute and sent none: its receiver is down, or a message keeps failing | critical |
| `stopped` | An interface's code was stopped (it ran away, or ran out of memory) | critical |
| `behind` | A queue is past its `monitor.behind` | warning |
| `silent` | Nothing received within `monitor.active_within` | warning, or critical for a core feed |
| `missed` | Nothing received by `monitor.active_by` | warning |
| `certificate` | A certificate the engine presents or connects to nears expiry (30, 14, 7 days) or expired (always critical) | warning |

`behind`, `silent` and `missed` need an interface's [`monitor`](keeping-and-watching.md#whats-expected-monitor)
settings: nothing is expected until you say so. **A paused destination never alerts** (it's paused
on purpose), and an alert that's open when someone pauses stays open, quietly, until it's resumed
([nuance](../nuances.md#a-pause-never-alerts-and-holds-an-alert-open-level-2)).

- **`warning`**: worth knowing today; held during quiet hours. **`critical`**: act now; never
  held. A warning that becomes critical is told again. On Windows a critical alert is also in the
  Event Log (1020).
- **`repeat`** (default `4h`): while an alert is open and unacknowledged, its recipients are
  reminded this often; `0` never. **Acknowledging** it on the Alerts page stops the reminders until
  it gets worse or recovers. Acknowledging isn't fixing, and silencing isn't pausing
  ([nuance](../nuances.md#silencing-is-not-pausing-and-acknowledging-is-not-fixing-level-1)).
- **`settle`** (default `2m`): how long a state must be fine before its alert is over, so a
  receiver flapping up and down is one alert, not dozens. **`recovery`** (default on) tells the
  recipients when it is.
- **`quiet`**: hours when warnings wait until morning; one that comes and goes in the night is
  never sent, only kept in the history. Keep `critical` out of `severities`
  ([nuance](../nuances.md#quiet-hours-hold-warnings-until-morning-critical-ones-never-wait-level-2)).
  The hours are the server's local time.
- **`certificates.warn_days`** (default `[30, 14, 7]`): the steps before a certificate expires.
  Make the first long enough for your slowest renewal (a vendor's CA, a change board).

## Owners as recipients

`to: owner` in a rule, or an interface's [`alert.to`](keeping-and-watching.md#who-hears-alert),
also tells whoever owns the interface or destination: its `owner`, else `about.owner`
([nuance](../nuances.md#who-owner-is-level-2)). An owner who is an account is mailed at the
account's email; one written as an address is mailed at it; a team without an account gets its
address from [`owners`](../reference/config/ductus.md#owners):

```yaml
# ductus.yaml, in part
owners:
  Lab Systems: { email: lab-systems@hospital.org }
```

**Lead with team channels, and add `owner` where a person should hear directly** (an interface
one analyst built and looks after, a certificate one person renews). Owners change; a rule that
sends only to owners goes quiet when one leaves, which is why `ductus owners --check` fails on an
owner that reaches nobody ([owners](about.md#owners)).

## The heartbeat: when the engine itself is down

An engine that is down, or can't reach its database, can't send alerts. `heartbeat` calls a URL
every `every` (default 5 minutes) while the engine runs, for a service **elsewhere** that raises
its own alarm when the calls stop: healthchecks.io, Better Stack, Uptime Kuma, or a monitoring
tool's push check. Set its grace period to about twice `every`. **Set it in production**: it's the
only way anyone hears that the engine is gone
([nuance](../nuances.md#an-engine-that-is-down-sends-nothing-use-the-heartbeat-level-2)).

## What to know

- **Notifications are sent at least once**: through an outbox in the database, retried until the
  channel takes them. A failing channel shows on the Alerts page, not as another alert
  ([nuance](../nuances.md#notifications-are-sent-at-least-once-and-a-failing-channel-only-shows-in-the-ui-level-3)).
- **After a start**, states the engine hasn't watched for a minute yet don't alert, so a restart
  doesn't page everyone ([nuance](../nuances.md#a-restart-doesnt-alert-on-what-it-hasnt-watched-yet-level-3)).
- **It costs little**: 35 ms a round at 1,001 interfaces, measured
  ([nuance](../nuances.md#judging-alerts-costs-little-and-runs-all-the-time-level-4)).
- **History** (what fired, who was told, who acknowledged) is kept a year (`alerts.keep`).
- An interface can add recipients (`alert: { to: [...] }`) or be muted (`alert: { mute: true }`):
  [keeping and watching](keeping-and-watching.md#who-hears-alert).
