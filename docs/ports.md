# Ports and firewalls

What to open for Ductus, in each direction, and how to get the exact list for a firewall request.
The short answer: **in**, one port for the web UI and API, and each interface's own listener;
**out**, the database and whatever your interfaces connect to. Nothing else: no thick client,
no extra ports for the web UI, no cluster port.

## What connects to the engine (in)

| Port | What | Who connects | Open it to |
|---|---|---|---|
| **7700** (`api.listen`) | The REST API and the web UI, one port for both | People's browsers, the `ductus` command line from other machines, monitoring (Prometheus), scripts | Developers' and operators' networks, the monitoring server. **Listens on 127.0.0.1 (this machine only) until you change it.** |
| Each interface's port | Its MLLP or HTTP listener, from its interface.yaml | The systems that send to it: the EHR, a lab system, another engine | The sending systems' addresses only. |

**The API and web UI** listen on this machine only by default
([`api.listen`](reference/config/ductus.md#api): `127.0.0.1:7700`), so opening them to the
network is a choice you make. When you do, give them a certificate and an allow list, in
ductus.yaml:

```yaml
# ductus.yaml, in part
api:
  listen: 0.0.0.0:7700
  tls: { certificate: ductus-api }          # a certificates: entry in resources.yaml
  allow: [127.0.0.1, 10.1.50.0/24]          # this machine, and the operators' subnet
```

Without `tls`, passwords and tokens cross the network readable and the web UI's sign-in doesn't
work at all (its cookie is only sent over HTTPS:
[nuance](nuances/tls-listening.md#the-api-on-the-network-needs-tls-level-1)). Keep `127.0.0.1` in the list, or
commands run on the server itself are refused
([nuance](nuances/allow.md#list-127001-on-the-api-if-you-use-it-from-its-own-server-level-2)).
Developers need this port and whatever they deploy with (SSH, Git), nothing more.

**Interfaces' listeners**: each MLLP source has a port of its own; HTTP sources can share one,
by path. `ductus new interface` picks a free one from the ranges in ductus.yaml
([`ports`](reference/config/ductus.md#ports): MLLP 6661–6699, HTTP 8080–8099 by default). Ask
the firewall team for a range once, set `ports:` to it, and new interfaces land inside it with no
new request:

```yaml
# ductus.yaml, in part
ports:
  mllp: 7000-7099     # the range the firewall team opened
  http: 9080-9099
```

The engine checks who may connect itself too: `allow:` on a source lists the senders' addresses,
subnets or host names, and anyone else is disconnected before a byte is read
([who may connect](configure/sources.md#who-may-connect-allow)). Keep the firewall rules as well:
each catches the other's mistakes
([nuance](nuances/allow.md#its-defence-in-depth-next-to-the-firewall-level-2)).

A port below 1024 (MLLP on 104, say) needs a capability under the Linux package's service; the
unit file says which two lines to add with `systemctl edit ductus`. Above 1024 needs nothing.

## What the engine connects to (out)

| To | Port | When |
|---|---|---|
| Its PostgreSQL | 5432 (or the URL's) | Always. |
| Receivers of MLLP destinations | as configured | For each `destination: { mllp: … }`. |
| HTTP destinations, APIs (`api()`, `fetch()`), webhooks, the heartbeat | 443 (or the URL's) | For each one configured. |
| Databases interfaces query (`db()`) | Postgres 5432; SQL Server 1433, **and UDP 1434** for a named instance (the SQL Server Browser) | For each `databases:` entry. |
| SFTP servers | 22 (or the entry's) | For each `sftp:` entry. |
| The directory (Active Directory, OpenLDAP) | 636 (ldaps), or 389 with StartTLS | With `auth.ldap`. |
| The mail server | 587 (STARTTLS), 465 (TLS) or 25 | With `alerts.smtp`. |
| DNS | 53 | To look up every host name above. |
| Maven Central (`repo1.maven.org`, 443) | | Only `ductus dev`, once per user, on developers' machines. Never on a server. |

The engine never connects anywhere to "phone home": no update checks, no telemetry.

## The exact list for a firewall request

`ductus ports` prints every port a project listens on and every host it connects to, for the
environment you name, with conflicts (two interfaces on one port) flagged:

For production, give it `--env prod`. Here, the project from
[Your first interface](first-interface.md), with TLS:

```text
$ ductus ports
environment: dev

Listening
  PORT  PROTOCOL       HOST       INTERFACE          ROUTE    TLS                                                            ALLOWED
  6661  MLLP over TLS  0.0.0.0    adt-in                      presents ductus-mllp, clients need a certificate from test-ca  anyone
  7700  HTTP           127.0.0.1  (engine REST API)  /api/v1  none                                                           anyone

Connects to
  (nothing)

✓ no port conflicts
```

`ductus doctor`, run on the engine's server, then checks each outbound connection from there:
DNS, TCP (refused, or no answer at all, which is a firewall), TLS, and each system's login. For
each one it prints the **source address** the connection left from: that is the address a
firewall request must name, which on a server with several networks isn't always the one you'd
guess. `--report doctor.txt` saves it as text for the network team or a vendor (`doctor.json`
as JSON)
([what a pass means](nuances/doctor.md#a-pass-means-can-connect-and-log-in-not-will-accept-your-messages-level-2)).

## Opening a range on the server itself

```sh
# RHEL, Rocky, Alma (firewalld)
sudo firewall-cmd --permanent --add-port=6661-6699/tcp && sudo firewall-cmd --reload
# Ubuntu (ufw)
sudo ufw allow from 10.20.30.0/24 to any port 6661:6699 proto tcp
```

```powershell
# Windows
New-NetFirewallRule -DisplayName "Ductus MLLP" -Direction Inbound -Protocol TCP -LocalPort 6661-6699 -RemoteAddress 10.20.30.0/24 -Action Allow
```
