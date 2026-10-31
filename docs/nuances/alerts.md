# Nuances: alerts

What could surprise you here, why it is so, and what to do. Each entry has a level, 1 (day one) to 5 (expert): the [index](index.md) lists them all by level.

## Alerts tell you once, then remind; they don't repeat every check (level 2)
- **What:** with `alerts:` in ductus.yaml, every interface and destination is judged every 15 s, whether or not anyone has the UI open. A state a rule names (`error`, `stalled`, `behind`, `stopped`, `silent`, `missed`; `certificate` has steps of its own, see Certificates) **fires** once, to the rule's channels. It **escalates** (people are told again) only when the severity rises, warning to critical. Getting better without being fine (stalled to behind) changes the alert quietly. While it stays open and nobody acknowledges it, a **reminder** goes every `repeat` (4 h by default; `0` = never). Once it has been fine for `settle` (2 min) it **recovers**, and the people who were told hear that too. Going bad again within `settle` reopens the same alert instead of firing a new one (a flapping receiver is one alert). More quarantined messages on a destination already in *error* are not a new alert.
- **Why:** an alert on every check is noise people learn to ignore. One per problem, a reminder while nobody owns it, and a word when it's over is what an on-call person can act on.
- **What to do:** acknowledge what you're working on (the Alerts page, or `POST /api/v1/alerts/{id}/ack`): reminders stop until it gets worse or recovers. Keep `repeat` unless the channel pages someone. Clear quarantines (resend, correct or dismiss): *error* stays open until the destination has none.

## A pause never alerts, and holds an alert open (level 2)
- **What:** a paused destination is paused on purpose, so it never fires. If it was already alerting, the alert stays open while paused: no reminders, and no recovery either (a pause isn't the receiver being fine). After the resume it's judged as usual.
- **What to do:** pause a destination you know is down (it stops the retries); the alert recovers once it delivers again.

## Silencing is not pausing, and acknowledging is not fixing (level 1)
- **What:** **silence** (up to 30 days, with a reason) sends nothing about an alert, not even its recovery, but it still shows, and you can silence a destination before planned work so nothing fires. **Pause** stops a destination sending. **Acknowledge** only says someone is on it. None of them touch messages: they keep waiting.
- **What to do:** for a planned outage, pause the destination and silence its alert, both with a reason. Both are in the audit log.

## An engine that is down sends nothing: use the heartbeat (level 2)
- **What:** alerts come from the engine. If it is stopped, crashed, or can't reach its database, nobody is told. `alerts.heartbeat: { url, every: 5m }` calls a URL every so often while the engine runs. A service that expects the calls (healthchecks.io, Better Stack, Uptime Kuma, your monitoring's own "push" check) raises its own alarm when they stop.
- **What to do:** set `heartbeat` in production, at a service that doesn't run on the engine's server, and set that service's grace period to about twice `every`.

## Notifications are sent at least once, and a failing channel only shows in the UI (level 3)
- **What:** a notification is stored with its alert (the outbox) before anything is sent, then sent by whichever engine claims it (two engines on one database send it once). A send that fails is retried: 30 s, then doubling, at most an hour apart, ten tries (about six hours), then counted as failed. A channel that fails never holds up another. If an engine dies after sending but before recording it, the notification goes out again after two minutes. A failing mail server or webhook can't alert about itself: failures show on the Alerts page (Channels), in the engine log, and in `ductus_alert_send_failures_total`.
- **What to do:** send critical rules to two channels of different kinds (email and a chat webhook). Watch `ductus_alert_send_failures_total` from Prometheus if you have it. Try a channel before going live with **Send test** or `ductus alerts test <channel> --env prod` (that one needs no engine).

## Quiet hours hold warnings until morning; critical ones never wait (level 2)
- **What:** `quiet: { between: "22:00-06:00", severities: [warning] }` holds notifications of those severities until the quiet hours end (server local time, like schedules). A warning that fires and recovers overnight is never sent: it's in the history. A critical one, or a warning that escalates to critical, goes at once.
- **What to do:** keep critical out of `severities`. Remember the server's time zone when the engine runs in UTC (containers often do).

## Who "owner" is (level 2)
- **What:** alerts go where a rule's `to:` says: **channels**, which are usually a team's mailing list or chat (`channels:` in `alerts:`), and an interface can add its own team with `alert: { to: [lab-team] }`. People change; a team's list doesn't, and someone on it is always awake. **`owner` in `to:` is optional**: it also mails whoever owns the destination (its own `owner:`, else its interface's `about.owner`). The owner text is matched to an account as "An owner is a person when the text names exactly one account" says (by a former name too: a renamed owner is mailed at their email now). Failing that it's looked up in `owners:` in ductus.yaml (a team's mailing list), and failing that it's used as is if it's an email address. An owner found nowhere gets nothing; the rule's other channels still do. Without a database `ductus validate` can't see accounts, so it only warns about owners that are neither in `owners:` nor addresses; with `DUCTUS_DATABASE_URL` it matches them. The engine logs every owner that reaches nobody at start, and the Alerts page lists them. Owners matter on their own anyway: the Owners page, Interfaces → Mine, `ductus owners` and the documentation say whom to ask about what.
- **What to do:** send alerts to team channels in your rules; add `owner` only where a person should hear directly. Put teams in `owners:` with their list address, write people as their email, and give accounts an email. A test interface can say `alert: { mute: true }`; `alert: { to: [pager] }` adds recipients for one interface.

## A restart doesn't alert on what it hasn't watched yet (level 3)
- **What:** states that need watching (`stalled`, `behind`, `silent`, `missed`) are judged only after the engine has run a minute. `error` and `stopped` are facts in the database and alert at once. Open alerts are in the database too, so a restart neither repeats them nor forgets them.
- **What to do:** nothing. Expect the first alerts about a problem that was already there about a minute after a start.

## Webhook URLs are secrets (level 2)
- **What:** a chat webhook URL is its own password: anyone with it can post to the channel. The engine shows only its scheme and host (`https://hooks.slack.com/…`), in the UI, the API, logs and errors.
- **What to do:** write it as `${secret:CHAT_WEBHOOK}` (`ductus secret set`); `alerts:` may use `${...}` like resources.yaml, filled in per environment. Formats: `slack` (also Mattermost and Rocket.Chat), `teams` (a Workflows webhook: the old Office 365 connectors are being retired), `json` (everything, for your own tools).

## Judging alerts costs little, and runs all the time (level 4)
- **What:** every 15 s the engine counts its queues (as live updates do) and compares each state with its stored alert, writing only what changed. Measured with 1,001 interfaces (2,002 alerts, WSL): 35 ms a round, about 0.1% of one core in all, and 7 MB of memory.
- **What to do:** nothing at that size. Each round's time is in the log at debug level (`RUST_LOG=ductus_runtime::alerts=debug`, "alerts judged").
