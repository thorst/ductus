# Transforms: mappings first, then code

A transform turns the message a sender sent into the one a receiver wants: a field moved, a code
translated, a segment removed, or a whole new message in another format. This page goes from the
smallest change to a full mapping, in the order you should reach for them.

## Change the message in place

For most HL7 receivers, the message you send is the one you received with a few fields changed.
Change it and return it:

```ts
// interfaces/adt-in/route.ts, in part
  msg.PV1[2] = translate(PATIENT_CLASS, msg.PV1[2], "patient class");
  for (const name of new Set(msg.segmentNames)) {
    if (name.startsWith("Z")) msg.remove(name); // the EHR's own segments mean nothing to the lab
  }
  msg.set("MSH-5", "LAB");
  return msg;
```

- **Only what you touch changes.** Segments your code doesn't change are written out exactly as
  they arrived, character for character, so a receiver's validation sees what the sender sent.
- **Two ways to name a field**: by position, `msg.PV1[2]` (with completion in the editor), or by
  path, `msg.get("PV1-2")` / `msg.set("PV1-2", "IN")`. Paths are handy when the field is data
  (a table, below); positions read better in code.
- **No copy needed.** Each destination's function already gets its own message as received, so
  changing it can't affect another destination ([nuance](../nuances/transforms.md#copy-what-you-change-not-the-whole-thing-through-json-level-2)).
- The reference for every way to read and write HL7 (components, repetitions, adding and removing
  segments, escaping) is [Formats: HL7 v2](formats.md#hl7-v2).

## A mapping as a table

When the receiver wants a different shape (JSON for an API, a flat record for a file), most of
the work is "this field goes there". Write that part as a table: one row per field, which an
analyst can read, review in a pull request and check against the receiver's spec line by line.

```ts
// interfaces/adt-to-billing/billing.ts, in part
/** The encounter the billing API takes, field by field. */
const ENCOUNTER: FieldMap = [
  ["PID-3.1", "patient.mrn"],
  ["PID-5.1", "patient.family"],
  ["PID-5.2", "patient.given"],
  ["PID-7", "patient.birthDate"],
  ["PV1-19", "visit.number"],
  ["PV1-3.1", "visit.unit"],
  ["PV1-44", "visit.admitted"],
  ["PV1-45", "visit.discharged"],
];

export async function transform(msg: Message, meta: Meta) {
  const encounter = mapFields(msg, ENCOUNTER);
  encounter.event = msg.MSH[9][2] === "A03" ? "discharge" : "admit";
```

The table is plain data; the few lines that apply it are a helper in the project's `lib/`, shared
by every interface that maps this way:

```ts
// lib/mapping.ts, in part
export function mapFields(msg: Message, map: FieldMap): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [from, to] of map) {
    const value = msg.get(from);
    if (value !== "") place(out, to, value);
  }
  return out;
}
```

- **The table does the predictable part; code does the rest**: the `event` that depends on the
  trigger, the NPI looked up in a database ([lookups](lookups.md)), the id that makes a repeat
  harmless. Nothing forces every field through the table.
- **Empty fields are left out** (`mapFields` skips `""`), so a receiver sees a missing discharge
  time as missing, not as an empty string. If your receiver wants `null` or `""` instead, change
  the helper once.
- **HL7 to HL7 works the same way**: `copyFields(from, to, [["PID-3.1", "PID-2.1"]])`, also in
  `lib/mapping.ts`, copies between two messages by path.
- **Test the table with a whole message** and compare the whole result
  ([testing](testing.md#compare-the-whole-result)): a wrong row is then a failing test, not a
  receiver's complaint.

Ductus has no mapping file or mapping screen of its own: the table *is* code, kept with the
interface in Git. That's deliberate for now (one place to look, one way to test, type-checked by
`ductus validate`); typed message definitions that underline a wrong field in the editor are on
the roadmap (v1.5).

## Codes from one system to another

A sending system's codes are rarely the receiver's: patient classes, sexes, result statuses,
units, locations. Keep each translation as a code table and translate through one function that
**quarantines a code it doesn't know** instead of passing it on or guessing:

```ts
// lib/codes.ts, in part
/** PV1-2 patient class, from the EHR's codes to the lab system's. */
export const PATIENT_CLASS: Readonly<Record<string, string>> = {
  I: "IN",
  O: "OUT",
  E: "ER",
  P: "PRE",
  R: "OUT", // recurring patients are outpatients to the lab
};
```

A new code at the sender then shows up as quarantined messages with "unknown patient class
\"B\"", the day it starts, instead of as wrong data in the receiver weeks later. Add the row,
deploy, and resend the quarantined messages. Where tables live (in code, or in a database the
business maintains) and how to keep a database lookup fast: [Lookups and code
tables](lookups.md).

## A new message from scratch

When the receiver's message has little in common with the sender's (a CSV row becoming HL7, an
HL7 result becoming FHIR), build the new one and return it:

```ts
// interfaces/census-in/census.ts, in part
export function transform(row: CsvRecord, meta: Meta) {
  if (!row.mrn) return kill("a row without an MRN");
  const now = hl7Time(new Date(meta.receivedAt));
  const msg = parseHl7(`MSH|^~\\&|BEDBOARD|HOSP|EHR|HOSP|${now}||ADT^A08^ADT_A01|BB${meta.id}|P|2.5`);
  msg.add("EVN", ["A08", now]);
  msg.add("PID");
  msg.set("PID-3.1", row.mrn);
  msg.set("PID-5.1", row.last);
  msg.set("PID-5.2", row.first);
```

- Start an HL7 message from its MSH with `parseHl7` (in a TypeScript string, the escape
  character `\` is written `\\`), then `add` segments and `set` fields; a field set past the end
  of a segment pads it with empty fields.
- **Times and ids from `meta`, not the clock**: `meta.receivedAt` and `meta.id` are the same every
  time this message is processed, so a retry, a resend or a [replay](../operate/resend.md#replay-a-period-before-a-change)
  produces the same message (and MSH-10 `BB501` the second time lets a receiver that dedupes
  drop the repeat). `new Date()` would give a different message each time.
- An object returned to a JSON destination is the request body as it is; there's an example in
  [Formats: FHIR](formats.md#fhir).

## When it's more than a mapping

Everything above is ordinary TypeScript, so anything a mapping table can't say is a few lines
more: a loop over every OBX, a value computed from two fields, a lookup, a decision on today's
date. Keep these rules:

- **Same message, same answer.** No `Math.random`, no `new Date()` in what you send, nothing kept
  from one message to the next except a bounded cache
  ([nuance](../nuances/delivery.md#your-transform-can-run-more-than-once-for-the-same-message-level-3)).
- **Say why a message can't be handled**: `throw quarantine("…")` with what's wrong and which
  value, never `return null` for a broken message ([routes and filters](routes-and-filters.md#quarantine-for-a-person-to-look-at)).
- **Ask once for many values**: a lookup per OBX in a loop is a round trip per OBX
  ([nuance](../nuances/transforms.md#one-query-for-many-values-not-one-query-per-value-level-3)).
- **Big messages**: build objects with every property in the same order, and let per-message
  objects go at the end ([nuance](../nuances/transforms.md#loops-over-thousands-of-items-give-objects-one-shape-level-4)).
- **Keep it fast**: a function running past `timeout` (5 s by default) without waiting on
  anything is stopped and its message quarantined. For most HL7 transforms `500ms` is plenty.
