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
│       ├── transform.ts     what happens to each one
│       ├── transform.test.ts
│       └── fixtures/sample.hl7
├── .ductus/, tsconfig.json, .vscode/settings.json    for your editor (below)
├── .gitignore, README.md
```

Open the folder in VS Code. With Red Hat's YAML extension, every setting in the YAML files
completes as you type and shows its description on hover; in `transform.ts`, hovering
`msg.get("PID-5")` shows *PID-5 Patient Name*. The schemas and typings in `.ductus/` come from
the `ductus` you have installed, so they match it ([editor support](configure/index.md#in-your-editor)).

## 2. The interface

`interfaces/adt-in/interface.yaml`, as generated (its comments show what else you could write):

```yaml
# interfaces/adt-in/interface.yaml
name: adt-in
about:
  description: ""
  owner: ""
  from: { application: "", vendor: "", hosts: [] }   # hosts: where senders connect from (IPs or subnets)
  to: { application: "", vendor: "" }
source: { mllp: { port: 6661 } }
script: transform.ts
destination: { file: { dir: out/adt-in } }
```

- [`source`](reference/config/interface.md#sourcemllp): listen for HL7 over MLLP on port 6661.
  Each message is stored before its ACK goes back to the sender.
- [`script`](reference/config/interface.md#script): run `transform.ts` on each message.
- [`destination`](reference/config/interface.md#destinationfile): write each result as a file.
  The folder is relative to the interface's own folder, so the files land in
  `interfaces/adt-in/out/adt-in/`.
- [`about`](reference/config/interface.md#about): what the interface is for, who owns it, and
  the systems on each side. Nothing here changes how messages flow; it's what `ductus impact`
  and the web UI read.

The code, `transform.ts`, gets each message and returns what to send:

```ts
import type { Message } from "@ductus/sdk";

export function transform(msg: Message) {
  // Example: set the receiving application.
  msg.set("MSH-5", "RECEIVER");
  return msg;
}
```

Returning `null` would drop the message (filtered, with the reason kept); throwing would
quarantine it for a person to look at ([nuance](nuances.md#return-null-filters-throwing-quarantines-level-1)).

## 3. Test and validate

```text
$ ductus test
adt-in
  ✓ sets the receiving application and keeps the patient (0ms)

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
script: transform.ts
destination: { file: { dir: out/adt-in } }
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
`transform.ts`:

```ts
import type { Message } from "@ductus/sdk";

export function transform(msg: Message) {
  msg.set("MSH-5", "LAB");
  msg.set("PID-5.1", msg.get("PID-5.1").toUpperCase());   // PID-5.1: family name
  return msg;
}
```

and say so in its test, `transform.test.ts`:

```ts
import { expect, input, type Message, test } from "@ductus/sdk";
import { transform } from "./transform.ts";

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
([nuance](nuances.md#missing-fields-read-as--and-never-throw-level-1)).

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
[why](nuances.md#give-the-whole-chain-in-cert_file-level-3).)

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
script: transform.ts
destination: { file: { dir: out/adt-in } }
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
([nuance](nuances.md#a-refused-handshake-is-logged-once-a-minute-per-sender-level-3)):

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
