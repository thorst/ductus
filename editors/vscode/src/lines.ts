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

/** An engine named in the setting `ductus.engines`. */
export interface NamedEngine {
  name: string;
  /** Its REST API, without a trailing `/`: `https://ductus-test:7700`. */
  url: string;
}

/**
 * The setting `ductus.engines` (`{ "test": "https://ductus-test:7700" }`) as engines in the order
 * written, and what's wrong with any entry left out (said once in the output, never thrown).
 */
export function parseEngines(value: unknown): { engines: NamedEngine[]; problems: string[] } {
  const engines: NamedEngine[] = [];
  const problems: string[] = [];
  if (value === undefined || value === null) return { engines, problems };
  if (typeof value !== "object" || Array.isArray(value)) {
    return {
      engines,
      problems: ['`ductus.engines` is a map of names to URLs: { "test": "https://ductus-test:7700" }'],
    };
  }
  for (const [name, url] of Object.entries(value as Record<string, unknown>)) {
    if (typeof url !== "string" || !/^https?:\/\/[^/\s]+/.test(url.trim())) {
      problems.push(
        `\`ductus.engines\`: ${name}'s URL isn't one (http:// or https:// and the host): ${JSON.stringify(url)}`,
      );
      continue;
    }
    engines.push({ name, url: url.trim().replace(/\/+$/, "") });
  }
  return { engines, problems };
}

/** Who a token is, as `GET /session` answers. */
export interface Session {
  kind: string;
  name: string;
  /** `viewer`, `operator` or `admin`. */
  role: string;
  permissions: string[];
}

export function parseSession(body: string): Session | undefined {
  try {
    const value = JSON.parse(body);
    return value && typeof value.name === "string" && typeof value.role === "string" && Array.isArray(value.permissions)
      ? (value as Session)
      : undefined;
  } catch {
    return undefined;
  }
}

/**
 * What a token is for. A promotion: `deploy` on the engine it goes to, `download` on the engine it
 * comes from (an operator or admin with `deploy` may download too). Debug this message: `payloads`
 * (reading the message). The History view: `read` (any token). docs/reference/api: ACCESS.
 */
export type Need = "deploy" | "download" | "payloads" | "read";

/** Whether `session` may do `need`; when not, what to give the token (undefined: it may). */
export function lacks(session: Session, need: Need): string | undefined {
  if (need === "read") return undefined;
  if (need === "payloads") {
    return session.permissions.includes("payloads")
      ? undefined
      : `${session.name} may not read payloads: debugging a stored message needs the \`payloads\` permission`;
  }
  const deploys = session.role !== "viewer" && session.permissions.includes("deploy");
  if (need === "deploy" && !deploys) {
    return session.role === "viewer"
      ? `${session.name} is a viewer's token: deploying needs an operator's with the \`deploy\` permission`
      : `${session.name} lacks the \`deploy\` permission`;
  }
  if (need === "download" && !deploys && !session.permissions.includes("download")) {
    return `${session.name} can't fetch releases: it needs the \`download\` permission (or an operator's \`deploy\`)`;
  }
  return undefined;
}

/** What `GET /engine` says that a promotion reads. */
export interface EngineInfo {
  version: string;
  environment: string;
  /** Release mode; null in folder mode (deploys are refused there). */
  releases?: { project?: string | null; running?: string | null; requires?: string[] } | null;
}

export function parseEngineInfo(body: string): EngineInfo | undefined {
  try {
    const value = JSON.parse(body);
    return value && typeof value.environment === "string" ? (value as EngineInfo) : undefined;
  } catch {
    return undefined;
  }
}

/** One release an engine has stored, as `GET /releases` lists it. */
export interface StoredRelease {
  id: string;
  short: string;
  project: string;
  made_at?: string | null;
  made_by?: string;
  reason?: string | null;
  branches?: string[];
  environments?: string[];
  running?: boolean;
  composed_from?: string;
}

export function parseReleases(body: string): StoredRelease[] | undefined {
  try {
    const value = JSON.parse(body);
    return Array.isArray(value)
      ? value.filter((r) => r && typeof r.id === "string" && typeof r.short === "string")
      : undefined;
  } catch {
    return undefined;
  }
}

/** A release file `ductus release` wrote: `<project>-<first 12 of its id>.tar.gz`. */
export function releaseFile(name: string): { project: string; short: string } | undefined {
  const m = /^(.+)-([0-9a-f]{12})\.tar\.gz$/.exec(name);
  return m ? { project: m[1], short: m[2] } : undefined;
}

