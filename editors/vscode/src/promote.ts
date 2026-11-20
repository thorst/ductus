import * as fs from "node:fs/promises";
import * as path from "node:path";
import * as vscode from "vscode";
import type { Finder } from "./binary";
import { type Engines, type Signed } from "./engines";
import {
  type DeployReport,
  type NamedEngine,
  type Promotion,
  type StoredRelease,
  deployArgs,
  deployLines,
  deploySummary,
  firstParagraph,
  parseDeploy,
  parseReleases,
  releaseFile,
} from "./lines";
import { output } from "./process";
import { type Projects, chooseRoot } from "./projects";

/** How many of an engine's releases the picker lists (newest first). */
const LISTED = 50;

/** Where `ductus release` writes its files in a project. */
const RELEASES = path.join(".ductus", "releases");

/** What a promotion did (for the extension's tests). */
export interface Promoted {
  /** What `ductus deploy` was asked (no tokens). */
  promotion?: Promotion;
  /** Its report; undefined when there was none (refused before it ran, or it failed). */
  report?: DeployReport;
  /** Why there's no report. */
  error?: string;
}

/** What to promote, and where (the command picks these; the extension's tests give them). */
export interface Choice {
  root: string;
  /** A release file's path, or a release id stored on `from` (or on `to`, without `from`). */
  release: string;
  /** The engine `release` (an id) is fetched from; none: a file, or a release `to` has. */
  from?: NamedEngine;
  to: NamedEngine;
  reason: string;
}

/**
 * **Promote…**: a release to one of the engines in `ductus.engines`, as `ductus deploy` does it
 * (it runs that, with the tokens in its environment): picked from the project's release files
 * (`.ductus/releases`) or from an engine's stored releases (fetched there with `--from`), then
 * the engine it goes to and the reason. The engine holds it to its environment's rules and the
 * "replaces" check, runs the gate, and applies it; the report (the gate's findings, what changed)
 * goes to the output, the outcome to a notification.
 */
export class Promote implements vscode.Disposable {
  private readonly disposables: vscode.Disposable[] = [];
  private readonly promoted = new vscode.EventEmitter<Promoted>();
  /** After each promotion (for the extension's tests). */
  readonly onDidPromote = this.promoted.event;

  constructor(
    private readonly binary: Finder,
    private readonly projects: Projects,
    private readonly engines: Engines,
    private readonly log: vscode.LogOutputChannel,
  ) {
    this.disposables.push(
      this.promoted,
      vscode.commands.registerCommand("ductus.promote", (uri?: vscode.Uri) => this.command(uri)),
    );
  }

  dispose() {
    this.disposables.forEach((d) => d.dispose());
  }

  /** Promote without asking anything but tokens not kept yet. */
  async promote(choice: Choice): Promise<Promoted> {
    const from =
      choice.from && choice.from.url !== choice.to.url ? await this.engines.signIn(choice.from, "download") : undefined;
    if (choice.from && choice.from.url !== choice.to.url && !from) {
      return this.done({ error: `no token for ${choice.from.name}` });
    }
    const to = await this.engines.signIn(choice.to, "deploy");
    if (!to) return this.done({ error: `no token for ${choice.to.name}` });
    return this.run(choice.root, choice.release, from, to, choice.reason);
  }

