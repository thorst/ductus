# Nuances: TLS to Postgres

What could surprise you here, why it is so, and what to do. Each entry has a level, 1 (day one) to 5 (expert): the [index](index.md) lists them all by level.

## Without `sslmode`, the database connection is TLS when offered, and nobody is checked (level 3)
- **What:** `DUCTUS_DATABASE_URL` means what it means in psql: without `sslmode` it is libpq's `prefer`, TLS when the server offers it (Ubuntu's Postgres and most packages do, with a self-signed certificate), plain when it doesn't, and the certificate is never checked. `require` insists on TLS and checks nothing either. Only `verify-full` checks that a trusted CA issued the server's certificate and that it is for the host connected to (`verify-ca`: the CA, not the name). resources.yaml `postgres` entries work the same with `tls:` (default `prefer`; `verify-full` when `ca:` names a CA). The start log, `ductus validate`, doctor and the Connections page say which ("TLS 1.3, not verified (sslmode=prefer)", "TLS 1.3, verified (sslmode=verify-full)", "plain, no TLS"), and warn when the database is on another machine and not verified.
- **Why:** a URL that works with psql must work here, and `verify-full` by default would refuse every install whose Postgres has its own self-signed certificate. But TLS that checks nothing stops only someone listening: anyone who can answer at the database's address (a DNS entry, a spoofed address on the way) gets the password and every query.
- **What to do:** for a database on another machine, `?sslmode=verify-full`, plus `&sslrootcert=/path/ca.pem` when this server doesn't already trust its CA (`sslrootcert=system`: the system's alone); in resources.yaml `tls: verify-full` and `ca:` (a `certificates:` entry). `ductus validate` says "verified" when it works. For one on this machine, a Unix socket (`postgres:///ductus?host=/var/run/postgresql`) needs no TLS and is the fastest.

## Cloud Postgres requires TLS (level 1)
- **What:** Azure Database for PostgreSQL and Amazon RDS are usually set to refuse plain connections. Azure's certificates chain to public roots this server already trusts; RDS's are issued by Amazon's own RDS CAs, which it doesn't.
- **What to do:** `sslmode=verify-full`; for RDS add `sslrootcert=` with Amazon's RDS CA bundle (downloaded from AWS) for the region. Connect by the endpoint's DNS name, the one on its certificate.

## A CA installed on this server counts (level 2)
- **What:** the database's certificate is checked against this server's trusted certificates (Linux: `update-ca-certificates`; Windows: the machine's store) plus `sslrootcert` (or `ca:`), as MLLP and HTTPS connections are. sqlx reads the system's certificates at every new connection (about 10 ms with `verify-full`), so a CA installed while the engine runs counts from the next one.
- **What to do:** install a hospital CA once on the server, or name it per connection; both work.

## The database's certificate is watched when the engine checks it (level 3)
- **What:** a Postgres connection with `verify-full` or `verify-ca` (the engine's own database or a resources.yaml entry) is in the engine's certificate checks: the 8-byte SSLRequest, then the handshake the engine would make, its dates kept and alerted at 30, 14 and 7 days, as "engine database" or the entry's name. Its expiry would stop the engine connecting. With `prefer` or `require`, doctor shows the certificate (and whether `verify-full` would accept it) but doesn't watch it: an expired one changes nothing for a connection that checks nothing.
- **What to do:** switch to `verify-full`, and it's watched.

## A client certificate renews at the next connection (level 3)
- **What:** `certificate:` on a Postgres entry logs in with a `certificates:` pair (pg_hba's `cert` method: no password; or with a password and `clientcert=verify-full`). Renewed from its files, the pool's next connection presents it; open ones keep theirs until they close (sqlx keeps a connection up to 30 minutes, an idle one 10). The engine's own database reads `sslcert=` and `sslkey=` at every new connection.
- **What to do:** renew before expiry: it counts down with "Ours" on the Connections page. Ask the DBA which CA they trust for clients and which name (the certificate's CN) maps to the database role.

## Opening a database connection over TLS costs ~60 ms, for now (level 4)
- **What:** sqlx 0.9.0 doesn't set `TCP_NODELAY`, and a TLS handshake followed by the login waits on the server's delayed ACKs: opening a connection took 64 ms (`prefer`) and 79 ms (`verify-full`) against 5 ms plain, on a local Postgres. Pools keep their connections, so messages rarely see it; a start does: 1,001 interfaces were ready in 0.91–1.06 s over TLS against 0.52–0.66 s plain. Strict ingest with 1 sender is within noise; with 16 senders it was ~7% lower (2,636 against 2,831 ACKs/s), ~2% with `TCP_NODELAY` set. Fixed in sqlx (transact-rs/sqlx#4336, after 0.9.0): with it, opening takes 8–11 ms (22 ms `verify-full`) and the start 0.63–0.72 s.
- **What to do:** nothing in most installs; the next sqlx release removes it. If start time or pool growth matters for a database on this machine, use a Unix socket. Don't give up verification for it.
