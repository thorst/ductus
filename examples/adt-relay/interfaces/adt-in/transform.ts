// v0.1: messages are raw HL7 strings. Typed access (msg.PID[3][1]) arrives in v0.2.
// Return the message to send, or null to filter it out.
function transform(msg: string): string | null {
  if (!msg.includes("|ADT^")) return null;
  return msg.replace(/\r?$/, "") + "\rZDU|relayed-by-ductus\r";
}
