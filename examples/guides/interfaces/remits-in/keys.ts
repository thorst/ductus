import { parseX12 } from "../../lib/x12.ts";

/**
 * The payment's trace number (TRN02): a payer that sends the same 835 again sends the same trace
 * number, so it's the file's dedupe key. Read with the separators the file declares in its ISA
 * segment, which a regex can't know. A file that isn't X12 is quarantined, with why.
 */
export function trace(text: string): string {
  return parseX12(text).find((seg) => seg.id === "TRN")?.e(2) ?? "";
}
