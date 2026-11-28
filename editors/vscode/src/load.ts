import * as vscode from "vscode";
import type { Finder } from "./binary";
import { firstParagraph } from "./lines";
import { pickInterface } from "./message";
import { run } from "./process";
import { type Projects, chooseRoot, interfaceOf } from "./projects";
import { environment } from "./settings";

/** How a load test ended: the verdict's first sentence (what limits it), or why it didn't run. */
export type LoadTested = { limit: string; code: 0 } | { why: string; code: number | null };

/**
 * **Ductus: Load test this interface**: `ductus bench interface` on the open file's interface (its
 * code on its fixtures in a loop, its lookups mocked), the report written to the "Ductus load
 * test" output as the CLI prints it, and what limits it said in a notification. Nothing runs until
 * the command is given.
 */
export class LoadTest implements vscode.Disposable {
  private readonly disposables: vscode.Disposable[] = [];
  private readonly report: vscode.OutputChannel;

  constructor(
    private readonly binary: Finder,
    private readonly projects: Projects,
    private readonly log: vscode.LogOutputChannel,
  ) {
    this.report = vscode.window.createOutputChannel("Ductus load test");
    this.disposables.push(
      this.report,
      vscode.commands.registerCommand("ductus.loadTest", (uri?: vscode.Uri) => this.command(uri)),
    );
  }

  dispose() {
    this.disposables.forEach((d) => d.dispose());
  }

  private async command(uri?: vscode.Uri) {
    const title = "Load test this interface";
    const root = await chooseRoot(this.projects, uri, title, "The project whose interface to load test");
    if (!root) return;
    const file = uri?.scheme === "file" ? uri.fsPath : vscode.window.activeTextEditor?.document.uri.fsPath;
    const iface = (file && interfaceOf(root, file)) || (await pickInterface(root, title, "The interface to load test"));
    if (!iface) return;
    const done = await this.run(root, iface);
    if ("limit" in done) void vscode.window.showInformationMessage(`${iface}: ${done.limit}`);
    else void vscode.window.showErrorMessage(`Ductus: load test of ${iface}: ${done.why}`);
  }

  /**
   * Run it (`extra`: more of the command's options, as `--seconds 1`), the report shown as it's
   * written. Its lookups are always mocked here: `--real` asks first, which a notification can't.
   */
  async run(root: string, iface: string, extra: string[] = []): Promise<LoadTested> {
    const binary = await this.binary.get();
    if (!binary) return { why: "no `ductus` to run it with (see the output)", code: null };
    const args = ["bench", "interface", iface, "--env", environment(), ...extra];
    this.report.clear();
    this.report.show(true);
    this.report.appendLine(`$ ductus ${args.join(" ")}`);
    const said: string[] = [];
    const ended = await vscode.window.withProgress(
      {
        location: vscode.ProgressLocation.Notification,
        title: `Load testing ${iface} (its code in a loop for a few seconds, lookups mocked)…`,
        cancellable: true,
      },
      (_, stop) =>
        run(
          binary.path,
          args,
          root,
          {
            line: (l) => {
              said.push(l);
              this.report.appendLine(l);
            },
            stderr: (t) => this.report.append(t),
          },
          stop,
        ),
    );
    this.log.info(`load test: ductus ${args.join(" ")} (${ended.ms} ms, exit ${ended.code ?? ended.signal})`);
    if (ended.code !== 0) {
      const why = ended.error?.message ?? firstParagraph(ended.stderr);
      return { why: why || `ductus ended with ${ended.code ?? ended.signal}`, code: ended.code };
    }
    // The verdict's lines start with "→": the first that names the limit is what to tell.
    const verdict = said.map((l) => l.trim()).filter((l) => l.startsWith("→ "));
    const limit = verdict.find((l) => / is the limit|are the limit/.test(l)) ?? verdict[0] ?? "done";
    return { limit: limit.replace(/^→ /, ""), code: 0 };
  }
}
