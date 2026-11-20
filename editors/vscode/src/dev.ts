import * as fs from "node:fs/promises";
import * as path from "node:path";
import * as vscode from "vscode";
import type { Finder } from "./binary";
import { type DevEngine, type ReloadReport, parseDevEngine, parseReload, reloadSummary } from "./lines";
import type { Projects } from "./projects";
import { deployToDevOnSave } from "./settings";

/** How long after a save Deploy to dev waits for the next (saving several files deploys once). */
const DEBOUNCE_MS = 400;
/** The engine's answer comes once the changed interfaces run their new code: never minutes. */
const ANSWER_WITHIN_MS = 5 * 60_000;
/** Where `ductus dev` says it runs (written once it's up, deleted when it stops). */
const FOUND = path.join(".ductus", "dev-engine.json");
/** The `dev` user's token `ductus dev` keeps for editors (`deploy`, `payloads`). */
const TOKEN = path.join(".ductus", "dev-token");

/** A project's running dev engine, with the token to call it. */
export interface Reachable extends DevEngine {
  token: string;
}

/** How a deploy to dev went (for the extension's tests). */
export interface Deployed {
  root: string;
  /** The engine's report; undefined when there was none (no engine, not reachable). */
  report?: ReloadReport;
  /** Why there's no report. */
  error?: string;
}

/**
 * **Deploy to dev**: the developer's `ductus dev` applies the project folder as it is now, in
 * place (`POST /engine/reload`: the changed interfaces take it, the others never pause). Found
 * through `.ductus/dev-engine.json` and signed in with `.ductus/dev-token`, both written by
 * `ductus dev`: nothing to configure. On save too with `ductus.deployToDevOnSave` (debounced, one
 * at a time per project, only while a dev engine runs).
 */
export class Dev implements vscode.Disposable {
  private readonly timers = new Map<string, ReturnType<typeof setTimeout>>();
  private readonly running = new Map<string, Promise<Deployed>>();
  private readonly again = new Set<string>();
  private readonly disposables: vscode.Disposable[] = [];
  private readonly deployed = new vscode.EventEmitter<Deployed>();
  /** After each deploy (for the extension's tests). */
  readonly onDidDeploy = this.deployed.event;

  constructor(
    private readonly binary: Finder,
    private readonly projects: Projects,
    private readonly log: vscode.LogOutputChannel,
  ) {
    // Whether a dev engine runs, for the editor title's button: told by the file, never polled.
    const watcher = vscode.workspace.createFileSystemWatcher(`**/${FOUND.replace(/\\/g, "/")}`);
    const refresh = () => void this.refreshContext();
    watcher.onDidCreate(refresh);
    watcher.onDidDelete(refresh);
    this.disposables.push(
      watcher,
      this.deployed,
      projects.onDidChange(refresh),
      vscode.workspace.onDidSaveTextDocument((d) => {
        if (!deployToDevOnSave() || d.uri.scheme !== "file") return;
        const root = projects.rootOf(d.uri.fsPath);
        if (root && path.relative(root, d.uri.fsPath).split(path.sep)[0] !== ".ductus") this.schedule(root);
      }),
      vscode.commands.registerCommand("ductus.deployToDev", (uri?: vscode.Uri) => this.command(uri)),
    );
  }

  dispose() {
    this.timers.forEach((t) => clearTimeout(t));
    this.disposables.forEach((d) => d.dispose());
  }

  /** The running dev engine of a project with its token, or undefined (said why in `why`). */
  async find(root: string): Promise<Reachable | { why: string }> {
    const found = parseDevEngine(await readText(path.join(root, FOUND)));
    if (!found) return { why: `no \`ductus dev\` is running for ${root}` };
    const token = (await readText(path.join(root, TOKEN))).trim();
    if (!token) return { why: `\`ductus dev\` runs for ${root}, but ${TOKEN} isn't there: restart it` };
    return { ...found, token };
  }

  /** Deploy on save: once for several saves close together, and only while dev runs. */
  schedule(root: string, delay = DEBOUNCE_MS) {
    clearTimeout(this.timers.get(root));
    this.timers.set(
      root,
      setTimeout(() => {
        this.timers.delete(root);
        void this.find(root).then((found) => {
          if ("api" in found) void this.deploy(root, false);
        });
      }, delay),
    );
  }

  /** Deploy a project now (or right after the deploy in progress). `told`: a person asked. */
  deploy(root: string, told = true): Promise<Deployed> {
    const current = this.running.get(root);
    if (current) {
      this.again.add(root);
      return current;
    }
    const next = this.once(root, told).finally(() => {
      this.running.delete(root);
      if (this.again.delete(root)) void this.deploy(root, false);
    });
    this.running.set(root, next);
    return next;
  }

  private async command(uri?: vscode.Uri) {
    const root = await this.chooseRoot(uri);
    if (root) await this.deploy(root, true);
  }