/** A promotion: what goes where, as `ductus deploy` takes it. */
export interface Promotion {
  /** A release file's path, or a stored release's id. */
  release: string;
  /** The engine to fetch the id from (`--from`); none: a file, or a release the target has. */
  from?: string;
  to: string;
  reason: string;
}

/** `ductus deploy`'s arguments (tokens go in its environment, never here). */
export function deployArgs(p: Promotion): string[] {
  const args = ["deploy", p.release, "--to", p.to];
  if (p.from) args.push("--from", p.from);
  if (p.reason.trim()) args.push("-m", p.reason.trim());
  args.push("--json");
  return args;
}

/** A finding of the gate: `ductus release check`'s report, as a deploy carries it. */
export interface GateFinding {
  level: string;
  text: string;
  more?: string[];
}

/**
 * What `ductus deploy --json` prints (`POST /deployments`' answer): docs/reference's
 * `DeployReport`. Fields are only ever added; only those shown here are read.
 */
export interface DeployReport {
  ok: boolean;
  outcome: string;
  short?: string;
  project?: string;
  environment?: string;
  rollback?: boolean;
  forced?: boolean;
  applied?: string;
  branches?: string[];
  rules?: string[];
  replaces?: Record<string, string>;
  gate?: {
    reused?: boolean;
    why_not_reused?: string;
    check?: {
      id?: number;
      at?: string;
      by?: string;
      ok?: boolean;
      seconds?: number;
      report?: { checks?: GateFinding[] };
    };
  } | null;
  plan?: { changes?: { interface: string; what?: string[]; restarts?: string | null }[]; project?: string[] } | null;
  steps?: string[];
  changes?: { interface: string; from: string | null; to: string }[];
}

export function parseDeploy(stdout: string): DeployReport | undefined {
  try {
    const value = JSON.parse(stdout);
    return value && typeof value.ok === "boolean" && typeof value.outcome === "string"
      ? (value as DeployReport)
      : undefined;
  } catch {
    return undefined;
  }
}

/** The gate's problems and warnings, the most important first. */
function gateFindings(report: DeployReport): GateFinding[] {
  const found = (report.gate?.check?.report?.checks ?? []).filter(
    (c) => c.level === "problem" || c.level === "warning",
  );
  return [...found.filter((c) => c.level === "problem"), ...found.filter((c) => c.level === "warning")];
}

/**
 * A deploy's report in one line for a notification: what changed, or the first reason it was
 * refused (a rule, someone else's work, the gate's first problem). The whole report goes to the
 * output (`deployLines`).
 */
export function deploySummary(report: DeployReport): string {
  if (!report.ok) {
    const why =
      report.rules?.[0] ??
      Object.entries(report.replaces ?? {}).map(([part, w]) => `it would undo ${part}: ${w}`)[0] ??
      gateFindings(report).find((c) => c.level === "problem")?.text;
    return why ?? report.outcome.split("\n")[0];
  }
  const changed = [
    ...(report.changes ?? []).map((c) => (c.from === null ? `${c.interface} (new)` : c.interface)),
    ...(report.plan?.changes ?? []).filter((c) => c.what?.[0] === "removed").map((c) => `${c.interface} (removed)`),
  ];
  const how = report.applied === "in_place" ? "in place" : report.applied ? report.applied.replace(/_/g, " ") : "";
  if (changed.length === 0) return report.outcome.split("\n")[0];
  return `${changed.join(", ")}${how ? ` (${how})` : ""}`;
}

/** A deploy's whole report as lines for the output: what `ductus deploy` prints, in short. */
export function deployLines(report: DeployReport): string[] {
  const lines: string[] = [];
  const what = report.rollback ? "rollback to release" : "release";
  const branches = report.branches?.length ? `, branch ${report.branches.join(", ")}` : "";
  lines.push(
    `${what} ${report.short ?? "?"} (project ${report.project ?? "?"}${branches}) to \`${report.environment ?? "?"}\``,
  );
  for (const rule of report.rules ?? []) lines.push(`${report.rollback ? "·" : "✗"} ${rule}`);
  for (const [part, why] of Object.entries(report.replaces ?? {})) {
    const mark = report.rollback ? "⚠ undoes" : report.forced ? "⚠ deployed over" : "✗ would undo";
    lines.push(`${mark} ${part}: ${why}`);
  }
  const gate = report.gate;
  if (gate) {
    const check = gate.check ?? {};
    const result = check.ok ? "passed" : "found problems";
    if (gate.reused) {
      lines.push(`gate: reused check #${check.id ?? "?"} (${check.at ?? "?"} by ${check.by ?? "?"}: ${result})`);
    } else {
      const why = gate.why_not_reused ? ` (${gate.why_not_reused})` : "";
      lines.push(`gate: ${result}${check.seconds === undefined ? "" : ` in ${check.seconds} s`}${why}`);
      for (const c of gateFindings(report)) {
        lines.push(`  ${c.level === "problem" ? "✗" : "⚠"} ${c.text}`);
        for (const more of (c.more ?? []).slice(0, 10)) lines.push(`      ${more}`);
      }
    }
  }
  for (const c of report.plan?.changes ?? []) {
    const how = c.restarts ? `in place, restarting alone: ${c.restarts}` : "in place";
    lines.push(`· ${c.interface}: ${(c.what ?? []).join(", ")} (${how})`);
  }
  for (const line of report.plan?.project ?? []) lines.push(`· ${line}`);
  for (const step of report.steps ?? []) lines.push(`  ${step}`);
  lines.push(`${report.ok ? "✓" : "✗"} ${report.outcome}`);
  for (const c of report.changes ?? []) lines.push(`  ${c.interface}: ${c.from ?? "new here"} → ${c.to}`);
  return lines;
}

