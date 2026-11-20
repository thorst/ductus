import * as net from "node:net";
import * as path from "node:path";
import * as vscode from "vscode";
import type { Finder } from "./binary";
import { type Chosen, type Invocation, type TestLine, parseTestLine, plan, testArgs } from "./lines";
import { run } from "./process";
import type { Projects } from "./projects";
import { environment } from "./settings";

type Data =
  | { kind: "project"; root: string }
  | { kind: "interface"; root: string; interface: string }
  | { kind: "test"; root: string; interface: string; name: string };

/** What a test's last run gave, kept for the extension's own tests (`api`). */
export interface Result {
  state: "passed" | "failed" | "errored" | "skipped";
  message?: string;
  expected?: string;
  actual?: string;
  location?: vscode.Location;
}

const id = {
  // JSON: any name can be in it (VS Code keeps \0 for its own ids' separator).
  project: (root: string) => JSON.stringify(["p", root]),
  interface: (root: string, iface: string) => JSON.stringify(["i", root, iface]),
  test: (root: string, iface: string, name: string) => JSON.stringify(["t", root, iface, name]),
};

/**
 * The Test Explorer: each project's tests found by `ductus test --list --json` (by interface, at
 * their `test(...)` line), run per test, interface or project with `--json` (each result shown as
 * its test ends), and debugged with `--inspect` and VS Code's JavaScript debugger attached.
 */
export class Tests implements vscode.Disposable {
  readonly controller = vscode.tests.createTestController("ductus", "Ductus");
  private readonly data = new WeakMap<vscode.TestItem, Data>();
  private readonly discovered = new Set<string>();
  private grouped = false;
  private readonly pending = new Map<string, ReturnType<typeof setTimeout>>();
  private readonly disposables: vscode.Disposable[] = [];
  /** The last result of each test, by item id (for the extension's tests). */
  readonly results = new Map<string, Result>();

  constructor(
    private readonly binary: Finder,
    private readonly projects: Projects,
    private readonly log: vscode.LogOutputChannel,
  ) {
    const c = this.controller;
    c.resolveHandler = async (item) => {
      if (!item) await this.discoverAll();
    };
    c.refreshHandler = () => this.discoverAll(true);
    c.createRunProfile("Run", vscode.TestRunProfileKind.Run, (r, t) => this.run(r, t, false), true);
    c.createRunProfile("Debug", vscode.TestRunProfileKind.Debug, (r, t) => this.run(r, t, true), true);
    this.disposables.push(
      c,
      projects.onDidChange(() => void this.discoverAll()),
      // A test file or an interface.yaml saved: its project's tests again (a test added, renamed).
      vscode.workspace.onDidSaveTextDocument((d) => {
        if (!/\.test\.ts$|interface\.yaml$/.test(d.uri.fsPath)) return;
        const root = projects.rootOf(d.uri.fsPath);
        if (root && this.discovered.has(root)) this.later(root);
      }),
      // A test file opened: its project's tests, for the run buttons beside each test.
      vscode.workspace.onDidOpenTextDocument((d) => this.opened(d)),
    );
    vscode.workspace.textDocuments.forEach((d) => this.opened(d));
  }

  dispose() {
    this.pending.forEach((t) => clearTimeout(t));
    this.disposables.forEach((d) => d.dispose());
  }

  private opened(document: vscode.TextDocument) {
    if (!document.uri.fsPath.endsWith(".test.ts")) return;
    const root = this.projects.rootOf(document.uri.fsPath);
    if (root && !this.discovered.has(root)) void this.discover(root);
  }

  private later(root: string) {
    clearTimeout(this.pending.get(root));
    this.pending.set(
      root,
      setTimeout(() => {
        this.pending.delete(root);
        void this.discover(root);
      }, 300),
    );
  }

  async discoverAll(force = false): Promise<void> {
    const roots = this.projects.roots;
    // A second project in the workspace puts each under an item of its own (and the other way).
    const grouped = roots.length >= 2;
    if (grouped !== this.grouped) {
      this.grouped = grouped;
      this.controller.items.replace([]);
      this.discovered.clear();
    }
    // Projects no longer there leave the tree.
    const keep = new Set(roots);
    for (const root of [...this.discovered]) {
      if (!keep.has(root)) {
        this.discovered.delete(root);
        this.forget(root);
      }
    }
    await Promise.all(roots.filter((r) => force || !this.discovered.has(r)).map((r) => this.discover(r)));
  }

