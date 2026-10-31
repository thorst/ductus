import { quarantine } from "@ductus/sdk";

// Code tables: one sending system's codes and what each receiver wants instead. Kept in Git with
// the interfaces that use them, so a change is reviewed, tested and deployed like code.

/** PV1-2 patient class, from the EHR's codes to the lab system's. */
export const PATIENT_CLASS: Readonly<Record<string, string>> = {
  I: "IN",
  O: "OUT",
  E: "ER",
  P: "PRE",
  R: "OUT", // recurring patients are outpatients to the lab
};

/** PID-8 administrative sex, from HL7's table 0001 to FHIR's `gender`. */
export const GENDER: Readonly<Record<string, string>> = {
  M: "male",
  F: "female",
  O: "other",
  U: "unknown",
  A: "other",
  N: "unknown",
};

/** OBX-11 result status (HL7 table 0085) as FHIR's Observation `status`. */
export const RESULT_STATUS: Readonly<Record<string, string>> = {
  F: "final",
  P: "preliminary",
  C: "corrected",
  X: "cancelled",
  W: "entered-in-error",
};

/**
 * The code's translation in `table`, or quarantine the message: an unknown code is for a person
 * to look at (add it to the table, or fix it at the source), never something to guess.
 *
 *   msg.PV1[2] = translate(PATIENT_CLASS, msg.PV1[2], "patient class");
 */
export function translate(table: Readonly<Record<string, string>>, code: string, what: string): string {
  const value = table[code];
  if (value === undefined) throw quarantine(`unknown ${what} "${code}"`);
  return value;
}
