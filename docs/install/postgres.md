# PostgreSQL

Ductus keeps everything in PostgreSQL: every message as received, each destination's delivery
and its status, the queue, the audit log, accounts, alerts. There is no other store and no
message broker. This page is what to ask your DBA for, and why; it's written so you can hand it
to them.

**What the engine does in its database, by itself:** creates its tables at its first start, and
applies a new version's changes at that version's first start (they only ever add: the version
before keeps working on them); makes each day's partitions ahead of time, hourly; drops whole
partitions once their messages are past retention (no row-by-row deletes, so no bloat from them);
holds one session for the whole time it runs, with an advisory lock that says which engine owns
which interfaces. It needs no superuser, no extension, and no cron job or scheduled task.

**What it doesn't do:** back the database up, replicate it, or tune it. Those are the DBA's, as
for any database that matters ([backups](#backups-are-the-dbas)).

## Version

**PostgreSQL 18 is what Ductus is tested on**, on every push, and what `ductus dev` runs. Any
version from **14** on should work: the oldest feature the engine relies on is
`DETACH PARTITION … CONCURRENTLY` (new in 14), which lets it drop old messages without blocking
anyone. 14–17 aren't tested yet. For a new install, use 18.

Managed services work the same: Azure Database for PostgreSQL (flexible server), Amazon RDS and
Aurora PostgreSQL, Google Cloud SQL. They require TLS: see [TLS](#tls-to-the-database).

## The login and its database

Give Ductus **a database of its own, owned by its login**:

```sql
CREATE ROLE ductus LOGIN PASSWORD '…';
CREATE DATABASE ductus OWNER ductus;
```

(Or, on the database server, `sudo -u postgres createuser --pwprompt ductus` and
`sudo -u postgres createdb --owner ductus ductus`.)

- **Why the owner:** the engine creates and changes its own tables, functions and partitions: at
  each new version's first start, and every hour for partitions. Owning the database (and so its
  `public` schema) is the one grant that covers all of it, now and in later versions.
- **Why its own database:** its tables are named for it (`messages`, `deliveries`, `users`…) and
  live in `public`; sharing a database with an application invites a clash. Several *engines*
  share one database without trouble: each runs its own interfaces, and an engine started with
  interfaces another runs is refused
  ([nuance](../nuances/engines.md#an-interface-runs-on-one-engine-at-a-time-a-second-one-is-refused-level-2)).
- **No superuser**, no `CREATEDB`, no `CREATEROLE`, no extensions.
- **A read-only login** for checks and audits: `ductus upgrade --check` and `ductus replay` only
  read, so give them one that can't write (Postgres 14+):

  ```sql
  CREATE ROLE ductus_check LOGIN PASSWORD '…';
  GRANT pg_read_all_data TO ductus_check;
  ```

Then the engine's URL, in `DUCTUS_DATABASE_URL`:

```text
postgres://ductus:PASSWORD@db1.hospital.org:5432/ductus?sslmode=verify-full
```

A password with `@`, `:`, `/`, `?` or `#` in it must be percent-encoded in the URL (`@` is `%40`).
The URL means what it means to `psql` and every libpq tool, so a URL that works there works here.

**Postgres on the engine's own server** can skip the password and TLS: connect over its Unix
socket, `postgres:///ductus?host=/var/run/postgresql`, and Postgres's `peer` login maps the
`ductus` service user to the `ductus` role (`local ductus ductus peer` in `pg_hba.conf`, often
there already as `local all all peer`). It's also the fastest connection.

## Connections

The engine opens up to **32 connections** to its database (`DUCTUS_DB_CONNECTIONS`), as it needs
them, **plus one** it holds for its lock. Budget for:

- each engine on the database: 33;
- **twice that for a few seconds during a Linux upgrade or deploy**: the new engine starts beside
  the running one, with its own pool, before the old one leaves
  ([handover](../nuances/upgrades.md#systemctl-reload-hands-over-receiving-never-stops-sending-pauses-briefly-level-2));
- a few more for the command line (`ductus status`, `doctor`, `user`) and the DBA's own tools.

The pool is for the engine's own database only. The databases interfaces query (`db()`, in
resources.yaml) have pools of their own, set per entry
([`max_connections`](../reference/config/resources.md#databases)).

**PgBouncer and other poolers:** session mode only. In transaction mode the engine's lock is
lost at once, since each transaction lands on another server connection
([nuance](../nuances/engines.md#pgbouncer-in-transaction-mode-cant-hold-the-engines-lock-level-3)). The
engine pools its own connections, so a pooler in front of it saves little.

## TLS to the database

Without `sslmode` in the URL, the connection is libpq's `prefer`: TLS when the server offers it,
and **the server's certificate is never checked**. That stops someone listening, not someone
answering in the database's place. For a database on another machine, ask for its certificate
to be checked
([nuance](../nuances/tls-postgres.md#without-sslmode-the-database-connection-is-tls-when-offered-and-nobody-is-checked-level-3)):

| `sslmode` | TLS | Checked | Use for |
|---|---|---|---|
| `disable` | no | | A database on this machine over TCP, when nothing else is possible (prefer the Unix socket). |
| `prefer` (the default) | if offered | nothing | Nothing you'd choose: it's the default only so a URL that works in `psql` works here. |
| `require` | yes | nothing | A network you trust, against listening only. |
| `verify-ca` | yes | a trusted CA issued it | Rarely: the name isn't checked. |
| `verify-full` | yes | a trusted CA issued it, **for this host name** | **Every database on another machine.** |

The CA is trusted when the engine's server trusts it (Linux: `update-ca-certificates`; Windows:
the machine's store), or when the URL names it: `&sslrootcert=/etc/ductus/tls/db-ca.pem`
([nuance](../nuances/tls-postgres.md#a-ca-installed-on-this-server-counts-level-2)). The engine's start log,
`ductus validate`, `ductus doctor` and the web UI's Connections page all say how the connection
is protected ("TLS 1.3, verified (sslmode=verify-full)"), and warn when a database on another
machine isn't verified. A verified database's certificate is also watched: its expiry alerts at
30, 14 and 7 days like every other certificate the engine uses.

**Managed Postgres** ([nuance](../nuances/tls-postgres.md#cloud-postgres-requires-tls-level-1)):

- **Azure**: `sslmode=verify-full`. Its certificates chain to public roots servers already
  trust.
- **Amazon RDS / Aurora**: `sslmode=verify-full&sslrootcert=/etc/ductus/tls/rds-global-bundle.pem`,
  with Amazon's RDS CA bundle downloaded from AWS: its CAs aren't in the system's store.
- **Google Cloud SQL**: `sslmode=verify-full&sslrootcert=…` with the instance's server CA,
  downloaded from the console. Connect by a name its certificate has.

Always connect by the endpoint's DNS name, the one on its certificate, not its address.

**A client certificate** instead of (or beside) a password, where the DBA logs the engine in by
certificate (`cert` in `pg_hba.conf`): `&sslcert=/etc/ductus/tls/db-client.pem&sslkey=/etc/ductus/tls/db-client.key`.
A renewed pair is used by the next new connection
([nuance](../nuances/tls-postgres.md#a-client-certificate-renews-at-the-next-connection-level-3)).

A verified connection's certificate (`verify-full`, `verify-ca`) is watched like any other:
alerts at 30, 14 and 7 days ([nuance](../nuances/tls-postgres.md#the-databases-certificate-is-watched-when-the-engine-checks-it-level-3)). Opening a
connection over TLS costs about 60 ms for now, which pools hide
([measured](../nuances/tls-postgres.md#opening-a-database-connection-over-tls-costs-60-ms-for-now-level-4)).

## Sizing

**The disk decides the speed.** Every message acknowledged with `durability: strict` (the
default) waits for one durable commit, so a sender that sends one message at a time can't go
faster than the database's disk can flush: about 400 a second at 2.4 ms a flush. Commits from
many senders at once are grouped, so the engine as a whole goes much faster. `ductus doctor`
measures the commit time from the engine's server and says what it means
([nuance](../nuances/doctor.md#the-commit-time-is-the-stores-disk-and-the-ceiling-per-strict-sender-level-4)).
Put Postgres's write-ahead log on the fastest disk you have (NVMe, or a cloud disk with low write
latency). For scale: a million HL7 messages a day averages 12 a second.

**Space** is roughly: messages a day × their average size × the days kept (retention: 7, 21 or
49 days; [retention](../reference/config/ductus.md#retention)), plus a small row per destination
per message (its delivery and status), the indexes, what destinations keep with
[`save_output`](../reference/config/interface.md#destinationsave_output) (about 1.15 GB per
million deliveries, measured), and anything waiting or quarantined past its window (kept until
it's resolved: [the held area](../nuances/retention.md#pending-messages-are-never-purged-past-the-window-theyre-held-level-2)).
Measure it on your own traffic after a week: `SELECT pg_size_pretty(pg_database_size('ductus'))`.
Space comes back as whole partitions are dropped, a day or a week at a time: there's nothing to
vacuum away.

**Long queries** against the message tables (a report run on the store) can make the engine's
hourly partition work wait; keep them short, or run them on a replica
([nuance](../nuances/store.md#a-query-that-holds-the-message-tables-for-days-can-pause-intake-level-4)).
Autovacuum's defaults are fine.

## Backups are the DBA's

Back the database up as you back up any database that holds records you must not lose: with
continuous WAL archiving and base backups (pgBackRest, Barman, or the managed service's own
point-in-time restore). The engine doesn't take backups and needs nothing from them; nothing in
it is scheduled outside the database.

What a backup means for messages:

- **A restore to an earlier point loses what was acknowledged after it.** Senders were told those
  messages arrived, so they won't send them again: get them resent from the source systems for
  the gap. The same holds for a failover to a replica that was behind (asynchronous replication):
  if no message may be lost, the DBA's answer is synchronous replication to the standby.
- **The engine reconnects by itself** after a restart or failover of the database. While it's
  cut off, nothing can be stored, so nothing is acknowledged and senders try again; when it
  can't hold its lock, it pauses sending until it can
  ([nuance](../nuances/engines.md#losing-the-database-connection-pauses-sending-not-receiving-level-3)).
- **Messages are kept 1, 3 or 7 weeks** by design, not forever: longer history belongs in the
  source systems or an archive, not in backups of this database.
