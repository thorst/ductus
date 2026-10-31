# TLS and certificates

TLS encrypts a connection and proves who is at the other end. The engine uses it in three
places, all set up the same way, with certificates named once in resources.yaml:

| Where | Settings | The engine |
|---|---|---|
| **On what it listens on**: MLLP and HTTP sources, the API and web UI | `tls:` on the source, `api.tls` | Presents its certificate; optionally asks senders for theirs |
| **On what it connects to**: MLLP and HTTP destinations, APIs, databases, the directory, the mail server, webhooks | `tls:` / `ca:` / `certificate:` on each | Checks the receiver's certificate; optionally presents its own |
| **To its own database** | `sslmode` in the URL | [PostgreSQL's TLS](../install/postgres.md#tls-to-the-database) |

**Use TLS** wherever messages cross a network you don't own, and for the API and web UI whenever
they're reachable from another machine. It costs nothing you'll see on an MLLP feed: ingest
measured the same with and without, about 1 ms per new connection, ~23 kB of memory per open one
([measured](../nuances/tls-listening.md#what-tls-costs-level-4)). Listeners speak TLS 1.2 and 1.3 only, and
every connection prefers a post-quantum key exchange where the other side offers one
([nuance](../nuances/tls-listening.md#post-quantum-key-exchange-on-by-default-never-in-the-way-level-3)).

## Certificates in resources.yaml

Every certificate and CA the engine uses is a
[`certificates`](../reference/config/resources.md#certificates) entry, by a name:

```yaml
# resources.yaml
certificates:
  ductus-mllp:                              # a pair the engine presents
    cert_file: ${TLS_DIR}/mllp.pem          # the certificate, then its intermediates
    key_file: ${TLS_DIR}/mllp.key
    owner: infrastructure@hospital.org      # who renews it: expiry alerts reach them
  ductus-api:
    cert_file: ${TLS_DIR}/api.pem
    key_file: ${TLS_DIR}/api.key
  hospital-ca:                              # a CA alone: trusted, or checked against
    ca_file: ${TLS_DIR}/hospital-ca.pem
  partner-ca:
    ca: ${secret:PARTNER_CA}                # PEM text from a secret instead of a file
```

- **A pair** (`cert_file` + `key_file`) is what the engine presents: on a listener, or as its
  client certificate. `cert_file` holds the certificate first, then the intermediate
  certificates that issued it (not the root): the "full chain" a CA sends. Java and .NET senders
  don't fetch a missing intermediate, and refuse the connection
  ([nuance](../nuances/tls-listening.md#give-the-whole-chain-in-cert_file-level-3)).
- **The key** is unencrypted PEM, readable by the engine's account and nobody else
  (`chmod 600`, owned by `ductus`; on Windows, the service's account only).
- **A CA** (`ca_file`, or `ca` as text) is trusted for a connection that names it, or checks
  senders' client certificates.
- **From a vault** instead of files: `cert` and `key` as PEM text, usually `${secret:…}`. Read at
  start only, so a renewal needs a restart, unlike files.
- **Paths**: relative to the project folder unless written whole; on Windows, write the drive
  ([nuance](../nuances/windows.md#certificate-files-write-the-drive-level-1)). Keep the files outside the
  project on a server (`/etc/ductus/tls`, readable by the `ductus` group), with the path in the
  environment (`${TLS_DIR}`), so keys never go near Git.
- **Checked before anything runs**: every entry is loaded at start, before the database. A
  missing file, a key that isn't the certificate's, or an expired certificate stops the start with
  the entry and the reason ([nuance](../nuances/tls-listening.md#a-certificate-the-engine-cant-use-stops-the-start-with-the-reason-level-2)).
  `ductus validate` checks the same, with each certificate's names and days left.

## Getting a certificate from your CA

```sh
ductus cert request /etc/ductus/tls/mllp --dns mllp.hospital.org --subject "O=General Hospital, OU=Interfaces"
```

writes a new private key (`mllp.key`, readable only by you, never overwritten) and a request
(`mllp.csr`), on the server that will use it, and says what to do next: send the `.csr` (never
the `.key`) to the hospital's CA, put what comes back in `mllp.pem` (yours first, then the
intermediates), and check it. The key is RSA 2048, which every CA takes (`--key ecdsa` for
P-256); one certificate serves as both a server's and a client's
([nuance](../nuances/tls-listening.md#a-certificate-from-the-hospitals-ca-request-then-show-level-2)).

```sh
ductus cert show /etc/ductus/tls/mllp.pem      # or the entry's name: ductus cert show ductus-mllp
```

says what the file holds and judges it the way the engine and senders will: the names, who
issued it, days left (the chain's earliest), whether the chain is complete and in order, and
whether the key matches. It exits 1 when the engine would refuse it, and says when a file is a
CSR, a key or DER instead of PEM. Run it before naming a certificate in resources.yaml.

**For a lab** with no CA, `ductus cert self-signed tls/lab --name localhost` makes a pair to try
TLS with; senders must be given its `.pem` to trust. [Your first interface](../first-interface.md#6-the-same-over-tls-with-client-certificates)
plays a CA with `openssl` for the whole path.

### From a PFX (Windows, Active Directory Certificate Services)

Windows tools export a certificate with its key as a `.pfx` (PKCS#12) file under a password. The
engine reads PEM: convert it with `openssl`, which is on every Linux server and comes with Git for
Windows (`C:\Program Files\Git\usr\bin\openssl.exe`):

```sh
# the certificate first, then the CA certificates in the file
openssl pkcs12 -in mllp.pfx -clcerts -nokeys | openssl x509 -out mllp.pem
openssl pkcs12 -in mllp.pfx -cacerts -nokeys | sed -n '/BEGIN/,/END/p' >>mllp.pem
# the key, unencrypted (protect the file: chmod 600, or the service's account only)
openssl pkcs12 -in mllp.pfx -nocerts -nodes | openssl pkey -out mllp.key
ductus cert show mllp.pem
```

Each asks for the PFX's password. A PFX from an older Windows (RC2-encrypted) makes OpenSSL 3 say
"unsupported … RC2-40-CBC": add `-legacy` to each `openssl pkcs12`. The PFX's CA certificates may
include the root, which `cert show` notes as harmless to send; it checks the order too. In
PowerShell without Git Bash, run the same with the full path to `openssl.exe`, and use
`Select-String` or an editor in place of `sed` to keep only the `BEGIN`…`END` blocks.

## On what the engine listens on

`tls:` on an MLLP or HTTP source makes its port TLS only, presenting the certificate named:

```yaml
# interfaces/adt-in/interface.yaml, in part
source:
  mllp:
    port: 6661
    tls:
      certificate: ductus-mllp
      clients:
        ca: hospital-ca
        subjects: ["CN=epic-bridge*.hospital.org"]
```

- **`certificate`**: the pair it presents, for the name senders connect to.
- **`clients`** (optional): senders must present a certificate of their own, issued by `ca`
  and, with `subjects`, naming an allowed system: `CN`, `O`, `OU`… parts with `*` and `?`
  wildcards, or `DNS=` for a certificate's DNS names. **Keep `subjects` narrow**: without it, every
  certificate that CA ever issued (often every machine in the hospital) is let in
  ([nuance](../nuances/tls-listening.md#a-client-certificate-says-which-system-sent-not-who-level-3)).
- **Senders on plain MLLP can't connect** once it's on, so agree on a date with them. One that
  tries gets its connection closed, no ACK; the engine's log says why, once a minute per sender
  ([nuance](../nuances/tls-listening.md#a-refused-handshake-is-logged-once-a-minute-per-sender-level-3)).
- **Senders on TLS 1.0 or 1.1** can't connect at all
  ([nuance](../nuances/tls-listening.md#senders-on-tls-10-or-11-cant-connect-level-3)).
- HTTP interfaces sharing a port share its `tls`.
- `allow:` beside it says where senders may connect from; client certificates say which system
  they are. Use both where it matters ([who may connect](sources.md#who-may-connect-allow)).

To see the chain a listener really serves, run the full checks (Connections → Run the checks):
the engine connects to each of its TLS listeners and judges what it gets, a missing
intermediate included. It can't know which name senders connect by, so a certificate without
that name shows only at a sender ([nuance](../nuances/certificates.md#doctor-reads-the-chain-our-listeners-serve-but-not-the-name-senders-use-level-3)).

### The web UI and the API

```yaml
# ductus.yaml, in part
api:
  listen: 0.0.0.0:7700
  tls: { certificate: ductus-api }
  allow: [127.0.0.1, 10.1.50.0/24]
```

**Needed as soon as the API is reachable from another machine**: the web UI's sign-in cookie is
only sent over HTTPS (without `tls`, signing in from another computer doesn't work at all), and
tokens and passwords would cross the network readable. The start and `ductus validate` warn about
an API on the network without it ([nuance](../nuances/tls-listening.md#the-api-on-the-network-needs-tls-level-1)).
Browsers that have seen it over HTTPS won't try plain HTTP to it again for a year (HSTS).
`clients:` works here too, for an API only certain systems may call.

## On what the engine connects to

Every TLS connection the engine makes checks that the other side's certificate comes from a CA
this server trusts and is for the host connected to. Add a private CA, or a client certificate,
per connection:

```yaml
# interfaces/orders-to-lab/interface.yaml, in part
destination:
  mllp:
    host: ${LAB_HOST}
    port: ${LAB_PORT}
    tls: { ca: hospital-ca, certificate: ductus-mllp }
```

- **`tls: true`** on an MLLP destination: TLS, trusting this server's CAs. **`ca:`** also trusts
  a `certificates:` CA, for that connection only, so a partner's CA isn't trusted by everything
  on the server ([nuance](../nuances/tls-connecting.md#a-private-ca-is-trusted-only-where-you-name-it-level-2)).
  Ask the receiver's team for the CA that issued their certificate, not their certificate itself
  (which changes at every renewal).
- **`certificate:`** presents a pair as the engine's client certificate (mutual TLS). Tell the
  receiver's team its subject and the CA that issued it
  ([nuance](../nuances/tls-connecting.md#our-client-certificate-tell-the-receiver-what-to-expect-level-2)).
- **Connect by the name on their certificate**: one for `lab.hospital.org` is refused at
  `10.1.2.3` unless it lists that address.
- **The same `ca:` and `certificate:`** go on an `apis:` entry (which `api()` and `http`
  destinations through it share), an HTTP destination with its own `https://` URL, a Postgres
  entry, the mail server, webhooks and the heartbeat; `ca:` alone on a SQL Server entry and the
  directory.
- **A refused handshake is a temporary error**: the message waits and is tried again, with the
  reason on the destination ("TLS: the receiver wants a client certificate")
  ([nuance](../nuances/tls-connecting.md#a-refused-handshake-is-a-temporary-error-the-message-waits-level-2)).
- `ductus doctor --interface orders-to-lab` makes the engine's own handshake, with the same
  settings, before you deploy ([nuance](../nuances/tls-connecting.md#doctor-makes-the-engines-handshake-level-3)).

An HTTP destination that goes through an `apis:` entry (`http: { api: billing }`) uses that
entry's `ca` and `certificate`; on the destination itself they're only for one with its own
https `url`, and `validate` refuses them elsewhere
([nuance](../nuances/tls-connecting.md#an-http-destination-through-an-api-uses-the-apis-tls-level-2)).

## Renewing

**Replace both files; no restart.** The engine reads every `cert_file` and `key_file` again every
minute (one task for the whole engine, comparing their contents). A new pair that loads is used
for new connections from then on; connections already open keep theirs until they close. A new
pair that doesn't load (half copied, a new certificate with the old key, expired) is logged once
as an error, and the engine keeps presenting the pair it had: **a bad renewal never takes a
listener down** ([nuance](../nuances/tls-listening.md#renewing-replace-both-files-no-restart-level-2)). After
writing them, look for `certificate renewed` in the log within a minute, or the "Ours" group on
the web UI's Connections page.

**You'll be told in time.** Every certificate the engine presents or checks is watched: an alert
at 30, 14 and 7 days before it expires, to its `owner` with `to: owner`, and critical once expired
([alerts](alerts.md); [nuance](../nuances/certificates.md#a-certificate-alert-is-told-at-30-14-and-7-days-and-when-it-expires-level-2)).

**What's watched, and how the days are counted.** Every TLS connection the project makes:
HTTPS APIs and destinations, MLLP with `tls:`, the directory, Postgres and SQL Server when
verified, mail and webhooks ([the list](../nuances/certificates.md#what-has-a-certificate-the-engine-watches-level-3)). The countdown
runs to the earliest expiry in the chain, intermediates included, in whole days
([nuance](../nuances/certificates.md#the-earliest-expiry-in-the-chain-counts-in-whole-days-level-2)). A check that can't connect shows the error and
keeps the dates it last read ([nuance](../nuances/certificates.md#a-check-that-cant-connect-keeps-the-last-dates-level-2)); behind a
load balancer that ends TLS, it's the balancer's certificate you see
([nuance](../nuances/certificates.md#behind-a-load-balancer-you-see-the-balancers-certificate-level-3)). The engine's own pairs count down from their
files, with no connection made ([nuance](../nuances/certificates.md#our-certificates-count-down-like-the-rest-with-no-check-level-2)), and
renewed files that can't be used show on the pair's row, not as an alert: the pair in use still
counts down ([nuance](../nuances/certificates.md#a-renewal-that-cant-be-used-shows-on-its-row-not-as-an-alert-level-2)). A refused certificate is
said once, in the check's result and the delivery's error, not logged as an error at every check
([nuance](../nuances/certificates.md#a-refused-certificate-is-said-once-not-logged-as-an-error-each-time-level-4)).

**By hand** (a CA without automation, such as AD CS's web enrollment): a new request under another
name, signed, checked, then copied over the files in use:

```sh
ductus cert request /etc/ductus/tls/mllp-2027 --dns mllp.hospital.org
# … the CA signs mllp-2027.csr; put the result in mllp-2027.pem …
ductus cert show /etc/ductus/tls/mllp-2027.pem
cp /etc/ductus/tls/mllp-2027.pem /etc/ductus/tls/mllp.pem
cp /etc/ductus/tls/mllp-2027.key /etc/ductus/tls/mllp.key
```

**With a renewal tool**: anything that writes new PEM files works, with no engine feature
needed. Point it at the files the entry names (or copy into them from its own folder after each
renewal), make sure the engine's account can read the result, and run `ductus cert show` on the
first one it writes. Three common ones:

- **certbot** (Linux, ACME: Let's Encrypt for public names, or your own ACME CA with `--server`):
  a deploy hook copies each renewal into place:

  ```sh
  certbot certonly --standalone -d mllp.hospital.org \
    --deploy-hook 'install -o ductus -g ductus -m 600 "$RENEWED_LINEAGE/privkey.pem" /etc/ductus/tls/mllp.key &&
                   install -o ductus -g ductus -m 644 "$RENEWED_LINEAGE/fullchain.pem" /etc/ductus/tls/mllp.pem'
  ```

  `fullchain.pem` is the certificate with its intermediates, as `cert_file` wants. Let's Encrypt
  only issues for names that resolve publicly (or through DNS validation): most interface
  engines' names are internal, so certbot is mostly for an internal ACME CA.
- **win-acme** (Windows, ACME): its PEM files store writes the certificate and key into a
  folder (`--store pemfiles --pemfilespath C:\ProgramData\Ductus\tls`); name the certificate
  with its chain and the key in the entry (`cert show` says which file is which), and grant the
  service's account read access to the folder.
- **step** (Smallstep's CLI, with step-ca or another ACME CA):
  `step ca certificate mllp.hospital.org mllp.pem mllp.key` once, then
  `step ca renew --daemon mllp.pem mllp.key` keeps renewing the same two files in place.

Certificates from secrets (`cert`/`key`) and CAs change with a deploy and a restart.

## When it doesn't work

| The engine (or the sender) says | It means | Do |
|---|---|---|
| "the sender isn't speaking TLS" | A plain MLLP or HTTP sender on a TLS port | Turn TLS on at the sender, or point it at a plain port |
| "the receiver wants a client certificate" | The receiver asks for mutual TLS | Name a pair in `certificate:` |
| "chain incomplete" (`cert show`, doctor) | `cert_file` lacks the intermediates | Append them after yours |
| "no TLS version or cipher in common" | A sender on TLS 1.0/1.1 | Upgrade the sender, or a TLS proxy for it alone |
| A receiver's certificate "presents a CA's certificate (CA:TRUE) as its own" | A lab receiver made with `openssl req -x509` | Name that exact certificate in `ca:` ([nuance](../nuances/tls-listening.md#a-cas-certificate-as-a-servers-level-3)) |
| "timed out waiting for ACK" right after turning `tls:` on | The receiver's port speaks plain MLLP | Ask which port is TLS |

To try a TLS listener or receiver before the real systems are ready, `ductus gen --tls` plays the
sender and `ductus sink --tls` the receiver, checking certificates as the engine does and saying
why a handshake fails ([nuance](../nuances/tls-listening.md#testing-tls-without-the-real-systems-level-1)).

`ductus doctor` checks every TLS connection the project makes, from the server, and names the
step that fails.
