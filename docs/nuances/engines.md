# Nuances: several engines on one database

What could surprise you here, why it is so, and what to do. Each entry has a level, 1 (day one) to 5 (expert): the [index](index.md) lists them all by level.

## An interface runs on one engine at a time; a second one is refused (level 2)
- **What:** every engine holds a lock on the database while it runs and claims its interfaces when it starts. An engine started with an interface another live engine runs **refuses to start** (exit 3), whatever else it has: "another engine is running 2 of this engine's interfaces on this database: adt, orders; owned by: host-b, pid 4120, ductus 0.6.0, started …". The refusal is in the audit log (`engine.refused`). Engines with *different* interfaces share a database without trouble; each lists the others (Overview, `GET /engine` `others`). Respond-mode HTTP interfaces aren't claimed: they have no queue, so several engines may answer them behind a load balancer.
- **Why:** two engines running one interface both read its queue: both send each message (duplicates), and per-patient order breaks. A DR server booted with production's config, or a test VM pointed at the production database, did exactly that before v0.6.
- **What to do:** stop the other engine first (the message names its host and pid), or take the interfaces out of this one's project. A refused start isn't retried by the service manager (systemd: `RestartPreventExitStatus=3`; Windows restarts only after a crash): fix the cause, then start it.

## A dead engine's interfaces are free at once; a vanished host's within ~25 s (level 3)
- **What:** an engine's lock lives in its own database session. When the process ends in any way (`kill -9`, a crash), the operating system closes the connection and Postgres frees the lock: the next engine can claim its interfaces immediately (0.3 s after `kill -9` in `demo-one-owner.sh`). When the whole host vanishes (power loss, a network cut), nothing closes the connection; the engine asks Postgres to probe it (TCP keepalives on that session: 10 s idle, then 3 probes 5 s apart), so the lock goes within about 25 s.
- **Automatic standby is not this:** a second engine doesn't wait to take over; it refuses, and you start it when the first is gone. A standby that takes over by itself is v1.2's clustering.
- **What to do:** for DR, keep the standby engine stopped (or pointed elsewhere) and start it after the primary is down. Nothing else to configure.

## Losing the database connection pauses sending, not receiving (level 3)
- **What:** if the engine loses the connection that holds its lock (a database restart, a failover, a DBA ending the session), it pauses sending, file pickup and schedules within a few seconds (it checks every 2 s), keeps receiving (storing is safe from any number of engines), and takes the lock and its interfaces back as soon as it can; then it sends what came in meanwhile. If another engine took an interface over in the gap, this one stops (exit 3) rather than run it twice. The Overview says "Cut off from the database" while it lasts.
- **What to do:** nothing for restarts and failovers. Don't end the `ductus engine lock` session by hand unless you mean to pause that engine.

## PgBouncer in transaction mode can't hold the engine's lock (level 3)
- **What:** the lock is a session advisory lock: it needs one database session for the engine's whole life. A pooler in transaction (or statement) mode hands each transaction a different server connection, so the lock would be taken on one and lost at once.
- **What to do:** point the engine at Postgres directly, or through a pooler in session mode. The engine pools its own connections (`--db-connections` / `DUCTUS_DB_CONNECTIONS`, 32 by default) plus one for the lock.