  private async command(uri?: vscode.Uri) {
    const root = await chooseRoot(this.projects, uri, "Promote", "The project whose release to promote");
    if (!root) return;
    const engines = this.engines.list();
    if (engines.length === 0) {
      const chosen = await vscode.window.showWarningMessage(
        'Promote: name your engines first, in the setting `ductus.engines` ({ "test": "https://ductus-test:7700", … }).',
        "Open the setting",
      );
      if (chosen) void vscode.commands.executeCommand("workbench.action.openSettings", "ductus.engines");
      return;
    }

    // 1. Where the release is: this project's files, or an engine.
    const files = await releaseFiles(root);
    const source = await vscode.window.showQuickPick(
      [
        {
          label: "$(file-zip) Release files in this project",
          description: `${files.length} in ${RELEASES.replace(/\\/g, "/")}`,
          engine: undefined as NamedEngine | undefined,
        },
        ...engines.map((e) => ({
          label: `$(server) On ${e.name}`,
          description: e.url,
          engine: e as NamedEngine | undefined,
        })),
      ],
      { title: "Promote: where is the release?", placeHolder: "A file `ductus release` wrote, or one an engine has" },
    );
    if (!source) return;

    // 2. Which release.
    let release: string;
    let short: string;
    let from: Signed | undefined;
    if (!source.engine) {
      if (files.length === 0) {
        void vscode.window.showWarningMessage(
          `Promote: no release files in ${RELEASES.replace(/\\/g, "/")}: make one with \`ductus release -m "why"\` in the project's folder.`,
        );
        return;
      }
      const picked = await vscode.window.showQuickPick(
        files.map((f) => ({
          label: `$(file-zip) ${f.short}`,
          description: `written ${f.written.toLocaleString()}`,
          detail: path.relative(root, f.path).replace(/\\/g, "/"),
          file: f,
        })),
        { title: "Promote: which release?", placeHolder: "Newest first" },
      );
      if (!picked) return;
      release = picked.file.path;
      short = picked.file.short;
    } else {
      from = await this.engines.signIn(source.engine, "download");
      if (!from) return;
      const listed = await this.engines.get(from, `/releases?limit=${LISTED}`);
      const releases = "body" in listed ? parseReleases(listed.body) : undefined;
      if (!releases) {
        void vscode.window.showErrorMessage(
          `Promote: ${"why" in listed ? listed.why : "its answer isn't a list of releases"}`,
        );
        return;
      }
      const project = path.basename(root);
      const ours = releases.filter((r) => r.project === project);
      if (ours.length === 0) {
        void vscode.window.showWarningMessage(
          `Promote: ${source.engine.name} has no release of ${project}` +
            (releases.length > 0 ? ` (it has ${releases.length} of other projects)` : "") +
            ": deploy one there first.",
        );
        return;
      }
      const picked = await vscode.window.showQuickPick(
        ours.map((r) => ({
          label: `${r.running ? "$(play) " : ""}${r.short}`,
          description: r.reason ?? "",
          detail: describe(r),
          release: r,
        })),
        {
          title: `Promote: which of ${source.engine.name}'s releases?`,
          placeHolder: "Newest first; ▷ runs there now",
          matchOnDescription: true,
        },
      );
      if (!picked) return;
      release = picked.release.id;
      short = picked.release.short;
    }

    // 3. Where it goes.
    const target = await vscode.window.showQuickPick(
      // The engine the release is on too: one CI stored there, deployed from here.
      engines.map((e) => ({
        label: e.name,
        description: e.url === source.engine?.url ? `${e.url} (where it's stored)` : e.url,
        engine: e,
      })),
      { title: `Promote ${short}: to which engine?` },
    );
    if (!target) return;
    const to = await this.engines.signIn(target.engine, "deploy");
    if (!to) return;
    const info = await this.engines.info(to);
    if ("why" in info) {
      void vscode.window.showErrorMessage(`Promote: ${info.why}`);
      return;
    }
    if (!info.releases) {
      void vscode.window.showErrorMessage(
        `Promote: ${target.engine.name} runs a project folder, not releases (\`ductus run --releases\`): it takes no deploys.`,
      );
      return;
    }

    // 4. Why (required, except to an environment named dev).
    const required = info.environment !== "dev";
    const reason = await vscode.window.showInputBox({
      title: `Promote ${short} to ${target.engine.name} (environment \`${info.environment}\`)`,
      prompt: `Why: recorded with the deployment, in its history and the audit log${required ? "" : " (optional for dev)"}. Enter deploys it.`,
      placeHolder: "lab wants PV1-3 mapped",
      ignoreFocusOut: true,
      validateInput: (v) => (required && !v.trim() ? `${info.environment} needs a reason` : undefined),
    });
    if (reason === undefined) return;
    const sameEngine = from && from.engine.url === to.engine.url;
    await this.run(root, release, sameEngine ? undefined : from, to, reason);
  }

