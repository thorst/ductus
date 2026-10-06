// Engine services for interface code: databases, HTTP, environment variables, secrets.
//
//   const rows = await db("patient_db").query("SELECT name FROM patients WHERE mrn = $1", [mrn]);
//   const patient = (await fetch(`${env("EMPI_URL")}/patients/${mrn}`)).json();
//   const url = env("EPIC_URL");

export type Param = string | number | boolean | null;
export type Row = Record<string, unknown>;

// Must match ductus_script::TRANSIENT.
const TRANSIENT = "[ductus:transient]";

/** An HTTP request as the engine receives it (ductus_script::FetchRequest). */
export interface FetchCall {
  url: string;
  method: string;
  /** Header names are lower-case. */
  headers: Record<string, string>;
  body: string | null;
  timeoutMs: number;
}

export interface RawResponse {
  status: number;
  headers: Record<string, string>;
  body: string;
}

/** The engine's native operations (ops.js), or test mocks in their place. */
export interface Ops {
  dbQuery(resource: string, sql: string, params: Param[]): Promise<Row[]>;
  dbExecute(resource: string, sql: string, params: Param[]): Promise<number>;
  fetch(request: FetchCall): Promise<RawResponse>;
  variable(name: string): string | null;
  secret(name: string): string | null;
}

let replacement: Ops | null = null;

/** @internal `ductus test` puts mocks in place of the engine's services. */
export function __useOps(ops: Ops | null): void {
  replacement = ops;
}

function ops(): Ops {
  if (replacement) return replacement;
  const o = (globalThis as { __ductus_ops?: Ops }).__ductus_ops;
  if (!o) throw new Error("engine services are not available");
  return o;
}

/** A database defined in resources.yaml. */
export interface Database {
  /**
   * Rows as objects. Use $1, $2... placeholders on Postgres and SQL Server alike; cast in SQL if
   * needed: `$1::int` (Postgres), `CAST($1 AS varchar(20))` (SQL Server sends text as nvarchar, so
   * cast it when comparing with an indexed varchar column).
   */
  query<T = Row>(sql: string, params?: Param[]): Promise<T[]>;
  /** The first row, or null. */
  one<T = Row>(sql: string, params?: Param[]): Promise<T | null>;
  /** Run a statement; returns rows affected. */
  execute(sql: string, params?: Param[]): Promise<number>;
}

/** A database defined in resources.yaml, by name. */
export function db(name: string): Database {
  return {
    query: <T>(sql: string, params: Param[] = []) => ops().dbQuery(name, sql, params) as Promise<T[]>,
    one: async <T>(sql: string, params: Param[] = []) =>
      ((await ops().dbQuery(name, sql, params))[0] as T | undefined) ?? null,
    execute: (sql: string, params: Param[] = []) => ops().dbExecute(name, sql, params),
  };
}

/** A variable from environments/<env>.yaml. Throws if it isn't defined. */
export function env(name: string): string {
  const value = ops().variable(name);
  if (value === null || value === undefined) throw new Error(`environment variable "${name}" is not defined`);
  return value;
}

/** A secret from the environment's secret store. Throws if it isn't defined. */
export function secret(name: string): string {
  const value = ops().secret(name);
  if (value === null || value === undefined) throw new Error(`secret "${name}" is not defined`);
  return value;
}

export interface FetchOptions {
  /** Default: GET, or POST when there's a body. */
  method?: string;
  headers?: Record<string, string>;
  /** A string is sent as is; anything else as JSON (with content-type application/json). */
  body?: unknown;
  /** Added to the URL's query string (values are encoded). */
  query?: Record<string, string | number | boolean>;
  /** Milliseconds; default 30000. */
  timeout?: number;
  /**
   * Default true: network errors, timeouts, 408, 429 and 5xx responses make the message wait
   * and retry (order is kept). false: those statuses come back as responses, and network
   * errors throw ordinary errors (the message is quarantined unless you catch them).
   */
  retry?: boolean;
}

/** An HTTP response. The body is already read, so `text()` and `json()` need no `await`. */
export class FetchResponse {
  constructor(
    readonly status: number,
    /** Header names are lower-case. */
    readonly headers: Record<string, string>,
    private readonly body: string,
    private readonly label: string,
  ) {}

  /** Status 200-299. */
  get ok(): boolean {
    return this.status >= 200 && this.status < 300;
  }

  text(): string {
    return this.body;
  }

  json<T = any>(): T {
    try {
      return JSON.parse(this.body) as T;
    } catch {
      throw new Error(`${this.label}: response is not JSON${preview(this.body)}`);
    }
  }
}

function preview(body: string): string {
  const text = body.trim();
  return text ? `: ${text.length > 200 ? text.slice(0, 200) + "..." : text}` : "";
}

function withQuery(url: string, query: FetchOptions["query"]): string {
  const pairs = Object.entries(query ?? {}).map(([k, v]) => `${encodeURIComponent(k)}=${encodeURIComponent(String(v))}`);
  if (pairs.length === 0) return url;
  const [base, hash] = url.split("#", 2);
  return `${base}${base.includes("?") ? "&" : "?"}${pairs.join("&")}${hash === undefined ? "" : "#" + hash}`;
}

/**
 * Call an HTTP API through the engine (interface code has no network of its own):
 *
 *   const r = await fetch(`${env("EMPI_URL")}/patients/${mrn}`, {
 *     headers: { authorization: `Bearer ${secret("EMPI_TOKEN")}` },
 *   });
 *   if (r.status === 404) throw quarantine(`unknown MRN ${mrn}`);
 *   const patient = r.json();
 */
export async function fetch(url: string, options: FetchOptions = {}): Promise<FetchResponse> {
  const hasBody = options.body !== undefined && options.body !== null;
  const method = (options.method ?? (hasBody ? "POST" : "GET")).toUpperCase();
  const headers: Record<string, string> = {};
  for (const [k, v] of Object.entries(options.headers ?? {})) headers[k.toLowerCase()] = String(v);
  let body: string | null = null;
  if (hasBody) {
    if (typeof options.body === "string") {
      body = options.body;
    } else {
      body = JSON.stringify(options.body);
      headers["content-type"] ??= "application/json";
    }
  }
  const full = withQuery(url, options.query);
  // Messages never show the query string: it may carry an API key.
  const label = `${method} ${full.split(/[?#]/)[0]}`;
  const retry = options.retry ?? true;
  let raw: RawResponse;
  try {
    raw = await ops().fetch({ url: full, method, headers, body, timeoutMs: options.timeout ?? 30_000 });
  } catch (e) {
    const message = e instanceof Error ? e.message : String(e);
    throw new Error(retry ? message : message.replace(TRANSIENT, "").trim());
  }
  if (retry && (raw.status >= 500 || raw.status === 408 || raw.status === 429)) {
    throw new Error(`${TRANSIENT} ${label} returned ${raw.status}${preview(raw.body)}`);
  }
  return new FetchResponse(raw.status, raw.headers, raw.body, label);
}