  /** One project's tests, from `ductus test --list --json`. */
  async discover(root: string): Promise<void> {
    const binary = await this.binary.get();
    if (!binary) return;
    this.discovered.add(root);
    const seen = new Map<string, Set<string>>();
    const failed = new Map<string, string>();
    const ended = await run(binary.path, ["test", "--list", "--json", "--env", environment()], root, {
      line: (text) => {
        const line = parseTestLine(text);
        if (line?.type === "test") {
          this.testItem(root, line);
          seen.set(line.interface, (seen.get(line.interface) ?? new Set()).add(line.name));
        } else if (line?.type === "load") {
          this.interfaceItem(root, line.interface);
          failed.set(line.interface, line.error);
          seen.set(line.interface, seen.get(line.interface) ?? new Set());
        }
      },
    });
    this.log.info(`found ${[...seen.values()].reduce((n, s) => n + s.size, 0)} tests in ${root} (${ended.ms} ms)`);
    if (ended.error || (ended.code !== 0 && ended.code !== 1)) {
      const why = ended.error?.message ?? ended.stderr.trim();
      this.log.error(`listing the tests in ${root} failed: ${why}`);
      const project = this.parentFor(root);
      if (project) project.error = why;
      return;
    }
    if (ended.code === 1 && seen.size === 0 && ended.stderr.trim()) {
      // The project itself doesn't load (an interface folder ductus can't read): say so on it.
      this.log.error(`listing the tests in ${root}: ${ended.stderr.trim()}`);
    }
    // Interfaces and tests no longer there leave the tree; a load error shows on its interface.
    const parent = this.parentFor(root);
    const interfaces = parent ? parent.children : this.controller.items;
    interfaces.forEach((item) => {
      const d = this.data.get(item);
      if (d?.kind !== "interface" || d.root !== root) return;
      const names = seen.get(d.interface);
      if (!names) return interfaces.delete(item.id);
      item.error = failed.get(d.interface);
      item.children.forEach((t) => {
        const td = this.data.get(t);
        if (td?.kind === "test" && !names.has(td.name)) item.children.delete(t.id);
      });
    });
  }

  private forget(root: string) {
    this.controller.items.delete(id.project(root));
    this.controller.items.forEach((item) => {
      if (this.data.get(item)?.root === root) this.controller.items.delete(item.id);
    });
  }

  /**
   * Where a project's interfaces go: under an item for the project when the workspace has more
   * than one, else at the top.
   */
  private parentFor(root: string): vscode.TestItem | undefined {
    if (this.projects.roots.length < 2) return undefined;
    let item = this.controller.items.get(id.project(root));
    if (!item) {
      const label = vscode.workspace.asRelativePath(root, true) || path.basename(root);
      item = this.controller.createTestItem(id.project(root), label, vscode.Uri.file(root));
      this.data.set(item, { kind: "project", root });
      this.controller.items.add(item);
    }
    return item;
  }

  /** An interface's item (its name is the one in interface.yaml, which `--interface` takes). */
  private interfaceItem(root: string, iface: string): vscode.TestItem {
    const parent = this.parentFor(root);
    const siblings = parent ? parent.children : this.controller.items;
    let item = siblings.get(id.interface(root, iface));
    if (!item) {
      item = this.controller.createTestItem(id.interface(root, iface), iface);
      this.data.set(item, { kind: "interface", root, interface: iface });
      siblings.add(item);
    }
    return item;
  }

  private testItem(root: string, line: Extract<TestLine, { type: "test" }>): vscode.TestItem {
    const file = line.file ? path.join(root, line.file) : undefined;
    const iface = this.interfaceItem(root, line.interface);
    const tid = id.test(root, line.interface, line.name);
    let item = iface.children.get(tid);
    if (!item || (file && item.uri?.fsPath !== file)) {
      item = this.controller.createTestItem(tid, line.name, file ? vscode.Uri.file(file) : undefined);
      this.data.set(item, { kind: "test", root, interface: line.interface, name: line.name });
      iface.children.add(item);
    }
    if (line.line) item.range = new vscode.Range(line.line - 1, (line.column ?? 1) - 1, line.line - 1, 1000);
    return item;
  }

  private chosen(item: vscode.TestItem): Chosen | undefined {
    const d = this.data.get(item);
    if (!d) return undefined;
    if (d.kind === "project") return { root: d.root };
    if (d.kind === "interface") return { root: d.root, interface: d.interface };
    return { root: d.root, interface: d.interface, name: d.name };
  }

