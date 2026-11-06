import { kill, type Message } from "@ductus/sdk";
import { PATIENT_CLASS, translate } from "../../lib/codes.ts";

// The lab's code. Each destination runs its own file on the message as it was received: what
// this changes, billing (billing.ts) never sees.

/** The visit events the lab system takes. */
const LAB_EVENTS = new Set(["A01", "A03", "A04", "A08"]);

export function transform(msg: Message) {
  if (!LAB_EVENTS.has(msg.MSH[9][2])) return null; // not for the lab: filtered, no reason needed
  if (msg.PID[3][1].startsWith("TEST")) return kill("test patient");

  msg.PV1[2] = translate(PATIENT_CLASS, msg.PV1[2], "patient class");
  for (const name of new Set(msg.segmentNames)) {
    if (name.startsWith("Z")) msg.remove(name); // the EHR's own segments mean nothing to the lab
  }
  msg.set("MSH-5", "LAB");
  return msg;
}
