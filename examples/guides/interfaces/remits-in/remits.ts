import type { CsvRecord } from "@ductus/sdk";
import { parseX12 } from "../../lib/x12.ts";

/** CLP02 claim status codes, as the billing office reads them. */
const CLAIM_STATUS: Record<string, string> = {
  "1": "paid (primary)",
  "2": "paid (secondary)",
  "3": "paid (tertiary)",
  "4": "denied",
  "22": "reversal",
};

/** An 835 remittance as one row per claim (CLP), with the payment's trace number (TRN02). */
export function transform(text: string): CsvRecord[] {
  const rows: CsvRecord[] = [];
  let trace = "";
  let payer = "";
  for (const seg of parseX12(text)) {
    if (seg.id === "TRN") trace = seg.e(2);
    else if (seg.id === "N1" && seg.e(1) === "PR") payer = seg.e(2);
    else if (seg.id === "CLP") {
      rows.push({
        trace,
        payer,
        claim: seg.e(1),
        status: CLAIM_STATUS[seg.e(2)] ?? `code ${seg.e(2)}`,
        charged: seg.e(3),
        paid: seg.e(4),
        patient_owes: seg.e(5),
        payer_claim: seg.e(7),
        patient: "",
      });
    } else if (seg.id === "NM1" && seg.e(1) === "QC" && rows.length > 0) {
      rows[rows.length - 1].patient = `${seg.e(3)}, ${seg.e(4)}`; // the claim's patient follows its CLP
    }
  }
  return rows;
}
