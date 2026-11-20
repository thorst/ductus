// What the extension reads from the `ductus` command line, and what it asks it to run: nothing
// here touches VS Code, so the unit tests (`test/unit`) run it under plain Node.

/** A place in a file: the file relative to the project with `/`, line and column from 1. */
export interface Place {
  file: string;
  line: number;
  column: number;
}

/**
 * One line of `ductus test --json` (docs/write/testing.md, "For editors and CI: JSON"). Fields are
 * only ever added there, so unknown ones are ignored.
 */
export type TestLine =
  | {
      type: "test";
      interface: string;
      name: string;
      file?: string;
      line?: number;
      column?: number;
      output?: string;
      ok?: boolean;
      ms?: number;
      error?: string;
      failure?: Place;
      expected?: string;
      actual?: string;
      notRun?: string;
    }
  | { type: "output"; interface: string; output: string }
  | { type: "load"; interface: string; error: string; output?: string }
  | { type: "summary"; passed?: number; failed?: number; ms?: number; notRun?: number; tests?: number };

const TYPES = new Set(["test", "output", "load", "summary"]);

/** A line of `test --json`, or undefined for anything else (a line that isn't ours). */
export function parseTestLine(text: string): TestLine | undefined {
  if (!text.startsWith("{")) return undefined;
  try {
    const value = JSON.parse(text);
    return value && typeof value === "object" && TYPES.has(value.type) ? (value as TestLine) : undefined;
  } catch {
    return undefined;
  }
}

/** Splits a stream into lines as chunks arrive (a line can span chunks; `\r\n` too). */
export class LineBuffer {
  private rest = "";

  push(chunk: string): string[] {
    const lines = (this.rest + chunk).split("\n");
    this.rest = lines.pop() ?? "";
    return lines.map((l) => l.replace(/\r$/, ""));
  }

  /** What's left once the stream has ended (a last line without `\n`). */
  end(): string[] {
    const last = this.rest.replace(/\r$/, "");
    this.rest = "";
    return last ? [last] : [];
  }
}

/** One finding of `ductus validate --json`. */
export interface Finding {
  severity: "error" | "warning" | "note";
  code: string;
  source: "typescript" | "ductus";
  message: string;
  file?: string;
  line?: number;
  column?: number;
  end_line?: number;
  end_column?: number;
  interface?: string;
  hint?: string;
}

export interface ValidateDocument {
  environment: string;
  valid: boolean;
  errors: number;
  warnings: number;
  findings: Finding[];
}

/** `validate --json`'s document, or undefined when the output isn't one (it didn't run). */
export function parseValidate(stdout: string): ValidateDocument | undefined {
  try {
    const value = JSON.parse(stdout);
    return value && Array.isArray(value.findings) ? (value as ValidateDocument) : undefined;
  } catch {
    return undefined;
  }
}

/** Where a finding about no one file goes in the Problems view. */
export const PROJECT_FILE = "interfaces";

/**
 * The findings the Problems view shows, by file: only Ductus's own (VS Code's TypeScript server
 * already shows TypeScript's, and twice would be noise), and not the notes about no file (what
 * wasn't checked, such as the database: said on every run, nothing to do in the editor). A finding
 * about no file that is an error or warning goes on the project's `interfaces` folder.
 */
export function problemsByFile(document: ValidateDocument): Map<string, Finding[]> {
  const byFile = new Map<string, Finding[]>();
  for (const finding of document.findings) {
    if (finding.source !== "ductus") continue;
    if (!finding.file && finding.severity === "note") continue;
    const file = finding.file ?? PROJECT_FILE;
    byFile.set(file, [...(byFile.get(file) ?? []), finding]);
  }
  return byFile;
}

/** A project's running `ductus dev`, as it wrote `.ductus/dev-engine.json` when it started. */
export interface DevEngine {
  /** Its API: `http://127.0.0.1:7700`. */
  api: string;
  pid: number;
}

/** `.ductus/dev-engine.json`'s text, or undefined when it isn't one. */
export function parseDevEngine(text: string): DevEngine | undefined {
  try {
    const value = JSON.parse(text);
    return value && typeof value.api === "string" && typeof value.pid === "number"
      ? { api: value.api.replace(/\/+$/, ""), pid: value.pid }
      : undefined;
  } catch {
    return undefined;
  }
}

/**
 * What `POST /engine/reload` answers (and `ductus reload --json` prints): docs/reference's
 * `ReloadReport`. Fields are only ever added.
 */
export interface ReloadReport {
  ok: boolean;
  /** `in_place`, `nothing`, `handover` or `refused`. */
  applied: string;
  outcome: string;
  steps?: string[];
  changes?: { interface: string; from: string | null; to: string }[];
  warnings?: Record<string, string>;
  took_ms?: number;
}

/** The answer's body as a reload's report, or undefined (an error body: `{ "error": … }`). */
export function parseReload(body: string): ReloadReport | undefined {
  try {
    const value = JSON.parse(body);
    return value && typeof value.ok === "boolean" && typeof value.outcome === "string"
      ? (value as ReloadReport)
      : undefined;
  } catch {
    return undefined;
  }
}

/**
 * A reload's report in one line for a notification: which interfaces took the change, or why
 * nothing changed. The whole report (each step, each warning) goes to the output.
 */
