# interface.yaml: an interface

An interface is a folder under `interfaces/`: its `interface.yaml`, its code, its tests and
sample messages. The YAML says where messages come from, what they are, where they go, and how
they're delivered, kept and watched; the code says what happens to each one on the way.

Only three things are required: a [`name`](../reference/config/interface.md#name), a
[`source`](../reference/config/interface.md#source), and a
[`destination`](../reference/config/interface.md#destination) (or several,
[`destinations`](../reference/config/interface.md#destinations)). Everything else has a default
that's safe to start with:

```yaml
# interfaces/adt-relay/interface.yaml
name: adt-relay
source: { mllp: { port: 6662 } }
destination: { mllp: { host: ${LAB_HOST}, port: ${LAB_PORT} } }
```

That relays HL7 from whoever connects on port 6662 to the lab, as received: no code, every
message stored before its ACK, delivered in arrival order, retried for as long as the lab is
down or answers AE (an AR, a refusal for good, is quarantined for a person), kept seven weeks.

A fuller one, with each part explained on the pages below:

```yaml
# interfaces/adt-to-lab/interface.yaml
name: adt-to-lab
about:
  description: ADT from Epic to the Sunquest lab
  owner: lab-it@hospital.org
  from: { application: Epic, vendor: Epic Systems, hosts: [10.20.30.0/24] }
  to: { application: Sunquest Lab, vendor: Sunquest }
source:
  mllp:
    port: ${EPIC_PORT}
    allow: [10.20.30.0/24]
script: transform.ts
timeout: 500ms
ordering: { key: PID-3.1 }
destinations:
  lab:
    mllp: { host: ${LAB_HOST}, port: ${LAB_PORT}, tls: true }
  billing:
    http: { api: billing, path: /charges }
    transform: toBilling
    output: json
retention: 3w
monitor:
  active_within: 30m
  between: "06:00-22:00"
  behind: { messages: 500, age: 10m }
alert: { to: [lab-team] }
```

| Part | Settings | Page |
|---|---|---|
| Where messages come from, what they are, who may send | `source`, `format`, `output`, `allow` | [Sources and formats](sources.md) |
| Where they go | `destination`, `destinations` | [Destinations](destinations.md) |
| How they're delivered | `ordering`, `retry`, `dedupe`, `rate_limit`, `durability`, `isolation`, `workers`, `dedicated_pools` | [Delivery](delivery.md) |
| How long they're kept, what's expected, who hears | `retention`, `quarantined`, `monitor`, `alert` | [Keeping and watching](keeping-and-watching.md) |
| What it is and who owns it | `about`, `custom` | [About and owners](about.md) |

## The name

[`name`](../reference/config/interface.md#name) is how the database, the web UI, logs, alerts and
routes from other interfaces know it. Use the folder's name (as `ductus new interface` does),
short and stable: lower-case words joined by `-`, saying what flows and between whom
(`adt-epic-to-lab`). **Don't rename an interface with messages waiting**: messages are stored
under the name, so a renamed interface is a new one; the old name's history stays under the old
name, and anything still waiting there is sent by nobody.

## The code

[`script`](../reference/config/interface.md#script) is the TypeScript file, relative to the
interface's folder, that exports `transform(msg, meta)` (and any function a destination names
with `transform:`). `ductus validate` type-checks it, and the engine loads it at start.

- **Without a script**, messages go to their destinations as they came. That's right for a
  relay; most interfaces have one.
- **A `schedule` source, and an HTTP source in `respond` mode, need one**: there, the code makes
  the message or the answer.
- What the code returns decides: a message to send, `null` to filter it (kept, with the reason),
  or a thrown error to quarantine it for a person
  ([nuance](../nuances.md#return-null-filters-throwing-quarantines-level-1)).

[`timeout`](../reference/config/interface.md#timeout) is the longest the code may run for one
message without pausing (time waiting on `db()`, `fetch()` or `api()` doesn't count), from `10ms`
to `10m`; default `5s`. It's the guard against code that never ends, such as a loop an unusual
message never lets finish: past it, the code is stopped, its message quarantined, and the
interface carries on; three in a row stop the interface until the engine restarts
([nuance](../nuances.md#a-runaway-message-is-quarantined-three-in-a-row-stop-the-interface-level-2)).
**Set it close to what the code needs**: `500ms` is plenty for most HL7 transforms. Interfaces in
the shared pool wait while code on their thread runs, so a generous limit lets one runaway hold
them all up that long; code that computes for seconds on purpose belongs in
`isolation: dedicated` ([delivery](delivery.md#dedicated-threads-and-connections)).

## Your own fields

[`custom`](../reference/config/interface.md#custom) holds your company's documentation fields for
the interface (a BAA reference, a change ticket, a runbook link), declared in `docs-schema.yaml`:
[Documentation fields](documentation-fields.md). The engine ignores them; `ductus validate`
checks them, and the web UI shows them.
