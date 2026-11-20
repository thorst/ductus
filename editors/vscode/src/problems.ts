import * as path from "node:path";
import * as vscode from "vscode";
import type { Finder } from "./binary";
import { type Finding, parseValidate, problemsByFile } from "./lines";
import { output } from "./process";
import type { Projects } from "./projects";
import { environment, validateOnSave } from "./settings";

/** How long after a save `validate` waits for the next (saving several files runs it once). */
const DEBOUNCE_MS = 400;

/**
 * Ductus's findings in the Problems view: `ductus validate --json` for a project when it's opened
 * and when one of its files is saved, debounced, one run at a time per project (a save during a
 * run runs it again after). Only Ductus's own findings: VS Code's TypeScript server already shows
 * TypeScript's.
 */
export class Problems implements vscode.Disposable {
  readonly collection = vscode.languages.createDiagnosticCollection("ductus");
  private readonly shown = new Map<string, vscode.Uri[]>();
  private readonly timers = new Map<string, ReturnType<typeof setTimeout>>();
  private readonly running = new Map<string, Promise<void>>();
  private readonly again = new Set<string>();
  private readonly disposables: vscode.Disposable[] = [];
  /** Called after each run, by project (for the extension's tests). */
  private readonly ran = new vscode.EventEmitter<string>();
  readonly onDidValidate = this.ran.event;

  constructor(
    private readonly binary: Finder,
    private readonly projects: Projects,
    private readonly log: vscode.LogOutputChannel,
  ) {
    this.disposables.push(
      this.collection,
      this.ran,
      vscode.workspace.onDidSaveTextDocument((d) => {
        if (!validateOnSave() || d.uri.scheme !== "file") return;
        const root = projects.rootOf(d.uri.fsPath);
        if (root && !inside(root, d.uri.fsPath, ".ductus")) this.schedule(root);
      }),
      projects.onDidChange(() => this.forgetGone()),
    );
  }

  dispose() {
    this.timers.forEach((t) => clearTimeout(t));
    this.disposables.forEach((d) => d.dispose());
  }

  schedule(root: string, delay = DEBOUNCE_MS) {
    clearTimeout(this.timers.get(root));
    this.timers.set(
      root,
      setTimeout(() => {
        this.timers.delete(root);
        void this.validate(root);
      }, delay),
    );
  }

  /** Validate a project now (or right after the run in progress). */
  validate(root: string): Promise<void> {
    const current = this.running.get(root);
    if (current) {
      this.again.add(root);
      return current;
    }
    const next = this.once(root).finally(() => {
      this.running.delete(root);
      if (this.again.delete(root)) void this.validate(root);
    });
    this.running.set(root, next);
    return next;
  }

  private async once(root: string): Promise<void> {
    const binary = await this.binary.get();
    if (!binary) return;
    const ran = await output(binary.path, ["validate", "--json", "--env", environment()], root);
    const document = parseValidate(ran.stdout);
    if (!document) {
      const why = ran.error?.message ?? (ran.stderr.trim() || ran.stdout.trim() || `exit ${ran.code}`);
      this.log.error(`ductus validate in ${root} didn't give its findings: ${why}`);
      this.ran.fire(root);
      return;
    }
    const byFile = problemsByFile(document);
    this.log.info(
      `validated ${root} (${document.environment}): ${document.errors} error(s), ${document.warnings} warning(s) (${ran.ms} ms)`,
    );
    this.shown.get(root)?.forEach((uri) => this.collection.delete(uri));
    const uris: vscode.Uri[] = [];
    for (const [file, findings] of byFile) {
      const uri = vscode.Uri.file(path.join(root, file));
      this.collection.set(uri, findings.map(diagnostic));
      uris.push(uri);
    }
    this.shown.set(root, uris);
    this.ran.fire(root);
  }

  private forgetGone() {
    for (const [root, uris] of this.shown) {
      if (!this.projects.roots.includes(root)) {
        uris.forEach((u) => this.collection.delete(u));
        this.shown.delete(root);
      }
    }
  }
}

function diagnostic(f: Finding): vscode.Diagnostic {
  const start = new vscode.Position(Math.max(0, (f.line ?? 1) - 1), Math.max(0, (f.column ?? 1) - 1));
  const end =
    f.end_line && f.end_column ? new vscode.Position(f.end_line - 1, f.end_column - 1) : start.translate(0, 0);
  const severity =
    f.severity === "error"
      ? vscode.DiagnosticSeverity.Error
      : f.severity === "warning"
        ? vscode.DiagnosticSeverity.Warning
        : vscode.DiagnosticSeverity.Information;
  const d = new vscode.Diagnostic(new vscode.Range(start, end), f.message, severity);
  d.source = "ductus";
  d.code = f.code;
  return d;
}

function inside(root: string, file: string, folder: string): boolean {
  return path.relative(root, file).split(path.sep)[0] === folder;
}
