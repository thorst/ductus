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

## Opening a database connection over TLS costs ~5 ms more (level 4)
- **What:** the TLS handshake comes before the login, so a new connection to the engine's database takes a little longer over TLS: 12–16 ms (`prefer`) and ~12 ms (`verify-full`) against 7–8 ms plain, on a local Postgres (`scripts/bench-pgtls.sh`, 200 in a row). Pools keep their connections, so messages rarely see it; a start opens many at once: 1,001 interfaces were ready in 0.65–0.77 s over TLS against 0.60–0.70 s plain. Strict ingest with 1 sender is within noise; with 16 senders it is ~8% lower (5,547–5,956 against 6,273–6,449 ACKs/s): the encryption itself. In 0.6 it was much more: sqlx 0.9.0 didn't set `TCP_NODELAY`, so the handshake and the login waited on the server's delayed ACKs (opening took ~65 ms `prefer`, ~80 ms `verify-full`, and the same start 0.87–0.96 s). sqlx 0.9.1 sets it (transact-rs/sqlx#4336).
- **Why:** TLS adds round trips and encryption; there is no way to have its protection for free.
- **What to do:** nothing in most installs. If start time or pool growth matters for a database on this machine, use a Unix socket (`postgres:///ductus?host=/var/run/postgresql`): it needs no TLS. Don't give up verification for it: `verify-full` costs no more than `prefer`.
