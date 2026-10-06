// HTTP endpoints in `respond` mode: transform(req) receives the request and returns the response.
//
//   export async function transform(req: HttpRequest) {
//     const p = await db("patient_db").one("SELECT ... WHERE mrn = $1", [req.params.mrn]);
//     return p ?? null;                          // object -> 200 JSON; null -> 404
//   }

import { decode, type FormatSpec } from "./formats.ts";

/** An incoming HTTP request (respond mode). */
export interface HttpRequest {
  method: string;
  path: string;
  /** Route parameters: `/patients/{mrn}` gives `params.mrn`. */
  params: Record<string, string>;
  /** Query string values (last one wins for repeated names). */
  query: Record<string, string>;
  /** Header names are lower-case. */
  headers: Record<string, string>;
  /** Raw body text. */
  text: string;
  /** Body parsed by the interface's `format` (JSON object, XmlElement, CSV records...). */
  readonly body: unknown;
}

/** A full HTTP response: status, headers and body. Create with `response()` or `html()`. */
export class HttpResponse {
  constructor(
    public status: number,
    public body: unknown,
    public headers: Record<string, string> = {},
  ) {}
}

/** A response with an explicit status and headers. The body is serialized by the output format
 *  unless it's a string. */
export function response(init: { status?: number; body?: unknown; headers?: Record<string, string> }): HttpResponse {
  return new HttpResponse(init.status ?? 200, init.body ?? "", init.headers ?? {});
}

/** An HTML page. */
export function html(body: string, status = 200): HttpResponse {
  return new HttpResponse(status, body, { "content-type": "text/html; charset=utf-8" });
}

interface Envelope {
  method: string;
  path: string;
  params: Record<string, string>;
  query: Record<string, string>;
  headers: Record<string, string>;
  body: string;
}

/** Build the request object from the engine's JSON envelope. The body is parsed on first use. */
export function requestFrom(raw: string, input: FormatSpec): HttpRequest {
  const e = JSON.parse(raw) as Envelope;
  let parsed: unknown;
  let done = false;
  return {
    method: e.method,
    path: e.path,
    params: e.params,
    query: e.query,
    headers: e.headers,
    text: e.body,
    get body() {
      if (!done) {
        parsed = e.body === "" ? undefined : decode(e.body, input);
        done = true;
      }
      return parsed;
    },
  };
}

const CONTENT_TYPES: Record<string, string> = {
  json: "application/json",
  xml: "application/xml",
  csv: "text/csv",
  fixed: "text/plain; charset=utf-8",
  text: "text/plain; charset=utf-8",
  hl7: "x-application/hl7-v2+er7",
};

export function contentType(output: FormatSpec): string {
  return CONTENT_TYPES[output.kind] ?? "application/octet-stream";
}