  /** Every test the tree knows, by project and interface (to turn exclusions into choices). */
  private known(): Map<string, Map<string, string[]>> {
    const known = new Map<string, Map<string, string[]>>();
    const walk = (items: vscode.TestItemCollection) =>
      items.forEach((item) => {
        const d = this.data.get(item);
        if (d?.kind === "test") {
          const byInterface = known.get(d.root) ?? new Map<string, string[]>();
          byInterface.set(d.interface, [...(byInterface.get(d.interface) ?? []), d.name]);
          known.set(d.root, byInterface);
        } else if (d?.kind === "interface") {
          const byInterface = known.get(d.root) ?? new Map<string, string[]>();
          if (!byInterface.has(d.interface)) byInterface.set(d.interface, []);
          known.set(d.root, byInterface);
        }
        walk(item.children);
      });
    walk(this.controller.items);
    return known;
  }

  /** The tests an invocation covers, as the tree has them. */
  private covered(inv: Invocation): vscode.TestItem[] {
    const out: vscode.TestItem[] = [];
    const walk = (items: vscode.TestItemCollection) =>
      items.forEach((item) => {
        const d = this.data.get(item);
        if (d?.kind === "test" && d.root === inv.root) {
          const inInterface = inv.interfaces.length === 0 || inv.interfaces.includes(d.interface);
          if (inInterface && (inv.tests.length === 0 || inv.tests.includes(d.name))) out.push(item);
        }
        walk(item.children);
      });
    walk(this.controller.items);
    return out;
  }

  async run(request: vscode.TestRunRequest, token: vscode.CancellationToken, debug: boolean): Promise<void> {
    const binary = await this.binary.get();
    const testRun = this.controller.createTestRun(request);
    try {
      if (!binary) {
        testRun.appendOutput("`ductus` wasn't found: see the Ductus output (View → Output → Ductus).\r\n");
        return;
      }
      if (this.discovered.size === 0) await this.discoverAll();
      const include: vscode.TestItem[] = [];
      if (request.include) include.push(...request.include);
      else this.controller.items.forEach((i) => include.push(i));
      const chosen = include.map((i) => this.chosen(i)).filter((c): c is Chosen => !!c);
      const excluded = (request.exclude ?? []).map((i) => this.chosen(i)).filter((c): c is Chosen => !!c);
      // A top-level interface (one project) chosen whole: when everything is, run the project.
      const roots = new Set(chosen.map((c) => c.root));
      const everything = !request.include && !request.exclude?.length;
      const invocations = plan(
        everything ? [...roots].map((root) => ({ root })) : chosen,
        excluded,
        this.known(),
        debug,
      );
      for (const inv of invocations) {
        if (token.isCancellationRequested) break;
        await this.invoke(binary.path, inv, testRun, token, debug);
      }
    } finally {
      testRun.end();
    }
  }

  /** One `ductus test`, its lines turned into results as they come. */
  private async invoke(
    binary: string,
    inv: Invocation,
    testRun: vscode.TestRun,
    token: vscode.CancellationToken,
    debug: boolean,
  ): Promise<void> {
    const expected = this.covered(inv);
    expected.forEach((t) => testRun.started(t));
    const reported = new Set<string>();
    const loadErrors = new Map<string, string>();
    const address = debug ? `127.0.0.1:${await freePort()}` : undefined;
    const args = testArgs(inv, environment(), address);
    this.log.info(`${binary} ${args.join(" ")}  (in ${inv.root})`);

    let attached: Promise<void> | undefined;
    const ended = await run(
      binary,
      args,
      inv.root,
      {
        line: (text) => {
          const line = parseTestLine(text);
          if (!line) {
            if (text.trim()) testRun.appendOutput(`${text}\r\n`);
            return;
          }
          if (line.type === "test") {
            const item = this.testItem(inv.root, line);
            reported.add(item.id);
            this.report(testRun, item, line, inv.root);
          } else if (line.type === "output") {
            testRun.appendOutput(crlf(`${line.interface}: while loading\n${line.output}`));
          } else if (line.type === "load") {
            loadErrors.set(line.interface, line.error);
            testRun.appendOutput(crlf(`${line.interface}: its tests didn't load\n${line.output ?? ""}${line.error}\n`));
          }
        },
        stderr: (chunk) => {
          // Under a debugger: attach once it listens ("…: waiting for a debugger on 127.0.0.1:…").
          if (address && !attached && chunk.includes("waiting for a debugger on")) {
            attached = this.attach(address, inv, testRun);
          }
          testRun.appendOutput(crlf(chunk));
        },
      },
      token,
    );
    if (debug && !attached && !ended.error) {
      testRun.appendOutput(crlf("ductus test ended without waiting for a debugger (see above)\n"));
    }
    await attached;
    const why =
      ended.error?.message ??
      (ended.code !== 0 && ended.code !== 1 && !token.isCancellationRequested
        ? `ductus test ended with ${ended.signal ?? `exit ${ended.code}`}: ${ended.stderr.trim()}`
        : undefined);
    for (const item of expected) {
      if (reported.has(item.id)) continue;
      const d = this.data.get(item);
      const load = d?.kind === "test" ? loadErrors.get(d.interface) : undefined;
      if (token.isCancellationRequested) testRun.skipped(item);
      else
        this.errored(
          testRun,
          item,
          load
            ? `the interface's tests didn't load:\n${load}`
            : (why ?? "not run: renamed or removed? Refresh the tests."),
        );
    }
  }

