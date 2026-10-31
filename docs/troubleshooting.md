# Troubleshooting by symptom

Find what you're seeing below. Each section shows the command that tells you what's going on, what
its output means, and what fixes it. Every output on this page is from a real engine, in a lab
with a lab system played by `ductus sink` and a sender by `ductus gen`.

**Four commands answer most questions**, in this order:

| Command | Answers | Needs |
|---|---|---|
| `ductus status` | What's waiting, sent, quarantined, per interface and destination; with a token, each one's health in words | the database; a token for health |
| `ductus hotspots` | Where a backlog sits and why: the message holding it up, the receiver's answer | a viewer token |
| `ductus doctor` | Every connection the project needs, checked from this server, step by step, with the fix for the step that fails | the project |
| The log | The *why*, in order, with times: `journalctl -u ductus -f` (Linux), `C:\ProgramData\Ductus\logs\` and the Event Log (Windows) | access to the server ([Logs](operate/logs.md)) |

The same answers are in the console: the Overview and the interface pages show health, the
Hotspots page the backlog, Connections the doctor's checks.

**By workload:** a *feed* (MLLP or HTTP in, stored, sent on) is what most of this page is about.
An *API* in respond mode stores nothing, so nothing waits: its caller gets the error instead, and
the log says why. A *batch* (files, SFTP, schedules) shows its problems at pickup, in the log, and
its messages afterwards behave like a feed's.

## Messages are stuck

**You see:** a receiving system says it isn't getting messages; the console shows a destination
*behind* or *stalled*; an alert about a backlog.

```text
$ ductus status
adt-in               pending      100
adt-in               sent         300

by destination:
adt-in → archive     sent         400
adt-in → lab         pending      100
adt-in → lab         sent         300

health:
adt-in               ok, received 8s ago
  → archive          ok, sent 8s ago
  → lab              ok, sent 1 min ago
```

The `lab` destination has 100 waiting and sent nothing for a minute. Health says *stalled* only
after a minute without progress ([what each state means](operate/monitoring.md#health-what-each-state-means)),
so early on, the counts and hotspots say more:

```text
$ ductus hotspots
adt-in → lab: 100 waiting in 20 of 64 lanes; blocked by message 301 (key 100001, lane 23, 4 attempts, waiting 8s): Connection refused (os error 111); all 99 behind it wait (one worker: order)
  lane   worker    waiting  first msg    key                     age attempts  last error
  7      0               5  312          100012                   8s        0  
  8      0               5  317          100017                   8s        0  
…
  23     0               5  301          100001                   8s        4  Connection refused (os error 111)