  /** Run `ductus deploy` for a promotion and say how it went. */
  private async run(
    root: string,
    release: string,
    from: Signed | undefined,
    to: Signed,
    reason: string,
  ): Promise<Promoted> {
    const binary = await this.binary.get();
    if (!binary) return this.done({ error: "no ductus to run" });
    const promotion: Promotion = { release, from: from?.engine.url, to: to.engine.url, reason };
    const args = deployArgs(promotion);
    const env: Record<string, string> = { DUCTUS_API_TOKEN: to.token };
    if (from) env.DUCTUS_FROM_TOKEN = from.token;
    const name = releaseFile(path.basename(release))?.short ?? release.slice(0, 12);
    const where = `${to.engine.name}${from ? ` from ${from.engine.name}` : ""}`;
    this.log.info(`promote ${name} to ${where}: ductus ${args.map(quoteArg).join(" ")} (as ${to.session.name})`);
    const ran = await vscode.window.withProgress(
      {
        location: vscode.ProgressLocation.Notification,
        title: `Promoting ${name} to ${to.engine.name}`,
      },
      async (progress) => {
        progress.report({ message: "the gate runs (or a recent check is reused), then it's applied…" });
        return output(binary.path, args, root, env);
      },
    );
    const report = parseDeploy(ran.stdout);
    if (!report) {
      // `Error: what\n\nCaused by: …` (anyhow): the first part in the notification, all of it logged.
      const said = ran.stderr.trim().replace(/^Error: /, "");
      const why = ran.error?.message ?? (firstParagraph(ran.stderr) || `exit ${ran.code}`);
      this.log.error(`promote ${name} to ${where}: ${said || why}`);
      void vscode.window
        .showErrorMessage(`Promote to ${to.engine.name}: ${why}`, "Show the output")
        .then(this.showOutput);
      return this.done({ promotion, error: why });
    }
    const lines = deployLines(report).join("\n");
    if (report.ok) this.log.info(`${lines}\n(${ran.ms} ms)`);
    else this.log.error(lines);
    const summary = deploySummary(report);
    const inEnvironment = report.environment ? ` (${report.environment})` : "";
    if (report.ok) {
      void vscode.window
        .showInformationMessage(
          `Promoted ${report.short ?? name} to ${to.engine.name}${inEnvironment}: ${summary}`,
          "Show the report",
        )
        .then(this.showOutput);
    } else {
      void vscode.window
        .showErrorMessage(`Promote to ${to.engine.name}${inEnvironment} refused: ${summary}`, "Show the report")
        .then(this.showOutput);
    }
    return this.done({ promotion, report });
  }

  private done(promoted: Promoted): Promoted {
    this.promoted.fire(promoted);
    return promoted;
  }

  private readonly showOutput = (chosen: string | undefined) => {
    if (chosen) this.log.show();
  };
}

/** A release file of the project's. */
interface ReleaseFileFound {
  path: string;
  short: string;
  written: Date;
}

/** The project's release files, newest first. */
async function releaseFiles(root: string): Promise<ReleaseFileFound[]> {
  const dir = path.join(root, RELEASES);
  let names: string[];
  try {
    names = await fs.readdir(dir);
  } catch {
    return [];
  }
  const found: ReleaseFileFound[] = [];
  for (const name of names) {
    const parsed = releaseFile(name);
    if (!parsed) continue;
    try {
      const stat = await fs.stat(path.join(dir, name));
      found.push({ path: path.join(dir, name), short: parsed.short, written: stat.mtime });
    } catch {
      // gone since
    }
  }
  return found.sort((a, b) => b.written.getTime() - a.written.getTime());
}

/** A stored release in one line: who made it when, its branch, where it went. */
function describe(r: StoredRelease): string {
  const parts: string[] = [];
  const made = r.made_at ? new Date(r.made_at).toLocaleString() : undefined;
  if (r.made_by || made) parts.push(`made${r.made_by ? ` by ${r.made_by}` : ""}${made ? ` ${made}` : ""}`);
  if (r.branches?.length) parts.push(`branch ${r.branches.join(", ")}`);
  parts.push(r.environments?.length ? `deployed to ${r.environments.join(", ")}` : "deployed nowhere yet");
  if (r.composed_from) parts.push(`some interfaces of ${r.composed_from.slice(0, 12)}`);
  return parts.join(" · ");
}

/** An argument as the output shows it (quoted when it has spaces). */
function quoteArg(a: string): string {
  return /[\s"]/.test(a) ? JSON.stringify(a) : a;
}
