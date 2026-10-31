# Logs

The engine writes one line per thing that happened worth knowing: a start, a pause, a delivery
that failed and when it will be tried again, a refused connection. States and counts are in the
console and Prometheus; the log is the *why*, in order, with times.

## Where they are

| | Linux | Windows |
|---|---|---|
| **The log** | the journal: `journalctl -u ductus -f` | `C:\ProgramData\Ductus\logs\ductus.<date>.log`, one file a day (named by the UTC date), the last 14 kept |
| **What operations watch** | the same journal (forward it to your SIEM as you do for other services) | also the **Event Log**, Application, source `Ductus`: starts, stops, failed starts, a stopped interface, critical alerts, and other errors at most once a minute per kind ([IDs](../reference/event-log.md)) |
| **How much** | `RUST_LOG` in `/etc/ductus/ductus.env` | `RUST_LOG` in `C:\ProgramData\Ductus\ductus.env` |
| **Kept** | as the journal is configured (`journalctl --disk-usage`) | 14 days of files |

`RUST_LOG` is `info` by default: every line on this page. `info,ductus_runtime=debug` adds detail
for the engine (each start phase and its time, each claim), for a support case; don't leave it on,
it's many lines per message. A restart applies a change (Linux: `systemctl restart ductus`).

Lines are plain text with fields (`interface=adt-in destination=lab`), so grep finds them: the
message comes first, then the fields (`grep 'delivery failed.*interface=adt-in'`). Under
`ductus run` in a terminal they're colored; elsewhere plain.

## What the lines mean

From a real engine, a start, a pause and resume, then a receiver that went down and a drain:

```text
INFO ductus_runtime: the engine's database database=plain, no TLS: the server doesn't offer it (sslmode=prefer)
INFO ductus_runtime::alerts: alerts on channels=1 rules=2 heartbeat=false
INFO ductus_runtime::certificates: no connection has a certificate: scheduled checks have nothing to do
INFO ductus_runtime::api: REST API listening (/api/v1) host=127.0.0.1 port=7700 https=false
INFO ductus_runtime: started interfaces=1 mllp=1 http=0 files=0 schedules=0 internal=0 shared=1 pool_threads=8 dedicated_threads=0 took=299.820201ms
WARN ductus_runtime::api::status: destination paused interface=adt-in destination=lab by="ops-probe" reason="lab system upgrade until 14:00, ticket CHG0042"
INFO ductus_runtime::worker: paused: waiting to be resumed interface=adt-in destination=lab slot=0
WARN ductus_runtime::api::status: destination resumed interface=adt-in destination=lab by="ops-probe"
WARN ductus_runtime::worker: delivery failed; retrying in 1s interface=adt-in destination=lab id=381 attempts=1 error=connection closed before ACK
WARN ductus_runtime::worker: delivery failed; retrying in 2s interface=adt-in destination=lab id=381 attempts=2 error=Connection refused (os error 111)
…
WARN ductus_runtime::worker: delivery failed; retrying in 60s interface=adt-in destination=lab id=381 attempts=7 error=Connection refused (os error 111)
INFO ductus_runtime::alerts: alert alert=dest:adt-in/lab kind="fired" state="stalled" severity="critical" notifications=1
WARN ductus_runtime::control: draining: not taking new messages; delivering what's stored stop=false
WARN ductus_runtime::worker: destination rejected; quarantined interface=adt-in destination=lab id=411 error=rejected (AR): rejected by the receiver
WARN ductus_runtime::control: drained: nothing left to deliver stop=false
INFO ductus_runtime: stopping: taking no new messages, finishing the ones in hand why="Ctrl-C" limit=30s
INFO ductus_runtime: stopped took=2.217222ms
```

(Each line starts with its time in UTC, `2026-10-31T10:34:06.069632Z`, left out here.)

| Line | Means | Act when |
|---|---|---|
| `started interfaces=N … took=` | every interface is up and taking messages; how long the start took | never: it's the "ready" line (systemd's `status` says the same) |
| `the engine's database database=…` | how the database connection is protected (TLS, verified or not) | it says plain or unverified in production: [TLS to the database](../install/postgres.md#tls-to-the-database) |
| `delivery failed; retrying in Ns … attempts=N error=…` | a temporary failure; the wait doubles to `max_backoff_ms` | the same id keeps failing: [hotspots](monitoring.md#where-a-backlog-sits-hotspots) |
| `destination rejected; quarantined … error=` | the receiver refused this message (an AR, a 4xx) | look at it the same day: [resend](resend.md) |
| `destination paused … by= reason=` / `resumed` | someone paused or resumed, with their name | |
| `alert … kind="fired"` / `"recovered"` | an alert opened or ended, and how many were told | `notifications=0` when you expected some: the rules or the channel |
| `draining` / `drained` | a drain started and finished | |
| `stopping … why=` / `stopped took=` | a clean stop, and why it stopped | a stop you didn't make |

A **start logs one line, not one per listener** ([nuance](../nuances/drain-and-api.md#a-start-logs-one-line-not-one-per-listener-level-2));
problems with one interface are their own lines. Things that could repeat many times a second
are said **once a minute** per sender or kind instead: a refused TLS handshake
([nuance](../nuances/tls-listening.md#a-refused-handshake-is-logged-once-a-minute-per-sender-level-3)), a
connection refused by `allow:` ([nuance](../nuances/allow.md#refusals-are-logged-once-a-minute-per-listener-and-address-level-3)),
so a misconfigured sender can't flood the log.

## When a sender says "connection refused"

The engine logs every refusal it makes, with the reason. If there's no line for the sender's
address, the engine never saw the connection: a firewall between, the wrong port, or the engine
not running (a stopped service, a drain, and a failed start close the ports on purpose:
[nuance](../nuances/upgrades.md#a-stopped-service-a-failed-start-and-a-drain-refuse-connections-on-purpose-level-2)).
`ductus ports` lists what the project listens on, and `ductus doctor` checks the ports from the
server.

## What not to look for in the logs

- **Message content.** The log names messages by id (`id=381`) and never prints a payload. Error
  texts are the receiver's answer and your code's quarantine reasons, which can quote a value:
  write reasons that name the field, not the patient. Look a message up by its id in the console.
- **Who read what.** That's the [audit log](cli.md#who-did-what-ductus-audit-export), kept six
  years, not the service log, kept days.
