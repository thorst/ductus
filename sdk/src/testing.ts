// Built-in test framework: test(), expect(), fixture(), mock. Run with `ductus test`.

import { type Message, parse } from "./hl7.ts";
import { decode, type FormatSpec } from "./formats.ts";
import { __useOps, type FetchCall, type Ops, type Param, type RawResponse, type Row } from "./services.ts";

interface TestCase {
  name: string;
  fn: () => unknown;
}

/** What a mocked `db()` answers: rows for `query`/`one`, a count (or rows) for `execute`. */
export type DbHandler = (sql: string, params: Param[]) => Row[] | number | Promise<Row[] | number>;
/** A mocked HTTP response; a non-string body is sent as JSON. */
export interface MockResponse {
  status?: number;
  headers?: Record<string, string>;
  body?: unknown;
}
export type FetchHandler = (request: FetchCall) => MockResponse | Promise<MockResponse>;
export interface DbCall {
  sql: string;
  params: Param[];
}

// Mocks last for one test: every test starts with none.
let dbMocks = new Map<string, { handler: DbHandler; calls: DbCall[] }>();
let fetchMock: { handler: FetchHandler; calls: FetchCall[] } | null = null;
let envMock: Record<string, string> = {};
let secretMock: Record<string, string> = {};

function resetMocks(): void {
  dbMocks = new Map();
  fetchMock = null;
  envMock = {};
  secretMock = {};
}

/**
 * Stand-ins for the engine's services in tests, so interfaces that use databases, HTTP APIs,
 * variables or secrets are tested without any of them. Each returns the list of calls made.
 *
 *   const calls = mock.db("patient_db", (sql, [mrn]) => [{ name: "SMITH" }]);
 *   mock.fetch((req) => ({ status: 200, body: { id: 1 } }));
 *   mock.env({ EMPI_URL: "https://empi.test" });
 */
export const mock = {
  /** Answer `db(name)` calls. Pass rows, or a function of (sql, params). */
  db(name: string, answer: DbHandler | Row[]): DbCall[] {
    const calls: DbCall[] = [];
    dbMocks.set(name, { handler: typeof answer === "function" ? answer : () => answer, calls });
    return calls;
  },
  /** Answer `fetch()` calls. Pass a response, or a function of the request. */
  fetch(answer: FetchHandler | MockResponse): FetchCall[] {
    const calls: FetchCall[] = [];
    fetchMock = { handler: typeof answer === "function" ? answer : () => answer, calls };
    return calls;
  },
  /** Values for `env()`. */
  env(values: Record<string, string>): void {
    envMock = { ...envMock, ...values };
  },
  /** Values for `secret()`. */
  secret(values: Record<string, string>): void {
    secretMock = { ...secretMock, ...values };
  },
};

async function mockedDb(name: string, sql: string, params: Param[]): Promise<Row[] | number> {
  const m = dbMocks.get(name);
  if (!m) throw new Error(`db("${name}") is not mocked in this test: add mock.db("${name}", (sql, params) => [/* rows */])`);
  m.calls.push({ sql, params });
  return await m.handler(sql, params);
}

const mockOps: Ops = {
  async dbQuery(name, sql, params) {
    const rows = await mockedDb(name, sql, params);
    if (!Array.isArray(rows)) throw new Error(`mock.db("${name}") returned ${rows} for a query; return rows (an array)`);
    return rows;
  },
  async dbExecute(name, sql, params) {
    const r = await mockedDb(name, sql, params);
    return typeof r === "number" ? r : r.length;
  },
  async fetch(request): Promise<RawResponse> {
    if (!fetchMock) {
      throw new Error(`fetch(${request.method} ${request.url}) is not mocked in this test: add mock.fetch({ status: 200, body: ... })`);
    }
    fetchMock.calls.push(request);
    const r = await fetchMock.handler(request);
    const headers: Record<string, string> = {};
    for (const [k, v] of Object.entries(r.headers ?? {})) headers[k.toLowerCase()] = v;
    let body = "";
    if (typeof r.body === "string") body = r.body;
    else if (r.body !== undefined) {
      body = JSON.stringify(r.body);
      headers["content-type"] ??= "application/json";
    }
    return { status: r.status ?? 200, headers, body };
  },
  variable(name) {
    if (!(name in envMock)) throw new Error(`env("${name}") is not mocked in this test: add mock.env({ ${name}: "..." })`);
    return envMock[name];
  },
  secret(name) {
    if (!(name in secretMock)) throw new Error(`secret("${name}") is not mocked in this test: add mock.secret({ ${name}: "..." })`);
    return secretMock[name];
  },
};

export interface TestResult {
  name: string;
  ok: boolean;
  error?: string;
  ms: number;
}

const tests: TestCase[] = [];
let fixtures: Record<string, string> = {};
let inputFormat: FormatSpec = { kind: "hl7" };

/** Register a test. */
export function test(name: string, fn: () => unknown): void {
  tests.push({ name, fn });
}

/** Contents of a file in the interface's `fixtures/` folder. */
export function fixture(name: string): string {
  const content = fixtures[name];
  if (content === undefined) {
    const available = Object.keys(fixtures).sort().join(", ") || "none";
    throw new Error(`fixture "${name}" not found (available: ${available})`);
  }
  return content;
}

