// @ductus/sdk: the library interface code imports.
//
//   import type { Message } from "@ductus/sdk";
//   export function transform(msg: Message) { ... }

import { isMessage, type Message, parse } from "./hl7.ts";

export { Field, type Message, parse, Segment, Segments } from "./hl7.ts";
export { expect, fixture, hl7, test } from "./testing.ts";
export { __runTests, __setFixtures } from "./testing.ts";
export { type Database, db, env, secret } from "./services.ts";
export { generateTypes as __generateTypes } from "./typegen.ts";

// Must match ductus_script::KILL / QUARANTINE.
const KILL = "\u0000ductus:kill\u0000";
const QUARANTINE = "[ductus:quarantine]";

/** A deliberate drop, with a reason. Create with `kill()`. */
export class Kill {
  constructor(readonly reason: string) {}
}

/**
 * Drop this message: it's never delivered, and it's kept (status "filtered") with the reason
 * for audit. `return null` does the same without a reason.
 *
 *   if (msg.PID[3][1].startsWith("TEST")) return kill("test patient");
 */
export function kill(reason = ""): Kill {
  return new Kill(reason);
}

/** Error that sends the message to quarantine with a clear reason. Create with `quarantine()`. */
export class QuarantineError extends Error {
  override name = "QuarantineError";
}

/**
 * Send this message to quarantine for review, with a reason:
 *
 *   if (!msg.PID[3][1]) throw quarantine("missing MRN");
 */
export function quarantine(reason: string): QuarantineError {
  return new QuarantineError(`${QUARANTINE} ${reason}`);
}

function serialize(result: unknown): string | null {
  if (result instanceof Kill) return KILL + result.reason;
  if (result === null || result === undefined) return null;
  if (typeof result === "string") return result;
  if (isMessage(result)) return result.toString();
  throw new TypeError(`transform must return a Message, a string, or null (got ${typeof result})`);
}

/** Engine entry point: parse, run the interface's transform (sync or async), serialize the result. */
export function __invoke(transform: (msg: Message) => unknown, raw: string): string | null | Promise<string | null> {
  const result = transform(parse(raw));
  return result instanceof Promise ? result.then(serialize) : serialize(result);
}