…
```

And the log, one line per failed try, each with when it tries again:

```text
WARN ductus_runtime::worker: delivery failed; retrying in 4s interface=adt-in destination=lab id=301 attempts=3 error=Connection refused (os error 111)
WARN ductus_runtime::worker: delivery failed; retrying in 8s interface=adt-in destination=lab id=301 attempts=4 error=Connection refused (os error 111)
```

**What it means, by what hotspots says:**

| Hotspots says | Meaning | Do |
|---|---|---|
| `Connection refused`, `timed out`, `no answer` on the first message, every lane waiting | The receiver is down or unreachable | Ask its owner; `ductus doctor --interface <name>` checks the way from this server. Nothing is lost: everything goes out, in order, when it's back |
| `AE: …` | The receiver answered *application error*: try again later | See [AE or AR](#a-partner-answers-ae-or-ar) |
| A TLS reason | The handshake fails | See [TLS](#a-tls-handshake-fails) |
| "blocked by message M" while other lanes were sent | One message keeps failing; the others of its worker wait behind it, to keep order | Look at message M: what the receiver says about it. Fix and let it go through, or make that destination give up after some tries (`retry.max_attempts`) so it's quarantined ([one stuck message](nuances/ordering.md#order-is-per-key-and-one-stuck-message-holds-up-its-key-level-3)) |
| paused, by whom and why | Someone paused it | `ductus resume` (or Resume on the console) once whatever they paused it for is done |
| Nothing waiting, but nothing arrives | The sender isn't sending | See [A sender says "connection refused"](operate/logs.md#when-a-sender-says-connection-refused); `monitor: { active_within: 30m }` makes the silence an alert next time |

**Once the receiver is back**, the destination waits out its current retry (doubling up to
`retry.max_backoff_ms`, 60 s by default), then sends everything, in order. In the lab, the 100
waiting were sent 8 s after the receiver came back
([up to a minute](nuances/monitoring.md#a-receiver-thats-back-can-stay-stalled-for-up-to-a-minute-level-2)).
Temporary failures are retried forever unless you say otherwise
([nuance](nuances/ordering.md#temporary-failures-retry-forever-by-default-level-3)).

**When the engine itself can't reach its database**, receiving goes on and sending pauses until
it's back ([nuance](nuances/engines.md#losing-the-database-connection-pauses-sending-not-receiving-level-3)):
see [Postgres connection errors](#postgres-connection-errors).

## A partner answers AE or AR

**You see:** a destination in *error* with quarantined messages, or messages waiting with a
receiver's text as their reason.

The two answers mean different things, and the engine treats them differently:

| The receiver answers | It means | The engine | Default |
|---|---|---|---|
| **AE** (application error) | "Something went wrong on my side; try again" | Tries again, like a receiver that's down; the message holds up its lane meanwhile | forever (`retry.max_attempts` sets a limit) |
| **AR** (application reject) | "I won't take this message" | Quarantines it at once, for a person to look at; the messages behind it go on | `on_reject: quarantine`; `retry` for receivers that answer AR when they're merely busy ([destinations](configure/destinations.md#mllp)) |

An HTTP receiver's answers map the same way: 408, 429 and 5xx are tried again, any other 4xx is
quarantined.

```text
$ ductus status
adt-in               sent         390
adt-in               quarantined  10

by destination:
adt-in → archive     sent         400
adt-in → lab         sent         390
adt-in → lab         quarantined  10

health:
adt-in               ok, received 15s ago
  → archive          ok, sent 15s ago
  → lab              error: 10 quarantined (last error: rejected (AR): rejected by the receiver)
