import * as vscode from "vscode";
import { Finder } from "./binary";
import { DebugConfigurations } from "./debug";
import { Dev } from "./dev";
import { Engines } from "./engines";
import { Problems } from "./problems";
import { Projects } from "./projects";
import { Promote } from "./promote";
import { Tests } from "./testing";

/** What `activate` returns: for the extension's own tests, not a public API. */
export interface Api {
  projects: Projects;
  tests: Tests;
  problems: Problems;
  dev: Dev;
  engines: Engines;
  promote: Promote;
  binary: Finder;
  /** How long `activate` took, in ms (measured, reported in the output). */
  activationMs: number;
}

/**
 * Activated when the workspace has a Ductus project (an `interfaces/<name>/interface.yaml`) or a
 * `ductus` debug configuration starts. Nothing here waits for `ductus`: the projects are found
 * and the first checks start after `activate` has returned.
 */
export function activate(context: vscode.ExtensionContext): Api {
  const started = performance.now();
  const log = vscode.window.createOutputChannel("Ductus", { log: true });
  const binary = new Finder(log);
  const projects = new Projects();
  const tests = new Tests(binary, projects, log);
  const problems = new Problems(binary, projects, log);
  const dev = new Dev(binary, projects, log);
  const engines = new Engines(context.secrets, log);
  const promote = new Promote(binary, projects, engines, log);
  context.subscriptions.push(
    log,
    binary,
    projects,
    tests,
    problems,
    dev,
    engines,
    promote,
    DebugConfigurations.register(binary, projects),
    vscode.commands.registerCommand("ductus.validate", async () => {
      await Promise.all(projects.roots.map((r) => problems.validate(r)));
    }),
    vscode.commands.registerCommand("ductus.showOutput", () => log.show()),
  );
  const activationMs = Math.round((performance.now() - started) * 10) / 10;
  log.info(`activated in ${activationMs} ms`);

  // Beside the activation, never in its path: find the projects, then check each once.
  void projects.scan().then(() => projects.roots.forEach((root) => problems.schedule(root, 0)));
  return { projects, tests, problems, dev, engines, promote, binary, activationMs };
}

export function deactivate() {}