/** What a failed command said on stderr, without anyhow's `Error: ` and `Caused by:` list. */
export function firstParagraph(stderr: string): string {
  return stderr
    .trim()
    .replace(/^Error: /, "")
    .split(/\n\s*\n/)[0]
    .trim();
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

/**
 * One entry of an interface's history, as `GET /interfaces/{name}/history` and `ductus history
 * --json` give it (docs/reference's `HistoryEntry`). Fields are only ever added; those read here.
 */
export interface HistoryEntry {
  at: string;
  kind: "commit" | "audit" | "documentation" | "deployment";
  by: string;
  what: string;
  key: string;
  touches?: { everything?: boolean; source?: boolean; destinations?: string[]; unsure?: boolean };
  commit?: { id: string; email?: string; summary?: string; files?: { path: string; change: string }[] };
  deployment?: {
    id: number;
    how: string;
    reason?: string | null;
    host?: string;
    environment?: string;
    version?: string;
    git_commit?: string | null;
    uncommitted?: string[] | null;
    from?: string | null;
    to?: string | null;
    warning?: string | null;
    variables_changed?: string[];
  };
  audit?: { id: number; action: string; outcome: string; subject?: string | null; message?: number | null };
  documentation?: {
    removed?: boolean;
    fields?: { field: string; before?: unknown; after?: unknown }[];
  };
}

/** A page of the history: newest first, `next_before` for the older ones. */
export interface HistoryPage {
  interface: string;
  entries: HistoryEntry[];
  next_before?: string | null;
  notes: string[];
}

export function parseHistory(text: string): HistoryPage | undefined {
  try {
    const value = JSON.parse(text);
    if (!value || typeof value.interface !== "string" || !Array.isArray(value.entries)) return undefined;
    return {
      interface: value.interface,
      entries: value.entries.filter(
        (e: HistoryEntry) => e && typeof e.at === "string" && typeof e.what === "string" && typeof e.key === "string",
      ),
      next_before: typeof value.next_before === "string" ? value.next_before : null,
      notes: Array.isArray(value.notes) ? value.notes.filter((n: unknown) => typeof n === "string") : [],
    };
  } catch {
    return undefined;
  }
}

/**
 * `ductus history`'s arguments for a page: the folder's own history (git, and the database too when
 * `DUCTUS_DATABASE_URL` is set where VS Code runs).
 */
export function historyArgs(iface: string, environment: string, limit: number, before?: string): string[] {
  const args = ["history", iface, "--json", "--env", environment, "--limit", String(limit)];
  if (before) args.push("--before", before);
  return args;
}

/** An engine's history path (under `/api/v1`) for a page. */
export function historyPath(iface: string, limit: number, before?: string): string {
  const query = new URLSearchParams({ limit: String(limit) });
  if (before) query.set("before", before);
  return `/interfaces/${encodeURIComponent(iface)}/history?${query}`;
}

/** A time as the History view shows it: the local date and minute (`2026-11-20 14:02`). */
export function shortTime(at: string): string {
  const d = new Date(at);
  if (Number.isNaN(d.getTime())) return at;
  const two = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${two(d.getMonth() + 1)}-${two(d.getDate())} ${two(d.getHours())}:${two(d.getMinutes())}`;
}

/** The parts an entry touches, as `ductus history` ends its lines (`source, lab`); "" for the whole. */
export function touched(entry: HistoryEntry): string {
  const t = entry.touches;
  if (!t || t.everything) return "";
  const parts = [...(t.source ? ["source"] : []), ...(t.destinations ?? [])];
  return parts.length > 0 ? `${parts.join(", ")}${t.unsure ? "?" : ""}` : "";
}

/** A documentation field's value before or after, in one short line. */
function shown(value: unknown): string {
  if (value === undefined || value === null) return "(none)";
  const text = typeof value === "string" ? value : JSON.stringify(value);
  return text.length > 80 ? `${text.slice(0, 79)}…` : text;
}

/** An entry's details for its tooltip, a line each (what `ductus history` shows of it). */
export function entryDetail(entry: HistoryEntry): string[] {
  const lines = [`${shortTime(entry.at)} · ${entry.by}`];
  const parts = touched(entry);
  if (parts) lines.push(`touches ${parts}`);
  const c = entry.commit;
  if (c) {
    lines.push(`commit ${c.id.slice(0, 12)}${c.email ? ` <${c.email}>` : ""}`);
    if (c.summary && c.summary !== entry.what) lines.push(c.summary);
    const mark: Record<string, string> = { added: "A", modified: "M", deleted: "D" };
    for (const f of c.files ?? []) lines.push(`${mark[f.change] ?? "?"} ${f.path}`);
  }
  const d = entry.deployment;
  if (d) {
    const where = [d.host, d.environment && `environment ${d.environment}`, d.version && `ductus ${d.version}`]
      .filter(Boolean)
      .join(", ");
    lines.push(`${d.how} #${d.id}${where ? ` (${where})` : ""}`);
    if (d.reason) lines.push(`why: ${d.reason}`);
    if (d.to) lines.push(`code ${d.from ?? "new here"} → ${d.to}`);
    for (const v of d.variables_changed ?? []) lines.push(`variable: ${v}`);
    if (d.git_commit) lines.push(`from commit ${d.git_commit.slice(0, 12)}`);
    if (d.uncommitted?.length) lines.push(`uncommitted: ${d.uncommitted.join(", ")}`);
    if (d.warning) lines.push(`⚠ ${d.warning}`);
  }
  const a = entry.audit;
  if (a) {
    lines.push(`${a.action} (${a.outcome})${a.subject ? `: ${a.subject}` : ""}`);
    if (a.message !== undefined && a.message !== null) lines.push(`message ${a.message}`);
  }
  const doc = entry.documentation;
  if (doc) {
    if (doc.removed) lines.push("the interface left the project");
    for (const f of doc.fields ?? []) lines.push(`${f.field}: ${shown(f.before)} → ${shown(f.after)}`);
  }
  return lines;
}