  private async attach(address: string, inv: Invocation, testRun: vscode.TestRun): Promise<void> {
    const [host, port] = address.split(":");
    const folder = vscode.workspace.getWorkspaceFolder(vscode.Uri.file(inv.root));
    const name = `Ductus: ${inv.interfaces[0] ?? "tests"}${inv.tests.length === 1 ? ` › ${inv.tests[0]}` : ""}`;
    const configuration: vscode.DebugConfiguration = {
      type: "node",
      request: "attach",
      name,
      address: host,
      port: Number(port),
      // The engine's runtime isn't Node: nothing of Node's to set up in it.
      attachExistingChildren: false,
      autoAttachChildProcesses: false,
      continueOnAttach: true,
    };
    const ok = await vscode.debug.startDebugging(folder, configuration, { testRun, suppressSaveBeforeStart: true });
    if (!ok) testRun.appendOutput(crlf(`VS Code's debugger didn't attach to ${address}\n`));
  }

  private report(
    testRun: vscode.TestRun,
    item: vscode.TestItem,
    line: Extract<TestLine, { type: "test" }>,
    root: string,
  ) {
    if (line.output) testRun.appendOutput(crlf(line.output), undefined, item);
    if (line.notRun !== undefined) {
      testRun.appendOutput(crlf(`not run: ${line.notRun}\n`), undefined, item);
      testRun.skipped(item);
      this.results.set(item.id, { state: "skipped", message: line.notRun });
      return;
    }
    if (line.ok === true) {
      testRun.passed(item, line.ms);
      this.results.set(item.id, { state: "passed" });
      return;
    }
    if (line.ok !== false) return; // listed, not run
    const error = line.error ?? "failed";
    const message =
      line.expected !== undefined && line.actual !== undefined
        ? vscode.TestMessage.diff(error, line.expected, line.actual)
        : new vscode.TestMessage(error);
    const at =
      line.failure ??
      (line.file && line.line ? { file: line.file, line: line.line, column: line.column ?? 1 } : undefined);
    if (at) {
      const position = new vscode.Position(at.line - 1, at.column - 1);
      message.location = new vscode.Location(vscode.Uri.file(path.join(root, at.file)), position);
    }
    testRun.failed(item, message, line.ms);
    this.results.set(item.id, {
      state: "failed",
      message: error,
      expected: line.expected,
      actual: line.actual,
      location: message.location,
    });
  }

  private errored(testRun: vscode.TestRun, item: vscode.TestItem, why: string) {
    testRun.errored(item, new vscode.TestMessage(why));
    this.results.set(item.id, { state: "errored", message: why });
  }

  /** A test's item, by project, interface and name (for the extension's tests). */
  find(root: string, iface: string, name?: string): vscode.TestItem | undefined {
    const parent = this.parentFor(root);
    const top = parent ? parent.children : this.controller.items;
    const i = top.get(id.interface(root, iface));
    return name === undefined ? i : i?.children.get(id.test(root, iface, name));
  }
}

const crlf = (text: string) => text.replace(/\r?\n/g, "\r\n");

/** A port free on 127.0.0.1 now, for one debugged run (a dev engine may hold 9229). */
export function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const server = net.createServer();
    server.unref();
    server.on("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      const port = typeof address === "object" && address ? address.port : 0;
      server.close(() => resolve(port));
    });
  });
}