```

The text after `rejected (AR):` is the receiver's own (MSA-3): the first thing to read, and what
to quote when you ask its owner. Each one is logged:

```text
WARN ductus_runtime::worker: destination rejected; quarantined interface=adt-in destination=lab id=395 error=rejected (AR): rejected by the receiver
```

The quarantine lists them (the console's Quarantine page, or the API; `ordering_key` and
`control_id` find the message on the receiver's side):

```text
$ curl -s -H "Authorization: Bearer $DUCTUS_API_TOKEN" 'http://127.0.0.1:7700/api/v1/quarantine?limit=2'
{
    "deliveries": [
        {
            "id": 395,
            "received_at": "2026-10-31T12:53:43.447943Z",
            "interface": "adt-in",
            "destination": "lab",
            "attempts": 0,
            "last_error": "rejected (AR): rejected by the receiver",
            "quarantined_at": "2026-10-31T12:53:58.559595Z",
            "ordering_key": "100015",
            "control_id": "95",
            "size": 150
        },
…
```

**Do:**

1. Read the receiver's reason, and the message (the console's message page shows it field by
   field).
2. If the *data* is wrong at the source, the sender fixes it and sends again; dismiss the
   rejected one with a reason ([Dismiss](operate/resend.md#dismiss)).
3. If *your mapping* is wrong, fix the code, [replay](upgrade.md#ductus-replay-what-it-does-to-real-traffic)
   to see what changes, deploy, then resend the quarantined ones: they go through the new code
   ([Resend](operate/resend.md#resend)). One message by hand: [Edit and resend](operate/resend.md#edit-and-resend).
4. If the receiver rejected it by mistake (it was down for maintenance and said AR), resend once
   it's back.

An AE that never clears looks like [stuck messages](#messages-are-stuck): hotspots shows `AE:`
and the receiver's text on the first message. If the receiver will never take that message, give
the destination a limit (`retry: { max_attempts: 10 }`) so it's quarantined instead of holding
up its lane ([delivery](configure/delivery.md)).

## A TLS handshake fails

**You see:** messages waiting for a receiver with a TLS reason; a sender who says they can't
connect since TLS was turned on; doctor ✗ on a TLS step.

**Outbound** (the engine to a receiver): each try fails with the reason, and what to change.
Here the receiver's certificate comes from a CA this server doesn't trust (a self-signed lab
certificate):

```text
WARN ductus_runtime::worker: delivery failed; retrying in 4s interface=adt-in destination=lab id=401 attempts=3 error=TLS: the receiver's certificate isn't from a CA we trust (name its CA in `tls.ca`, or install it on this server)
```

`ductus doctor` shows the same at the step that fails, with what the certificate says:

```text
$ ductus doctor --interface adt-in
…
✗ MLLP destination  127.0.0.1:6663 MLLP over TLS
    used by  adt-in → lab
    address  127.0.0.1 (an address, nothing to look up)
    connect  127.0.0.1:6663 from 127.0.0.1 in 0.2 ms
  ✗ TLS      not trusted: its issuer isn't one this server trusts (self-signed, or a private CA); certificate for 127.0.0.1, self-signed, valid until 2027-10-31 (364 days)
  → Name the issuing CA in `ca` (a `certificates:` entry in resources.yaml), or add it to this server's trusted certificates, or ask for a certificate from a public CA.
…
```

The fix, as it says: name the receiver's CA (here, its own certificate) in resources.yaml, and
point the destination's `tls:` at it:

```yaml
# resources.yaml
certificates:
  lab-ca:
    ca_file: tls/lab.pem
```

```yaml
# interfaces/adt-in/interface.yaml, in part
destinations:
  lab: { mllp: { host: 127.0.0.1, port: 6663, tls: { ca: lab-ca } } }
  archive: { file: { dir: out/archive } }
```

Deployed with a handover (`systemctl reload ductus` on a server; the lab ran `ductus handover`,
which is what the reload runs), the waiting messages went out within 2 s, and doctor agrees:

```text
✓ MLLP destination  127.0.0.1:6663 MLLP over TLS
    used by  adt-in → lab
    address  127.0.0.1 (an address, nothing to look up)
    connect  127.0.0.1:6663 from 127.0.0.1 in 0.3 ms
    TLS      TLS 1.3, post-quantum key exchange (X25519MLKEM768), trusted; certificate for 127.0.0.1, self-signed, valid until 2027-10-31 (364 days)
```

**Other reasons, and their fixes:**

| The reason says | Do |
|---|---|
| the certificate isn't valid for the name | Connect by a name the certificate lists (doctor shows them), or ask for one with the name you use |
| expired, or not valid yet | The receiver renews it; "not valid yet" can also be this server's clock |
| the receiver wants a client certificate | Name a pair in `tls: { certificate: … }` ([TLS to receivers](configure/tls.md#on-what-the-engine-connects-to)) |
| presents a CA's certificate as its own | A lab receiver made with `openssl req -x509`: name that exact certificate in `ca:` |
| "timed out waiting for ACK" right after turning `tls:` on | That port speaks plain MLLP: ask which port is TLS |

**Inbound** (a sender to the engine): the engine logs each refused sender once a minute with the
reason ("the sender isn't speaking TLS", "no client certificate", "a subject not on the list").
No line at all means the connection never reached the engine: a firewall, or the wrong port. The
full table is in [TLS: when it doesn't work](configure/tls.md#when-it-doesnt-work)
([why once a minute](nuances/tls-listening.md#a-refused-handshake-is-logged-once-a-minute-per-sender-level-3)).
With TLS 1.3 a sender whose client certificate is refused learns it at its first read, not when
it connects, so its own error may say the connection closed rather than the certificate.

## Postgres connection errors

**You see:** the engine won't start and names the database; or it runs, but sending has paused;
or messages wait on a lookup (`db()`).

**The engine's own database, at start.** The start says what's wrong with the URL in
`DUCTUS_DATABASE_URL` (on a server, in `/etc/ductus/ductus.env` or `C:\ProgramData\Ductus\ductus.env`):

```text
$ ductus run
Error: connecting to PostgreSQL at 127.0.0.1:40713: the password for `postgres` was refused: check the password in the database URL (DUCTUS_DATABASE_URL; characters like @ or / in it are written %40, %2F)
```

```text
$ ductus run
Error: connecting to PostgreSQL at 127.0.0.1:40713: there's no database `nosuch` on that server: create it (the PostgreSQL install guide says how) or correct its name in the database URL
```

```text
$ ductus run
Error: connecting to PostgreSQL: no host is named `db1.hospital.invalid` (DNS has no address for it): check the host in the database URL (DUCTUS_DATABASE_URL)
```

A refused connection is tried again for 30 s before the start gives up, so a database that's
starting at the same time as the engine (a server's boot) doesn't fail it
([nuance](nuances/store.md#a-start-waits-30-s-for-a-database-that-refuses-then-says-why-level-2)):

```text
$ ductus run
Error: connecting to PostgreSQL at 127.0.0.1:5999: refused (tried for 30 s): nothing accepts connections there. Postgres is down, listens on another port, or a firewall rejects it: check the host and port in the database URL (DUCTUS_DATABASE_URL), and that Postgres is running
```

`ductus doctor` checks the same connection step by step (address, connect, TLS, login, how fast a
commit is), and is the quicker way to try a fix:

```text
$ ductus doctor
…
✗ engine database  127.0.0.1:5999 Postgres
    used by  the engine (DUCTUS_DATABASE_URL)
    address  127.0.0.1 (an address, nothing to look up)
  ✗ connect  127.0.0.1:5999 from 127.0.0.1: refused
  → 127.0.0.1 is reachable but nothing accepts connections on port 5999: the service is down, listens on another port, or a firewall at the host rejects it. Ask its owner whether it's up on 5999.
```

| The start or doctor says | Do |
|---|---|
| the password was refused | Correct it in the URL (special characters percent-encoded); a secrets manager's value may have changed |
| no database `x` | Create it ([PostgreSQL](install/postgres.md#the-login-and-its-database)), or correct the name |
| the server refused the login (pg_hba.conf, the role) | The DBA allows this server's address for the login in `pg_hba.conf`, or creates the role |
| refused, or no answer | Postgres is down, on another port, or a firewall is in the way: [Ports](ports.md#what-the-engine-connects-to-out) lists what the engine connects to |
| a TLS reason (the certificate, a CA) | [TLS to the database](install/postgres.md#tls-to-the-database); the start and doctor name the URL parameter to add |

**While it runs.** If the database goes away, the engine keeps receiving (acknowledging only what
it could store) and pauses sending; it carries on by itself when the database is back
([nuance](nuances/engines.md#losing-the-database-connection-pauses-sending-not-receiving-level-3)).
Behind PgBouncer in transaction mode the engine can't hold its lock
([nuance](nuances/engines.md#pgbouncer-in-transaction-mode-cant-hold-the-engines-lock-level-3)).

**A database your code looks things up in** (`db("patient_db")`): a failed login or an
unreachable server makes the message wait and retry, not fail
([nuance](nuances/ordering.md#database-login-failures-make-messages-wait-not-fail-level-3)); the
reason is on the destination (hotspots) and in the log, and doctor checks every database in
resources.yaml the same way as the engine's.

## The service won't start

**You see:** `systemctl start ductus` fails or the service keeps restarting; on Windows the
service stops right after starting.

**Where the reason is:**

```sh
systemctl status ductus                 # the last lines, and the exit code
journalctl -u ductus -n 50              # the start's lines
```

```powershell
Get-Service Ductus
Get-WinEvent -FilterHashtable @{ LogName = 'Application'; ProviderName = 'Ductus' } -MaxEvents 10 | Format-List TimeCreated, Id, Message
```

On Windows, a failed start is Event Log **1003** with the reason; the full start is in
`C:\ProgramData\Ductus\logs\` ([Event Log IDs](reference/event-log.md)). **Before deploying**,
`ductus validate` finds most of what would stop a start (a configuration error, a type error, a
certificate, the lanes), without starting anything.

**The reasons a start refuses, as it says them:**

A mistake in a configuration file names the file, the key and what's allowed:

```text
$ ductus run
Error: in …/interfaces/adt-in/interface.yaml

Caused by:
    unknown field `destinatons`, expected one of `name`, `source`, `script`, `format`, `output`, `ordering`, `isolation`, `workers`, `dedicated_pools`, `durability`, `destination`, `destinations`, `retry`, `dedupe`, `rate_limit`, `timeout`, `about`, `retention`, `quarantined`, `monitor`, `alert`, `custom`
```

A port another program holds names the port and how to find who:

```text
$ ductus run
Error: interface `adt-copy`

Caused by:
    0: binding 0.0.0.0:6661
    1: port 6661 is in use: another program listens on it (another engine, or a test tool); find it with `sudo ss -ltnp 'sport = :6661'`, or give this one another port
$ sudo ss -ltnp 'sport = :6661'
State  Recv-Q Send-Q Local Address:Port Peer Address:PortProcess
LISTEN 0      128          0.0.0.0:6661      0.0.0.0:*    users:(("ductus",pid=1696,fd=28),("ductus",pid=1696,fd=17))
```

Another engine already running one of these interfaces on the same database is refused on
purpose, with **exit code 3**:

```text
$ ductus run
error: another engine is running one of this engine's interfaces on this database:
  adt-in
    owned by: DESKTOP-GCT6E6V, pid 1696, ductus 0.6.0-dev, started 2026-10-31 12:54:28 UTC (instance 18e39e6d3fcb787f-6a0)
Two engines running one interface would both send its messages: duplicates, and per-patient order breaks.
Stop that engine first, or remove these interfaces from this one's project.
```

And the [database errors](#postgres-connection-errors) above.

**How the service manager retries:**

| Exit | Means | Linux (systemd) | Windows |
|---|---|---|---|
| 3 | Refused on purpose: another engine owns an interface, or a change to an interface's lanes would strand waiting messages | Not retried (`RestartPreventExitStatus=3`): fix it, then `systemctl start ductus` | Event 1003; the service stays stopped: fix it, then `Start-Service Ductus` |
| 1 | Anything else (a configuration error, the database, a port, a certificate) | Retried every 5 s, so it starts by itself once the cause is gone; each try is in the journal | Event 1003; the service stays stopped: fix it, then `Start-Service Ductus` |

On Windows the service's recovery settings restart it only when a *running* engine dies (after a
minute, twice a day); a start that fails waits for you.

While a start fails, the ports are closed, so senders are refused: that's on purpose, so they
don't wait on an engine that isn't there
([nuance](nuances/upgrades.md#a-stopped-service-a-failed-start-and-a-drain-refuse-connections-on-purpose-level-2)).
On Windows, a service account with a password leaves the service stopped after an install or
upgrade until `ductus service account NAME --start` (event 1005); a missing "Log on as a service"
right is [a Group Policy matter](nuances/windows.md#log-on-as-a-service-the-installer-grants-it-group-policy-can-take-it-away-level-3).

## Other symptoms

| Symptom | Where to look |
|---|---|
| A sender says "connection refused" | [Logs: when a sender says "connection refused"](operate/logs.md#when-a-sender-says-connection-refused) |
| A sender is refused by `allow:` | The log names the address and the list ([who may connect](configure/sources.md)) |
| An alert didn't reach anyone | `ductus alerts test` ([Alerts in practice](operate/alerts.md)) |
| An interface was stopped (its code ran past its timeout three times) | [Runaway scripts](nuances/runaway.md#a-runaway-message-is-quarantined-three-in-a-row-stop-the-interface-level-2); the code needs a fix and the engine a restart |
| A certificate is about to expire | [Renewing](configure/tls.md#renewing): replace both files, no restart |
| Someone can't sign in | [People](configure/people.md) |
| Upgrade or deploy problems | [Upgrade](upgrade.md) |
