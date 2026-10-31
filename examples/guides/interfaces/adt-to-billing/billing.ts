import { db, type Message, type Meta, quarantine } from "@ductus/sdk";
import { type FieldMap, mapFields } from "../../lib/mapping.ts";

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
  const attending = msg.get("PV1-7.1");
  if (attending) encounter.attendingNpi = await npiOf(attending);
  encounter.sourceMessage = meta.id; // the same on every retry: billing ignores a repeat
  return encounter;
}

// Providers' NPIs by the EHR's id, kept for an hour: they rarely change, and messages come often.
const npis = new Map<string, { npi: string; until: number }>();

async function npiOf(ehrId: string): Promise<string> {
  const known = npis.get(ehrId);
  if (known && known.until > Date.now()) return known.npi;
  const row = await db("provider_db").one<{ npi: string }>("SELECT npi FROM providers WHERE ehr_id = $1", [ehrId]);
  if (!row) throw quarantine(`provider ${ehrId} has no NPI in provider_db`);
  npis.set(ehrId, { npi: row.npi, until: Date.now() + 60 * 60 * 1000 });
  return row.npi;
}
