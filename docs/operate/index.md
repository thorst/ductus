# Running Ductus day to day

These pages are for whoever keeps the interfaces running: the on-call analyst, the integration
team, the operations center. They assume the engine is installed ([install](../install/index.md))
and the project deployed; what each setting means is in [Configure](../configure/index.md).

## Where to look

| | For | Page |
|---|---|---|
| **The web UI** (the console) | Everything a person does: health at a glance, messages, quarantine, resend, pause, alerts | [The console](console.md) |
| **The `ductus` command** | The same from a terminal or a script, and what needs the server (doctor, the audit export, users and tokens) | [The CLI day to day](cli.md) |
| **The REST API** | Your own tools and runbooks; everything the console does | [REST API reference](../reference/api/index.md) |
| **Prometheus** | Graphs and your monitoring system's own alerts | [Monitoring and metrics](monitoring.md) |
| **Logs** | What the engine did and why, line by line | [Logs](logs.md) |
| **Alerts** | Being told, so nobody has to watch | [Alerts in practice](alerts.md) |

The console, the CLI and the API are three ways into the same engine: a pause made in one shows
in the others, and every change, whoever makes it and however, is in the audit log with who made
it.

## What a normal day looks like

- **The Overview is green**, or shows exactly what isn't: a destination behind, a quarantined
  message, a certificate expiring. Start every shift there, or let [alerts](alerts.md) tell you.
- **Quarantined messages get looked at the same day.** Each one is a message a receiver didn't
  get. Read the reason, fix what caused it (the sender, the code, a code table), then resend,
  correct or dismiss it ([resend and correct](resend.md)).
- **A receiver's planned downtime is a pause**, made before it starts, with the ticket as the
  reason, and a resume after ([pause](cli.md#pause-and-resume-a-destination)).
- **Backlogs drain on their own.** A receiver that was down gets everything it missed, in order,
  when it's back; nothing needs resending.
- **Nothing to clean up.** Old messages go by themselves, partition by partition, at the
  retention you set; there's no purge job and no vacuum to schedule
  ([retention and storage](retention.md)).

## When something's wrong

| You see | Look at |
|---|---|
| A destination **stalled** | Its receiver is down, or one message keeps failing: [hotspots](monitoring.md#where-a-backlog-sits-hotspots) says which |
| A destination **behind** | It's sending, but slower than messages arrive: the receiver is slow, or a rate limit |
| A destination in **error** | Quarantined messages: [the quarantine](resend.md) |
| An interface **silent** or **missed** | Nothing received when it should have been: the sender, or the network between |
| An interface **stopped** | Its code ran away or ran out of memory three times: the logs and the quarantined messages say which message ([nuance](../nuances.md#a-runaway-message-is-quarantined-three-in-a-row-stop-the-interface-level-2)) |
| A sender says **connection refused** | Is the engine running, draining, or refusing it by `allow:`? [Logs](logs.md#what-the-lines-mean), `ductus doctor` |
| A **TLS** handshake fails | The engine logs the reason once a minute per sender; `ductus cert show` and `ductus doctor` ([TLS](../configure/tls.md)) |

What each state means exactly, and when it shows:
[the states](monitoring.md#health-what-each-state-means).

## Before you change anything

- **Upgrades**: `ductus upgrade --check` with the new version first, then `ductus replay` for the
  evidence a change board asks for ([resend and replay](resend.md#replay-a-period-before-a-change)).
- **A project change** (new code, a new destination): `ductus test` and `ductus validate --env
  prod` in CI, then deploy. On Linux, `systemctl reload ductus` deploys without refusing a single
  connection ([high availability](high-availability.md#deploying-and-upgrading-without-a-gap)).
- **A shorter retention** deletes messages, so it waits for a confirmation
  ([retention](retention.md#changing-retention)).
