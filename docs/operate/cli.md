# The CLI day to day

Everything the console does, the `ductus` command does from a terminal, for runbooks, scripts
and the moments you're already on the server. Every command's options are in the
[command-line reference](../reference/cli/index.md); this page is which one to reach for, and
what it tells you. The outputs below are from a real engine.

## Which engine, as whom

Commands that act on the running engine talk to its REST API (`--api`, `DUCTUS_API_URL`,
`http://127.0.0.1:7700` by default) with a token (`--token`, `DUCTUS_API_TOKEN`). Commands that
read the database directly need its URL (`DUCTUS_DATABASE_URL`). On the server, the service's
environment file already has the URL; give the token yourself:

```text
$ ductus token create ops-probe --role operator --permission payloads
dux_12b318581a5c9072d1603a927b0fd7c869aea43730050f668b48a94597568401
token `ops-probe` (operator) created. This is the only time it's shown: keep it in a secret store.
Use it as `Authorization: Bearer <token>`, or DUCTUS_API_TOKEN for `ductus drain`.
```

- **One token per person or tool**, named after it (`ops-desk`, `monitoring`, `ci`): every
  action made with it is in the audit log under that name, and revoking one
  (`ductus token revoke ops-desk`) stops only it.
- **The smallest role that does the job**: `viewer` for status, hotspots and metrics; `operator`
  to pause, resend, drain. A token reads payloads only with `--permission payloads`.
- `ductus token create` and `ductus user` work on the database directly (that's how the first
  admin is made), so they need `DUCTUS_DATABASE_URL`, not a token.

| Command | Does | Needs |
|---|---|---|
| `ductus status` | Counts by interface and status; with a token, health | database; token for health |
| `ductus pause` / `resume` | Pause or resume one destination | operator token |
| `ductus hotspots` | Where a backlog sits, and why | viewer token |
| `ductus drain` / `resume` | Stop taking messages, deliver what's stored | operator token |
| `ductus alerts` | What's open; `alerts test` sends a test notification | viewer token; the project for `test` |
| `ductus retention` | Retention per interface, changes, confirmations | viewer token; operator to `confirm` |
| `ductus doctor` | Every connection the project needs, checked from here | the project |
| `ductus audit export` | The audit log, as JSON lines | database |
| `ductus user`, `token` | Accounts and tokens | database |

## How things are: `ductus status`

```text
$ ductus status
adt-in               sent         300

by destination:
adt-in → archive     sent         300
adt-in → lab         sent         300

health:
adt-in               ok, received 3s ago
  → archive          ok, sent 3s ago
  → lab              ok, sent 3s ago
```

