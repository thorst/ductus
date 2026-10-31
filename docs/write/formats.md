# Formats: HL7 v2, FHIR, X12, CSV, JSON and XML

An interface's [`format`](../reference/config/interface.md#format) says what its messages are, so
your code gets them parsed; [`output`](../reference/config/interface.md#output) (on the interface
or on one destination) says what your code's results are written as. Both default to `hl7`.

| Format | Your code gets | Return | Written as |
|---|---|---|---|
| `hl7` | a `Message` | the `Message`, or a string | HL7, segments ending in `\r` |
| `json` | the parsed value (object, array…) | an object, array or string | JSON |
| `xml` | an `XmlElement`, the document's root | an `XmlElement`, or a string | XML |
| `csv` | records, objects by column name (or one record with `split: records`) | a record or a list of them | CSV with a header row |
| `fixed` | records by the layout's field names (or one) | a record or a list | fixed-width lines |
| `text` | the payload as a string | a string | as it is |

A string returned is always sent as it is, whatever the output format: the way out when a
receiver wants something no format writes. An MLLP source always carries HL7.

**Converting** is the two settings together: an HL7 feed to an API is `format: hl7` (the default)
with `output: json`; a CSV file to HL7 is `format: csv` with `output: hl7`. Each destination can
set its own `output`, so one HL7 feed can go out as HL7 to the lab and JSON to billing.

FHIR and X12 have no format of their own yet: FHIR is JSON, and X12 is read as text with a few
lines of code. Both are below, with what's planned.

## HL7 v2

```ts
// interfaces/adt-in/route.ts, in part
export function toLab(msg: Message) {
  if (!LAB_EVENTS.has(msg.MSH[9][2])) return null; // not for the lab: filtered, no reason needed
  if (msg.PID[3][1].startsWith("TEST")) return kill("test patient");
```

**Numbering is HL7's**, 1-based everywhere: `msg.PID[3][1]` is PID-3.1, `msg.MSH[9][2]` is MSH-9.2
(the trigger event). MSH is counted as the standard counts it: MSH-1 is the field separator
itself, so MSH-9 is the message type, as in every spec.

| To | Write | Gives |
|---|---|---|
| Read a field | `msg.PV1[2]`, `msg.get("PV1-2")` | `"I"` (text) |
| Read a component | `msg.PID[5][1]`, `msg.get("PID-5.1")` | `"SMITH"` |
| A composite field's whole text | `String(msg.PID[5])`, `msg.PID[5].value` | `"SMITH^ANNA^M"` |
| Every occurrence of a segment | `for (const obx of msg.OBX)` | each OBX, in order |
| The n-th occurrence | `msg.OBX.rep(2)`, `msg.OBX.count` | the second OBX; how many |
| Repetitions of a field | `for (const id of msg.PID[3])`, `msg.PID[3].rep(2)`, `.count` | each PID-3 identifier |
| Whether a segment is there | `msg.PV1.exists` | `true` / `false` |
| Segment names in order | `msg.segmentNames` | `["MSH", "EVN", "PID", "PV1"]` |
| Set a field or component | `msg.PV1[2] = "IN"`, `msg.set("PID-5.1", "SMITH")` | |
| Add a segment | `msg.add("ZDU", ["value"])`, `msg.insertAfter("PID", "PD1")` | the new segment |
| Remove segments | `msg.remove("ZPV")`, `msg.NTE.remove()` | every segment with that name |
| The whole message | `msg.toString()` | the HL7 text |

- **Indexing goes deeper, iterating goes across**: `msg.OBX[5]` is OBX-5 of the *first* OBX;
  `for (const obx of msg.OBX)` visits each one. Most "only the first result was mapped" bugs are
  this.
