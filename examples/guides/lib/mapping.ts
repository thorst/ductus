import type { Message } from "@ductus/sdk";

/** A mapping as a table: each row copies an HL7 field ("PID-3.1") to a place in a JSON object
 *  ("patient.mrn"). */
export type FieldMap = readonly (readonly [from: string, to: string])[];

/**
 * A JSON object filled from `msg` by a table of field copies. Empty fields are left out, so the
 * receiver sees a missing value as missing rather than as "".
 *
 *   const body = mapFields(msg, [["PID-3.1", "patient.mrn"], ["PV1-19", "visit.number"]]);
 */
export function mapFields(msg: Message, map: FieldMap): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [from, to] of map) {
    const value = msg.get(from);
    if (value !== "") place(out, to, value);
  }
  return out;
}

/** Set `path` ("a.b.c") in `target`, making the objects on the way. */
function place(target: Record<string, unknown>, path: string, value: unknown): void {
  const steps = path.split(".");
  let at = target;
  for (const step of steps.slice(0, -1)) {
    at[step] ??= {};
    at = at[step] as Record<string, unknown>;
  }
  at[steps[steps.length - 1]] = value;
}

/** HL7 to HL7: copy fields between two messages by a table of [from, to] paths. */
export function copyFields(from: Message, to: Message, map: FieldMap): void {
  for (const [a, b] of map) to.set(b, from.get(a));
}