  private async once(root: string, told: boolean): Promise<Deployed> {
    const found = await this.find(root);
    if (!("api" in found)) {
      this.log.warn(`deploy to dev: ${found.why}`);
      if (told) void this.offerStart(root, found.why);
      return this.done({ root, error: found.why });
    }
    const url = `${found.api}/api/v1/engine/reload`;
    let status: number;
    let body: string;
    try {
      const response = await fetch(url, {
        method: "POST",
        headers: { authorization: `Bearer ${found.token}`, "content-type": "application/json" },
        body: JSON.stringify({}),
        signal: AbortSignal.timeout(ANSWER_WITHIN_MS),
      });
      status = response.status;
      body = await response.text();
    } catch (e) {
      const why = `\`ductus dev\` doesn't answer at ${found.api} (${cause(e)}): was it stopped?`;
      this.log.error(`deploy to dev: ${why}`);
      if (told) void this.offerStart(root, why);
      return this.done({ root, error: why });
    }
    const report = parseReload(body);
    if (!report) {
      const why = `${status}: ${errorText(body)}`;
      this.log.error(`deploy to dev (${found.api}): ${why}`);
      void vscode.window.showErrorMessage(`Deploy to dev: ${why}`, "Show the output").then(this.showOutput);
      return this.done({ root, error: why });
    }
    this.logReport(found.api, report);
    const summary = reloadSummary(report);
    if (!report.ok) {
      void vscode.window.showErrorMessage(`Deploy to dev refused: ${summary}`, "Show the output").then(this.showOutput);
    } else if (told) {
      void vscode.window.showInformationMessage(`Deployed to dev: ${summary}`);
    } else {
      vscode.window.setStatusBarMessage(`$(check) Ductus: deployed to dev: ${summary}`, 5000);
    }
    return this.done({ root, report });
  }

  private done(deployed: Deployed): Deployed {
    this.deployed.fire(deployed);
    return deployed;
  }

  private logReport(api: string, report: ReloadReport) {
    const lines = [`deploy to dev (${api}): ${report.applied}: ${report.outcome}`];
    for (const step of report.steps ?? []) lines.push(`  ${step}`);
    for (const c of report.changes ?? []) lines.push(`  ${c.interface}: ${c.from ?? "(new)"} → ${c.to}`);
    for (const [name, warning] of Object.entries(report.warnings ?? {}))
      lines.push(`  ⚠ ${name}: over someone else's change: ${warning}`);
    if (report.ok) this.log.info(lines.join("\n"));
    else this.log.error(lines.join("\n"));
  }

  /** No dev engine: say so, with a button that starts one in a terminal. */
  private async offerStart(root: string, why: string) {
    const chosen = await vscode.window.showWarningMessage(`Deploy to dev: ${why}.`, "Start ductus dev");
    if (chosen !== "Start ductus dev") return;
    const binary = await this.binary.get();
    if (!binary) return;
    const terminal = vscode.window.createTerminal({ name: "ductus dev", cwd: root });
    terminal.show();
    terminal.sendText(`${quote(binary.path)} dev`);
  }

  private readonly showOutput = (chosen: string | undefined) => {
    if (chosen === "Show the output") this.log.show();
  };

  /** The project of the file given or open, else the only one, else the one picked. */
  private async chooseRoot(uri?: vscode.Uri): Promise<string | undefined> {
    const file = uri?.scheme === "file" ? uri.fsPath : vscode.window.activeTextEditor?.document.uri.fsPath;
    const fromFile = file ? this.projects.rootOf(file) : undefined;
    if (fromFile) return fromFile;
    const roots = this.projects.roots;
    if (roots.length <= 1) {
      if (roots.length === 0) void vscode.window.showWarningMessage("Deploy to dev: no Ductus project is open.");
      return roots[0];
    }
    return vscode.window.showQuickPick([...roots], { placeHolder: "The project to deploy to its ductus dev" });
  }

  private async refreshContext() {
    const found = await Promise.all(this.projects.roots.map((r) => readText(path.join(r, FOUND))));
    void vscode.commands.executeCommand(
      "setContext",
      "ductus.devRunning",
      found.some((t) => t !== ""),
    );
  }
}

async function readText(file: string): Promise<string> {
  try {
    return await fs.readFile(file, "utf8");
  } catch {
    return "";
  }
}

/** What a failed fetch says, without the useless "fetch failed". */
function cause(e: unknown): string {
  const inner = e instanceof Error && e.cause instanceof Error ? e.cause.message : undefined;
  return inner ?? (e instanceof Error ? e.message : String(e));
}

/** An error answer's `error`, else its text. */
function errorText(body: string): string {
  try {
    const value = JSON.parse(body);
    if (value && typeof value.error === "string") return value.error;
  } catch {
    // not JSON
  }
  return body.trim() || "no details";
}

/** A path as a shell reads it (spaces). */
function quote(p: string): string {
  return /\s/.test(p) ? `"${p}"` : p;
}
