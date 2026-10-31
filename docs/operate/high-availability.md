# High availability and lanes

What keeps interfaces running through a restart, an upgrade, a dead server or a database
failover, and how the work inside an interface is spread so one slow patient or one bad message
doesn't hold up the rest. Read the first section before you plan a second server: what Ductus
does today is **one engine per interface, a standby you start**, and no automatic failover yet.

## What protects what

| Event | What happens | Senders see |
|---|---|---|
| A restart (Linux) | systemd keeps the listening sockets across it | nothing refused; ~0.2–0.3 s wait for an ACK ([measured](../nuances.md#on-linux-a-restart-refuses-no-connection-senders-wait-for-it-level-2)) |
| A crash (Linux) | systemd restarts it after 5 s, with the same sockets | nothing refused; ~5 s wait |
| A project deploy or an upgrade (Linux) | `systemctl reload`: the new engine starts beside the old one and takes over | nothing refused; sending paused ~35 ms (1 interface) to ~0.6 s (1,001) ([measured](../nuances.md#systemctl-reload-hands-over-receiving-never-stops-sending-pauses-briefly-level-2)) |
| A restart (Windows) | the service stops and starts | a short gap: measured 0 refused, a few resets, ~1.1 s longest wait |
| An upgrade (Windows) | the MSI stops, replaces and starts the service | down 3.1–3.5 s: a few refused, nothing lost ([nuance](../nuances.md#windows-and-containers-a-restart-is-a-short-gap-level-2)) |
| The database restarts or fails over | sending, file pickup and schedules pause; receiving goes on; the engine reconnects and catches up | nothing, as long as the database takes writes ([nuance](../nuances.md#losing-the-database-connection-pauses-sending-not-receiving-level-3)) |
| A receiver is down | its destination retries, in order, until it's back; other destinations carry on | nothing |
| The engine's server dies | nothing runs until an engine is started on another server | refused until then |

Nothing acknowledged is lost in any of these: a message is stored in the database before its
sender gets the ACK, and stays until every destination has finished with it. What a *sender*
experiences is the column on the right; senders resend by themselves when refused, as MLLP
senders do.

## One engine per interface

Several engines can share one database, each running **different** interfaces: that's how you
spread a large project over servers (feeds on one, APIs on another). **An interface runs on one
engine at a time**: an engine started with an interface another live engine is running refuses
to start, exit 3, naming the other engine's host and pid
([nuance](../nuances.md#an-interface-runs-on-one-engine-at-a-time-a-second-one-is-refused-level-2)).
Two engines on one interface would both send each message, and one patient's order would break.

The Overview's **other engines on this database** card, and `GET /api/v1/engine` `others`, list
them. A refused start is in the audit log (`engine.refused`).

The exception is an HTTP source in `respond` mode: it has no queue, so several engines can answer
it behind your load balancer.

## A standby server (disaster recovery)

Keep a second server installed and configured with the same project and database, **with the
service stopped**. When the first server is gone:

1. Make sure the first engine is really down (or its server is): if it's alive and cut off from
   you but not from the database, two engines would fight, and the second is refused anyway.
2. Start the standby's service. It claims the interfaces as soon as the first engine's database
   session is gone: **at once** if the process ended (a crash, `kill -9`: measured 0.3 s), **within
   about 25 s** if the whole host vanished (the database's keepalive probes notice)
   ([nuance](../nuances.md#a-dead-engines-interfaces-are-free-at-once-a-vanished-hosts-within-25-s-level-3)).
3. Point senders at it: a DNS name or a virtual IP that moves, so nobody reconfigures an EHR
   during an outage. Its `allow:` lists and certificates must cover the names senders use.
4. It sends what was waiting, in order, then everything new.

Practice it: start the standby against a test database with the same project, and time the
switch.

**Not yet: a standby that takes over by itself.** An automatic failover, where a waiting engine
claims a dead one's interfaces with nobody starting it, is planned with clustering (v1.2).
Until then the switch is a person's (or your cluster manager's) decision, on purpose: a split
network that started a second engine automatically could make the duplicates the one-owner rule
prevents.

## The database is the single point to protect

Everything the engine knows is in Postgres: the messages, the queues, the audit log, the people.
**Its availability is the engine's**, so it needs what your DBA gives any critical database:

- **Backups** with point-in-time recovery (pgBackRest or WAL archiving). The engine needs no cron
  job of its own ([backups](../install/postgres.md#backups-are-the-dbas)).
- **A replica to fail over to.** The engine reconnects after a failover by itself. With
  asynchronous replication, messages acknowledged in the last moments before the failover can be
  missing on the replica: if no acknowledged message may be lost, the DBA's answer is synchronous
  replication to the standby.
- **A direct connection, or a pooler in session mode**: PgBouncer in transaction mode can't hold
  the engine's lock ([nuance](../nuances.md#pgbouncer-in-transaction-mode-cant-hold-the-engines-lock-level-3)).

## Deploying and upgrading without a gap

- **Linux**: `systemctl reload ductus` for a project change, and the package's upgrade for a new
  version, both hand over: the new engine starts on the same listening sockets, takes over once
  it's ready, and the old one stops. Receiving never stops. A new version or project that can't
  start leaves the running one serving, and the reload says why
  ([nuance](../nuances.md#a-new-version-or-project-that-cant-start-leaves-the-running-one-serving-level-2)).
  No drain, no maintenance window for the senders.
- **Windows**: a restart or the MSI is a short gap (above): pick a quiet moment. Zero-downtime
  Windows upgrades are planned for v1.0.
- **Before every upgrade**: `ductus upgrade --check` with the new version, and `ductus replay`
  for what it does to real traffic ([replay](resend.md#replay-a-period-before-a-change)).

## Lanes: how an interface spreads its work

Inside an interface, each destination is its own queue. With an
[`ordering.key`](../reference/config/interface.md#orderingkey) (the MRN), the queue is split
into **lanes** by the key's hash (64 by default, `ordering.partitions`); each lane is read by one
worker at a time, so one patient's messages stay in order while different patients go in
parallel. Without a key, the whole destination is one lane: strict arrival order, one message at
a time ([order](../configure/delivery.md#order-ordering)).

What that means when you run it:

- **A message that keeps failing holds up its lane**, and the lanes its worker reads; the other
  workers' lanes go on ([nuance](../nuances.md#order-is-per-key-and-one-stuck-message-holds-up-its-key-level-3)).
  `ductus hotspots` shows which message. For receivers that refuse a message for good with errors
  that look temporary, `retry.max_attempts` quarantines it so its lane moves.
- **More workers help only with a key**, and only up to about the number of cores; they need
  `isolation: dedicated` ([dedicated threads](../configure/delivery.md#dedicated-threads-and-connections)).
- **One hot patient** (a batch load sent as one patient's updates) goes as fast as one worker,
  whatever the number of workers: that's what order per patient costs.
- **Change `partitions` only with an empty queue**: lowering it with messages waiting is refused
  at start, raising it can reorder a patient's waiting and new messages
  ([nuance](../nuances.md#change-orderingpartitions-only-with-an-empty-queue-level-4)). `ductus
  drain --wait`, change, deploy, `ductus resume`.
- **Several destinations are independent**: one receiver down holds up only its own queue
  ([nuance](../nuances.md#one-destination-down-holds-up-only-itself-level-2)).
