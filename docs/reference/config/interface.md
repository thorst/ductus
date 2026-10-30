# `interface.yaml`

`interface.yaml`: one interface's settings. Where its messages come from (`source`), how they're read (`format`), where they go (`destination`, or `destinations` for several), and how they're delivered, kept, watched and documented. Its code is in the `script` beside it.

Each interface has a folder of its own under `interfaces/`, holding this file, its code, its tests and sample messages. Only `name`, `source` and a destination must be given; everything else has a default that's safe to start with.

Schema for editors: `docs/schema/interface.json` (written into a project by `ductus types`). Files: `interfaces/**/interface.yaml`.

### `name`

**Type:** text Â· **Required**

The interface's name, as the database, the web UI, logs, alerts and routes (`destination: { interface: <name> }`) know it. Use the folder's name, as `ductus new interface` does, and keep it short and stable: lower-case words joined by `-`.

Messages are stored under the name, so a renamed interface is a new one: the old name's history stays under the old name, and messages still waiting there are sent by nobody. Rename only when nothing is waiting. Required. Example: `name: adt-epic-to-lab`.

### `source`

**Type:** settings, below, or text Â· **Required**

Where messages come from: exactly one of `mllp`, `http`, `file`, `sftp`, `schedule` or `internal`, each with its own settings. Required. Example: `source: { mllp: { port: 6661 } }`.

### `source.mllp`

**Type:** settings, below

HL7 v2 over MLLP: the engine listens on `port`, and senders (an EHR, a lab system) connect and send. Each message is stored before its ACK goes back, so a sender that has its ACK never has to send it again. Example: `source: { mllp: { port: 6661 } }`.

### `source.mllp.host`

**Type:** text

The address to listen on: `0.0.0.0` (every IPv4 address of the server), `::` (every address, IPv6 too), one of the server's addresses, or `127.0.0.1` (this machine only, for testing). Default: `0.0.0.0`. Example: `host: 10.20.0.15`.

### `source.mllp.port`

**Type:** number Â· **Required**

The port senders connect to. Each MLLP source needs a port of its own; `ductus new interface` picks a free one from `ports.mllp` in ductus.yaml, and `ductus validate` reports two interfaces on one port. Required. Example: `port: 6661`.

### `source.mllp.tls`

**Type:** settings, below

Accept only TLS on this port: the engine presents the `certificate` named here, and with `clients`, senders must present one too. Senders on plain MLLP can't connect then, so agree on a date with them. Default: plain MLLP. Example: `tls: { certificate: ductus-mllp }`.

### `source.mllp.tls.certificate`

**Type:** text Â· **Required**

The certificate this listener presents: a `certificates:` entry in resources.yaml with the certificate and its key, for the name senders connect to. A certificate the engine can't use (expired, its key doesn't match) stops the start with the reason; renewed files are picked up within a minute, without a restart. Required. Example: `certificate: ductus-mllp`.

### `source.mllp.tls.clients`

**Type:** settings, below

Ask senders for a certificate of their own, and refuse any connection without one that `ca` issued and, with `subjects`, that names an allowed system. A client certificate says which system is connecting, beside `allow:`'s where from. Default: no client certificates asked for. Example: `clients: { ca: hospital-ca, subjects: ["CN=*.lab.hospital.org"] }`.

### `source.mllp.tls.clients.ca`

**Type:** text Â· **Required**

The CA that issues senders' certificates: a `certificates:` entry holding the CA's certificate (`ca` or `ca_file`). Only certificates it issued are taken. Required. Example: `ca: hospital-ca`.

### `source.mllp.tls.clients.subjects`

**Type:** list of text

Which of that CA's certificates are taken: a certificate matching any one pattern. A pattern is `ATTR=value` parts separated by commas, all of which must match, with `*` and `?` wildcards and case ignored: `CN`, `O`, `OU`, `C`, `L`, `ST`, `DC`, or `DNS` for the certificate's DNS names. Needed when the CA issues certificates to more than your senders (a hospital-wide CA). Default: any certificate the CA issued. Example: `subjects: ["CN=bridges*.hospital.org", "O=Quest, OU=Lab"]`.

### `source.mllp.allow`

**Type:** list of text

Who may connect: addresses (`10.1.30.15`), subnets (`10.1.20.0/24`) and host names (looked up at start and every 5 minutes). Anyone else is disconnected before a byte is read and logged once a minute per address. A second line of defence beside the firewall, and the way to say "only the lab's interface engine". Default: anyone who can reach the port. Example: `allow: [10.1.20.0/24, epic-bridge01.hospital.org]`.

### `source.http`

**Type:** settings, below

An HTTP endpoint: the engine listens on `port` for requests to `path`. In `queue` mode the body is stored as a message and answered `202 Accepted`; in `respond` mode the code answers the request itself (a REST API, a lookup). Several HTTP interfaces can share a port with different paths, if they have the same `tls` and `allow`. Example: `source: { http: { port: 8080, path: /orders } }`.

### `source.http.host`

**Type:** text

The address to listen on: `0.0.0.0` (every IPv4 address of the server), `::` (every address, IPv6 too), one of the server's addresses, or `127.0.0.1` (this machine only). Default: `0.0.0.0`. Example: `host: 10.20.0.15`.

### `source.http.port`

**Type:** number Â· **Required**

The port to listen on. HTTP interfaces may share one (each with its own path); `ductus new interface` picks a free one from `ports.http` in ductus.yaml. Required. Example: `port: 8080`.

