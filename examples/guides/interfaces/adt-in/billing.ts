import type { Message } from "@ductus/sdk";

/** Billing's code: admissions and discharges, as received. */
export function transform(msg: Message) {
  return ["A01", "A03"].includes(msg.MSH[9][2]) ? msg : null;
}
