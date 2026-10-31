# Nuances: checking connections (`ductus doctor`)

What could surprise you here, why it is so, and what to do. Each entry has a level, 1 (day one) to 5 (expert): the [index](index.md) lists them all by level.

## Doctor checks from where it runs (level 2)
- **What:** every result (DNS, routes, firewalls, the source address in a firewall request) is about the machine running `ductus doctor`. A laptop on the VPN and the engine's server can get opposite answers.
- **What to do:** run it on the engine's server, as the account the engine runs as (folder rights and trusted certificates are per account), with the environment it uses (`--env prod`).

## A pass means "can connect and log in", not "will accept your messages" (level 2)
- **What:** doctor never sends anything to a destination: MLLP and HTTP destinations are only connected to (and TLS checked). For an API it sends one `GET` to the API's base URL with its headers and reports the status: a 404 there is normal, a 401/403 means the token was refused. Databases get a login and a version query; SFTP a host key check, login and SFTP start. Code that writes to a table it has no rights to still fails at run time.
- **What to do:** use doctor for "can we reach it"; use the interface's tests and a test message for "does it accept what we send". The engine runs the certificate part of these checks by itself every few hours (see Certificates).

## One working address is enough to pass, with a warning (level 3)
- **What:** a name can resolve to several addresses (IPv4 and IPv6, or a cluster). Doctor tries each. If some fail and one works, the check warns: the engine (like any client) may try a failing address first and be slow, or fail now and then.
- **What to do:** ask for the DNS entry to list only working addresses, or for the others to be opened.

## The commit time is the store's disk, and the ceiling per strict sender (level 4)
- **What:** doctor times 20 durable commits against the engine's database. With `durability: strict`, each ACK waits for one, so one sender connection can't go faster than about 1 / commit time (2.4 ms ≈ 410 msg/s); more connections share commits. Doctor warns above 10 ms.
- **What to do:** if that's near your peak per connection, use a faster disk (SSD/NVMe, write cache with battery), more sender connections, or `durability: relaxed` where losing the last fraction of a second on a database crash is acceptable.
