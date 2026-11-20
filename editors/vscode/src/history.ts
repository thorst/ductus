import * as path from "node:path";
import * as vscode from "vscode";
import type { Finder } from "./binary";
import type { Dev } from "./dev";
import type { Engines, Reach } from "./engines";
import {
  type HistoryEntry,
  type HistoryPage,
  type NamedEngine,
  entryDetail,
  firstParagraph,
  historyArgs,
  historyPath,
  parseHistory,
  shortTime,
  touched,
} from "./lines";
import { output } from "./process";
import { type Projects, interfaceOf } from "./projects";
import { environment } from "./settings";

/** Entries read at a time (the CLI's and the API's default). */
const PAGE = 50;

/**
 * Where the History view reads from: the project folder (`ductus history --json`: git, and the
 * database when `DUCTUS_DATABASE_URL` is set), the project's running `ductus dev`, or an engine
 * named in `ductus.engines` (`GET /interfaces/{name}/history`).
 */
export type From = { kind: "folder" } | { kind: "dev" } | { kind: "engine"; engine: NamedEngine };

const ICONS: Record<HistoryEntry["kind"], string> = {
  commit: "git-commit",
  deployment: "rocket",
  audit: "shield",
  documentation: "book",
};

type Item = { entry: HistoryEntry } | { older: string };

/**
 * The **Ductus History** view (Explorer): who changed the open file's interface, what and why,
 * newest first, as `ductus history` and the web UI's History tab show it. It follows the editor
 * (a file of another interface shows that one's) and reads only while the view is visible.
 */
export class History implements vscode.Disposable, vscode.TreeDataProvider<Item> {
  private readonly disposables: vscode.Disposable[] = [];
  private readonly changed = new vscode.EventEmitter<Item | undefined>();
  readonly onDidChangeTreeData = this.changed.event;
  private readonly view: vscode.TreeView<Item>;
  /** The interface shown (the last interface file the editor had open). */
  private shown?: { root: string; iface: string };
  private entries: HistoryEntry[] = [];
  private next?: string;
  /** What was asked last: an answer to an older question is dropped. */
  private asked = 0;
  private stale = true;
  private readonly loaded = new vscode.EventEmitter<HistoryPage | { why: string }>();
  /** After each page is read (for the extension's tests). */
  readonly onDidLoad = this.loaded.event;

  constructor(
    private readonly binary: Finder,
    private readonly projects: Projects,
    private readonly dev: Dev,
    private readonly engines: Engines,
    private readonly state: vscode.Memento,
    private readonly log: vscode.LogOutputChannel,
  ) {
    this.view = vscode.window.createTreeView("ductus.history", { treeDataProvider: this });
    this.disposables.push(
      this.view,
      this.changed,
      this.loaded,
      this.view.onDidChangeVisibility(() => this.view.visible && this.stale && void this.load()),
      vscode.window.onDidChangeActiveTextEditor((e) => this.follow(e?.document.uri)),
      projects.onDidChange(() => this.follow(vscode.window.activeTextEditor?.document.uri)),
      // A deploy to dev is a new entry in dev's history: shown when that's where it reads.
      dev.onDidDeploy((d) => {
        if (d.report?.ok && this.shown?.root === d.root) this.refresh();
      }),
      vscode.commands.registerCommand("ductus.historyRefresh", () => this.refresh()),
      vscode.commands.registerCommand("ductus.historyFrom", () => this.chooseFrom()),
      vscode.commands.registerCommand("ductus.historyOlder", (before: string) => void this.load(before)),
    );
    this.follow(vscode.window.activeTextEditor?.document.uri);
  }

  dispose() {
    this.disposables.forEach((d) => d.dispose());
  }

  /** Show an interface's history (as opening one of its files does), read from `from` when given. */
  async show(root: string, iface: string, from?: From): Promise<HistoryPage | { why: string }> {
    if (from) await this.state.update(stateKey(root), fromKey(from));
    this.shown = { root, iface };
    return this.load();
  }

  /** The entries shown now (for the extension's tests). */
  get shownEntries(): readonly HistoryEntry[] {
    return this.entries;
  }

  getTreeItem(item: Item): vscode.TreeItem {
    if ("older" in item) {
      const older = new vscode.TreeItem("Older…");
      older.iconPath = new vscode.ThemeIcon("history");
      older.command = { command: "ductus.historyOlder", title: "Older", arguments: [item.older] };
      return older;
    }
    const e = item.entry;
    const t = new vscode.TreeItem(e.what.split("\n")[0]);
    const parts = touched(e);
    t.description = `${e.by} · ${shortTime(e.at)}${parts ? ` · ${parts}` : ""}`;
    t.iconPath = new vscode.ThemeIcon(ICONS[e.kind] ?? "circle-outline");
    const tip = new vscode.MarkdownString();
    tip.appendText(e.what);
    tip.appendMarkdown("\n\n");
    tip.appendCodeblock(entryDetail(e).join("\n"), "text");
    t.tooltip = tip;
    return t;
  }

  getChildren(item?: Item): Item[] {
    if (item) return [];
    const items: Item[] = this.entries.map((entry) => ({ entry }));
    if (this.next) items.push({ older: this.next });
    return items;
  }

