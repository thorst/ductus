import type { Message } from "@ductus/sdk";

// Relay ADT messages, tagged with a ZDU segment. Anything else is filtered out.
export function transform(msg: Message) {
  if (msg.MSH[9][1] !== "ADT") return null;
  msg.add("ZDU", ["relayed-by-ductus"]);
  return msg;
}
