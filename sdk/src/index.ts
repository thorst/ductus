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
  /** When this run was due, on the schedule's clock with its offset (`2026-11-22T06:00:00-06:00`): its `timezone`, else ductus.yaml's, else the server's. `new Date(run.due)` reads it. */
  due: string;
  /** When the previous run was due, in the same form (null on the first run): use it to pick up what changed since. */
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
  type XmlNames,
} from "./formats.ts";
export { getPath } from "./paths.ts";
export { expect, fixture, hl7, input, mock, type MockResponse, test } from "./testing.ts";
export { __runTests, __setFixtures } from "./testing.ts";
export { type Api, api, type Database, db, env, fetch, type FetchOptions, FetchResponse, secret } from "./services.ts";
export { generateTypes as __generateTypes } from "./typegen.ts";
import { DATATYPES, PRIMITIVES, SEGMENT_NAMES, SEGMENTS } from "./schema.ts";
import { GENERIC_SEGMENTS } from "./typegen.ts";
/** The HL7 definitions, for the SDK's own tests (`sdk/tests/schema.test.ts`).
 *  @internal */
export const __schema = { DATATYPES, GENERIC_SEGMENTS, PRIMITIVES, SEGMENT_NAMES, SEGMENTS };

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

/**
 * About the message being processed, the second argument of `transform` (and of each
 * destination's function):
 *
 *   export function transform(msg: Message, meta: Meta) { ... }
 */
export interface Meta {
  /** The stored message's id: the same every time this message is processed (retries, restarts,
   *  each destination). Use it as a unique key or `Idempotency-Key` so a receiver can ignore
   *  a repeat. Unique within one engine database. */
  id: number;
  /** When the message was received (ISO 8601). */
  receivedAt: string;
  /** The destination this call prepares the message for: its name under `destinations:`. */
  destination: string;
  /** Routed from another interface (`source: internal`): the `id` of the message there. Absent
   *  for messages received from outside. */
  origin?: number;
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
 *  `meta` is the message's `Meta` as JSON (none in respond mode).
 *  @internal Exported for the engine and SDK tests. */
export function __invoke(
  transform: (input: unknown, meta?: Meta) => unknown,
  raw: string,
  formats: Formats = HL7,
  meta?: string,
): string | null | Promise<string | null> {
  if (formats.http) {
    const result = transform(requestFrom(raw, formats.input));
    return result instanceof Promise ? result.then((r) => respond(r, formats.output)) : respond(result, formats.output);
  }
  const result = transform(decode(raw, formats.input), meta === undefined ? undefined : JSON.parse(meta));
  return result instanceof Promise
    ? result.then((r) => serialize(r, formats.output))
    : serialize(result, formats.output);
}

/** `__invoke` with each stage's end stamped (`ductus bench interface`'s per-stage times): 0 as it
 *  starts, 1 once the message is read (decoded, and its `meta`), 2 when the function's result is
 *  there (a promise's once it settles) or it threw, 3 once that's serialized. A stamp is an op that records
 *  the time; with no stamp op (outside the engine) it's plain `__invoke`. The engine never binds
 *  it: only the bench does, so a message's path costs nothing more.
 *  @internal Exported for the bench and SDK tests. */
export function __invokeTimed(
  transform: (input: unknown, meta?: Meta) => unknown,
  raw: string,
  formats: Formats = HL7,
  meta?: string,
): string | null | Promise<string | null> {
  const stamp = (globalThis as { __ductus_stamp?: (stage: number) => void }).__ductus_stamp ?? (() => {});
  stamp(0);
  const input = formats.http ? requestFrom(raw, formats.input) : decode(raw, formats.input);
  const parsed = formats.http || meta === undefined ? undefined : (JSON.parse(meta) as Meta);
  stamp(1);
  // A function that throws (a quarantine, a retry) still has its time: stamped 2, and no 3.
  const failed = (e: unknown): never => {
    stamp(2);
    throw e;
  };
  let result: unknown;
  try {
    result = formats.http ? transform(input) : transform(input, parsed);
  } catch (e) {
    return failed(e);
  }
  const done = (r: unknown) => {
    stamp(2);
    const out = formats.http ? respond(r, formats.output) : serialize(r, formats.output);
    stamp(3);
    return out;
  };
  return result instanceof Promise ? result.then(done, failed) : done(result);
}

/** Engine entry point for a key function (`ordering: { key: { script: key.ts } }`): parse by
 *  format, run it, and answer its key as text. Nothing or `null` is an empty key, a number its
 *  digits; anything else, or a promise, is an error (the message is quarantined with it).
 *  @internal Exported for the engine and SDK tests. */
export function __key(key: (input: unknown) => unknown, raw: string, formats: Formats = HL7): string {
  const result = key(decode(raw, formats.input));
  if (result === null || result === undefined) return "";
  if (typeof result === "string") return result;
  if (typeof result === "number" || typeof result === "bigint") return String(result);
  if (typeof (result as { then?: unknown }).then === "function") {
    throw new Error(
      `key function \`${key.name}\` returned a promise: a key is read while the sender waits for its ACK, so it can't wait for anything (no async, await, db, api or fetch)`,
    );
  }
  throw new Error(`key function \`${key.name}\` returned ${typeof result}: a key is text (or a number)`);
}
