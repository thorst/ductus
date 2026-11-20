import * as fs from "node:fs";
import * as path from "node:path";
import * as vscode from "vscode";
import type { Finder } from "./binary";
import type { Dev } from "./dev";
import type { Engines, Reach } from "./engines";
import { type DebugLink, type NamedEngine, parseDebugLink, sameEngine, tryMessageArgs } from "./lines";
import { type Projects, chooseRoot, interfaceOf } from "./projects";
import { environment } from "./settings";
import { freePort } from "./testing";

/** What a debugging of a stored message needs: the project, what to read, and how to sign in. */
export interface MessageDebug {
  root: string;
  link: DebugLink;
  reach: Reach;
}

/**
 * **Debug this message**: a message an engine stored, run through your copy of its interface's
 * code under VS Code's debugger, as `ductus try <interface> --message ID --from URL --inspect`
 * does in a terminal (nothing is sent; the read is in that engine's audit log). From the Command
 * Palette (a typed id, the open file's interface), or from the web UI's **Debug in VS Code** link
 * on a message's page (`vscode://ductus.ductus/debug-message?engine=…&interface=…&message=…`).
 *
 * A link opens only an engine you named in `ductus.engines` or your own `ductus dev`: one from
 * anywhere else asks first, showing its URL, and adds it once you name it (a web page can't send
 * you, or a token of yours, to an engine you don't know).
 */
export class Messages implements vscode.Disposable, vscode.UriHandler {
  private readonly disposables: vscode.Disposable[] = [];

  constructor(
    private readonly binary: Finder,
    private readonly projects: Projects,
    private readonly dev: Dev,
    private readonly engines: Engines,
    private readonly log: vscode.LogOutputChannel,
  ) {
    this.disposables.push(
      vscode.window.registerUriHandler(this),
      vscode.commands.registerCommand("ductus.debugMessage", (uri?: vscode.Uri) => this.command(uri)),
    );
  }

  dispose() {
    this.disposables.forEach((d) => d.dispose());
  }

  handleUri(uri: vscode.Uri): void {
    if (uri.path.replace(/\/+$/, "") === "/debug-message") {
      void this.fromLink(uri.query);
    } else {
      this.log.warn(`a link the extension doesn't know: ${uri.toString(true)}`);
      void vscode.window.showWarningMessage(
        `Ductus: the link asks for \`${uri.path}\`, which this extension doesn't do.`,
      );
    }
  }

  /** The web UI's link: the project with the interface, the engine named, then the debugger. */
  async fromLink(query: string): Promise<boolean> {
    const link = parseDebugLink(query);
    if ("why" in link) {
      void vscode.window.showErrorMessage(`Ductus: Debug this message: ${link.why}.`);
      return false;
    }
    this.log.info(`debug message ${link.message} of ${link.interface} from ${link.engine} (a link)`);
    await this.projects.scan();
    const root = await this.projectWith(link.interface);
    if (!root) return false;
    const reach = await this.reach(root, link.engine, true);
    if (!reach) return false;
    return this.debug({ root, link: { ...link, engine: reach.engine.url }, reach }, true);
  }

  /** The command: the open file's interface (else one picked), an engine, a typed id. */
  private async command(uri?: vscode.Uri) {
    const root = await chooseRoot(this.projects, uri, "Debug this message", "The project whose code runs the message");
    if (!root) return;
    const file = uri?.scheme === "file" ? uri.fsPath : vscode.window.activeTextEditor?.document.uri.fsPath;
    const iface = (file && interfaceOf(root, file)) || (await pickInterface(root));
    if (!iface) return;
    const engine = await this.pickEngine(root);
    if (!engine) return;
    const id = await vscode.window.showInputBox({
      title: `Debug a message of ${iface} stored on ${engine.name}`,
      prompt:
        "Its id: on the web UI's message page, or `ductus messages` (the message is read from the engine, nothing is sent)",
      placeHolder: "12345",
      ignoreFocusOut: true,
      validateInput: (v) => (/^[1-9][0-9]{0,15}$/.test(v.trim()) ? undefined : "a message id: digits"),
    });
    if (!id) return;
    const reach = await this.reach(root, engine.url, false);
    if (!reach) return;
    await this.debug(
      { root, link: { engine: reach.engine.url, interface: iface, message: Number(id.trim()) }, reach },
      true,
    );
  }

  /**
   * Start the debugger: `ductus try … --inspect` on a free port, the token in its environment.
   * `told`: a person asked (with no breakpoint set in the project, it opens the interface's
   * interface.yaml and waits for them to set one). True once the session started.
   */
  async debug(d: MessageDebug, told = false): Promise<boolean> {
    const binary = await this.binary.get();
    if (!binary) return false;
    if (told && !hasBreakpoint(d.root)) {
      const yaml = path.join(d.root, "interfaces", d.link.interface, "interface.yaml");
      if (fs.existsSync(yaml)) await vscode.window.showTextDocument(vscode.Uri.file(yaml), { preview: true });
      const go = await vscode.window.showInformationMessage(
        `Debug message ${d.link.message} of ${d.link.interface}: there's no breakpoint in the project, so it would run through without stopping. Set one in the code it runs (interface.yaml names the files), then Debug.`,
        "Debug",
        "Debug without one",
      );
      if (!go) return false;
    }
    const port = await freePort();
    const args = tryMessageArgs(d.link, environment(), `127.0.0.1:${port}`);
    this.log.info(`debug message: ductus ${args.join(" ")} (in ${d.root}, as ${d.reach.engine.name}'s token)`);
    const folder = vscode.workspace.getWorkspaceFolder(vscode.Uri.file(d.root));
    return vscode.debug.startDebugging(folder, {
      type: "node",
      request: "launch",
      name: `Ductus: debug message ${d.link.message}`,
      runtimeExecutable: binary.path,
      runtimeArgs: args,
      cwd: d.root,
      // Never in the arguments: other users of the machine can list those.
      env: { DUCTUS_API_TOKEN: d.reach.token },
      attachSimplePort: port,
      console: "internalConsole",
      outputCapture: "std",
    });
  }