/** What the web UI's **Debug in VS Code** link (or the command) asks for. */
export interface DebugLink {
  /** The engine's URL, as the browser reached it (no trailing `/`). */
  engine: string;
  interface: string;
  message: number;
}

/**
 * The query of a `vscode://<extension id>/debug-message?engine=…&interface=…&message=…` link, or
 * why it isn't one (said to the person: links come from web pages).
 */
export function parseDebugLink(query: string): DebugLink | { why: string } {
  const q = new URLSearchParams(query);
  const engine = (q.get("engine") ?? "").trim().replace(/\/+$/, "");
  const iface = (q.get("interface") ?? "").trim();
  const id = (q.get("message") ?? "").trim();
  if (!/^https?:\/\/[^/\s]+$/.test(engine))
    return { why: "the link names no engine (http:// or https:// and the host)" };
  if (!/^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(iface)) return { why: "the link names no interface" };
  if (!/^[1-9][0-9]{0,15}$/.test(id)) return { why: "the link names no message id" };
  return { engine, interface: iface, message: Number(id) };
}

/**
 * Whether two engine URLs are the same engine: scheme, host and port alike, a trailing `/` aside,
 * with `localhost`, `127.0.0.1` and `[::1]` one host (a browser on `localhost:7700` and `ductus
 * dev`'s `127.0.0.1:7700`).
 */
export function sameEngine(a: string, b: string): boolean {
  const norm = (u: string) => {
    try {
      const url = new URL(u.trim());
      const local = ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname);
      const port = url.port || (url.protocol === "https:" ? "443" : "80");
      const host = local ? "127.0.0.1" : url.hostname.toLowerCase();
      return `${url.protocol}//${host}:${port}${url.pathname.replace(/\/+$/, "")}`;
    } catch {
      return u.trim().replace(/\/+$/, "");
    }
  };
  return norm(a) === norm(b);
}

/**
 * `ductus try`'s arguments to debug a stored message read from an engine (the token goes in
 * `DUCTUS_API_TOKEN`, never here).
 */
export function tryMessageArgs(link: DebugLink, environment: string, inspect: string): string[] {
  return [
    "try",
    link.interface,
    "--message",
    String(link.message),
    "--from",
    link.engine,
    `--inspect=${inspect}`,
    "--env",
    environment,
  ];
}
