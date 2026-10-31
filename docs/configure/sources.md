# Sources and formats

An interface's [`source`](../reference/config/interface.md#source) is where its messages come
from: exactly one of these.

| Source | Messages come from | Use it for |
|---|---|---|
| [`mllp`](../reference/config/interface.md#sourcemllp) | Senders connecting over MLLP | HL7 v2 feeds: ADT, orders, results from an EHR, a lab, a device gateway |
| [`http`](../reference/config/interface.md#sourcehttp) | Requests to a path | Systems that push JSON or XML; REST APIs other systems call and wait on |
| [`file`](../reference/config/interface.md#sourcefile) | Files dropped in a folder on this server or a share | Batch files, exports from systems that only write files |
| [`sftp`](../reference/config/interface.md#sourcesftp) | Files on an SFTP server | Partners who leave files on their server, or yours |
| [`schedule`](../reference/config/interface.md#sourceschedule) | The code, on a timer | Pulling: a nightly query, an API polled every few minutes |
| `internal` | Other interfaces' routes | The second half of a chain: messages one interface hands to another |

Every source **stores a message before it says it has it**: an MLLP sender's ACK, an HTTP
`202`, a file's move or delete come only after the message is in the database. A sender that
has its acknowledgement never needs to send again; a crash before it loses nothing the sender
thinks was taken.

## MLLP

```yaml
# interfaces/adt-in/interface.yaml, in part
source:
  mllp:
    port: 6661
    host: 0.0.0.0            # the default: every IPv4 address of the server
    allow: [10.20.30.0/24]   # who may connect (below)
```

The engine listens on `port`; senders connect, send HL7, and get an ACK once each message is
stored (AA), or an AE when it can't be (the database is unreachable: they send it again). Each
MLLP source has a port of its own; `ductus new interface` picks a free one from `ports.mllp`
in ductus.yaml, and `ductus validate` reports two on one port. A sender may open any number of
connections. An MLLP source always carries HL7.

- **`host`**: which of the server's addresses to listen on: `0.0.0.0` (default, every IPv4
  address), `::` (IPv6 too), one address, or `127.0.0.1` (this machine only, for tests).
- **`tls`**: accept only TLS, presenting a certificate, and optionally take only senders with a
  client certificate from a CA you name: [TLS on listeners](tls.md#on-what-the-engine-listens-on).
  Senders on plain MLLP can't connect then, so agree on a date with them.

## HTTP

```yaml
# interfaces/orders-in/interface.yaml, in part
source:
  http:
    port: 8080
    path: /orders
    method: POST             # the default
    mode: queue              # the default
format: json
```

- **`mode: queue`** (the default): the request body is stored as a message, answered
  `202 Accepted` once stored, and delivered like any other. For systems that push data.
- **`mode: respond`**: the code runs on the request and its result is the response, at once.
  **Nothing is stored or queued**, there's no destination, and if something fails the caller
  gets an error status and decides whether to try again
  ([nuance](../nuances.md#respond-mode-http-requests-arent-stored-level-3)). For REST APIs and
  lookups that other systems call and wait on:

  ```yaml
  # interfaces/patient-lookup/interface.yaml
  name: patient-lookup
  source:
    http: { port: 8081, path: "/patients/{mrn}", method: GET, mode: respond }
  script: lookup.ts
  format: json
  ```

  `{mrn}` matches one path segment and is given to the code. In respond mode, calls the code
  makes one after another add up; `await` them together where you can
  ([nuance](../nuances.md#in-respond-mode-calls-run-one-after-another-unless-you-await-them-together-level-2)).
- **Several HTTP interfaces can share a port**, each with its own path (two can't have the same
  method and path). They're one server then, so they must have the same `tls` and `allow`
  ([nuance](../nuances.md#http-interfaces-on-one-port-share-its-tls-level-2)).

## Files and SFTP

```yaml
# interfaces/census-in/interface.yaml, in part
source:
  file:
    dir: '/data/inbound/census'   # Windows: 'D:\inbound\census', single-quoted
    pattern: "CENSUS_*.csv"
    poll: 30s
    after: move                   # the default: into dir/archive
    split: records                # each row is a message
format: csv
```

```yaml
# interfaces/quest-results/interface.yaml, in part
source:
  sftp: { server: quest, dir: outbound, pattern: "*.csv", after: delete, split: records }
format: csv
```

An [`sftp`](../reference/config/interface.md#sourcesftp) source takes the same settings as a
[`file`](../reference/config/interface.md#sourcefile) one, plus `server`: an `sftp:` entry in
resources.yaml with the address and login ([SFTP servers](resources.md#sftp-servers)).

- **A file is read once it stops changing**: its size and time the same at two looks in a row,
  `poll` apart (default `10s`), so a file still being written is never read half done. Expect it
  one to two polls after its writer finishes
  ([nuance](../nuances.md#a-file-is-picked-up-one-poll-after-it-stops-changing-level-3)). A writer
  that says when it's done (a `.done` file beside it) can use `marker: .done` instead.
- **Files keep their order**, oldest first, one at a time: if one fails, later ones wait
  ([nuance](../nuances.md#one-file-at-a-time-oldest-first-a-failure-stops-the-rest-level-3)).
- **`split: records`** makes each record a message (an HL7 batch at each MSH, CSV at each row
  with the header kept, fixed-length and text at each line), read as a stream, so a file of any
  size works; after a stop it carries on from the last record stored. Without it, a file is one
  message, up to 256 MB.
- **`after`**: `move` (the default, to `archive_dir`, by default `archive` inside `dir`) or
  `delete`, once everything in the file is stored. The engine never deletes what it moved: clean
  the archive yourself.
- **The service must be allowed to move them**: on Linux, add the folders to the unit
  ([nuance](../nuances.md#on-linux-the-service-can-write-only-varlibductus-until-you-add-a-folder-level-2));
  on Windows, grant the service's account
  ([nuance](../nuances.md#the-windows-service-can-only-touch-what-you-give-it-level-2)).
- A relative `dir` is relative to the interface's folder.

## Schedules

```yaml
# interfaces/nightly-census/interface.yaml
name: nightly-census
source: { schedule: { cron: "0 2 * * *" } }   # 02:00 every day, the server's local time
script: census.ts
format: csv
destination: { sftp: { server: quest, dir: census } }
```

No input: the code runs on the timer and what it returns becomes the message. `every: 15m` runs
at start, then at that interval (for polling); `cron` runs at the times a five-field expression
gives (for a time of day). A run is skipped while the previous one's message is still waiting,
and runs missed while the engine was down are made up by one run, not one per missed time
([nuance](../nuances.md#cron-uses-the-servers-local-time-missed-runs-collapse-to-one-level-3)).

## Internal: from another interface

```yaml
# interfaces/adt-to-billing/interface.yaml
name: adt-to-billing
source: internal
script: billing.ts
destination: { http: { api: billing, path: /charges } }
```

An interface with `source: internal` receives what other interfaces route to it with
`destination: { interface: adt-to-billing }`: stored in the same transaction that marks it sent
there, so nothing is lost or sent twice between the two
([routes](destinations.md#to-another-interface)).

## Who may connect: `allow`

```yaml
# interfaces/adt-in/interface.yaml, in part
source:
  mllp:
    port: 6661
    allow: [10.20.30.0/24, 10.20.31.15, epic-bridge01.hospital.org]
```

`allow` on an MLLP or HTTP source lists who may connect: addresses, subnets and host names.
Anyone else is disconnected before a byte is read, before any TLS handshake, and logged once a
minute per address. **Without it, anyone who can reach the port may connect.**

- **Why**: it names the sending system, which "allow one connection only" (a habit from other
  engines) never did; a sender may open several connections, and its reconnect after a blip
  isn't refused ([nuance](../nuances.md#an-allow-list-says-who-which-one-connection-only-never-did-level-2)).
- **Keep the firewall rules too**: each catches the other's mistakes
  ([nuance](../nuances.md#its-defence-in-depth-next-to-the-firewall-level-2)).
- **List** the sender's addresses or subnet, its failover and DR hosts, and anything that relays
  for it. Host names are looked up at start and every 5 minutes, never per connection
  ([nuance](../nuances.md#host-names-are-looked-up-at-start-and-every-5-minutes-level-3)). Behind
  a proxy or load balancer, the address seen is the proxy's.
- `ductus validate` warns when `about.from.hosts` names a sender the list would refuse.

## Formats

[`format`](../reference/config/interface.md#format) is what the messages are, so the code gets
them parsed and keys (an ordering key, dedupe keys) can be read from them: `hl7` (the default),
`json`, `xml`, `text`, [`csv`](../reference/config/interface.md#formatcsv) or
[`fixed`](../reference/config/interface.md#formatfixed) (fixed-length records).

| Format | The code gets | Keys are written |
|---|---|---|
| `hl7` | Segments and fields (`msg.get("PID-3.1")`), numbered from 1 as the standard does | `PID-3.1`, `MSH-10` |
| `json` | The document | `order.id` |
| `xml` | The document | `Order/@id` |
| `csv` | Each row an object by column name (with a header line) or a list (without) | A column's name, or its number from 1 |
| `fixed` | Each record an object by field name | A field's name |
| `text` | The payload as a string | |

```yaml
# interfaces/claims-in/interface.yaml, in part
format:
  fixed:
    fields:
      - { name: mrn, width: 10 }
      - { name: last_name, width: 20 }
      - { name: amount, width: 10, align: right, pad: "0" }
```

`format: csv` alone means commas and a header line; `{ csv: { delimiter: "|", header: false } }`
for others.

[`output`](../reference/config/interface.md#output) is the format the code's results are written
in when it isn't `format`: HL7 in, JSON out to an API. A destination can set its own `output`
([destinations](destinations.md)). Default: the same as `format`.
