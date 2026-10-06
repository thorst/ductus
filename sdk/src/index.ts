// @ductus/sdk: the library interface code imports.
//
//   import type { Message } from "@ductus/sdk";
//   export function transform(msg: Message) { ... }

import { isMessage, type Message, parse } from "./hl7.ts";

export { Field, type Message, parse, Segment, Segments } from "./hl7.ts";
export { expect, fixture, hl7, test } from "./testing.ts";
export { __runTests, __setFixtures } from "./testing.ts";
export { generateTypes as __generateTypes } from "./typegen.ts";

/** Engine entry point: parse, run the interface's transform, serialize the result. */
export function __invoke(transform: (msg: Message) => unknown, raw: string): string | null {
  const result = transform(parse(raw));
  if (result === null || result === undefined) return null;
  if (typeof result === "string") return result;
  if (isMessage(result)) return result.toString();
  throw new TypeError(`transform must return a Message, a string, or null (got ${typeof result})`);
}
