# How interface code works

An interface's code is a TypeScript file in its folder, named by
[`script`](../reference/config/interface.md#script) in its interface.yaml. The engine calls one of
its functions for each message and each destination, and sends on what the function returns. This
page is how that works; the pages after it are the tasks: [routing and
filtering](routes-and-filters.md), [mapping one message to another](transforms.md),
[formats](formats.md), [lookups and code tables](lookups.md) and [testing](testing.md).

**Say it in YAML first.** Much of what interface code does in other engines is a setting here,
checked by `ductus validate` and visible to `ductus impact` without reading any code. Write code
for what's left:

| You want to | Without code | Page |
|---|---|---|
| Send each message to several places, each with its own queue and retries | [`destinations`](../reference/config/interface.md#destinations) | [Destinations](../configure/destinations.md) |
| Pass a feed into another team's interface | [`destination.interface`](../reference/config/interface.md#destinationinterface) and `source: internal` | [Routes and filters](routes-and-filters.md#to-another-interface) |
| Keep one patient's messages in order | [`ordering.key`](../reference/config/interface.md#orderingkey): `PID-3.1` | [Delivery](../configure/delivery.md) |
| Ignore a sender's repeat after a lost ACK | [`dedupe.keys`](../reference/config/interface.md#dedupekeys): `[MSH-10]` | [Delivery](../configure/delivery.md) |
| Read CSV, fixed-width, JSON or XML; write another of them | [`format`](../reference/config/interface.md#format) and [`output`](../reference/config/interface.md#output) | [Formats](formats.md) |
| Split a batch file into one message per record | `split: records` on a file source | [Sources](../configure/sources.md) |
| Relay messages as they came | no `script` at all | below |

What needs code: deciding which messages a destination gets, changing fields, building a
message in another shape, and looking things up. There is no field-mapping screen or mapping
file: **the mapping is code**, and a mapping can still be written as a table in that code
([transforms](transforms.md#a-mapping-as-a-table)), reviewed in Git and tested like the rest.

## What the engine does with each message

1. A sender's message arrives and is **stored** in the database; only then does the sender get
   its acknowledgement. Nothing about your code delays or decides the ACK (except an HTTP source
   in `respond` mode, below).
2. For **each destination**, in its own queue and order, a worker calls that destination's
   function with the message **as it was received**, parsed by the interface's `format`.
3. What the function returns decides what happens, for that destination only:

| The function | The delivery is | Shown as |
|---|---|---|
| returns a message, an object or a string | sent: written in the destination's `output` format and delivered | sent |
| returns `null` (or nothing) | filtered: never sent, kept with the message for audit | filtered |
| returns `kill("reason")` | filtered, with the reason shown on the message | filtered: reason |
| throws `quarantine("reason")` | quarantined: held for a person to look at, resend, correct or dismiss | quarantined: reason |
| throws any other error | quarantined, with the error's text | quarantined: error |
| meets a temporary failure (a database or API that's down, an HTTP 5xx, 408 or 429) | waiting: tried again later, in order, with nothing sent meanwhile in its lane | pending, with the error |

4. A message is **finished** when every destination is done with it: sent, filtered or
   quarantined. Its overall status is quarantined if any destination quarantined it, else sent if
   any sent it, else filtered.

So a function's job is to say *what this destination should get*, or that it gets nothing, or
that a person must look. Never swallow a problem with `return null`: a message nobody can see
again is the failure people find months later. Quarantine it, with a reason a person can act on
([nuance](../nuances.md#return-null-filters-throwing-quarantines-level-1)).

## The functions

```ts
// interfaces/adt-in/route.ts, in part
export function toLab(msg: Message) {
  if (!LAB_EVENTS.has(msg.MSH[9][2])) return null; // not for the lab: filtered, no reason needed
  if (msg.PID[3][1].startsWith("TEST")) return kill("test patient");
```

- **`transform(msg, meta)`** is the function a destination calls unless it names another with
  [`transform:`](../reference/config/interface.md#destinationtransform). With several
  destinations, export one function per destination (`toLab`, `toBilling`) and name each; one
  file can hold them all.
- **The first argument** is the message parsed by [`format`](../reference/config/interface.md#format):
  a `Message` for HL7, an object or array for JSON, an `XmlElement` for XML, a record (or a list
  of them) for CSV and fixed-width, a string for `text` ([formats](formats.md)).
- **The second, `meta`**, is about this message: `id` (the stored message's id, the same on
  every retry and for every destination: give it to receivers that can ignore a repeat),
  `receivedAt`, `destination` (which destination this call is for) and `origin` (for a message
  routed from another interface, its id there).
- **A function may be `async`** and `await` lookups ([lookups](lookups.md)). While one waits, the
  thread runs other messages.
- **What it returns is written in the destination's output format**: a `Message` or an
  `XmlElement` as itself, an object as JSON, records as CSV or fixed-width lines, and a string as
  it is, whatever the format.
- **Without a `script`**, every destination gets the message exactly as received.

## What code may and may not do

- **It may only import project files and `@ductus/sdk`.** No npm packages, no network or files
  of its own: databases, APIs, variables and secrets come through the SDK
  (`db()`, `api()`, `fetch()`, `env()`, `secret()`), so the engine can time them, retry them,
  mock them in tests and list them in `ductus impact`.
- **It can run more than once for one message**: after a temporary failure, a restart in the
  middle, a resend or a replay ([nuance](../nuances.md#your-transform-can-run-more-than-once-for-the-same-message-level-3)).
  So it must give the same answer for the same message, and anything it *writes* elsewhere
  (`db().execute`, an API `POST`) must be safe to repeat: use `meta.id` as the key.
- **It shouldn't run long.** A function that runs past [`timeout`](../reference/config/interface.md#timeout)
  (5 s by default) without pausing is stopped, its message quarantined, and three in a row stop
  the interface ([runaway scripts](../nuances.md#a-runaway-message-is-quarantined-three-in-a-row-stop-the-interface-level-2)).
  Waiting on a lookup doesn't count.
- **Missing fields read as `""` and never throw**, so a typo in a field number gives `""` quietly:
  test with real messages and check the fields you map
  ([nuance](../nuances.md#missing-fields-read-as--and-never-throw-level-1)).

## Code shared between interfaces

A file outside any interface folder, such as `lib/codes.ts`, can be imported by several
interfaces (`import { translate } from "../../lib/codes.ts"`). Use it for code tables, mappings
and helpers that more than one interface needs. Two rules
([shared pool](../shared-pool.md)):

- **Keep it read-only**: tables as `const`, functions that only use their arguments. A top-level
  `let` is one copy shared by every interface on that thread, and `ductus validate` warns about it
  (`DX3`).
- **A heavy loop belongs in the interface's own file**: a runaway in a shared file after an
  `await` can't be blamed on one interface
  ([nuance](../nuances.md#a-runaway-in-a-shared-library-after-await-cant-be-blamed-on-anyone-level-5)).

## A new interface, and the editor

`ductus new interface orders-in` (with `--source mllp|http|file|sftp|schedule` and `--format`)
writes a working interface: its interface.yaml, a `transform.ts`, a test and a sample message,
passing `ductus test` and `ductus validate` as written. Start from there.

`ductus test` and `ductus validate` write the SDK's typings into `.ductus/types` and a
`tsconfig.json`, so VS Code (or any editor that speaks TypeScript) completes `msg.PID[` with the
fields' names and underlines mistakes as you type. After an upgrade, run `ductus types` so the
editor knows the new version ([nuance](../nuances.md#your-editors-help-is-the-installed-versions-run-ductus-types-after-an-upgrade-level-1)).

## The examples on these pages

The code on these pages is from a small project in the repository, `examples/guides`: ADT routed
to a lab and to billing, results to a FHIR server, an X12 835 to CSV, a census file to HL7. A
code block that starts with a file's path, such as `// interfaces/adt-in/route.ts`, is a piece
of that file, and the docs' tests check that the page and the file agree and that the project's
own tests pass. Copy it, run `ductus test` in it, and change it.
