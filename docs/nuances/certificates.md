# Nuances: certificates

What could surprise you here, why it is so, and what to do. Each entry has a level, 1 (day one) to 5 (expert): the [index](index.md) lists them all by level.

## The engine checks certificates by itself, and logs in to nothing (level 2)
- **What:** every `doctor.schedule` (6 h by default; the first a minute or two after start), the engine runs the connection checks in `certificates` mode: DNS, TCP and the TLS handshake of every connection with a certificate, nothing else. It keeps what it saw (the Connections page, `GET /api/v1/certificates`) and the run itself (the last 30). `mode: full` runs everything `ductus doctor` does, every database and SFTP login included, every few hours.
- **Why:** a full run logs in to every system four times a day. Hospital security teams notice service accounts that do that, and a password that changed locks the account out after a few runs.
- **What to do:** keep `certificates` unless someone asked for scheduled login checks; run full checks when you want them (the Connections page, `POST /api/v1/doctor`, `ductus doctor`). `doctor: { schedule: off }` turns the scheduled checks off (and with them the countdown's freshness: dates then come from the last check someone ran).

## What has a certificate the engine watches (level 3)
- **What:** HTTPS APIs (resources.yaml) and HTTPS destinations, MLLP destinations with `tls:`, the directory people sign in with (`ldap:` in resources.yaml, ldaps:// or StartTLS), Postgres with `verify-full` or `verify-ca` (the engine's own database and resources.yaml entries: see "The database's certificate is watched when the engine checks it"), SQL Server without `trust_server_certificate` (see [SQL Server's certificate is read through PRELOGIN](tls-sqlserver.md#sql-servers-certificate-is-read-through-prelogin-and-watched-when-verified-level-3)), and what alerts connect to: the mail server (`tls: starttls` or `tls`), HTTPS webhooks and the heartbeat. Not watched: databases the engine doesn't verify, SFTP (host keys, not certificates), MLLP destinations without `tls:` (plain TCP). Our own certificates are watched too, from what the engine loaded (see [Our certificates count down like the rest](#our-certificates-count-down-like-the-rest-with-no-check-level-2)).
- **What to do:** verify every database on another machine, and its certificate is watched with it.

## The earliest expiry in the chain counts, in whole days (level 2)
- **What:** a server presents its certificate and usually one or more intermediates. Clients refuse the connection when any of them has expired, so the countdown runs to the earliest of them (the Connections page names an intermediate that ends first). Days are whole days in UTC dates: a certificate ending tomorrow at 00:30 UTC has 1 day left all of today, wherever the server is.
- **What to do:** when the intermediate is the one ending, the fix is the server's certificate bundle (the CA has a newer intermediate), not a new certificate.

## A certificate alert is told at 30, 14 and 7 days, and when it expires (level 2)
- **What:** with a rule naming `certificate` (`{ when: [certificate], severity: warning, to: [oncall] }`), a certificate alerts when it is 30 days from expiry, again at 14 and at 7 (escalated, even if someone acknowledged it), and as **critical** once expired, whatever the rule's severity. Each step is told once: no reminders before it expires (the steps are the reminders), the usual `repeat` after. Renewed past 30 days, it recovers (after `settle`); renewed with a certificate that is itself short, it moves to that step quietly. `alerts.certificates.warn_days` changes the steps. Without a rule naming `certificate`, nothing is sent (`ductus validate` says so) and the Connections page still shows them.
- **What to do:** send `certificate` to the people who renew certificates, not the on-call pager; warnings respect quiet hours, an expired one doesn't. `to: [owner]` reaches a destination's owner for a destination with its own URL, and a resources.yaml entry's `owner:` (an API, the directory) for its certificate; entries without one go only to the rule's channels.

## A check that can't connect keeps the last dates (level 2)
- **What:** a certificate is read even when this server doesn't trust it (a private CA, self-signed): the Connections page says "not trusted by this server", and the dates count all the same. When a check can't connect at all, the row shows the error and keeps the dates the last successful check read ("dates from ..."); the countdown and alerts go on from those. A server never reached shows "Not read yet" and can't alert.
- **What to do:** a certificate stuck on "Not read yet", or with an old "dates from", needs the connection fixed first (the same row's error says why).

## Behind a load balancer, you see the balancer's certificate (level 3)
- **What:** the check reads whatever certificate answers at the configured name and port: a load balancer or proxy that ends TLS presents its own, not the servers' behind it. If the balancer's backends present different certificates, each check may see another one; the alert follows what it sees (renewed, then not, is one alert held open by `settle`, but a backend left on an old certificate shows again at the next check).
- **What to do:** renew every backend, and ask the balancer's owner which certificate clients actually get.

## Our certificates count down like the rest, with no check (level 2)
- **What:** every `certificates:` pair the engine presents, on a TLS listener (an interface's or the API's) or as a client certificate, is one row in the Connections page's "Ours" group and in `GET /api/v1/certificates` (`side: ours`, on this server at port 0, with where it is presented). It is read from what the engine loaded, never over the network, and alerts like the servers' (30, 14, 7 days, expired) to its `owner:` in resources.yaml, or, without one, to the owner of the one interface it serves. Its row in the database is written at the start and again only when a renewal is picked up or can't be used: nothing runs for it while nothing changes (the minute's file check reads the files, not the database).
- **What to do:** give each pair an `owner:` (the people who renew it), and a rule naming `certificate` to someone. After replacing the files, "Check now" (or any connection check) reads them at once instead of within the minute.

## A renewal that can't be used shows on its row, not as an alert (level 2)
- **What:** files that changed but don't load (a new certificate with the old key, half copied, expired) keep the pair in use, log one error, and show on the pair's row ("the new files can't be used: ..."). The countdown and its alerts go on from the pair still presented; nothing new is sent about the broken files themselves.
- **What to do:** after a renewal, look at the "Ours" group (or the log's `certificate renewed`): an error there means senders still get the old certificate, and it still expires when it did.

## Doctor reads the chain our listeners serve, but not the name senders use (level 3)
- **What:** in full mode (the Connections page's "Run the checks", `POST /doctor` with `mode: full`, a scheduled `mode: full`), the engine connects to each of its TLS listeners, over loopback when it listens on every address (`0.0.0.0`, `::`), takes the chain it serves whatever it is, and judges it the way senders build a path: complete when it ends at a root, or at a certificate issued by one this server trusts or a `certificates:` CA; a warning when it stops short ("only our certificate is served, not the one that issued it"), since Java and .NET senders don't fetch a missing intermediate. `ductus doctor` does the same for a listener whose port is in use (a running engine). Certificates-only runs skip it (it reads nothing new about expiry).
- **Why it can't say more:** it doesn't know the name senders connect with (it shows the names the certificate covers), nor which roots they trust: a chain ending at your private CA is complete only for senders that trust that CA. A warning about a chain that ends at your private root can be right: name that root as a `certificates:` CA and doctor knows it.
- **What to do:** fix a warning by appending the intermediate to `cert_file`; ask senders which name they connect with and check it is among the names shown.

## A refused certificate is said once, not logged as an error each time (level 4)
- **What:** the TLS verifier the engine uses logs every certificate it refuses at ERROR. The engine turns that log off by default (`rustls_platform_verifier=off`): the refusal is already in the check's result and in a failed delivery's error, and scheduled checks of a private-CA server would otherwise log an error every few hours.
- **What to do:** to see the verifier's own lines, set `RUST_LOG=info,sqlx=warn,rustls_platform_verifier=debug`.
