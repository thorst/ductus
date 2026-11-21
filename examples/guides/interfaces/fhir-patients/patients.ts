import { getPath, quarantine } from "@ductus/sdk";

/** A FHIR Patient as the master patient index loads it: our MRN, the family name. */
export function transform(patient: Record<string, unknown>) {
  const mrn = getPath(patient, "identifier[system=urn:oid:1.2.3].value"); // the MRN, wherever it is
  if (!mrn) throw quarantine("a Patient without our MRN");
  return { mrn, family: getPath(patient, "name[0].family") };
}