export function reloadSummary(report: ReloadReport): string {
  if (!report.ok) return report.outcome.split("\n")[0].replace(/^refused: /, "");
  if (report.applied === "nothing") return "nothing to deploy: dev runs the folder as it is";
  const changed = (report.changes ?? []).map((c) => (c.from === null ? `${c.interface} (new)` : c.interface));
  const took = report.took_ms === undefined ? "" : ` in ${report.took_ms} ms`;
  const what = changed.length > 0 ? changed.join(", ") : report.outcome.split("\n")[0];
  const warned = Object.keys(report.warnings ?? {}).length;
  return `${what}${took}${warned > 0 ? ` (⚠ ${warned} over someone else's change: see the output)` : ""}`;
}

/** `ductus 0.7.0-dev` → [0, 7, 0]; undefined when it isn't a version line. */
export function parseVersion(text: string): [number, number, number] | undefined {
  const m = /(\d+)\.(\d+)\.(\d+)/.exec(text);
  return m ? [Number(m[1]), Number(m[2]), Number(m[3])] : undefined;
}

/**
 * Whether `version` is `minimum` or newer. A pre-release counts as its release (0.7.0-dev is
 * 0.7.0): a developer's own build of the version the extension needs has what it needs.
 */
export function atLeast(version: [number, number, number], minimum: [number, number, number]): boolean {
  for (let i = 0; i < 3; i++) {
    if (version[i] !== minimum[i]) return version[i] > minimum[i];
  }
  return true;
}

/** What the Test Explorer asked to run, as places in the tree. */
export type Chosen =
  { root: string } | { root: string; interface: string } | { root: string; interface: string; name: string };

/** One `ductus test` to run: the whole project, some interfaces whole, or some of one's tests. */
export interface Invocation {
  root: string;
  interfaces: string[];
  tests: string[];
}

/**
 * The fewest `ductus test` runs for a choice. `known` is every test the tree has, by project and
 * interface, so a choice with some tests left out (`excluded`) becomes those it keeps. A debugged
 * run is one interface each (`test --inspect` runs one interface); otherwise a whole project is one
 * run, interfaces chosen whole one run together, and each interface with only some of its tests
 * chosen a run of its own (`--test` names a test in every interface given).
 */
export function plan(
  chosen: Chosen[],
  excluded: Chosen[],
  known: Map<string, Map<string, string[]>>,
  debug: boolean,
): Invocation[] {
  const key = (c: Chosen) =>
    JSON.stringify([c.root, "interface" in c ? c.interface : null, "name" in c ? c.name : null]);
  const out = new Set(excluded.map(key));
  const isOut = (c: Chosen) => out.has(key(c));
  const covers = (c: Chosen, e: Chosen) =>
    e.root === c.root && (!("interface" in c) || ("interface" in e && e.interface === c.interface));
  const exclusionUnder = (c: Chosen) => excluded.some((e) => covers(c, e));

  // Per project: whole, or interfaces whole, or some tests per interface.
  const projects = new Map<string, { whole: boolean; interfaces: Map<string, Set<string> | "whole"> }>();
  const project = (root: string) => {
    let p = projects.get(root);
    if (!p) projects.set(root, (p = { whole: false, interfaces: new Map() }));
    return p;
  };
  const addTests = (root: string, iface: string, names: string[]) => {
    const p = project(root);
    const had = p.interfaces.get(iface);
    if (had === "whole") return;
    const set = had ?? new Set<string>();
    for (const n of names) if (!isOut({ root, interface: iface, name: n })) set.add(n);
    if (set.size > 0) p.interfaces.set(iface, set);
  };
  const addInterface = (root: string, iface: string) => {
    if (isOut({ root, interface: iface })) return;
    if (exclusionUnder({ root, interface: iface })) {
      addTests(root, iface, known.get(root)?.get(iface) ?? []);
    } else {
      project(root).interfaces.set(iface, "whole");
    }
  };

  for (const c of chosen) {
    if (isOut(c)) continue;
    if ("name" in c) addTests(c.root, c.interface, [c.name]);
    else if ("interface" in c) addInterface(c.root, c.interface);
    else if (!debug && !exclusionUnder(c)) project(c.root).whole = true;
    else for (const iface of known.get(c.root)?.keys() ?? []) addInterface(c.root, iface);
  }

  const runs: Invocation[] = [];
  for (const [root, p] of projects) {
    if (p.whole) {
      runs.push({ root, interfaces: [], tests: [] });
      continue;
    }
    const whole = [...p.interfaces].filter(([, v]) => v === "whole").map(([k]) => k);
    if (debug) whole.forEach((i) => runs.push({ root, interfaces: [i], tests: [] }));
    else if (whole.length > 0) runs.push({ root, interfaces: whole, tests: [] });
    for (const [iface, names] of p.interfaces) {
      if (names !== "whole") runs.push({ root, interfaces: [iface], tests: [...names] });
    }
  }
  return runs;
}

/** `ductus test`'s arguments for an invocation (with `--json`; `--inspect` when debugged). */
export function testArgs(run: Invocation, environment: string, inspect?: string): string[] {
  const args = ["test", "--json", "--env", environment];
  if (inspect) args.push(`--inspect=${inspect}`);
  for (const i of run.interfaces) args.push("--interface", i);
  for (const t of run.tests) args.push("--test", t);
  return args;
}