  /** The project to run the message in: the one with that interface's folder (picked from several). */
  private async projectWith(iface: string): Promise<string | undefined> {
    const roots = this.projects.roots;
    const having = roots.filter((r) => fs.existsSync(path.join(r, "interfaces", iface, "interface.yaml")));
    if (having.length === 1) return having[0];
    if (having.length > 1) {
      return vscode.window.showQuickPick(having, { placeHolder: `The project whose ${iface} runs the message` });
    }
    void vscode.window.showErrorMessage(
      roots.length === 0
        ? `Ductus: to debug a message of ${iface}, open the project that has it first.`
        : `Ductus: none of the open projects has an interface \`${iface}\` (interfaces/${iface}/interface.yaml): open the one that does.`,
    );
    return undefined;
  }

  /** The engines a typed id can come from: the project's running dev, then the named ones. */
  private async pickEngine(root: string): Promise<NamedEngine | undefined> {
    const dev = await this.dev.find(root);
    const choices: NamedEngine[] = [
      ...("api" in dev ? [{ name: "your ductus dev", url: dev.api }] : []),
      ...this.engines.list(),
    ];
    if (choices.length === 0) {
      void vscode.window.showWarningMessage(
        "Debug this message: no engine to read it from. Start `ductus dev`, or name your engines in `ductus.engines`.",
      );
      return undefined;
    }
    if (choices.length === 1) return choices[0];
    const chosen = await vscode.window.showQuickPick(
      choices.map((e) => ({ label: e.name, description: e.url, engine: e })),
      { placeHolder: "The engine the message is stored on" },
    );
    return chosen?.engine;
  }

  /**
   * How to read from the engine at `url`: your `ductus dev` with its own token, else an engine named
   * in `ductus.engines` with a token that may read payloads. `fromLink`: an engine named nowhere
   * may be added (asked, showing its URL); typed, it's always one of those listed.
   */
  private async reach(root: string, url: string, fromLink: boolean): Promise<Reach | undefined> {
    const dev = await this.dev.find(root);
    if ("api" in dev && sameEngine(dev.api, url)) {
      return { engine: { name: "your ductus dev", url: dev.api }, token: dev.token };
    }
    let engine = this.engines.list().find((e) => sameEngine(e.url, url));
    if (!engine && fromLink) engine = await addEngine(url);
    if (!engine) return undefined;
    return this.engines.signIn(engine, "payloads");
  }
}

/** Whether any breakpoint is set in a file of the project. */
function hasBreakpoint(root: string): boolean {
  const inside = (file: string) => !path.relative(root, file).startsWith("..");
  return vscode.debug.breakpoints.some(
    (b) => b.enabled && b instanceof vscode.SourceBreakpoint && inside(b.location.uri.fsPath),
  );
}

/** A project's interfaces, by folder, to pick one. */
async function pickInterface(root: string): Promise<string | undefined> {
  let names: string[] = [];
  try {
    names = fs
      .readdirSync(path.join(root, "interfaces"), { withFileTypes: true })
      .filter((d) => d.isDirectory() && fs.existsSync(path.join(root, "interfaces", d.name, "interface.yaml")))
      .map((d) => d.name)
      .sort();
  } catch {
    // no interfaces folder: said below
  }
  if (names.length === 0) {
    void vscode.window.showWarningMessage(`Debug this message: ${root} has no interfaces.`);
    return undefined;
  }
  return names.length === 1
    ? names[0]
    : vscode.window.showQuickPick(names, { placeHolder: "The interface the message is of" });
}

/**
 * A link's engine named nowhere: asked first (its URL shown), then added to `ductus.engines` in
 * your user settings under the name given.
 */
async function addEngine(url: string): Promise<NamedEngine | undefined> {
  const add = "Name it and go on…";
  const chosen = await vscode.window.showWarningMessage(
    `Debug this message: the link is for ${url}, an engine that isn't in \`ductus.engines\`. Only go on if it's one of yours.`,
    { modal: true },
    add,
  );
  if (chosen !== add) return undefined;
  let host = "engine";
  try {
    host = new URL(url).hostname;
  } catch {
    // parseDebugLink checked it
  }
  const name = (
    await vscode.window.showInputBox({
      title: `A name for ${url}`,
      prompt: "Added to `ductus.engines` in your user settings (Promote… and the History view list it too)",
      value: host,
      ignoreFocusOut: true,
      validateInput: (v) => (v.trim() ? undefined : "a name"),
    })
  )?.trim();
  if (!name) return undefined;
  const settings = vscode.workspace.getConfiguration("ductus");
  const current = (settings.inspect<Record<string, string>>("engines")?.globalValue ?? {}) as Record<string, string>;
  await settings.update("engines", { ...current, [name]: url }, vscode.ConfigurationTarget.Global);
  return { name, url };
}
