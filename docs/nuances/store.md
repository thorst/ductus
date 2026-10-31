# Nuances: the store (Postgres)

What could surprise you here, why it is so, and what to do. Each entry has a level, 1 (day one) to 5 (expert): the [index](index.md) lists them all by level.

## A query that holds the message tables for days can pause intake (level 4)
- **What:** each day's tables are created ahead of time, hourly. Creating one needs a brief exclusive lock on `messages`/`deliveries`, which has to wait for every open query on them. Maintenance gives up after 2 s and tries again in an hour (partitions exist 3 days ahead), so a long report never holds up ACKs. If tomorrow's partitions still don't exist (something held the tables through every try for two days), maintenance stops giving up and waits: new messages (and their ACKs) then wait as long as the blocking query runs, rather than fail at midnight. The engine logs a warning when that happens.
- **What to do:** run long reports against a replica, or keep them short; never leave a transaction open on the store. Watch for the warning "a partition needed soon is missing".

## A start waits 30 s for a database that refuses, then says why (level 2)
- **What:** when the engine's database refuses connections or doesn't answer, the start keeps trying for 30 s before it gives up and says why ("refused (tried for 30 s): nothing accepts connections there…"). A wrong password, a database that doesn't exist or a host name nobody knows fail at once, since trying again can't help.
- **Why:** at a server's boot the database may still be starting when the engine does; 30 s covers that without a failed start. Under the Linux package's service a failed start is tried again every 5 s anyway.
- **What to do:** nothing at boot. When you're fixing a URL, `ductus doctor` answers in under a second, step by step.