The counts come from the database, so they work while the engine is down (on the server, after
a crash, to see what's waiting). The **health** lines come from the running engine and need a
token: each interface and destination in the words the console uses. When something's wrong it
says what and how much:

```text
health:
adt-in               ok, received 1 min ago
  → archive          ok, sent 1 min ago
  → lab              stalled: nothing sent for 1 min, 30 waiting; retrying: Connection refused (os error 111)
```

## Pause and resume a destination

For a receiver's planned downtime (an upgrade, a weekend cut-over), pause its destination
**before** it goes down: nothing is sent or retried, messages keep arriving and wait in order,
and a paused destination never alerts ([nuance](../nuances/destinations.md#pausing-a-destination-at-once-no-questions-and-it-holds-level-2)).

```text
$ ductus pause adt-in lab
`adt-in` destination `lab` paused by ops-probe at 2026-10-31 06:34: nothing is sent until `ductus resume adt-in lab` (0 waiting)
no reason yet: add one when you can with `ductus pause adt-in lab --reason "..."`
$ ductus pause adt-in lab --reason "lab system upgrade until 14:00, ticket CHG0042"
`adt-in` destination `lab` paused by ops-probe at 2026-10-31 06:34: nothing is sent until `ductus resume adt-in lab` (0 waiting)
reason: lab system upgrade until 14:00, ticket CHG0042
```

- **Act first, explain after**: in an incident, pause without a reason and add it when you can.
  The reason shows on the destination's card, in `status` and in the audit log, so the next shift
  knows why and until when.
- **The interface keeps receiving**: senders get their ACKs as usual. Only this destination
  waits; the interface's other destinations carry on.
- **Resume** sends the backlog in order:

```text
$ ductus resume adt-in lab
`adt-in` destination `lab` resumed (paused since 2026-10-31 06:34): sending 80 waiting, in order
```

**When not to pause:** for an unplanned outage you haven't seen yet, nothing is needed: the
destination retries by itself and sends everything when the receiver is back. A pause is for
when you *know* it's down and want it quiet.

## Where a backlog sits: `ductus hotspots`

```text
$ ductus hotspots adt-in --lanes 3
adt-in → lab: 30 waiting in 8 of 8 lanes; blocked by message 381 (key 102001, lane 1, 7 attempts, waiting 1 min): Connection refused (os error 111); all 29 behind it wait (one worker: order)
  lane   worker    waiting  first msg    key                     age attempts  last error
  1      0               6  381          102001                1 min        7  Connection refused (os error 111)
  6      0               6  388          102008                1 min        0
  0      0               3  382          102002                1 min        0
  (5 more lanes waiting: --lanes N lists more)
```

The first line is the diagnosis. Here every message waits behind message 381, which can't reach
the lab: the receiver is down, and nothing is to be done on this side. When instead the others
were sent and one lane is stuck, it says "look at this message": one message the receiver keeps
refusing with a temporary error, holding up its patient's lane
([monitoring](monitoring.md#where-a-backlog-sits-hotspots)).

## Drain before a change

`ductus drain` stops taking new messages (MLLP listeners close, queued HTTP answers 503, file
pickup and schedules pause) and delivers everything stored. Use it before moving the engine to
another server, or a change that needs empty queues (fewer `ordering.partitions`).

```text
$ ductus drain --wait
not taking new messages; delivering what's stored
draining: 31 pending (adt-in)
drained
$ ductus resume
accepting: taking new messages
```

- `--stop` exits the engine once drained (the service manager then sees a clean stop).
- **Senders are refused while it drains**, on purpose: they hold their messages and resend once
  the engine takes them again ([nuance](../nuances/upgrades.md#a-stopped-service-a-failed-start-and-a-drain-refuse-connections-on-purpose-level-2)).
- **A drain isn't needed for an upgrade or a project deploy** on Linux: `systemctl reload ductus`
  hands over without refusing anything ([high availability](high-availability.md#deploying-and-upgrading-without-a-gap)).
- A drain finishes only when every destination has caught up, so a receiver that's down keeps it
  draining ([nuance](../nuances/drain-and-api.md#a-drain-finishes-only-when-every-destination-has-caught-up-level-4)).
  Drain mode isn't remembered across a restart.

What a drain does to each kind of interface: MLLP listeners close (a message half received when
it started is the sender's to send again: no AE is answered,
[nuance](../nuances/drain-and-api.md#draining-closes-mllp-listeners-it-doesnt-answer-ae-level-4)); respond-mode HTTP keeps answering, since
nothing waits in a queue for it ([nuance](../nuances/drain-and-api.md#respond-mode-http-keeps-answering-while-draining-level-4)); and a drain isn't
remembered across a restart: a restarted engine takes messages again
([nuance](../nuances/drain-and-api.md#drain-mode-isnt-remembered-across-a-restart-level-4)).

## Alerts

```text
$ ductus alerts
CRITICAL  adt-in → lab: stalled since 2026-10-31 06:35  [alert 3]
          stalled: nothing sent for 1 min, 30 waiting; retrying: Connection refused (os error 111)
```

What's open now. `ductus alerts test chat` sends a test notification to the `chat` channel
straight from ductus.yaml, without an engine: try every channel before going live
([alerts in practice](alerts.md)).

## Check every connection: `ductus doctor`

Run on the engine's server (where the firewall rules apply), it checks every connection the
project needs: DNS, TCP from which address, TLS and the certificate, then each system's own login.
It never sends a message to a destination.

```text
$ ductus doctor
ductus doctor · environment dev · 2026-10-31 06:35:42 -04:00
on DESKTOP-GCT6E6V · ductus 0.6.0-dev
checked 5 connection(s) in 0.0 s: 4 ok, 1 warning(s), 0 failed

✓ engine database  127.0.0.1:41293 Postgres
    used by  the engine (DUCTUS_DATABASE_URL)
    address  127.0.0.1 (an address, nothing to look up)
    connect  127.0.0.1:41293 from 127.0.0.1 in 0.2 ms
    TLS      none: the server doesn't offer TLS (sslmode=prefer), the connection is plain
    login    as `postgres`, database `ductus`: PostgreSQL 18.6; plain, no TLS: the server doesn't offer it (sslmode=prefer)
    commits  0.1 ms median of 20 (each waits for the disk): strict ingest ≈ 10000 msg/s per sender connection, one commit per ACK; more connections share commits. A 1M/day ADT feed averages ~12 msg/s.

✓ MLLP destination  127.0.0.1:6670 MLLP
    used by  adt-in → lab
…
⚠ listening port  0.0.0.0:6661 MLLP
    used by  adt-in
  ⚠ port     in use: by a running engine, or another program
  → If the engine isn't running on this server, another program holds port 6661: find it with `ss -ltnp` (Linux) or `netstat -ano` (Windows).
```

- **Run it after any network change**, a firewall request, a certificate renewal, and when a
  destination is stalled and you want to know whether the receiver is reachable at all.
- A **pass** means "can connect and log in", not "will accept your messages"
  ([nuance](../nuances/doctor.md#a-pass-means-can-connect-and-log-in-not-will-accept-your-messages-level-2)).
- The `commits` line is the store's disk: on a laptop (here) it's quick; on a server, what it
  says per sender connection is the ceiling for strict senders. A port "in use" while the engine
  runs is expected, as here.
- The engine runs the certificate part of it on its own every 6 hours
  ([`doctor`](../reference/config/ductus.md#doctor) in ductus.yaml); the console's
  **Connections** page shows the last runs and has **Check now**.

- **It checks from where it runs.** DNS, routes, firewalls and the source address in a firewall
  request are this machine's: a laptop on the VPN and the engine's server can get opposite
  answers. Run it on the server ([nuance](../nuances/doctor.md#doctor-checks-from-where-it-runs-level-2)).
- **A name with several addresses** (IPv4 and IPv6, a cluster) passes when one of them works,
  with a warning: the engine may try a failing one first and be slow
  ([nuance](../nuances/doctor.md#one-working-address-is-enough-to-pass-with-a-warning-level-3)).

## Who did what: `ductus audit export`

```text
$ ductus audit export --since 2026-10-31
5 entries
{"id":1,"at":"2026-10-31T10:34:05.714979Z","actor_kind":"system","actor":"todd","via":"cli","ip":null,"action":"token.create","subject":"ops-probe","interface":null,"message":null,"outcome":"ok","detail":{"role":"operator","permissions":["payloads"]}}
{"id":2,"at":"2026-10-31T10:34:06.045144Z","actor_kind":"system","actor":"engine","via":"engine","ip":null,"action":"engine.start","subject":null,"interface":null,"message":null,"outcome":"ok","detail":{"host":"DESKTOP-GCT6E6V","version":"0.6.0-dev","instance":"18e396c42b8b8823-620","previous":"first","environment":"dev","previous_start":null}}
{"id":3,"at":"2026-10-31T10:34:10.284626Z","actor_kind":"token","actor":"ops-probe","via":"token","ip":"127.0.0.1","action":"destination.pause","subject":"adt-in/lab","interface":"adt-in","message":null,"outcome":"ok","detail":{"reason":null}}
…
```

One JSON object per line, oldest first: for a SIEM, or an auditor's question ("who resent
messages for this patient last month?" is `--action message --interface adt-in --since …`).
Filters: `--since`, `--until`, `--actor`, `--action` (an action or a group: `message`, `user`,
`session`), `--interface`, `--message`. The export is itself logged. Every action is listed in
[audit actions](../reference/audit.md).

## Retention

```text
$ ductus retention
adt-in                       7w
```

Each interface's retention, and, when there are any, changes in progress, shorter values waiting
for `ductus retention confirm <interface>`, and expired partitions that had to stay because they
held messages someone still needs ([retention and storage](retention.md)).

## On the server: the service

| | Linux | Windows |
|---|---|---|
| Start, stop | `systemctl start\|stop ductus` | `Start-Service Ductus`, `Stop-Service Ductus` |
| Deploy a project change | `systemctl reload ductus` (no gap) | `Restart-Service Ductus` |
| Is it running | `systemctl status ductus` | `Get-Service Ductus` |
| Its logs | `journalctl -u ductus` | `C:\ProgramData\Ductus\logs`, the Event Log |

[Linux](../install/linux.md#start-stop-look), [Windows](../install/windows.md#start-stop-look),
[logs](logs.md).
