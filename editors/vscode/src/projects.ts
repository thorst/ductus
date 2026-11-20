import * as path from "node:path";
import * as vscode from "vscode";

/** What makes a folder a Ductus project: interfaces, each a folder with its interface.yaml. */
export const INTERFACE_FILES = "**/interfaces/*/interface.yaml";
const SKIP = "**/{node_modules,target,.git,.ductus}/**";

/**
 * The Ductus projects in the workspace: every folder holding `interfaces/<name>/interface.yaml`
 * (ductus.yaml is optional). Found again when an interface.yaml is added or deleted.
 */
export class Projects implements vscode.Disposable {
  private list: string[] = [];
  private readonly changed = new vscode.EventEmitter<void>();
  readonly onDidChange = this.changed.event;
  private readonly watcher = vscode.workspace.createFileSystemWatcher(INTERFACE_FILES, false, true, false);

  constructor() {
    this.watcher.onDidCreate(() => void this.scan());
    this.watcher.onDidDelete(() => void this.scan());
  }

  dispose() {
    this.watcher.dispose();
    this.changed.dispose();
  }

  get roots(): readonly string[] {
    return this.list;
  }

  async scan(): Promise<void> {
    const files = await vscode.workspace.findFiles(INTERFACE_FILES, SKIP);
    const roots = [...new Set(files.map((f) => path.dirname(path.dirname(path.dirname(f.fsPath)))))].sort();
    if (roots.join("\n") !== this.list.join("\n")) {
      this.list = roots;
      this.changed.fire();
    }
  }

  /** The project a file is in (the deepest, should one hold another), if any. */
  rootOf(file: string): string | undefined {
    const norm = (p: string) => (process.platform === "win32" ? p.toLowerCase() : p);
    const f = norm(file);
    return this.list
      .filter((r) => f === norm(r) || f.startsWith(norm(r) + path.sep))
      .sort((a, b) => b.length - a.length)[0];
  }
}

/**
 * The project a command acts on: the file given's, else the open file's, else the only one, else
 * the one picked (`placeHolder` says what for). Undefined, said why, when there's none.
 */
export async function chooseRoot(
  projects: Projects,
  uri: vscode.Uri | undefined,
  command: string,
  placeHolder: string,
): Promise<string | undefined> {
  const file = uri?.scheme === "file" ? uri.fsPath : vscode.window.activeTextEditor?.document.uri.fsPath;
  const fromFile = file ? projects.rootOf(file) : undefined;
  if (fromFile) return fromFile;
  const roots = projects.roots;
  if (roots.length <= 1) {
    if (roots.length === 0) void vscode.window.showWarningMessage(`${command}: no Ductus project is open.`);
    return roots[0];
  }
  return vscode.window.showQuickPick([...roots], { placeHolder });
}

/** The interface a file is in: the folder under `interfaces/`, when it is in one. */
export function interfaceOf(root: string, file: string): string | undefined {
  const parts = path.relative(root, file).split(path.sep);
  return parts[0] === "interfaces" && parts.length > 2 ? parts[1] : undefined;
}
