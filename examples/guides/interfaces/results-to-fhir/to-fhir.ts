import { type Message, quarantine, type Segment } from "@ductus/sdk";
import { RESULT_STATUS, translate } from "../../lib/codes.ts";

const MRN_SYSTEM = "http://hospital.org/mrn";
const RESULT_ID_SYSTEM = "http://hospital.org/lab-result";
const CODE_SYSTEMS: Record<string, string> = { LN: "http://loinc.org" };

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

function observation(obx: Segment, id: string, subject: { reference: string }) {
  const code = obx.field(3);
  const value = String(obx[5]);
  return {
    resourceType: "Observation",
    identifier: [{ system: RESULT_ID_SYSTEM, value: id }],
    status: translate(RESULT_STATUS, String(obx[11]), "result status (OBX-11)"),
    code: { coding: [{ system: CODE_SYSTEMS[code[3]] ?? code[3], code: code[1], display: code[2] }] },
    subject,
    effectiveDateTime: dateTime(String(obx[14]), "OBX-14"),
    ...(obx[2] === "NM"
      ? { valueQuantity: { value: Number(value), unit: obx.field(6)[1] } }
      : { valueString: value }),
  };
}

/**
 * An HL7 timestamp with its offset (`20261031083000-0500`) as a FHIR dateTime
 * (`2026-10-31T08:30:00-05:00`). FHIR requires the offset whenever there's a time, and guessing
 * one is wrong half the year, so a time without one is quarantined.
 */
function dateTime(ts: string, field: string): string {
  const m = /^(\d{4})(\d{2})(\d{2})(\d{2})(\d{2})(\d{2})?(?:\.\d+)?([+-]\d{2})(\d{2})$/.exec(ts);
  if (!m) throw quarantine(`${field} "${ts}" isn't a time with an offset (YYYYMMDDHHMM[SS]±ZZZZ)`);
  const [, y, mo, d, h, mi, s = "00", oh, om] = m;
  return `${y}-${mo}-${d}T${h}:${mi}:${s}${oh}:${om}`;
}
