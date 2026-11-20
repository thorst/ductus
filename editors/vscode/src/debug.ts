import * as path from "node:path";
import * as vscode from "vscode";
import type { Finder } from "./binary";
import { type Projects, interfaceOf } from "./projects";
import { environment } from "./settings";
import { freePort } from "./testing";

/** What a `ductus` debug configuration runs under the debugger. */
type Command = "test" | "try" | "dev";

const NAMES: Record<Command, string> = {
  test: "Ductus: debug these tests",
  try: "Ductus: debug this message",
  dev: "Ductus: debug this interface",
};

/**
 * Debugging without `launch.json`: a `ductus` debug type whose configurations (the three
 * `ductus types` writes, offered in Run and Debug's list, or F5 with nothing configured) become
 * VS Code's own JavaScript debugger starting `ductus test`, `try` or `dev` with `--inspect` on a
 * free port, the binary the extension found, in the open file's project.
 */
export class DebugConfigurations implements vscode.DebugConfigurationProvider {
  constructor(
    private readonly binary: Finder,
    private readonly projects: Projects,
  ) {}

  static register(binary: Finder, projects: Projects): vscode.Disposable {
    const provider = new DebugConfigurations(binary, projects);
    return vscode.Disposable.from(
      vscode.debug.registerDebugConfigurationProvider("ductus", provider),
      vscode.debug.registerDebugConfigurationProvider(
        "ductus",
        { provideDebugConfigurations: () => provider.provideDebugConfigurations() },
        vscode.DebugConfigurationProviderTriggerKind.Dynamic,
      ),
    );
  }

  provideDebugConfigurations(): vscode.DebugConfiguration[] {
    return (Object.keys(NAMES) as Command[]).map((command) => ({
      type: "ductus",
      request: "launch",
      name: NAMES[command],
      command,
    }));
  }

  async resolveDebugConfiguration(
    folder: vscode.WorkspaceFolder | undefined,
    config: vscode.DebugConfiguration,
  ): Promise<vscode.DebugConfiguration | undefined> {
    const active = vscode.window.activeTextEditor?.document.uri;
    const file = expand(config.file, folder, active) ?? (active?.scheme === "file" ? active.fsPath : undefined);
    // F5 with nothing configured: a message file (a fixture) runs as a message, anything else the tests.
    const command: Command =
      config.command ?? (file && path.basename(path.dirname(file)) === "fixtures" ? "try" : "test");
    const root = file ? this.projects.rootOf(file) : undefined;
    if (!file || !root || !interfaceOf(root, file)) {
      void vscode.window.showWarningMessage(
        `${NAMES[command] ?? "Ductus"}: open a file of an interface first (its code, a test or a fixture), then start it again.`,
      );
      return undefined;
    }
    const binary = await this.binary.get();
    if (!binary) return undefined;
    const address = `127.0.0.1:${await freePort()}`;
    const extra: string[] = Array.isArray(config.args) ? config.args.map(String) : [];
    const env = ["--env", environment()];
    const runtimeArgs =
      command === "try"
        ? ["try", file, file, `--inspect=${address}`, ...env, ...extra]
        : command === "dev"
          ? ["dev", "--inspect", path.dirname(file), "--inspect-address", address, ...env, ...extra]
          : ["test", `--inspect=${address}`, "--interface", path.dirname(file), ...env, ...extra];
    const node: vscode.DebugConfiguration = {
      type: "node",
      request: "launch",
      name: config.name || NAMES[command],
      runtimeExecutable: binary.path,
      runtimeArgs,
      cwd: root,
      attachSimplePort: Number(address.split(":")[1]),
      console: "internalConsole",
      outputCapture: "std",
    };
    if (command === "dev") {
      // As `ductus types` writes it (docs/write/debugging.md): dev starts its Postgres first, and
      // stopping the session asks it to stop (on Windows it can't be asked, so it's ended).
      Object.assign(node, { timeout: 120_000, killBehavior: process.platform === "win32" ? "forceful" : "polite" });
    }
    return node;
  }
}

/** `${file}`, `${fileDirname}` and `${workspaceFolder}` in a configuration's `file`. */
function expand(
  value: unknown,
  folder: vscode.WorkspaceFolder | undefined,
  active: vscode.Uri | undefined,
): string | undefined {
  if (typeof value !== "string" || !value) return undefined;
  const open = active?.scheme === "file" ? active.fsPath : "";
  const text = value
    .replace(/\$\{file\}/g, open)
    .replace(/\$\{fileDirname\}/g, open && path.dirname(open))
    .replace(/\$\{workspaceFolder\}/g, folder?.uri.fsPath ?? "");
  return path.isAbsolute(text) || !folder ? text : path.join(folder.uri.fsPath, text);
}
