import { api, type HttpRequest, response } from "@ductus/sdk";

/** GET /patients/{mrn}: the EMPI's enterprise id for the MRN, or 404 when it has none. */
export async function transform(req: HttpRequest) {
  const mrn = req.params.mrn;
  const found = await api("empi").get(`/patients/${encodeURIComponent(mrn)}`);
  if (found.status === 404) return response({ status: 404, body: { error: `no patient with MRN ${mrn}` } });
  return response({ body: { mrn, enterpriseId: found.json().enterpriseId } });
}