/** A fixture parsed as an HL7 message. */
export function hl7(name: string): Message {
  return parse(fixture(name));
}

/**
 * A fixture parsed the way the engine parses this interface's input (its `format:`), ready
 * to pass to `transform`: `transform(input("orders.csv"))`.
 */
export function input<T = unknown>(name: string): T {
  return decode(fixture(name), inputFormat) as T;
}

class AssertionError extends Error {}

function show(value: unknown): string {
  if (typeof value === "string") return JSON.stringify(value);
  if (value === undefined) return "undefined";
  try {
    return JSON.stringify(value);
  } catch {
    return String(value);
  }
}

function deepEqual(a: unknown, b: unknown): boolean {
  if (Object.is(a, b)) return true;
  if (typeof a !== "object" || typeof b !== "object" || a === null || b === null) return false;
  if (Array.isArray(a) !== Array.isArray(b)) return false;
  const ka = Object.keys(a as object);
  const kb = Object.keys(b as object);
  if (ka.length !== kb.length) return false;
  return ka.every((k) => deepEqual((a as Record<string, unknown>)[k], (b as Record<string, unknown>)[k]));
}

export class Expectation {
  constructor(private readonly actual: unknown, private readonly negate = false) {}

  get not(): Expectation {
    return new Expectation(this.actual, !this.negate);
  }

  private check(pass: boolean, message: string): void {
    if (pass === this.negate) throw new AssertionError(this.negate ? `not: ${message}` : message);
  }

  toBe(expected: unknown): void {
    this.check(Object.is(this.actual, expected), `expected ${show(expected)}, got ${show(this.actual)}`);
  }

  toEqual(expected: unknown): void {
    this.check(deepEqual(this.actual, expected), `expected ${show(expected)}, got ${show(this.actual)}`);
  }

  toBeNull(): void {
    this.check(this.actual === null, `expected null, got ${show(this.actual)}`);
  }

  toBeTruthy(): void {
    this.check(Boolean(this.actual), `expected a truthy value, got ${show(this.actual)}`);
  }

  toContain(item: unknown): void {
    const a = this.actual;
    const pass = typeof a === "string" ? a.includes(String(item)) : Array.isArray(a) && a.includes(item);
    this.check(pass, `expected ${show(a)} to contain ${show(item)}`);
  }

  toThrow(messagePart?: string): void {
    if (typeof this.actual !== "function") throw new AssertionError("toThrow() needs a function");
    let thrown: unknown = undefined;
    let threw = false;
    let result: unknown;
    try {
      result = (this.actual as () => unknown)();
    } catch (e) {
      threw = true;
      thrown = e;
    }
    if (result instanceof Promise) {
      result.catch(() => {}); // reported below; don't leave it unhandled
      throw new AssertionError("the function is async: use `await expect(...).toReject()` instead of toThrow()");
    }
    const text = thrown instanceof Error ? thrown.message : String(thrown);
    const pass = threw && (messagePart === undefined || text.includes(messagePart));
    this.check(pass, threw ? `expected error containing ${show(messagePart)}, got ${show(text)}` : "expected function to throw");
  }

  /** For async code: `await expect(transform(msg)).toReject("unknown MRN")` (a promise or an async function). */
  async toReject(messagePart?: string): Promise<void> {
    const a = this.actual;
    let threw = false;
    let text = "";
    try {
      await (typeof a === "function" ? (a as () => unknown)() : a);
    } catch (e) {
      threw = true;
      text = e instanceof Error ? e.message : String(e);
    }
    const pass = threw && (messagePart === undefined || text.includes(messagePart));
    this.check(pass, threw ? `expected error containing ${show(messagePart)}, got ${show(text)}` : "expected it to reject");
  }
}

/** Assert on a value. HL7 Fields are compared by their text. */
export function expect(actual: unknown): Expectation {
  const value = actual !== null && typeof actual === "object" && typeof (actual as object).toString === "function" &&
      !Array.isArray(actual) && !(actual instanceof Promise) && Object.getPrototypeOf(actual) !== Object.prototype
    ? String(actual)
    : actual;
  return new Expectation(value);
}

export function __setFixtures(map: Record<string, string>, formats?: { input: FormatSpec }): void {
  fixtures = map;
  if (formats) inputFormat = formats.input;
  // Tests never reach real databases or APIs; unmocked calls fail with a hint.
  __useOps(mockOps);
}

/** Run every registered test, one at a time (async ones are awaited); returns a JSON report. */
export async function __runTests(): Promise<string> {
  const results: TestResult[] = [];
  for (const t of tests) {
    resetMocks();
    const start = Date.now();
    try {
      await t.fn();
      results.push({ name: t.name, ok: true, ms: Date.now() - start });
    } catch (e) {
      const error = e instanceof AssertionError ? e.message : e instanceof Error ? (e.stack ?? e.message) : String(e);
      results.push({ name: t.name, ok: false, error, ms: Date.now() - start });
    }
  }
  return JSON.stringify(results);
}
