import * as fs from "node:fs";
import * as path from "node:path";
import * as vscode from "vscode";
import { definitionAt, lensesFor, type Place, scriptPaths } from "./lines";
import type { Problems } from "./problems";
import type { Projects } from "./projects";

/**
 * interface.yaml and the code it names, read as one: a code lens above each exported function an
 * interface runs ("used by adt-in → lab"; clicking it peeks those places in interface.yaml),
 * go-to-definition from a `transform:` or `script:` value to the function, and `script:` paths as
 * links to their files. From `validate --json`'s `uses`, which the Problems view's run gets anyway
 * (on opening the project and each save): no process of its own. A file with unsaved changes gets
 * none (its lines may have moved since the run); they're back after the save's run.
 */
export class Wiring implements vscode.CodeLensProvider, vscode.DefinitionProvider, vscode.DocumentLinkProvider {
  private readonly changed = new vscode.EventEmitter<void>();
  readonly onDidChangeCodeLenses = this.changed.event;
  private readonly disposables: vscode.Disposable[];

  constructor(
    private readonly problems: Problems,
    private readonly projects: Projects,
  ) {
    const code: vscode.DocumentSelector = [
      { scheme: "file", language: "typescript" },
      { scheme: "file", language: "javascript" },
    ];
    const yaml: vscode.DocumentSelector = { scheme: "file", pattern: "**/interfaces/*/interface.yaml" };
    this.disposables = [
      this.changed,
      vscode.languages.registerCodeLensProvider(code, this),
      vscode.languages.registerDefinitionProvider(yaml, this),
      vscode.languages.registerDocumentLinkProvider(yaml, this),
      problems.onDidValidate(() => this.changed.fire()),
      // A saved file's lenses come back with the save's run; one changed back to as saved, now.
      vscode.workspace.onDidChangeTextDocument((e) => {
        if (!e.document.isDirty || e.contentChanges.length === 0) this.changed.fire();
      }),
    ];
  }

  dispose() {
    this.disposables.forEach((d) => d.dispose());
  }

  /** The project a document is in, and its path there with `/`. */
  private where(document: vscode.TextDocument): { root: string; file: string } | undefined {
    const root = this.projects.rootOf(document.uri.fsPath);
    if (!root) return undefined;
    return { root, file: path.relative(root, document.uri.fsPath).split(path.sep).join("/") };
  }

  provideCodeLenses(document: vscode.TextDocument): vscode.CodeLens[] {
    const at = this.where(document);
    if (!at || document.isDirty) return [];
    return lensesFor(this.problems.uses.get(at.root) ?? [], at.file).map((lens) => {
      const position = new vscode.Position(lens.line - 1, lens.column - 1);
      const locations = lens.targets.map((t) => location(at.root, t));
      return new vscode.CodeLens(new vscode.Range(position, position), {
        title: lens.title,
        tooltip: "Where interface.yaml names this function",
        command: "editor.action.showReferences",
        arguments: [document.uri, position, locations],
      });
    });
  }

  provideDefinition(document: vscode.TextDocument, position: vscode.Position): vscode.Location | undefined {
    const at = this.where(document);
    if (!at || document.isDirty) return undefined;
    const uses = this.problems.uses.get(at.root) ?? [];
    const found = definitionAt(uses, at.file, position.line + 1, position.character + 1);
    return found && location(at.root, found);
  }

  provideDocumentLinks(document: vscode.TextDocument): vscode.DocumentLink[] {
    const dir = path.dirname(document.uri.fsPath);
    return scriptPaths(document.getText()).flatMap((s) => {
      const file = path.resolve(dir, s.path);
      if (!fs.existsSync(file)) return []; // validate says so (DUC125): no link to nowhere
      const range = new vscode.Range(s.line - 1, s.column - 1, s.line - 1, s.end - 1);
      const link = new vscode.DocumentLink(range, vscode.Uri.file(file));
      link.tooltip = "Open the file";
      return [link];
    });
  }
}

function location(root: string, p: Place): vscode.Location {
  const start = new vscode.Position(p.line - 1, p.column - 1);
  const end = p.end_line && p.end_column ? new vscode.Position(p.end_line - 1, p.end_column - 1) : start;
  return new vscode.Location(vscode.Uri.file(path.join(root, p.file)), new vscode.Range(start, end));
}
