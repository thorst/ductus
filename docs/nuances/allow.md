# Nuances: who may connect (`allow:`)

What could surprise you here, why it is so, and what to do. Each entry has a level, 1 (day one) to 5 (expert): the [index](index.md) lists them all by level.

## An allow list says who, which "one connection only" never did (level 2)
- **What:** `allow: [10.1.20.0/24, 10.1.30.15, epic-bridge01.hospital.org]` on an MLLP or HTTP source (or on `api` in ductus.yaml) closes a connection from any other address before the TLS handshake and before a byte is read; without `allow`, anyone who can reach the port may connect, as before. Listeners take any number of connections; there is no `connections: 1` setting.
- **Why:** sites limit a listener to one connection as a stand-in for "only the sending system", but it doesn't say which system: whoever connects first wins, and after a network blip the real sender's reconnect can be refused while its old half-dead connection holds the slot. An allow list names the senders, and a sender that opens several connections (common: one per thread, or a new one before the old has timed out) isn't refused for it.
- **What to do:** list the sending system's addresses (or its subnet), its failover or DR hosts, and anything that relays for it. `ductus validate` warns when `about.from.hosts` names a sender the list would refuse, and refuses a malformed entry (a bad subnet, a wildcard `*.hospital.org`, a range `10.1.20.1-10.1.20.50`, `allow: []`).

## It's defence in depth, next to the firewall (level 2)
- **What:** the check is in the engine, so it holds wherever the engine runs (a new server, a container, a firewall rule opened too wide), and its refusals are in the engine's log with the listener and address. But the connection has already reached the engine's server: the firewall still keeps everyone else off the network, and stops floods the engine would otherwise have to accept and close.
- **What to do:** keep the firewall rules; add `allow` so a mistake in either is caught by the other. Behind a proxy or load balancer the address seen is the proxy's: list the proxy (we don't trust `X-Forwarded-For`, which any caller can write).

## Host names are looked up at start and every 5 minutes (level 3)
- **What:** a host name in a list is resolved forwards (name → addresses: hosts file, then DNS) when the engine starts, every 5 minutes after, and at once after a refusal at a listener that lists names (at most once a minute per name, however many connections are refused). Connections never trigger a lookup: ten connections from `epic-bridge01` compare against addresses already in memory. A name that doesn't resolve at start lets no one in for it (the engine still starts, with a warning); a name that stops resolving keeps the addresses it last had, so a DNS outage doesn't cut off senders that were working. No reverse lookups: a sender controls the reverse record of its own address. Cost: one lookup per distinct name every 5 minutes, and nothing at all when no list names a host.
- **What to do:** prefer addresses or subnets for senders whose address never changes; use a name when the sending side moves (failover, a cloud host) and its DNS is kept current. After a sender moves, its first connection is refused and the next one, seconds later, gets in.

## Refusals are logged once a minute per listener and address (level 3)
- **What:** `connection refused: not on the allow list` with the listener, the address and the list (names with their addresses now), at most once a minute per listener and address, like refused TLS handshakes. The Ports card on the Connections page and `GET /ports` count refusals since the engine started and show the last address refused; the interface page shows the list. The sender sees its connection closed with nothing read: an MLLP sender gets no ACK and retries.
- **What to do:** a sender that says it can't connect: look for its address in the log or on the Ports card. The engine's own checks (Connections → Check now) are let in whatever the list says; `ductus doctor` run as its own program on the same server is refused unless 127.0.0.1 is listed, and it says so instead of checking the TLS it can't reach.

## List 127.0.0.1 on the API if you use it from its own server (level 2)
- **What:** `api.allow` applies to everyone, including `ductus status`, `drain` and a browser on the engine's own server. `ductus validate` warns when the list leaves out 127.0.0.1.
- **What to do:** `api: { listen: 0.0.0.0:7700, tls: ..., allow: [127.0.0.1, 10.1.5.0/24] }` (this machine and the operators' subnet).

## HTTP interfaces on one port share its `allow` (level 2)
- **What:** the list is checked before the request is read, so before the path is known: interfaces on one port share one list, and `ductus validate` and the engine refuse a port whose interfaces disagree.
- **What to do:** give them the same `allow:`, or put callers with different lists on different ports.

## Dead connections are noticed in about 30 seconds (level 3)
- **What:** every connection our listeners accept has TCP keepalive on: after 15 s of silence the server's kernel probes the sender every 5 s, and after 3 unanswered probes the connection is closed (the operating system's default would take about 2 hours). A sender that was powered off or cut off by a firewall no longer holds a connection open for hours, and quiet MLLP connections stay open through firewalls that drop idle ones. The kernel sends the probes; the engine does nothing.
- **What to do:** nothing; a firewall that forgets idle connections in under 15 s would still drop them (rare: most use minutes to an hour).