### `source.http.tls`

**Type:** settings, below

HTTPS only, presenting the `certificate` named here; with `clients`, callers must present one too. Interfaces sharing a port must name the same. Default: plain HTTP. Example: `tls: { certificate: ductus-api }`.

Its settings are those of [`source.mllp.tls`](#sourcemllptls).

### `source.http.allow`

**Type:** list of text

Who may connect: addresses, subnets and host names, as for MLLP. Checked before the request is read, so interfaces sharing a port must have the same list. Default: anyone who can reach the port. Example: `allow: [10.1.20.0/24]`.

### `source.http.path`

**Type:** text Â· **Required**

The path requests come to, starting with `/`. `{name}` parts match one path segment and are given to the code (`/patients/{mrn}` takes `/patients/12345`). Two interfaces on one port can't have the same method and path. Required. Example: `path: /patients/{mrn}`.

### `source.http.method`

**Type:** text

The HTTP method this interface answers: `GET`, `POST`, `PUT`, `PATCH` or `DELETE`. Default: `POST`. Example: `method: GET`.

### `source.http.mode`

**Type:** `queue`, `respond`

- `queue`: store the request body as a message, answer `202 Accepted` once it's stored, and deliver it like any other message. For senders that push data.
- `respond`: run the code on the request and send its result back as the response, at once. Nothing is stored or queued, and there's no destination; if something fails, the caller gets an error status and decides whether to try again. For REST APIs and lookups that other systems call and wait on.

Default: `queue`. Example: `mode: respond`.

### `source.file`

**Type:** settings, below

Files dropped in a folder on this server (or a share it mounts): each file, or each record in it with `split: records`, becomes a message. A file is moved or deleted only after everything in it is stored. Example: `source: { file: { dir: in, pattern: "*.hl7" } }`.

### `source.file.server`

**Type:** text

The SFTP server to read from: an `sftp:` entry in resources.yaml, which holds its address and login. Only for `sftp` sources, and required there. Example: `server: lab_server`.

### `source.file.dir`

**Type:** text Â· **Required**

The folder to watch. A local folder is relative to the interface's folder unless written whole (`/data/inbound/lab`, or on Windows `D:\inbound\lab`, in single quotes); an SFTP folder is relative to the folder the login starts in. Only files directly in it are read, not subfolders. Required. Example: `dir: inbound`.

### `source.file.pattern`

**Type:** text

Which file names to pick up, with `*` (any characters) and `?` (one character). Names starting with `.` are never picked up, since writers often use them for files still being written. Default: `*` (every file). Example: `pattern: "ADT_*.hl7"`.

### `source.file.poll`

**Type:** duration

How often the folder is looked at, at least `100ms`. A file is picked up once its size and time are the same at two looks in a row, so a file still being written is never read half-done: a file appears one to two polls after its writer finishes. Lower it for quicker pickup; on SFTP each look is a listing on the server. Default: `10s`. Example: `poll: 30s`.

### `source.file.marker`

**Type:** text

Pick a file up only once a marker file next to it exists: with `marker: .done`, `a.csv` is read once `a.csv.done` appears. For writers that say when they're finished (many mainframe and batch jobs do); the file is then read at the next look, without waiting for it to stop changing. The marker goes with the file. Default: none (wait until the file stops changing). Example: `marker: .done`.

### `source.file.after`

**Type:** `move`, `delete`

What happens to a file once everything in it is stored: `move` it to `archive_dir`, or `delete` it. Moving keeps a copy to look at or drop in again; the engine never deletes what it moved, so clean the archive yourself. Default: `move`. Example: `after: delete`.

### `source.file.archive_dir`

**Type:** text

Where `after: move` puts files, relative to the interface's folder (or the SFTP login folder) unless written whole. Default: `archive` inside `dir`. Example: `archive_dir: /data/archive/lab`.

### `source.file.split`

**Type:** `none`, `records`

- `none`: each file is one message.
- `records`: each record in a file is a message, read as a stream, so a file of any size works: an HL7 batch file is split at each MSH, CSV at each row (the header line kept with every row), fixed-length, text and JSON Lines at each line. If the engine stops halfway, it carries on from the last record stored.

A file as one message is limited to 256 MB; split bigger ones. XML files are always one message each. Default: `none`. Example: `split: records`.

### `source.sftp`

**Type:** settings, below

Files on an SFTP server, named by `server` (an `sftp:` entry in resources.yaml); the same options as `file`. The engine logs in, picks up what's ready, and moves or deletes it on the server. Example: `source: { sftp: { server: lab_server, dir: outbound } }`.

Its settings are those of [`source.file`](#sourcefile).

### `source.schedule`

**Type:** settings, below

No input: the code runs on a timer (`every` or `cron`) and what it returns becomes the message, for jobs that pull (a nightly query, an API polled every few minutes). A run is skipped while the previous one is still waiting. Needs a `script`. Example: `source: { schedule: { cron: "0 2 * * *" } }`.

### `source.schedule.every`

**Type:** duration

Run at start, then at this interval: for polling (an API every `5m`). Give this or `cron`, not both. Example: `every: 15m`.

### `source.schedule.cron`

**Type:** text

Run at the times a five-field cron expression gives (minute, hour, day of month, month, day of week), in the server's local time: for jobs at a time of day. Runs missed while the engine was down are made up by one run at start, not one per missed time. Give this or `every`. Example: `cron: "0 2 * * *"` (02:00 every day).

### `script`

**Type:** text

The TypeScript file with the interface's code, relative to its folder. It exports `transform(msg, meta)`, plus any function a destination names with `transform:`.

`ductus validate` type-checks it and the engine loads it at start. Without a script, messages go to the destinations as they came. A `schedule` source and an HTTP source in `respond` mode need one: there, the code makes the message or the answer. Example: `script: transform.ts`.

### `format`

**Type:** hl7, json, xml, text, csv, or { csv | fixed: options }

What the messages are, so the code gets them parsed and keys can be read: `hl7`, `json`, `xml`, `text`, `csv` or `fixed` (fixed-length records, with their fields' widths).

`hl7` gives the code segments and fields (`msg.get("PID-3.1")`); `json` and `xml` a document; `csv` rows (objects by column name with a header line); `fixed` an object per record; `text` the payload as a string. An MLLP source always carries HL7. What the code returns is written in this format too, unless `output` says otherwise. Default: `hl7`. Example: `format: { csv: { delimiter: "|" } }`.

### `format.csv`

**Type:** settings, below

Delimited text with its options (`delimiter`, `header`). `format: csv` alone means commas and a header line. Example: `format: { csv: { delimiter: "|", header: false } }`.

### `format.csv.delimiter`

**Type:** text

The one character between fields: `,`, `|`, `;` or a tab (`"\t"`). Quote it in YAML. Default: `","`. Example: `delimiter: "|"`.

### `format.csv.header`

**Type:** `true` or `false`

Whether the first line holds the column names. With it, each record is an object by column name (`row.mrn`) and keys name columns; without it, each record is a list of fields and keys are column numbers from 1. Default: `true`. Example: `header: false`.

### `format.fixed`

**Type:** settings, below

Fixed-length records: each field at a set column and width, listed in `fields`. Each line of a file is a record. Example: `format: { fixed: { fields: [{ name: mrn, width: 10 }, { name: name, width: 30 }] } }`.

### `format.fixed.fields`

**Type:** list of settings, below Â· **Required**

The record's fields, in order, each with its `name` and `width`: what the code sees as the record's properties, and what's written back in the same columns. At least one. Example: `fields: [{ name: mrn, width: 10 }, { name: last_name, width: 20 }]`.

### `format.fixed.fields.name`

**Type:** text Â· **Required**

The field's name, as the code reads it (`record.mrn`) and as keys name it. Required. Example: `name: mrn`.

### `format.fixed.fields.width`

**Type:** number Â· **Required**

How many characters the field takes, at least 1. Required. Example: `width: 10`.

### `format.fixed.fields.start`

**Type:** number

The column the field starts at, counting from 1, for layouts with gaps or fields out of order. Default: right after the previous field (1 for the first). Example: `start: 41`.

### `format.fixed.fields.align`

**Type:** `left`, `right`

When writing: `left` pads after the value (names), `right` before it (amounts, numbers). Default: `left`. Example: `align: right`.

### `format.fixed.fields.pad`

**Type:** text

When writing: the character that fills the field's unused width. Default: a space. Example: `pad: "0"` (amounts as `0000012.50`).

### `format.fixed.trim`

**Type:** `true` or `false`

Remove the padding around each field's value when reading (`"SMITH     "` reads as `"SMITH"`). Turn it off where spaces are part of the value. Default: `true`. Example: `trim: false`.

### `output`

**Type:** hl7, json, xml, text, csv, or { csv | fixed: options }

The format the code's results are written in, when it isn't `format`: an HL7 feed turned into JSON for an API, a CSV file turned into HL7. Same values as `format`. A destination can set its own `output`. Default: the same as `format`. Example: `output: json`.

Its settings are those of [`format`](#format).

### `ordering`

**Type:** settings, below

Which messages must be delivered in the order they arrived: those with the same `key` (one patient's), in lanes spread over the workers. Without it, the whole interface is one lane, delivered strictly in arrival order. Default: no key (one lane). Example: `ordering: { key: PID-3.1 }`.

### `ordering.key`

**Type:** text

The field whose value decides which messages stay in order with each other, as a key path in the interface's `format`: usually the patient (`PID-3.1`, the MRN), or an order or account. Messages with the same value are delivered in arrival order; different values can overtake each other, so one patient's failing message doesn't hold up everyone.

Choose the narrowest value correctness allows: by patient for ADT, by order for results. A message whose field is empty goes to the lane of the empty value. Without a key, the whole interface is delivered in one strict order, one message at a time per destination. Default: none. Example: `key: PID-3.1`.

### `ordering.partitions`

**Type:** number

How many lanes messages are spread over by their `key`. Messages with the same key value always go to the same lane, and each lane is delivered strictly in arrival order, so more lanes let more patients' messages move at once while each patient's stay in order. Without a `key` this is ignored: the whole interface is one lane.

The lanes are divided among the workers (one per destination in the shared pool; `workers` with `isolation: dedicated`), and a worker with no lane has nothing to do. The default suits up to 64 workers; to give N workers 64 lanes each, use `64 * N`.

Change it only when nothing is waiting (`ductus drain --wait` first). Lowered with messages waiting, the engine refuses to start: they'd be in lanes no worker reads. Raised, it starts, but a patient's waiting messages and their new ones can go out of order until the backlog drains. Default: `64`. Example: `ordering: { key: PID-3.1, partitions: 128 }`.

### `isolation`

**Type:** `shared`, `dedicated`

Where the interface's code runs.

- `shared`: on the engine's shared pool, one thread per CPU core, each running many interfaces. Cheap: most interfaces belong here.
- `dedicated`: on threads of its own (`workers` of them, each with its own copy of the JavaScript engine), so its load and its runaways touch no other interface, and it can have database connections of its own (`dedicated_pools`).

Use `dedicated` for a busy interface that needs more than one worker (with an ordering `key`, so there are lanes to share out), or for heavy or untrusted code. Each dedicated worker costs a thread and up to `isolates.heap_mb` of memory. Default: `shared`. Example: `isolation: dedicated`.

### `workers`

**Type:** number

How many workers a dedicated interface has, each a thread with its own JavaScript engine, taking a share of the lanes (`ordering.partitions`). Only with `isolation: dedicated`.

More workers help only with an ordering `key` (without one there's a single lane, and one worker does everything). Up to about the number of cores helps; past that they wait for each other. Default: `4`. Example: `workers: 8`.

### `dedicated_pools`

**Type:** names, each with number

Database connections of the interface's own, by resources.yaml database name: `{ patient_db: 4 }` gives it 4 connections to `patient_db` that no other interface uses. Only with `isolation: dedicated`.

Its `db("patient_db")` calls use these first, so a busy interface can't take every connection of the shared pool (`max_connections`) from the others, nor theirs from it. Add them when a dedicated interface's lookups queue for connections, or when it must keep working while other interfaces are busy. Default: none. Example: `dedicated_pools: { patient_db: 4 }`.

### `durability`

**Type:** `strict`, `relaxed`

When a sender gets its acknowledgement (ACK, or HTTP 202): after the message is safely on the database's disk, or as soon as the database has it.

- `strict`: the ACK waits until the database has flushed the message to disk. Once a sender has its ACK, the message survives a crash or power loss of any machine.
- `relaxed`: the ACK goes out without waiting for the flush. One sender connection can go much faster: with `strict` it can't send faster than one message per disk flush (about 400 a second at 2.4 ms a flush; `ductus doctor` measures yours). The cost: if the *database server* crashes or loses power, the last fraction of a second of acknowledged messages is lost, and the sender won't resend them because it has its ACK. An engine crash alone loses nothing in either mode.

Use `relaxed` only where one connection's peak is more than the disk allows and a lost message can be recovered another way (a resend from the source system, a nightly reconciliation). Before that, try a faster disk or more sender connections, which share flushes. Default: `strict`. Example: `durability: relaxed`.

### `destination`

**Type:** a connector (mllp, file, sftp, http, interface) and options

Where messages go, when they go to one place: one connector (`mllp`, `file`, `sftp`, `http` or `interface`) and its options. Its name is `default` (in the web UI, alerts and `ductus pause adt-in default`).

Use `destinations` instead to send to several places. An HTTP source in `respond` mode has none: its answer is the result. Required otherwise (this or `destinations`). Example: `destination: { mllp: { host: lab.hospital.org, port: 7001 } }`.

### `destination.transform`

**Type:** text

The function in the interface's `script` that prepares messages for this destination: its exported name. Each destination runs its function on the message as it was received, not on another destination's result, so `toLab` and `toBilling` can each shape it for their receiver. Default: `transform`. Example: `transform: toBilling`.

### `destination.output`

**Type:** hl7, json, xml, text, csv, or { csv | fixed: options }

### `destination.output.csv`

**Type:** settings, below

Delimited text with its options (`delimiter`, `header`). `format: csv` alone means commas and a header line. Example: `format: { csv: { delimiter: "|", header: false } }`.

Its settings are those of [`format.csv`](#formatcsv).

### `destination.output.fixed`

**Type:** settings, below

Fixed-length records: each field at a set column and width, listed in `fields`. Each line of a file is a record. Example: `format: { fixed: { fields: [{ name: mrn, width: 10 }, { name: name, width: 30 }] } }`.

Its settings are those of [`format.fixed`](#formatfixed).

### `destination.retry`

**Type:** settings, below

### `destination.retry.max_attempts`

**Type:** number

Give up after this many failed delivery attempts and quarantine the message ("gave up after N attempts: …"). While a message is being retried it holds up its lane, so set this where a person looking at it beats waiting for the receiver (a receiver that refuses some messages for good with errors that look temporary). Leave it unset where every message must get there once the receiver is back. The code's own temporary errors (a database down) are always retried. Default: no limit. Example: `max_attempts: 10`.

### `destination.retry.initial_backoff_ms`

**Type:** number

The first wait after a failure, in milliseconds; each later one doubles, up to `max_backoff_ms`. Default: `1000`. Example: `initial_backoff_ms: 5000`.

### `destination.retry.max_backoff_ms`

**Type:** number

The longest wait between tries, in milliseconds: how soon after a receiver comes back the waiting messages start moving, at worst. Default: `60000`. Example: `max_backoff_ms: 300000`.

### `destination.rate_limit`

**Type:** settings, below

### `destination.rate_limit.per_minute`

**Type:** number Â· **Required**

How many messages a minute the destination may be sent, at least 1: what the receiver (or its vendor's quota) allows. Required. Example: `per_minute: 600` (10 a second).

### `destination.rate_limit.mode`

**Type:** `average`, `hard`

- `average`: messages go at `per_minute` on average, and up to `burst` may go at once after a quiet spell (a token bucket).
- `hard`: never more than `per_minute` in any one calendar minute, for quotas counted that way.

Default: `average`. Example: `mode: hard`.

### `destination.rate_limit.burst`

**Type:** number

With `mode: average`: how many may go at once after a quiet spell. Lower it for a receiver that can't take a burst at all. Default: `per_minute`. Example: `burst: 20`.

### `destination.rate_limit.over_limit`

**Type:** `wait`, `drop`

What happens to messages over the limit: `wait` (they're held, in order, until they're allowed) or `drop` (they're filtered, with the reason, and never sent). Drop only what is worthless late, such as frequent status updates where the next one replaces the last. Default: `wait`. Example: `over_limit: drop`.

### `destination.save_output`

**Type:** `true` or `false`

### `destination.application`

**Type:** text

The receiving application, when this destination's differs from `about.to` (an interface that sends to several systems). `ductus impact` and the web UI use it. Default: `about.to.application`. Example: `application: Sunquest Lab`.

### `destination.vendor`

**Type:** text

The receiving application's vendor, when this destination's differs from `about.to`. Default: `about.to.vendor`. Example: `vendor: Sunquest`.

### `destination.owner`

**Type:** text

Who to ask about this destination, when it isn't the interface's `about.owner` (its code or its receiver is another team's): best written as an email address. Alert rules that send to `owner` reach them for this destination's alerts. Default: `about.owner`. Example: `owner: billing-it@hospital.org`.

### `destination.monitor`

**Type:** settings, below

A destination's own expectations (`monitor:` on the destination).

### `destination.monitor.behind`

**Type:** messages, age, or { messages, age }

When this destination's queue counts as behind, in place of the interface's `monitor.behind`: `500`, `10m`, or `{ messages: 500, age: 10m }`. Default: the interface's. Example: `behind: 2h`.

### `destination.monitor.behind.messages`

**Type:** number

Behind once this many messages wait for the destination, at least 1. Default: no count limit. Example: `messages: 500`.

### `destination.monitor.behind.age`

**Type:** text

Behind once the oldest waiting message has waited this long, at least `1s`. Default: no age limit. Example: `age: 10m`.

### `destination.custom`

**Type:** names and values of your own

Your company's documentation fields for this destination (those whose `applies_to` in `docs-schema.yaml` includes `destination`). Default: none. Example: `custom: { analyst: ana@hospital.org }`.

### `destination.mllp`

**Type:** settings, below

Send each message over MLLP to a receiver (a lab system, another interface engine) and wait for its ACK: AA is delivered, AE is tried again, AR follows `on_reject`. One connection, kept open between messages. Example: `mllp: { host: lab.hospital.org, port: 7001 }`.

### `destination.mllp.host`

**Type:** text Â· **Required**

The receiver's host name or address. With `tls`, its certificate must be for this name. Required. Example: `host: lab.hospital.org`.

### `destination.mllp.port`

**Type:** number Â· **Required**

The receiver's port. Required. Example: `port: 7001`.

### `destination.mllp.timeout_ms`

**Type:** number

How long one send may take, in milliseconds: connecting if needed, sending, and waiting for the ACK. Past it the connection is dropped and the message tried again later (it may have arrived: receivers that dedupe on MSH-10 take care of the repeat). Raise it for a receiver that is slow to answer; on Windows keep it above 2000. Default: `10000`. Example: `timeout_ms: 30000`.

### `destination.mllp.ack`

**Type:** `wait`, `none`

- `wait`: wait for the receiver's ACK before the next message; the message counts as delivered only on AA.
- `none`: send and carry on without reading an answer, for the rare receiver that never sends ACKs. Nothing then tells the engine a message was refused or lost.

Default: `wait`. Example: `ack: none`.

### `destination.mllp.on_reject`

**Type:** `quarantine`, `retry`

What an AR (the receiver rejects the message) does: `quarantine` it for a person to look at, or `retry` it like an AE, for receivers that answer AR when they're busy or half-down. A message that keeps being refused holds up its lane while it's retried. AE is always tried again. Default: `quarantine`. Example: `on_reject: retry`.

### `destination.mllp.tls`

**Type:** true, or { ca, certificate }

Send over TLS. `true` trusts the certificates this server trusts (the system's store); `{ ca, certificate }` also trusts the CA named by `ca`, and presents the client certificate named by `certificate`, both `certificates:` entries in resources.yaml. A receiver whose certificate isn't trusted or isn't for `host` is refused and tried again later, with the reason in the log and on the destination. Default: plain MLLP. Example: `tls: { ca: hie-ca, certificate: ductus-client }`.

### `destination.mllp.tls.ca`

**Type:** text

A CA to trust for this receiver besides the ones this server trusts: a `certificates:` entry in resources.yaml holding the CA's certificate, for a receiver whose certificate comes from a private CA. Default: the system's trusted CAs only. Example: `ca: hie-ca`.

### `destination.mllp.tls.certificate`

**Type:** text

The client certificate to present, for receivers that ask for one (mutual TLS): a `certificates:` entry with its key. Renewing its files takes effect without a restart. Default: none. Example: `certificate: ductus-client`.

### `destination.file`

**Type:** settings, below

Write each message as a file in a folder of this server (or a share it mounts), named by the message's id and the output format (`1234.hl7`). Files are written under a hidden temporary name and renamed when complete, so a reader never sees half a file. Example: `file: { dir: outbound }`.

### `destination.file.dir`

**Type:** text Â· **Required**

The folder to write to, made if missing. Relative to the interface's folder unless written whole. Required. Example: `dir: /data/outbound/lab`.

### `destination.sftp`

**Type:** settings, below

Write each message as a file on an SFTP server, named like the `file` destination's. Files are written under a temporary name and renamed when complete, so a reader on the server never sees half a file. Example: `sftp: { server: lab_server, dir: inbound }`.

### `destination.sftp.server`

**Type:** text Â· **Required**

The SFTP server: an `sftp:` entry in resources.yaml, which holds its address and login. Required. Example: `server: lab_server`.

### `destination.sftp.dir`

**Type:** text Â· **Required**

The folder on the server to write to, relative to the folder the login starts in unless written whole. Required. Example: `dir: inbound/adt`.

### `destination.http`

**Type:** settings, below

Send each message as an HTTP request (POST unless `method` says otherwise) to a `url`, or to a `path` under an API from resources.yaml (`api: billing`, which brings its URL, headers, timeout and TLS). A 2xx answer is delivered; 408, 429, 5xx and network errors are tried again; any other 4xx is quarantined (the receiver refused this message). Example: `http: { api: billing, path: /charges }`.

### `destination.http.url`

**Type:** text

The address to send to, `https://` or `http://`. Give this or `api`, not both. A token in it, or in `headers`, belongs in a secret: `${secret:NAME}`. Example: `url: "https://lab.example.org/api/orders"`.

### `destination.http.api`

**Type:** text

An `apis:` entry in resources.yaml to send through: it brings the base URL, headers (an `Authorization` token) and timeout, so the same API is set up once for every interface and each environment. Give this or `url`. Example: `api: billing`.

### `destination.http.path`

**Type:** text

With `api`: the path under its URL (`/charges` under `https://billing/api/v2`). Default: the API's URL itself. Example: `path: /charges`.

### `destination.http.method`

**Type:** text

The HTTP method: `POST`, `PUT`, `PATCH`, or any other the receiver expects. Default: `POST`. Example: `method: PUT`.

### `destination.http.headers`

**Type:** names, each with text

Headers sent with every request, added to the API's (and replacing an API header of the same name). Put tokens in secrets. Default: none (the API's, with `api`). Example: `headers: { X-Facility: north }`.

### `destination.http.timeout_ms`

**Type:** number

How long a request may take, in milliseconds, before it's given up and tried again later. Default: the API's `timeout`, else `10000`. Example: `timeout_ms: 30000`.

### `destination.http.ca`

**Type:** text

With an `https://` `url`: a CA to trust for this server besides the system's (a `certificates:` entry), when the receiver's certificate comes from a private CA. With `api`, the API's own `ca` applies instead. Example: `ca: lab-ca`.

### `destination.http.certificate`

**Type:** text

With an `https://` `url`: the client certificate to present (a `certificates:` entry with its key), for receivers that ask for one (mutual TLS). With `api`, the API's own applies instead. Example: `certificate: ductus-client`.

### `destination.interface`

**Type:** text

Put each message into another interface's queue: name an interface that has `source: internal`. It's stored there in the same transaction that marks it sent here: no network hop, nothing lost or sent twice between the two. The other interface's `format` must be what this destination outputs, and routes can't form a loop (`ductus validate` checks both). Example: `interface: adt-to-billing`.

### `destinations`

**Type:** names, each with a connector (mllp, file, sftp, http, interface) and options

Several places messages go, by a name you choose (lower-case letters, digits, `-` and `_`). Each gets every message and is delivered independently: its own queue, order, retries and pause, so one that's down holds up only itself. Each runs its own function on the original message (`transform:`, default `transform`).

A message is finished once every destination is done with it. Adding a destination later gives it only the messages still in flight, not older ones; removing one filters what was waiting for it. Use instead of `destination`, not with it. Example: `destinations: { lab: { mllp: { host: lab.hospital.org, port: 7001 } }, archive: { file: { dir: archive } } }`.

Its settings are those of [`destination`](#destination).

### `retry`

**Type:** settings, below

How a delivery that fails for a reason that may pass (a receiver down, a timeout, a database unreachable from the code) is tried again: waits doubling from `initial_backoff_ms` to `max_backoff_ms`, and given up only after `max_attempts`, if set. A destination can set its own. Default: retry forever, 1 s doubling to 60 s.

### `retry.max_attempts`

**Type:** number

Give up after this many failed delivery attempts and quarantine the message ("gave up after N attempts: …"). While a message is being retried it holds up its lane, so set this where a person looking at it beats waiting for the receiver (a receiver that refuses some messages for good with errors that look temporary). Leave it unset where every message must get there once the receiver is back. The code's own temporary errors (a database down) are always retried. Default: no limit. Example: `max_attempts: 10`.

### `retry.initial_backoff_ms`

**Type:** number

The first wait after a failure, in milliseconds; each later one doubles, up to `max_backoff_ms`. Default: `1000`. Example: `initial_backoff_ms: 5000`.

### `retry.max_backoff_ms`

**Type:** number

The longest wait between tries, in milliseconds: how soon after a receiver comes back the waiting messages start moving, at worst. Default: `60000`. Example: `max_backoff_ms: 300000`.

### `dedupe`

**Type:** settings, below

Recognize a message sent again within a window (a sender resending after a lost ACK, a file dropped twice) by the values of `keys`, and don't deliver it twice.

A repeat is stored with the status `duplicate` (so it can be found) but never queued. Use it when the receiver can't tolerate repeats and the sender is known to resend; most HL7 receivers already ignore a repeated control ID (MSH-10). Default: off. Example: `dedupe: { keys: [MSH-10], window: 24h }`.

### `dedupe.keys`

**Type:** list of text

The fields whose values, together, make a message the same as one seen before, as key paths in the interface's `format`: HL7 `MSH-10`, JSON `order.id`, XML `Order/@id`, a CSV column or a fixed-length field's name. Choose fields a resend repeats and a new message never does: the sender's control ID, or an order number with its status. Default: `[MSH-10]` (HL7's message control ID). Example: `keys: [ORC-2, ORC-5]`.

### `dedupe.window`

**Type:** duration

How long a message's keys are remembered: a repeat within this long is a duplicate, after it a new message. Cover the longest a sender may take to resend (after a restart, a weekend). Default: `24h`. Example: `window: 7d`.

### `rate_limit`

**Type:** settings, below

The most messages a minute each destination is sent, for receivers that can't take bursts (a device, a vendor API with a quota). Messages over it wait, in order, or are dropped (`over_limit: drop`). A destination can set its own; either way each destination has its own allowance. Default: no limit. Example: `rate_limit: { per_minute: 600 }`.

### `rate_limit.per_minute`

**Type:** number Â· **Required**

How many messages a minute the destination may be sent, at least 1: what the receiver (or its vendor's quota) allows. Required. Example: `per_minute: 600` (10 a second).

### `rate_limit.mode`

**Type:** `average`, `hard`

- `average`: messages go at `per_minute` on average, and up to `burst` may go at once after a quiet spell (a token bucket).
- `hard`: never more than `per_minute` in any one calendar minute, for quotas counted that way.

Default: `average`. Example: `mode: hard`.

### `rate_limit.burst`

**Type:** number

With `mode: average`: how many may go at once after a quiet spell. Lower it for a receiver that can't take a burst at all. Default: `per_minute`. Example: `burst: 20`.

### `rate_limit.over_limit`

**Type:** `wait`, `drop`

What happens to messages over the limit: `wait` (they're held, in order, until they're allowed) or `drop` (they're filtered, with the reason, and never sent). Drop only what is worthless late, such as frequent status updates where the next one replaces the last. Default: `wait`. Example: `over_limit: drop`.

### `timeout`

**Type:** duration

The longest the interface's code may run for one message without pausing, from `10ms` to `10m`. Time spent waiting on `db()`, `fetch()` or `api()` doesn't count, only running code.

It's the guard against code that never ends, such as a loop over segments that an unusual message never lets finish. Past it, the code is stopped, its message is quarantined ("transform stopped: …") and the interface carries on; three in a row, or one that runs out of memory, stop the interface until the engine restarts. Interfaces in the shared pool wait while code on their thread runs, so a generous limit lets one runaway hold them all up that long.

Set it close to what the code needs: `500ms` is plenty for most HL7 transforms. Raise it only for code that computes for seconds on purpose, and give that interface `isolation: dedicated` so no one else waits for it. Default: `5s`. Example: `timeout: 500ms`.

### `about`

**Type:** settings, below

What the interface is for, who owns it, and the systems on each side. `ductus impact` reads it to answer "which interfaces does this vendor's outage, breach or VPN change touch?", and the web UI and alerts show it. Nothing here changes how messages flow. Default: empty (`require_about: true` in ductus.yaml makes `ductus validate` insist). Example: `about: { description: ADT from Epic to the lab, owner: lab-it@hospital.org }`.

### `about.description`

**Type:** text

What the interface does, in a sentence someone on call understands: shown in the web UI, `ductus impact` and alerts. Example: `description: ADT from Epic to the Sunquest lab`.

### `about.owner`

**Type:** text

Who to ask about the interface: a person or a team, best written as an email address (`lab-it@hospital.org`), which matches their account and survives a change of name. An owner that names an account shows as that person in the web UI (and their "Mine" list); alert rules with `to: owner` mail them. `ductus owners` lists who owns what. Default: none. Example: `owner: lab-it@hospital.org`.

### `about.from`

**Type:** settings, below

The system that sends (for an HTTP source in `respond` mode: the one that calls). Example: `from: { application: Epic, vendor: Epic Systems, hosts: [10.20.30.0/24] }`.

### `about.from.application`

**Type:** text

The sending application, as people name it. `ductus impact Epic` finds every interface that names it here or as a receiver. Example: `application: Epic`.

### `about.from.vendor`

**Type:** text

The sending application's vendor: `ductus impact` by vendor answers "what does this vendor's breach or VPN change touch?". Example: `vendor: Epic Systems`.

### `about.from.hosts`

**Type:** list of text

Where the sender's connections come from: addresses, subnets (`10.20.30.0/24`) or host names. The engine can't learn these from the configuration, so a network change (a new subnet, a firewall move) can only be traced to interfaces that say. `ductus validate` warns when an `allow:` list doesn't cover them. Default: none. Example: `hosts: [10.20.30.0/24, epic-bridge01.hospital.org]`.

### `about.to`

**Type:** settings, below

The system messages go to. A destination's own `application` and `vendor` take precedence for that destination. Example: `to: { application: Sunquest Lab, vendor: Sunquest }`.

### `about.to.application`

**Type:** text

The application, as people name it. `ductus impact` finds interfaces by it. Example: `application: Sunquest Lab`.

### `about.to.vendor`

**Type:** text

The application's vendor. `ductus impact` finds interfaces by it. Example: `vendor: Sunquest`.

### `retention`

**Type:** 1w, 3w or 7w

How long this interface's messages are kept: `1w`, `3w` or `7w` (or `7d`, `21d`, `49d`), when it should differ from `retention` in ductus.yaml.

A message is kept at least its window and is gone within a day (`1w`) or a week after. Waiting and quarantined messages are never deleted: past the window they move to the held area. A shorter value deletes messages, so it waits for `ductus retention confirm <interface>`; a longer one applies at once. Use `1w` for busy feeds nobody looks back on, `7w` where problems surface weeks later. Not for HTTP `respond` mode (it keeps nothing). Default: `retention` in ductus.yaml (`7w`). Example: `retention: 1w`.

### `quarantined`

**Type:** `keep`, `purge`

What happens to quarantined messages once the retention window has passed: `keep` them in the held area until someone resends, corrects or dismisses them, or `purge` them with the rest. A quarantined message is a question nobody has answered yet, so `keep` is the safe choice; `purge` suits a feed whose sender resends anyway. Not for HTTP `respond` mode. Default: `quarantined` in ductus.yaml (`keep`). Example: `quarantined: purge`.

### `monitor`

**Type:** settings, below

What operations expects of this interface, in the terms its teams use: something received within a time (`active_within`) or by a deadline (`active_by`), and queues not too far behind (`behind`). Breaking one shows as a health state (silent, missed, behind) in the web UI, `ductus status` and Prometheus, and alerts when a rule names it.

Nothing is expected by default: the engine can't know that a quiet feed is a problem, so these are operations' decisions. Example: `monitor: { active_within: 30m, between: "06:00-22:00", behind: 10m }`.

### `monitor.active_within`

**Type:** duration

Something must be received at least this often (at least `1m`): longer than this with nothing, and the interface is *silent*. For feeds that never stop in normal operation, such as ADT from the EHR: set it a little above the longest normal gap, so a dead sender is noticed before the users call. Only receiving counts, not delivering. Default: none. Example: `active_within: 30m`.

### `monitor.between`

**Type:** hours (06:00-22:00)

The hours `active_within` applies, in the server's local time: `"06:00-22:00"`, or across midnight, `"22:00-06:00"`. The quiet time is counted from when the hours start, so a feed that sleeps at night isn't silent at 06:01. Only with `active_within`. Default: all day. Example: `between: "06:00-22:00"`.

### `monitor.active_by`

**Type:** time (09:00, Mon-Fri 09:00, or cron)

Something must be received by this time, or the interface has *missed* its deadline: for feeds that come once a day or week (a nightly file, a Monday batch). `"09:00"` every day, `"Mon 09:00"`, `"Mon-Fri 09:00"`, `"Mon,Thu 09:00"`, or a five-field cron expression, in the server's local time. Only receiving counts. Default: none. Example: `active_by: "Mon-Fri 07:00"`.

### `monitor.from`

**Type:** time (09:00, Mon-Fri 09:00, or cron)

With `active_by`: when the window for each deadline opens. Only what arrives after it counts, so yesterday's late file can't hide today's missing one. `"18:00"` opens it the evening before; `"Fri 18:00"` the Friday before. Default: midnight of the deadline's day. Example: `from: "18:00"`.

### `monitor.behind`

**Type:** messages, age, or { messages, age }

When a destination's queue counts as *behind*: `500` (that many messages waiting), `10m` (the oldest has waited that long), or `{ messages: 500, age: 10m }` (either one).

A count alone misses an overnight stall when little arrives; an age alone misses a daytime pile-up that still moves, so give both for busy feeds. Set it at what the receiving department would call late. A destination can set its own. Default: none (a destination that stops sending still shows as *stalled* after a minute). Example: `behind: { messages: 1000, age: 15m }`.

### `monitor.behind.messages`

**Type:** number

Behind once this many messages wait for the destination, at least 1. Default: no count limit. Example: `messages: 500`.

### `monitor.behind.age`

**Type:** text

Behind once the oldest waiting message has waited this long, at least `1s`. Default: no age limit. Example: `age: 10m`.

### `alert`

**Type:** settings, below

This interface's alerts: more recipients on top of the rules' (`to`), or none at all (`mute`). The rules in ductus.yaml decide which states alert. Default: the rules' recipients only. Example: `alert: { to: [lab-team] }`.

### `alert.to`

**Type:** list of text

More recipients for this interface's alerts, on top of the matching rule's: channel names from `alerts.channels` in ductus.yaml, or `owner` (whoever owns the interface or the destination). For a team that wants to hear about its own feeds only. Default: none. Example: `to: [lab-team]`.

### `alert.mute`

**Type:** `true` or `false`

Send no notifications about this interface: for a test or half-built interface. Its alerts still open and show in the web UI, so nothing is hidden from people who look. Default: `false`. Example: `mute: true`.

### `custom`

**Type:** names and values of your own

Your company's own documentation fields (a BAA reference, a ticket, a runbook link), declared in `docs-schema.yaml` and checked by `ductus validate`; the web UI shows them on the interface's page. The engine itself ignores them. Default: none. Example: `custom: { baa_signed: "yes", runbook: "https://wiki/adt" }`.

