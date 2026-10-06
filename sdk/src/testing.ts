// Built-in test framework: test(), expect(), fixture(). Run with `ductus test`.

import { type Message, parse } from "./hl7.ts";
import { decode, type FormatSpec } from "./formats.ts";

interface TestCase {
  name: string;
  fn: () => unknown;
}

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
    try {
      (this.actual as () => unknown)();
    } catch (e) {
      threw = true;
      thrown = e;
    }
    const text = thrown instanceof Error ? thrown.message : String(thrown);
    const pass = threw && (messagePart === undefined || text.includes(messagePart));
    this.check(pass, threw ? `expected error containing ${show(messagePart)}, got ${show(text)}` : "expected function to throw");
  }
}

/** Assert on a value. HL7 Fields are compared by their text. */
export function expect(actual: unknown): Expectation {
  const value = actual !== null && typeof actual === "object" && typeof (actual as object).toString === "function" &&
      !Array.isArray(actual) && Object.getPrototypeOf(actual) !== Object.prototype
    ? String(actual)
    : actual;
  return new Expectation(value);
}

export function __setFixtures(map: Record<string, string>, formats?: { input: FormatSpec }): void {
  fixtures = map;
  if (formats) inputFormat = formats.input;
}

/** Run every registered test; returns a JSON report. */
export function __runTests(): string {
  const results: TestResult[] = [];
  for (const t of tests) {
    const start = Date.now();
    try {
      const r = t.fn();
      if (r instanceof Promise) throw new Error("async tests are not supported yet");
      results.push({ name: t.name, ok: true, ms: Date.now() - start });
    } catch (e) {
      const error = e instanceof AssertionError ? e.message : e instanceof Error ? (e.stack ?? e.message) : String(e);
      results.push({ name: t.name, ok: false, error, ms: Date.now() - start });
    }
  }
  return JSON.stringify(results);
}
