# Your first interface

From nothing to HL7 messages flowing over MLLP, then the same over TLS with client certificates,
on your own machine, in about fifteen minutes. Every command here is real: run them as you read.
You need the `ductus` program on your `PATH` ([how](install/dev.md#get-the-program)) and, for
the TLS part, `openssl` (it plays the hospital's certificate authority).

## 1. A new project

```sh
ductus init hospital
cd hospital
git init        # a project is a Git repository: what runs is what's committed
```

`ductus init` makes a project that passes its tests and validates as it is:

```text
hospital/
├── ductus.yaml              the project's settings (all optional; the defaults written out)
├── resources.yaml           databases, servers and APIs interfaces share (none yet)
├── environments/
│   ├── dev.yaml             values for `--env dev` (the default)
│   └── prod.yaml            the same names, production's values
├── interfaces/
│   └── adt-in/              one interface: HL7 over MLLP on port 6661, written to files
│       ├── interface.yaml   where messages come from and go
│       ├── archive.ts       what happens to each one on its way to the destination `archive`
│       ├── archive.test.ts
│       └── fixtures/sample.hl7
├── .ductus/, tsconfig.json, .vscode/settings.json    for your editor (below)
├── .gitignore, README.md
```

Open the folder in VS Code. With Red Hat's YAML extension, every setting in the YAML files
completes as you type and shows its description on hover; in `archive.ts`, hovering
`msg.get("PID-5")` shows *PID-5 Patient Name*. The schemas and typings in `.ductus/` come from
the `ductus` you have installed, so they match it ([editor support](configure/index.md#in-your-editor)).

## 2. The interface

`interfaces/adt-in/interface.yaml`, as generated (without its last lines, comments about
`monitor:`):

```yaml
# interfaces/adt-in/interface.yaml, in part
name: adt-in
about:
  description: ""
  owner: ""
  from: { application: "", vendor: "", hosts: [] }   # hosts: where senders connect from (IPs or subnets)
  to: { application: "", vendor: "" }
source: { mllp: { port: 6661 } }
destinations:
  archive:                      # named for where it goes: rename it (and its file) when it sends elsewhere
    file: { dir: out/adt-in }
    # or: mllp: { host: receiver.example.org, port: 5000 }
    # or: http: { url: https://api.example.org/messages }
    script: archive.ts           # its code: one file per destination
  # another, with its own queue and its own code:
  # billing: { http: { api: billing, path: /charges }, script: billing.ts, output: json, vendor: Experian }
```

- [`source`](reference/config/interface.md#sourcemllp): listen for HL7 over MLLP on port 6661.
  Each message is stored before its ACK goes back to the sender.
- [`destinations`](reference/config/interface.md#destinations): where messages go, each by a
  name: the web UI, alerts and `ductus pause` call it that. This one, `archive`, writes each result as a file
  ([`file`](reference/config/interface.md#destinationsnamefile)); the folder is relative to the
  interface's own folder, so the files land in `interfaces/adt-in/out/adt-in/`. Its
  [`script`](reference/config/interface.md#destinationsnamescript), `archive.ts`, runs on each
  message on its way there. Each destination is named for where it goes and has its code in a
  file of its own ([destinations](configure/destinations.md#named-destinations-each-with-its-own-code)).
- [`about`](reference/config/interface.md#about): what the interface is for, who owns it, and
  the systems on each side. Nothing here changes how messages flow; it's what `ductus impact`
  and the web UI read.

The code, `archive.ts`, gets each message and returns what to send:

```ts
import type { Message } from "@ductus/sdk";

/**
 * adt-in: called once per message.
 * Return what to send on, null to drop it, kill("reason") to drop it with a reason,
 * or throw quarantine("reason") to hold it for review.
 */
export function transform(msg: Message) {
  // Example: set the receiving application.
  msg.set("MSH-5", "RECEIVER");
  return msg;
}
```

Returning `null` would drop the message (filtered, with the reason kept); throwing would
quarantine it for a person to look at ([nuance](nuances/transforms.md#return-null-filters-throwing-quarantines-level-1)).

## 3. Test and validate

```text
$ ductus test
adt-in
  ✓ sets the receiving application and keeps the patient (2ms)

1 passed, 0 failed
```

The tests run in the same embedded JavaScript engine the interfaces run in, with no database or
network. Then check the whole project as the engine would read it:

```text
$ ductus validate
environment: dev
✓ adt-in: config and code load
⚠ adt-in: about.from: add the sending system's `application` or `vendor`
⚠ adt-in: about.to: add the receiving system's `application` or `vendor`
  (warnings: `ductus impact` can't find what isn't named; `require_about: true` in ductus.yaml makes them errors)
note: not checked against a database (set DUCTUS_DATABASE_URL): lanes that would strand waiting messages, retention changes
✓ types: no errors

valid
```

The warnings are about `about:`, which is still empty. Fill it in: the day a vendor has an
outage or a breach, `ductus impact --vendor "Epic Systems"` lists every interface it touches,
but only the ones that say so.

```yaml
# interfaces/adt-in/interface.yaml
name: adt-in
about:
  description: ADT from the EHR, kept as files for the lab's archive
  owner: interfaces@hospital.org
  from: { application: Epic, vendor: Epic Systems, hosts: [10.20.30.0/24] }
  to: { application: Lab archive, vendor: In-house }
source: { mllp: { port: 6661 } }
destinations:
  archive:
    file: { dir: out/adt-in }
    script: archive.ts
```

`ductus validate` now has no warnings.

## 4. Run it

```text
$ ductus dev
Postgres 18.6.0 is running for this project:
  postgresql://postgres:…@127.0.0.1:42731/ductus
  (data in ./.ductus/postgres/data; stops with the engine)
Web UI login: user `dev`, password `…` (kept in ./.ductus/dev-login)
… INFO ductus_runtime::api: REST API listening (/api/v1) host=127.0.0.1 port=7700 https=false
… INFO ductus_runtime: started interfaces=1 mllp=1 http=0 files=0 schedules=0 internal=0 …
```

`ductus dev` is the engine with a Postgres of its own (downloaded once, the first time:
[more](install/dev.md)). In a second terminal, send it ten synthetic ADT messages, as an EHR
would:

```text
$ ductus gen --count 10
sent 10 messages over 1 connection(s) in 0.0s (297/s), retries 0, rejected 0, refused 0, dropped 0, longest wait 23 ms
```

Each one was stored before its ACK went back ("longest wait" is the slowest ACK). Look at what
arrived:

```sh
ls interfaces/adt-in/out/adt-in/       # 1.hl7 … 10.hl7, named by message id
```

and open the web UI at `http://127.0.0.1:7700`, signing in with the `dev` login `ductus dev`
printed. The interface's Messages tab lists the ten, and each message shows its HL7 fields by
name, what was received, and what happened at each destination.

## 5. Change it

Make the code do something of its own: say the lab wants patient names in capitals. In
`archive.ts`:

```ts
import type { Message } from "@ductus/sdk";

export function transform(msg: Message) {
  msg.set("MSH-5", "LAB");
  msg.set("PID-5.1", msg.get("PID-5.1").toUpperCase());   // PID-5.1: family name
  return msg;
}
```

and say so in its test, `archive.test.ts`:

```ts
import { expect, input, type Message, test } from "@ductus/sdk";
import { transform } from "./archive.ts";

test("addresses the lab and capitalizes the family name", () => {
  const out = transform(input<Message>("sample.hl7"));
  expect(out.get("MSH-5")).toBe("LAB");
  expect(out.get("PID-5.1")).toBe("SMITH");
  expect(out.get("PID-3.1")).toBe("123456");
});
```

`ductus test`, then stop `ductus dev` (Ctrl-C) and start it again: it starts in well under a
second, and new messages get the new code. HL7 is numbered as the standard numbers it, from 1,
and a field that isn't there reads as `""`, never an error
([nuance](nuances/transforms.md#missing-fields-read-as--and-never-throw-level-1)).

## 6. The same over TLS, with client certificates

MLLP crossing a network you don't own should be encrypted, and the listener should know which
systems may send. That's two settings: a certificate the listener presents, and the CA whose
certificates it takes from senders.

**A certificate authority for the lab.** In real life the hospital's PKI team signs your
certificate. Here, `openssl` plays that part (keep its folder out of Git: `ca.key` signs
anything, and the server will trust the hospital's CA, not this one):

```sh
mkdir -p lab-ca tls
echo "lab-ca/" >>.gitignore
openssl req -x509 -newkey rsa:2048 -nodes -keyout lab-ca/ca.key -out lab-ca/ca.pem -days 365 -subj "/CN=Test Hospital CA"
```

**The listener's certificate.** `ductus cert request` makes a private key that never leaves
this machine, and a request (CSR) to send to the CA. It says what to send and what comes back:

```text
$ ductus cert request tls/mllp --dns localhost --ip 127.0.0.1
wrote tls/mllp.key (the new RSA 2048 private key: it stays on this server; only you can read it) and tls/mllp.csr (the request)

Send tls/mllp.csr to the hospital's CA (its PKI or security team). It asks for a certificate for localhost, 127.0.0.1, for a TLS server and client (one certificate can serve listeners and be the engine's client certificate). Never send tls/mllp.key.
…
```

Sign it as the CA would, and do the same for a sender (in real life, the sending system's team
asks their CA, or yours, for theirs):

```sh
openssl x509 -req -in tls/mllp.csr -CA lab-ca/ca.pem -CAkey lab-ca/ca.key -CAcreateserial -out tls/mllp.pem -days 90 -copy_extensions copy
ductus cert request tls/sender --dns sender.test
openssl x509 -req -in tls/sender.csr -CA lab-ca/ca.pem -CAkey lab-ca/ca.key -CAcreateserial -out tls/sender.pem -days 90 -copy_extensions copy
```

**Name them in resources.yaml**, so interfaces refer to them by name:

```yaml
# resources.yaml
certificates:
  ductus-mllp: { cert_file: tls/mllp.pem, key_file: tls/mllp.key }
  test-ca: { ca_file: lab-ca/ca.pem }
```

[`certificates`](reference/config/resources.md#certificates) holds both kinds: a pair the engine
presents (`ductus-mllp`), and a CA it trusts or checks clients against (`test-ca`). Ask
`ductus cert show` what the engine will make of one, before a sender finds out:

```text
$ ductus cert show ductus-mllp
✓ certificate `ductus-mllp` (resources.yaml): ./tls/mllp.pem
  subject  CN=localhost
  names    localhost, 127.0.0.1
  issuer   issued by Test Hospital CA
  valid    2026-10-31 → 2027-01-29: 89 days left
  chain    complete: localhost, issued by Test Hospital CA (the CA `test-ca` in resources.yaml)
  key      matches (./tls/mllp.key)
→ Senders must trust Test Hospital CA (the hospital's CA): ask them to confirm.
```

(Shown the file alone, `ductus cert show tls/mllp.pem` warns that the chain is incomplete: it
can't know Test Hospital CA is a root until resources.yaml names it. With a real CA, append its
intermediate certificates to `mllp.pem` after yours:
[why](nuances/tls-listening.md#give-the-whole-chain-in-cert_file-level-3).)

**Turn TLS on** for the listener, and ask senders for a certificate from `test-ca` naming
`sender.test`:

```yaml
# interfaces/adt-in/interface.yaml
name: adt-in
about:
  description: ADT from the EHR, kept as files for the lab's archive
  owner: interfaces@hospital.org
  from: { application: Epic, vendor: Epic Systems, hosts: [10.20.30.0/24] }
  to: { application: Lab archive, vendor: In-house }
source:
  mllp:
    port: 6661
    tls:
      certificate: ductus-mllp
      clients: { ca: test-ca, subjects: ["CN=sender.test"] }
destinations: { archive: { file: { dir: out/adt-in }, script: archive.ts } }
```

`ductus validate` checks the certificates as the engine's start will (a certificate it can't use
stops the start, with the reason), and `ductus ports` shows the port is TLS now:

```text
$ ductus ports
…
  PORT  PROTOCOL       HOST       INTERFACE          ROUTE    TLS                                                            ALLOWED
  6661  MLLP over TLS  0.0.0.0    adt-in                      presents ductus-mllp, clients need a certificate from test-ca  anyone
```

**Send.** Start `ductus dev` again, then send as a sender with its certificate, trusting the
lab CA for the engine's:

```text
$ ductus gen --count 10 --to localhost:6661 --ca lab-ca/ca.pem --cert tls/sender.pem --key tls/sender.key
sent 10 messages over 1 connection(s) in 0.1s (79/s), retries 0, rejected 0, refused 0, dropped 0, longest wait 110 ms
```

And see what other senders get. One without TLS (plain `ductus gen --count 1`) sees its
connection closed with no ACK, and keeps retrying as a real sender would (Ctrl-C to stop it):

```text
sending failed, retrying: connection closed before ACK
```

while the engine's log says why, once a minute per sender address
([nuance](nuances/tls-listening.md#a-refused-handshake-is-logged-once-a-minute-per-sender-level-3)):

```text
WARN ductus_runtime::tls: TLS handshake refused (logged once a minute per sender) listener="interface `adt-in` (MLLP)" peer=127.0.0.1 reason=the sender isn't speaking TLS (a plain MLLP or HTTP sender on a TLS port?)
```

One with TLS but no certificate of its own (`--ca lab-ca/ca.pem` without `--cert`) is told
`TLS: the receiver wants a client certificate (name one in --cert)`.

The certificate is watched from now on: the engine alerts 30, 14 and 7 days before it expires,
and a renewal is two files replaced, with no restart
([renewing](configure/tls.md#renewing)).

## 7. To a server

What you built is a folder of files: commit it, and the server runs what's committed.

```sh
git add -A && git commit -m "adt-in: ADT from Epic to the lab archive"
```

`.gitignore` already leaves out what mustn't be in Git (`.ductus/`, `*.key`). For the server:

1. [Install](install/index.md) the engine there, and give it a [database](install/postgres.md).
2. Clone the project as the service's user, and set `DUCTUS_PROJECT` and `DUCTUS_ENV=prod`.
3. **Certificates on the server**: run `ductus cert request` there, so the key never leaves it,
   with the name senders connect to (`--dns mllp.hospital.org`), and send the CSR to the
   hospital's CA. Paths that differ per server belong in the environment
   ([environments](configure/environments.md)), and the CA is the hospital's, not `lab-ca`.
4. `ductus doctor` on the server, then start the service.

The [Configure](configure/index.md) pages go through every file in turn.

### On a new server, start to finish

**What:** the steps above on a fresh Ubuntu 24.04 server, with the Linux package, a database on
another machine (`db1.hospital.org`), MLLP over TLS with client certificates, and a sending
system that has its own certificate. Every command and output below is from the v0.6 demo
(`scripts/demo-v06.sh`), which runs them under real systemd on every push to the repository;
only what's marked `…` is left out.

**Why:** a laptop hides what a server adds: a service account that owns the files, a database
elsewhere, a CA that isn't yours, and senders that must be told what to trust. Seeing it once end
to end shows where each of those comes in.

**When not:** this builds the project on the server, to show every step in one place. In real
life the project comes from Git (step 2 above), and only the certificate's key is made on the
server.

**Install**, then tell the engine where its database is. The package doesn't start the engine:

<!-- scripts/demo-v06.sh -->
```text
$ cd /tmp
$ sudo apt install ./ductus_0.6.0~dev.1_amd64.deb
…
Unpacking ductus (0.6.0~dev.1) ...
Setting up ductus (0.6.0~dev.1) ...

Ductus is installed. It isn't running yet: first tell it where its database and project are.
  1. Edit /etc/ductus/ductus.env (the database URL, the project folder, the environment).
  2. Put the project in /var/lib/ductus/project (git clone), readable by the ductus user.
  3. sudo systemctl enable --now ductus
  4. journalctl -u ductus -f      (it says "started" once every interface is up)
```

(`0.6.0~dev.1` is the demo's development package; a release is `ductus_0.6.0_amd64.deb`.) In
`/etc/ductus/ductus.env` the demo sets one line, the database; `DUCTUS_ENV=prod` and
`DUCTUS_PROJECT=/var/lib/ductus/project` are already the package's defaults
([the file](install/linux.md#configure)):

```sh
# /etc/ductus/ductus.env, in part
DUCTUS_DATABASE_URL=postgres://ductus:…@db1.hospital.org/ductus
```

**The project**, made as the `ductus` user so the service owns it (on a real server: `sudo -u
ductus git clone …` instead), and **the listener's certificate**, its key made on this server:

<!-- scripts/demo-v06.sh -->
```text
$ cd /var/lib/ductus
$ sudo -u ductus ductus init project
created interfaces/adt-in/ (MLLP on port 6661)
…
$ cd /var/lib/ductus/project
$ sudo -u ductus ductus cert request tls/mllp --dns mllp.hospital.org
wrote tls/mllp.key (the new RSA 2048 private key: it stays on this server; only you can read it) and tls/mllp.csr (the request)

Send tls/mllp.csr to the hospital's CA (its PKI or security team). It asks for a certificate for mllp.hospital.org, for a TLS server and client (one certificate can serve listeners and be the engine's client certificate). Never send tls/mllp.key.
…
```

The hospital's PKI team signs the request and sends back the certificate and their CA's own. In
the demo `openssl` plays the CA, on the same machine:

<!-- scripts/demo-v06.sh -->
```text
$ openssl x509 -req -in tls/mllp.csr -CA /root/hospital-ca/ca.pem -CAkey /root/hospital-ca/ca.key -CAcreateserial -out tls/mllp.pem -days 90 -copy_extensions copy
Certificate request self-signature ok
subject=CN = mllp.hospital.org
$ cp /root/hospital-ca/ca.pem tls/hospital-ca.pem
```

The certificates by name, and the interface sending to the lab system's receiver instead of
files (a service writes only where its unit lets it): a destination named `lab` now, running the
code `ductus init` wrote (`archive.ts`; a file's name needn't match its destination's):

```yaml
# resources.yaml
certificates:
  ductus-mllp: { cert_file: tls/mllp.pem, key_file: tls/mllp.key }
  hospital-ca: { ca_file: tls/hospital-ca.pem }
```

```yaml
# interfaces/adt-in/interface.yaml
name: adt-in
about:
  description: ADT from the EHR to the lab system
  owner: interfaces@hospital.org
  from: { application: Epic, vendor: Epic Systems }
  to: { application: Lab system, vendor: In-house }
source:
  mllp:
    port: 6661
    tls:
      certificate: ductus-mllp
      clients: { ca: hospital-ca, subjects: ["CN=epic.hospital.org"] }
destinations: { lab: { mllp: { host: 127.0.0.1, port: 6662 }, script: archive.ts } }
```

**Check before starting**, with the service's own settings (`--env-file` reads `ductus.env` as
the service does), then start it and let `ductus doctor` check every connection from the server:

<!-- scripts/demo-v06.sh -->
```text
$ sudo -u ductus ductus cert show ductus-mllp
✓ certificate `ductus-mllp` (resources.yaml): ./tls/mllp.pem
  subject  CN=mllp.hospital.org
  names    mllp.hospital.org
  issuer   issued by Test Hospital CA
  valid    2026-10-31 → 2027-01-29: 89 days left
  chain    complete: mllp.hospital.org, issued by Test Hospital CA (the CA `hospital-ca` in resources.yaml)
  key      matches (./tls/mllp.key)
→ Senders must trust Test Hospital CA (the hospital's CA): ask them to confirm.
$ sudo -u ductus ductus validate --env-file /etc/ductus/ductus.env
environment: prod
✓ resources: 0 database(s), 0 SFTP server(s), 0 API(s), 2 certificate(s)
✓ adt-in: config and code load
✓ certificate `ductus-mllp`: CN=mllp.hospital.org (mllp.hospital.org), expires 2027-01-29 (89 days)
✓ certificate `hospital-ca`: a CA
⚠ database: plain, no TLS: the server doesn't offer it (sslmode=prefer); whoever answers at its address is believed: add sslmode=verify-full to DUCTUS_DATABASE_URL (sslrootcert=/path/ca.pem for a private CA)
✓ database: new (no engine has run on it yet): no messages wait, no retention is applied
✓ types: no errors

valid
$ sudo systemctl enable --now ductus
Created symlink /etc/systemd/system/multi-user.target.wants/ductus.service → /usr/lib/systemd/system/ductus.service.
$ systemctl show -p StatusText ductus
StatusText=running 1 interface
$ sudo -u ductus ductus doctor --env-file /etc/ductus/ductus.env
ductus doctor · environment prod · 2026-10-31 23:46:41 +00:00
on ductus1 · ductus 0.6.0-dev
checked 3 connection(s) in 0.1 s: 1 ok, 2 warning(s), 0 failed

⚠ engine database  db1.hospital.org:5432 Postgres
    used by  the engine (DUCTUS_DATABASE_URL)
    DNS      db1.hospital.org → 172.18.0.2 (0.9 ms)
    connect  172.18.0.2:5432 from 172.18.0.3 in 0.2 ms
  ⚠ TLS      none: the server doesn't offer TLS (sslmode=prefer), the connection is plain
    login    as `ductus`, database `ductus`: PostgreSQL 18.6 (Debian 18.6-1.pgdg13+2); plain, no TLS: the server doesn't offer it (sslmode=prefer)
    commits  2.2 ms median of 20 (each waits for the disk): strict ingest ≈ 450 msg/s per sender connection, one commit per ACK; more connections share commits. A 1M/day ADT feed averages ~12 msg/s.
  → Queries, their results and the login cross the network readable: ask the DBA to turn on ssl, then set sslmode=verify-full.

✓ MLLP destination  127.0.0.1:6662 MLLP
    used by  adt-in
    address  127.0.0.1 (an address, nothing to look up)
    connect  127.0.0.1:6662 from 127.0.0.1 in 0.3 ms

⚠ listening port  0.0.0.0:6661 MLLP over TLS
    used by  adt-in
  ⚠ port     in use: by a running engine, or another program
    address  127.0.0.1 (an address, nothing to look up)
    connect  127.0.0.1:6661 from 127.0.0.1 in 0.2 ms
    TLS      TLS 1.3, post-quantum key exchange (X25519MLKEM768), serves `ductus-mllp`: certificate for mllp.hospital.org, issued by Test Hospital CA, valid until 2027-01-29 (89 days); covers mllp.hospital.org
    chain    complete: mllp.hospital.org, issued by Test Hospital CA (the CA `hospital-ca` in resources.yaml)
  → If the engine isn't running on this server, another program holds port 6661: find it with `ss -ltnp` (Linux) or `netstat -ano` (Windows).
  → Senders must trust Test Hospital CA (the hospital's CA): ask them to confirm.
  → Doctor can't know the name senders connect with: it must be one of mllp.hospital.org (ask senders which name or address they use).
```

Doctor passes (nothing failed: it exits 0) with two warnings to read, not ignore. The database
connection isn't encrypted: the demo's database server offers no TLS, and a real one should
([PostgreSQL](install/postgres.md)); the warning goes once `DUCTUS_DATABASE_URL` says
`sslmode=verify-full`. Port 6661 is in use: here by the engine itself, which doctor can't tell
from another program; run before the start, the same check says whether the port is free.

**The sending system** gets its own certificate from the same CA (in real life its team asks
for it), and sends:

<!-- scripts/demo-v06.sh -->
```text
$ ductus cert request /root/epic/sender --dns epic.hospital.org
…
$ openssl x509 -req -in /root/epic/sender.csr -CA /root/hospital-ca/ca.pem -CAkey /root/hospital-ca/ca.key -CAcreateserial -out /root/epic/sender.pem -days 90 -copy_extensions copy
Certificate request self-signature ok
subject=CN = epic.hospital.org
$ ductus gen --to mllp.hospital.org:6661 --ca /root/hospital-ca/ca.pem --cert /root/epic/sender.pem --key /root/epic/sender.key --count 100
sent 100 messages over 1 connection(s) in 0.6s (156/s), retries 0, rejected 0, refused 0, dropped 0, longest wait 139 ms
```

The lab system's receiver (`ductus sink` in the demo) got all 100, once each, in order. A sender
without a certificate of its own is refused before anything is read, and the engine's log says
why, once a minute per sender:

<!-- scripts/demo-v06.sh -->
```text
$ timeout 5 ductus gen --to mllp.hospital.org:6661 --ca /root/hospital-ca/ca.pem --count 1
sending failed, retrying: TLS: the receiver wants a client certificate (name one in --cert)
$ journalctl -u ductus -o cat --no-pager | grep "TLS handshake refused" | tail -1
2026-10-31T23:47:03.775177Z  WARN ductus_runtime::tls: TLS handshake refused (logged once a minute per sender) listener="interface `adt-in` (MLLP)" peer=127.0.0.1 reason=the sender presented no client certificate
```

The demo then upgrades this server and goes back, both under traffic:
[Upgrade](upgrade.md#on-a-server-the-v06-demo) shows it.