  private follow(uri: vscode.Uri | undefined) {
    if (uri?.scheme !== "file") return;
    const root = this.projects.rootOf(uri.fsPath);
    const iface = root && interfaceOf(root, uri.fsPath);
    if (!root || !iface || (this.shown?.root === root && this.shown.iface === iface)) return;
    this.shown = { root, iface };
    this.refresh();
  }

  private refresh() {
    this.stale = true;
    if (this.view.visible) void this.load();
  }

  /** Read a page (the newest, or the one older than `before`, added below what's shown). */
  private async load(before?: string): Promise<HistoryPage | { why: string }> {
    const asked = ++this.asked;
    const shown = this.shown;
    if (!shown) {
      this.view.message = "Open a file of an interface to see its history.";
      return { why: "no interface open" };
    }
    this.stale = false;
    const from = await this.from(shown.root);
    this.view.description = `${shown.iface} · ${label(from)}`;
    if (!before) this.view.message = "Reading…";
    const page = await this.read(shown.root, shown.iface, from, before);
    if (asked !== this.asked) return page;
    if ("why" in page) {
      this.log.warn(`history of ${shown.iface} (${label(from)}): ${page.why}`);
      this.view.message = page.why;
      if (!before) this.entries = [];
      this.next = undefined;
    } else {
      this.entries = before ? [...this.entries, ...page.entries] : page.entries;
      this.next = page.next_before ?? undefined;
      const notes = page.notes.join(" ");
      this.view.message =
        this.entries.length === 0
          ? `Nothing recorded for ${shown.iface} yet.${notes ? ` ${notes}` : ""}`
          : notes || undefined;
    }
    this.changed.fire(undefined);
    this.loaded.fire(page);
    return page;
  }

  private async read(root: string, iface: string, from: From, before?: string): Promise<HistoryPage | { why: string }> {
    if (from.kind === "folder") {
      const binary = await this.binary.get();
      if (!binary) return { why: "no `ductus` to read the history with (see the output)" };
      const args = historyArgs(iface, environment(), PAGE, before);
      const ran = await output(binary.path, args, root);
      this.log.info(`history: ductus ${args.join(" ")} (${ran.ms} ms)`);
      const page = ran.code === 0 ? parseHistory(ran.stdout) : undefined;
      return page ?? { why: firstParagraph(ran.stderr) || `ductus history ended with ${ran.code ?? ran.signal}` };
    }
    const reach = await this.reach(root, from);
    if ("why" in reach) return reach;
    const answer = await this.engines.get(reach, historyPath(iface, PAGE, before));
    if ("why" in answer) return answer;
    return parseHistory(answer.body) ?? { why: `${reach.engine.name}'s answer isn't a history page` };
  }

  private async reach(root: string, from: Exclude<From, { kind: "folder" }>): Promise<Reach | { why: string }> {
    if (from.kind === "dev") {
      const found = await this.dev.find(root);
      if (!("api" in found)) return found;
      return { engine: { name: "your ductus dev", url: found.api }, token: found.token };
    }
    return (await this.engines.signIn(from.engine, "read")) ?? { why: `no token for ${from.engine.name}` };
  }

  /** Where a project's history is read from: the choice kept, else dev while it runs, else the folder. */
  private async from(root: string): Promise<From> {
    const kept = this.state.get<string>(stateKey(root));
    if (kept === "folder") return { kind: "folder" };
    if (kept === "dev") return { kind: "dev" };
    const named = kept?.startsWith("engine:") && this.engines.list().find((e) => `engine:${e.name}` === kept);
    if (named) return { kind: "engine", engine: named };
    return "api" in (await this.dev.find(root)) ? { kind: "dev" } : { kind: "folder" };
  }

  private async chooseFrom() {
    const root = this.shown?.root ?? this.projects.roots[0];
    if (!root) {
      void vscode.window.showInformationMessage("Ductus: no project is open.");
      return;
    }
    const dev = "api" in (await this.dev.find(root));
    const choices: (vscode.QuickPickItem & { from: From })[] = [
      {
        label: "This folder",
        description: "ductus history: git (and the database, with DUCTUS_DATABASE_URL)",
        from: { kind: "folder" },
      },
      {
        label: "Your ductus dev",
        description: dev ? "running" : "not running now",
        from: { kind: "dev" },
      },
      ...this.engines.list().map((engine) => ({
        label: engine.name,
        description: engine.url,
        from: { kind: "engine", engine } as From,
      })),
    ];
    const chosen = await vscode.window.showQuickPick(choices, {
      placeHolder: `Where the history of ${path.basename(root)}'s interfaces is read from`,
    });
    if (!chosen) return;
    await this.state.update(stateKey(root), fromKey(chosen.from));
    this.refresh();
  }
}

function stateKey(root: string): string {
  return `ductus.historyFrom:${root}`;
}

function fromKey(from: From): string {
  return from.kind === "engine" ? `engine:${from.engine.name}` : from.kind;
}

function label(from: From): string {
  return from.kind === "folder" ? "this folder" : from.kind === "dev" ? "your ductus dev" : from.engine.name;
}
