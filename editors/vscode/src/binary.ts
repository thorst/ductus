import * as fs from "node:fs";
import * as path from "node:path";
import * as vscode from "vscode";
import { atLeast, parseVersion } from "./lines";
import { output } from "./process";

/**
 * The oldest `ductus` the extension works with: 0.7.0 streams `test --json` per test and has
 * `--test NAME` and `--timeout`, which the Test Explorer runs on.
 */
export const MINIMUM: [number, number, number] = [0, 7, 0];

export interface Binary {
  path: string;
  version: string;
}

const INSTALL = "https://getductus.com/#download";

/**
 * Finds `ductus`: the `ductus.path` setting, else the PATH (as a terminal would). Remembered until
 * the setting changes; says once what's wrong when it can't be used, with what to do.
 */
export class Finder implements vscode.Disposable {
  private found: Promise<Binary | undefined> | undefined;
  private told = false;
  private readonly watching: vscode.Disposable;

  constructor(private readonly log: vscode.LogOutputChannel) {
    this.watching = vscode.workspace.onDidChangeConfiguration((e) => {
      if (e.affectsConfiguration("ductus.path")) {
        this.found = undefined;
        this.told = false;
      }
    });
  }

  dispose() {
    this.watching.dispose();
  }

  /** The binary, or undefined (and the developer told why, once) when there's none to use. */
  get(): Promise<Binary | undefined> {
    this.found ??= this.find();
    return this.found;
  }

  private async find(): Promise<Binary | undefined> {
    const setting = vscode.workspace.getConfiguration("ductus").get<string>("path")?.trim();
    const candidate = setting ? this.resolveSetting(setting) : onPath();
    if (!candidate) {
      this.tell(
        "Ductus: `ductus` isn't on the PATH, so tests and checks can't run. Install it, or set `ductus.path` to where it is.",
      );
      return undefined;
    }
    const ran = await output(candidate, ["--version"], path.dirname(candidate));
    const version = parseVersion(ran.stdout);
    if (ran.error || !version) {
      const why = ran.error?.message ?? (ran.stderr.trim() || ran.stdout.trim() || `exit ${ran.code}`);
      this.tell(`Ductus: \`${candidate} --version\` didn't run (${why}). Check \`ductus.path\`.`);
      return undefined;
    }
    if (!atLeast(version, MINIMUM)) {
      this.tell(
        `Ductus: ${candidate} is ${ran.stdout.trim()}; this extension needs ${MINIMUM.join(".")} or newer (tests reported one by one as they end). Upgrade it, or set \`ductus.path\` to a newer one.`,
      );
      return undefined;
    }
    this.log.info(`using ${candidate} (${ran.stdout.trim()})`);
    return { path: candidate, version: ran.stdout.trim() };
  }

  private resolveSetting(setting: string): string {
    const folder = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
    const expanded = setting.replace(/\$\{workspaceFolder\}/g, folder ?? "");
    return path.isAbsolute(expanded) || !folder ? expanded : path.join(folder, expanded);
  }

  private tell(text: string) {
    this.log.warn(text);
    if (this.told) return;
    this.told = true;
    void vscode.window.showWarningMessage(text, "Set ductus.path", "Install").then((chosen) => {
      if (chosen === "Set ductus.path")
        void vscode.commands.executeCommand("workbench.action.openSettings", "ductus.path");
      if (chosen === "Install") void vscode.env.openExternal(vscode.Uri.parse(INSTALL));
    });
  }
}

/** `ductus` on the PATH, as a shell finds it (`ductus.exe` on Windows). */
function onPath(): string | undefined {
  const names = process.platform === "win32" ? ["ductus.exe", "ductus.cmd"] : ["ductus"];
  for (const dir of (process.env.PATH ?? "").split(path.delimiter)) {
    if (!dir) continue;
    for (const name of names) {
      const file = path.join(dir.replace(/^"|"$/g, ""), name);
      try {
        fs.accessSync(file, fs.constants.X_OK);
        if (fs.statSync(file).isFile()) return file;
      } catch {
        // not here
      }
    }
  }
  return undefined;
}
