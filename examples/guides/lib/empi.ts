import { api, quarantine } from "@ductus/sdk";

/** The patient's enterprise id from the EMPI, by MRN. */
export async function enterpriseId(mrn: string): Promise<string> {
  const r = await api("empi").get(`/patients/${encodeURIComponent(mrn)}`);
  if (r.status === 404) throw quarantine(`the EMPI doesn't know MRN ${mrn}`);
  return r.json().enterpriseId;
}
