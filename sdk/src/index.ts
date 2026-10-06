// @ductus/sdk: the library interface code imports.
//
//   import type { Message } from "@ductus/sdk";
//   export function transform(msg: Message) { ... }

import { isMessage, type Message } from "./hl7.ts";
import { decode, encode, type FormatSpec } from "./formats.ts";
import { contentType, HttpResponse, requestFrom } from "./http.ts";

export { html, type HttpRequest, HttpResponse, response } from "./http.ts";

/** What a schedule source's `transform` receives. Times are ISO 8601, server local time. */
export interface ScheduledRun {
  /** When this run was due. */
  due: string;
  /** When the previous run was due (null on the first run): use it to pick up what changed since. */
  previous: string | null;
}

export { Field, type Message, parse, parse as parseHl7, Segment, Segments } from "./hl7.ts";
export {
  type CsvRecord,
  type FixedField,
  type FixedLayout,
  type FormatSpec,
  parseCsv,
  parseFixed,
  parseXml,
  toCsv,
  toFixed,
  XmlElement,
} from "./formats.ts";
export { expect, fixture, hl7, input, test } from "./testing.ts";
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

/** Input and output formats of an interface, from interface.yaml. */
export interface Formats {
  input: FormatSpec;
  output: FormatSpec;
  /** An HTTP endpoint in respond mode: transform receives the request, returns the response. */
  http?: boolean;
}

/** Turn a respond-mode result into the engine's response envelope (JSON). */
function respond(result: unknown, output: FormatSpec): string {
  const json = (status: number, body: unknown) =>
    JSON.stringify({ status, headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
  if (result instanceof Kill) return json(404, { error: result.reason || "not found" });
  if (result === null || result === undefined) return json(404, { error: "not found" });
  const r = result instanceof HttpResponse ? result : new HttpResponse(200, result);
  const headers: Record<string, string> = {};
  for (const [k, v] of Object.entries(r.headers)) headers[k.toLowerCase()] = v;
  const body = typeof r.body === "string" ? r.body : encode(r.body, output, isMessage);
  if (!headers["content-type"] && body !== "") headers["content-type"] = contentType(output);
  return JSON.stringify({ status: r.status, headers, body });
}

const HL7: Formats = { input: { kind: "hl7" }, output: { kind: "hl7" } };

function serialize(result: unknown, output: FormatSpec): string | null {
  if (result instanceof Kill) return KILL + result.reason;
  if (result === null || result === undefined) return null;
  return encode(result, output, isMessage);
}

/** Engine entry point: parse by format, run the transform (sync or async), serialize the result.
 *  @internal Exported for the engine and SDK tests. */
export function __invoke(
  transform: (input: unknown) => unknown,
  raw: string,
  formats: Formats = HL7,
): string | null | Promise<string | null> {
  if (formats.http) {
    const result = transform(requestFrom(raw, formats.input));
    return result instanceof Promise ? result.then((r) => respond(r, formats.output)) : respond(result, formats.output);
  }
  const result = transform(decode(raw, formats.input));
  return result instanceof Promise
    ? result.then((r) => serialize(r, formats.output))
    : serialize(result, formats.output);
}
