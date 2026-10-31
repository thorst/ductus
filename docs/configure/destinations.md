# Destinations

Where an interface's messages go: one place with
[`destination`](../reference/config/interface.md#destination), or several, each by a name you
choose, with [`destinations`](../reference/config/interface.md#destinations).

| Connector | Sends each message | Delivered when |
|---|---|---|
| [`mllp`](../reference/config/interface.md#destinationmllp) | Over MLLP to a receiver, one connection kept open | The receiver answers AA |
| [`http`](../reference/config/interface.md#destinationhttp) | As an HTTP request to a URL, or a path under an API from resources.yaml | A 2xx answer |
| [`file`](../reference/config/interface.md#destinationfile) | As a file in a folder of this server or a share | The file is complete under its name |
| [`sftp`](../reference/config/interface.md#destinationsftp) | As a file on an SFTP server | The same, on the server |
| [`interface`](../reference/config/interface.md#destinationinterface) | Into another interface's queue | It's stored there, in the same transaction |

## One or several

```yaml
# interfaces/adt-to-lab/interface.yaml, in part
destinations:
  lab:
    mllp: { host: ${LAB_HOST}, port: ${LAB_PORT} }
  billing:
    http: { api: billing, path: /charges }
    transform: toBilling
    output: json
  archive:
    file: { dir: ${OUTBOUND_DIR} }
```

- **Each destination gets every message and is delivered independently**: its own queue, order,
  retries and pause. One that's down holds up only itself; the others carry on
  ([nuance](../nuances/destinations.md#one-destination-down-holds-up-only-itself-level-2)).
- **Each runs its own function on the original message**, the one named by `transform:` (default
  `transform`), not on another destination's result: `toLab` and `toBilling` each shape it for
  their receiver ([nuance](../nuances/destinations.md#each-destination-runs-its-own-function-on-the-original-message-level-2)).
  `output:` sets the format that destination's result is written in.
- **A message is finished** once every destination is done with it: sent, filtered or
  quarantined. Filtering and quarantine are per destination too
  ([nuance](../nuances/destinations.md#filtering-and-quarantine-are-per-destination-level-2)).
- `destination:` (one) is the same as a single destination named `default`. Use one or the
  other, not both.
- **Adding a destination later** gives it only messages that arrive from then on (and those
  still in flight), not older ones; removing one filters what was waiting for it
  ([nuance](../nuances/destinations.md#adding-removing-or-renaming-a-destination-level-3)).
- **Many destinations on one interface**: past a handful, consider splitting
  ([when](../nuances/destinations.md#many-destinations-on-one-interface-when-to-split-level-3)).

Each destination can also set its own `retry`, `rate_limit` and `monitor` (in place of the
interface's: [delivery](delivery.md)), its own `application`, `vendor` and `owner` when they
differ from the interface's `about.to` ([about](about.md)), and `custom:` fields.

**Keeping what was sent.** By default only the original message is stored, and what a
destination was sent is reproduced by running its code again when you look
([nuance](../nuances/delivery.md#only-the-original-is-stored-outputs-are-recomputed-level-3)). Where you
must be able to prove exactly what was sent, `save_output: true` on that destination keeps it
with its delivery (about 1.15 GB per million deliveries, measured).

Each destination runs its own function, but code that runs away after an `await` can be blamed
only on its interface: every call of that interface in flight on the thread is quarantined,
whichever destination it was for ([nuance](../nuances/destinations.md#a-runaway-is-blamed-on-the-interface-not-the-destination-level-5)).

## MLLP

```yaml
# interfaces/orders-to-lab/interface.yaml, in part
destination:
  mllp:
    host: ${LAB_HOST}
    port: ${LAB_PORT}
    timeout_ms: 10000       # the default: connect, send and wait for the ACK
    on_reject: quarantine   # the default: an AR is for a person to look at
    tls: true               # TLS, trusting this server's CAs
```

The engine sends a message and waits for the ACK before the next one, on one connection it keeps
open. **AA** is delivered; **AE** is tried again; **AR** follows `on_reject`: `quarantine` (the
default) or `retry`, for receivers that answer AR when they're busy. A send that takes longer
than `timeout_ms` is dropped and tried again; the receiver may have it already, so receivers that
dedupe on MSH-10 take care of the repeat ([twice](../nuances/delivery.md#a-message-can-be-delivered-twice-level-3)).
On Windows keep `timeout_ms` above 2000
([why](../nuances/windows.md#a-refused-connection-takes-about-a-second-on-windows-and-localhost-may-be-1-first-level-3)).

`tls: true` sends over TLS, trusting the certificates this server trusts; `tls: { ca, certificate }`
also trusts a private CA and presents a client certificate:
[TLS to receivers](tls.md#on-what-the-engine-connects-to). A refused handshake is a temporary
error: the message waits, with the reason on the destination
([nuance](../nuances/tls-connecting.md#a-refused-handshake-is-a-temporary-error-the-message-waits-level-2)).

`ack: none` sends without reading an answer, for the rare receiver that never sends ACKs:
nothing then tells the engine a message was refused or lost.

## HTTP

```yaml
# interfaces/results-to-portal/interface.yaml, in part
destination:
  http:
    api: portal             # an apis: entry: its URL, token, timeout and TLS
    path: /results
    method: POST            # the default
output: json
```

Each message is a request: to `url`, or to `path` under an `apis:` entry in resources.yaml
(`api:`), which brings the base URL, headers (the token), timeout and TLS, set once for every
interface and environment ([APIs](resources.md#http-apis)). Prefer `api:` for anything with a
token. **A 2xx answer is delivered; 408, 429, 5xx and network errors are tried again; any other
4xx is quarantined**: the receiver refused this message, and sending it again won't change that.

With a `url` of its own instead, a token belongs in a secret:
`headers: { Authorization: "Bearer ${secret:PORTAL_TOKEN}" }`.

## Files and SFTP

```yaml
# interfaces/adt-to-archive/interface.yaml, in part
destinations:
  local:
    file: { dir: ${OUTBOUND_DIR} }
  partner:
    sftp: { server: quest, dir: inbound/adt }
```

Each message is a file named by the message's id and the output format (`1234.hl7`), written
under a hidden temporary name and renamed when complete, so a reader never sees half a file. A
`file` destination's `dir` is relative to the interface's folder unless written whole, and is made
if it's missing; the service must be allowed to write there (Linux:
[the sandbox](../install/linux.md#what-the-package-installs); Windows:
[the service's folders](../install/windows.md#the-services-account)). An `sftp` destination's
`server` is an `sftp:` entry in resources.yaml.

## To another interface

```yaml
# interfaces/adt-in/interface.yaml, in part
destinations:
  lab:
    mllp: { host: ${LAB_HOST}, port: ${LAB_PORT} }
  billing:
    interface: adt-to-billing
```

`interface:` puts each message into the queue of an interface with `source: internal`, in the
same database transaction that marks it sent here: no network hop, and nothing lost or sent
twice between the two ([nuance](../nuances/routes.md#a-route-is-exactly-once-inside-the-engine-not-beyond-it-level-3)).
Use it to split one feed into teams' interfaces, or to chain steps each with its own owner, code,
retries and alerts. The receiving interface's `format` must be what this destination outputs, its
own keys, dedupe and destinations apply ([nuance](../nuances/routes.md#the-receiving-interfaces-keys-dedupe-and-destinations-apply-level-2)),
and routes can't form a loop: `ductus validate` checks both, and `ductus impact` follows routes
([nuance](../nuances/routes.md#routes-are-fixed-in-interfaceyaml-and-loops-are-refused-level-2)).

A drain stops what comes in from outside; routes between interfaces keep moving, so what's
already inside reaches its last interface before the engine stops
([nuance](../nuances/routes.md#routes-keep-moving-while-draining-level-4)).

## Pausing one

A destination can be paused (from the web UI, the API or `ductus pause adt-in lab`), at once,
with a reason now or later (`--reason`): its messages wait, in order, until it's resumed, and a pause never alerts. For a
receiver's planned downtime ([nuance](../nuances/destinations.md#pausing-a-destination-at-once-no-questions-and-it-holds-level-2)).