- **Missing reads as `""` and never throws**: a segment, field or component that isn't there
  is `""`, so code needs no null checks, but a wrong position is a quiet `""`. Test the fields you
  map ([nuance](../nuances/transforms.md#missing-fields-read-as--and-never-throw-level-1)).
- **Writing into a segment the message doesn't have throws** ("message has no PV1 segment; add
  it first with msg.add("PV1")"): a missing segment is usually a mistake worth seeing.
- **Simple fields read as text, composite ones as fields.** PV1-2 (a code) is a string;
  PID-5 (a name, with components) is a field you index into. The editor knows which is which for
  the common segments of HL7 2.5 (MSH, EVN, PID, NK1, PV1, ORC, OBR, OBX, NTE, AL1, DG1, MRG, MSA,
  ERR) and shows each field's name; any other segment, Z segments included, works the same way
  with numbers.
- **Escaping is done for you.** Reading decodes `\F\`, `\S\`, `\T\`, `\R\` and `\E\`; writing a
  component or simple field encodes the delimiters in it. A composite field set as a whole
  (`msg.PID[5] = "SMITH^ANNA"`) is taken as HL7, separators and all.
- **MSH-1 and MSH-2 are read-only**: the delimiters are the sender's, and the message is written
  with them.
- **Untouched segments go out exactly as they came**, so a message relayed with one field changed
  differs from the original in that field only.
- **The ACK isn't your code's job.** The engine answers the sender (AA once the message is stored)
  and reads the receiver's ACK (AA delivered, AE retried, AR quarantined by default:
  [destinations](../configure/destinations.md#mllp)).
- **Batch files** (many messages in one file) are split at each MSH into one message each by a
  file source with `split: records` ([sources](../configure/sources.md)).

## FHIR

FHIR R4 resources are JSON, so a FHIR interface is a JSON one: `format: json` for resources
coming in, `output: json` for resources going out, and an HTTP destination (or `api()`) to the
FHIR server. Here, each lab result (an HL7 ORU) becomes a FHIR transaction Bundle with one
Observation per OBX:

```yaml
# interfaces/results-to-fhir/interface.yaml
name: results-to-fhir
about:
  description: Lab results as FHIR R4 Observations, posted to the FHIR server as one transaction each
  owner: integration-team
  from: { application: Lab system }
  to: { application: FHIR server }
source: { mllp: { port: 6662 } }
script: to-fhir.ts
ordering: { key: PID-3.1 }
output: json
destination:
  http: { api: fhir, path: / }      # a transaction Bundle is posted to the server's base URL
```

```ts
// interfaces/results-to-fhir/to-fhir.ts, in part
/** An ORU^R01 as a FHIR R4 transaction Bundle: one Observation per OBX, for the patient by MRN. */
export function transform(msg: Message) {
  const mrn = msg.get("PID-3.1");
  if (!mrn) throw quarantine("a result without an MRN (PID-3.1)");
  const order = msg.get("OBR-3.1"); // the lab's own order number
  const subject = { reference: `Patient?identifier=${MRN_SYSTEM}|${mrn}` };

  const entry = [];
  for (const obx of msg.OBX) {
    const id = `${order}-${obx[1]}`; // the same on every resend, so the server can tell
    entry.push({
      resource: observation(obx, id, subject),
      request: { method: "POST", url: "Observation", ifNoneExist: `identifier=${RESULT_ID_SYSTEM}|${id}` },
    });
  }
  if (entry.length === 0) throw quarantine("a result with no OBX segments");
  return { resourceType: "Bundle", type: "transaction", entry };
}
```

What it shows, and why each part is there:

- **A delivery can happen twice** (a lost answer, a resend), so each Observation carries an
  identifier made from the result itself and is created with `ifNoneExist`: the server creates it
  once and answers the repeat with the one it has
  ([nuance](../nuances/delivery.md#a-message-can-be-delivered-twice-level-3)). Every FHIR create should
  have such a key.
- **The patient is found by MRN** with a conditional reference (`Patient?identifier=…|123456`),
  which the server resolves inside the transaction: no lookup from your code first.
- **A transaction is all or nothing** on the server: if one Observation is refused, none is
  stored, and the server's 4xx quarantines the message with its answer.
- **FHIR's times need an offset** whenever they have a time of day. HL7 v2 times often have
  none; guessing the server's offset is wrong half the year, so the example quarantines a time
  without one ("ask the sender to send offsets" is the fix). If your sender can't, convert
  with the site's time zone rules deliberately, in one helper, and test both sides of a
  daylight-saving change.
- **The answer**: a 2xx is delivered; 408, 429 and 5xx are tried again; other 4xx quarantine
  the message with the server's answer (an `OperationOutcome` saying which element was wrong) for
  a person to read ([destinations](../configure/destinations.md#http)).
- **Content type**: JSON goes out as `application/json`, which FHIR servers accept. One that
  insists on `application/fhir+json` gets it from the destination's
  [`headers`](../reference/config/interface.md#destinationhttpheaders), which win over the
  default: `http: { api: fhir, path: /, headers: { Content-Type: application/fhir+json } }`.
- **Tokens**: the FHIR server's base URL and token live on its `apis:` entry in resources.yaml,
  never in code ([resources](../configure/resources.md#http-apis)). Servers that want OAuth2
  client credentials or SMART backend services (Epic, Cerner): the engine doesn't fetch those
  tokens yet (planned with OAuth2 for outbound APIs); until then, a token your organization
  issues and renews into the secret.

**Reading FHIR from a server** is `api("fhir").get("/Patient?identifier=…")` in any interface's
code ([lookups](lookups.md)); **receiving** FHIR (a subscription's notifications, an app's
`POST`) is an HTTP source with `format: json`. There are no FHIR typings or resource validation
in the SDK yet: your code works with plain objects, and the server's validation is the check.
FHIR typings and helpers are planned for v1.5, a FHIR client and server connector for v1.6.

## X12 (EDI)

X12 files (835 remittances, 837 claims, 270/271 eligibility) are read as `format: text`, and a
few lines in `lib/` split them into segments and elements:

```ts
// lib/x12.ts, in part
/** Split an X12 interchange into its segments, with the separators its ISA segment declares. */
export function parseX12(text: string): X12Segment[] {
  const data = text.replace(/^﻿/, "").trimStart();
  if (!data.startsWith("ISA") || data.length < 106) throw quarantine("not an X12 interchange: it must start with ISA");
  const element = data[3]; // the character right after "ISA"
  const segment = data[105]; // ISA is fixed-length: its terminator is the 106th character
```

```ts
// interfaces/remits-in/remits.ts, in part
  for (const seg of parseX12(text)) {
    if (seg.id === "TRN") trace = seg.e(2);
    else if (seg.id === "N1" && seg.e(1) === "PR") payer = seg.e(2);
    else if (seg.id === "CLP") {
```

- **Read the separators from ISA**, never assume `*` and `~`: trading partners use `|`, `^` and
  others, and ISA is fixed-length precisely so its characters can be found by position.
- **Elements are numbered as X12 numbers them**: `seg.e(1)` is CLP01.
- **Loops are code**: X12's structure (the 2100 claim loop, its NM1 patient) is found by walking
  the segments in order, as the example does for each claim's patient.
- **What isn't there yet**: acknowledgements (TA1, 999) aren't produced for you; an X12 format
  with loops and typed segments is planned (v1.6). Until then, send a 999 back as its own
  destination if your trading partner needs one.

The whole interface turns an 835 into one CSV row per claim, for the billing office:
`examples/guides/interfaces/remits-in` ([CSV](#csv-and-fixed-width) below for the output).

## CSV and fixed-width

```yaml
# interfaces/census-in/interface.yaml
name: census-in
about:
  description: The bed board's nightly census file, each row an ADT A08 for the EHR
  owner: integration-team
  from: { application: Bed board }
  to: { application: EHR, vendor: Epic }
source:
  file: { dir: inbound, pattern: "census_*.csv", split: records }   # each row is a message
format: csv
script: census.ts
output: hl7
destination: { file: { dir: out } }
```

- **Records are objects by column name** (`row.mrn`), from the header line. Without one,
  `format: { csv: { header: false } }` gives arrays of strings; another delimiter is
  `format: { csv: { delimiter: "|" } }`. Quoted fields with delimiters, quotes and line breaks
  inside are read as RFC 4180 says.
- **One message per file, or per row**: without `split`, the whole file is one message and
  `transform` gets every record (an array); with `split: records`, each row is a message of its
  own (the header kept with each), with its own status, retries and quarantine, and `transform`
  gets one record. Split files whose rows are separate things (patients, orders); keep whole
  files that only mean something together (a batch the receiver must get complete).
- **Writing CSV**: return a record or a list of records with `output: csv`; columns are written
  in the order first seen, with a header row. `toCsv` and `parseCsv` from the SDK do the same in
  your own code.
- **Fixed-width** layouts name each field's start column (0-based) and width
  (`format: { fixed: { fields: [{ name: mrn, start: 0, width: 10 }, …] } }`); reading trims the
  padding unless `trim: false`, writing pads (`align: right`, `pad: "0"` for numbers) and cuts
  values that are too long. Cutting is silent: check lengths in code when a cut value would be
  wrong (a name is fine shortened, an account number isn't).

## JSON

`format: json` gives your code the parsed value; return an object (or array) and it's sent as
JSON. A file of JSON Lines (one document per line) with `split: records` is one message per line.
Numbers are JavaScript's: integers past 2^53 (some systems' ids) lose digits when parsed, so
ask for them as strings or read them with `format: text` and parse yourself.

## XML

```ts
// interfaces/orders-xml/orders.ts
import { quarantine, type XmlElement } from "@ductus/sdk";

/** A home-care equipment order for the supplier: the patient's MRN checked, the SSN taken out. */
export function transform(doc: XmlElement) {
  const mrn = doc.get("Patient/Identifiers/MRN"); // text at a path, "" if missing
  if (!mrn) throw quarantine("an order without Patient/Identifiers/MRN");
  doc.find("Patient")?.remove("SSN"); // remove child elements by name
  doc.set("Patient/Status", "verified"); // creates missing elements
  doc.set("@source", "ductus"); // an attribute, on the root
  const second = doc.get("Items/Item[2]/@sku"); // the second Item's attribute, 1-based
  if (second) doc.add("Note", `more than one item; the second is ${second}`);
  return doc;
}
```

- **Paths are relative to the element**, with `/` between names, `[n]` for the n-th of a name
  (1-based) and `@name` for an attribute as the last step. `find` gives the element,
  `elements(name)` all children with a name, `add(name, text)` a new child.
- **Namespaces are part of the name**: an element written `<hl7:patient>` is `hl7:patient` in a
  path, prefix and all. The prefix is the document's, so if a sender can change its prefixes,
  compare local names (`el.name.split(":").pop()`).
- **The `<?xml …?>` declaration is kept**; comments and processing instructions are dropped, and
  whitespace between elements is formatting, not content.
- **An XML file is always one message**: `split: records` doesn't split XML.

## Text

`format: text` gives the payload as a string and sends a string back as it is: for formats
Ductus doesn't parse (X12 above, a vendor's own layout), and for relaying anything unchanged.
