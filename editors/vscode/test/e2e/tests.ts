// The extension in a real VS Code on a copy of the guides' project (run.ts): ductus found, the
// tests discovered at their lines, a run's results, a failure at its place with both values,
// Problems on save, a breakpoint hit from the Test Explorer's Debug, and a debug configuration
// with no launch.json. Lines starting `measure:` are read by scripts/demo-vscode.sh.
import * as assert from "node:assert/strict";
import * as fs from "node:fs";
import * as path from "node:path";
import * as vscode from "vscode";
import type { Api } from "../../src/extension";

const root = process.env.DUCTUS_E2E_PROJECT ?? vscode.workspace.workspaceFolders![0].uri.fsPath;
const at = (file: string) => path.join(root, file);
const VISITS = "the lab gets visits, with its own patient class and no Z segments";
let api: Api;

async function waitFor<T>(what: string, check: () => T | undefined | false, ms = 30_000): Promise<T> {
  const until = Date.now() + ms;
  for (;;) {
    const value = check();
    if (value) return value;
    if (Date.now() > until) throw new Error(`waited ${ms / 1000} s for ${what}`);
    await new Promise((r) => setTimeout(r, 100));
  }
}

/** Every `stopped` event any debug session sends, with its top frame's file and line. */
const stops: { file: string; line: number }[] = [];
const sessions = new Set<string>();

function run(items: vscode.TestItem[], debug = false) {
  const tokens = new vscode.CancellationTokenSource();
  return api.tests.run(new vscode.TestRunRequest(items), tokens.token, debug);
}

suiteSetup(async () => {
  const extension = vscode.extensions.getExtension<Api>("ductus.ductus");
  assert.ok(extension, "the extension is installed");
  api = await extension.activate();
  vscode.debug.registerDebugAdapterTrackerFactory("*", {
    createDebugAdapterTracker(session) {
      sessions.add(session.type);
      return {
        async onDidSendMessage(m) {
          // What the debug console shows, in this log (a session that went wrong says why there).
          if (m.type === "event" && m.event === "output" && m.body.category !== "telemetry") {
            console.log(`  [${session.name}] ${String(m.body.output).trimEnd()}`);
          }
          if (m.type !== "event" || m.event !== "stopped") return;
          const trace = await session.customRequest("stackTrace", { threadId: m.body.threadId, levels: 1 });
          const top = trace.stackFrames[0];
          stops.push({ file: top.source?.path ?? "", line: top.line });
          await session.customRequest("continue", { threadId: m.body.threadId });
        },
      };
    },
  });
});

suite("Ductus", () => {
  test("finds ductus and activates", async () => {
    const binary = await api.binary.get();
    assert.ok(binary, "ductus found");
    assert.match(binary.version, /^ductus 0\.\d+\.\d+/);
    console.log(`measure: activation ${api.activationMs} ms`);
    await waitFor("the project", () => api.projects.roots.includes(root));
  });

  test("the tests are found by interface, each at its test(...) line", async () => {
    const item = await waitFor("adt-in's tests", () => api.tests.find(root, "adt-in", VISITS));
    assert.equal(item.uri?.fsPath, at("interfaces/adt-in/lab.test.ts"));
    assert.equal(item.range?.start.line, 3, "line 4, from 0");
    const adt = api.tests.find(root, "adt-in")!;
    assert.ok(adt.children.size >= 4, `adt-in has its tests (${adt.children.size})`);
    await waitFor("the other interfaces' tests", () => api.tests.find(root, "census-in"));
  });

  test("an interface's tests run, each result as it comes", async () => {
    const started = Date.now();
    await run([api.tests.find(root, "adt-in")!]);
    console.log(`measure: adt-in's tests from the Test Explorer ${Date.now() - started} ms`);
    const results: [string, string][] = [];
    api.tests
      .find(root, "adt-in")!
      .children.forEach((t) => results.push([t.label, api.tests.results.get(t.id)?.state ?? "none"]));
    assert.ok(results.length >= 4);
    assert.deepEqual(
      results.filter(([, s]) => s !== "passed"),
      [],
      "every adt-in test passed",
    );
  });

  test("a failure is shown at its expect with the expected and actual values", async () => {
    fs.writeFileSync(
      at("interfaces/adt-in/broken.test.ts"),
      [
        'import { expect, hl7, type Message, test } from "@ductus/sdk";',
        'import { transform } from "./lab.ts";',
        "",
        'test("a failing check shows where and both values", () => {',
        '  const out = transform(hl7("a01.hl7")) as Message;',
        '  expect(out.get("PV1-2")).toBe("I");',
        "});",
        "",
      ].join("\n"),
    );
    await api.tests.discover(root);
    const item = api.tests.find(root, "adt-in", "a failing check shows where and both values");
    assert.ok(item, "the new test is found");
    await run([item]);
    const result = api.tests.results.get(item.id);
    assert.equal(result?.state, "failed");
    assert.equal(result?.expected, '"I"');
    assert.equal(result?.actual, '"IN"');
    assert.equal(result?.location?.uri.fsPath, at("interfaces/adt-in/broken.test.ts"));
    assert.equal(result?.location?.range.start.line, 5, "the expect's line, 6 from 1");
    fs.rmSync(at("interfaces/adt-in/broken.test.ts"));
    await api.tests.discover(root);
    assert.equal(api.tests.find(root, "adt-in", "a failing check shows where and both values"), undefined, "gone");
  });

  test("validate's findings are in Problems when a file is saved, and go when fixed", async () => {
    const file = vscode.Uri.file(at("interfaces/adt-in/interface.yaml"));
    const document = await vscode.workspace.openTextDocument(file);
    const edit = async (from: string, to: string) => {
      const i = document.getText().indexOf(`port: ${from} `) + "port: ".length;
      assert.ok(i > "port: ".length, `port ${from} is in interface.yaml`);
      const e = new vscode.WorkspaceEdit();
      e.replace(file, new vscode.Range(document.positionAt(i), document.positionAt(i + from.length)), to);
      assert.ok(await vscode.workspace.applyEdit(e));
      assert.ok(await document.save());
    };
    const duc122 = () => vscode.languages.getDiagnostics(file).find((d) => d.code === "DUC122");
    const saved = Date.now();
    await edit("6661", "66610");
    const found = await waitFor("DUC122 in Problems", duc122);
    console.log(`measure: save to Problems ${Date.now() - saved} ms`);
    assert.equal(found.severity, vscode.DiagnosticSeverity.Error);
    assert.equal(found.range.start.line, 5, "on the port's line, 6 from 1");
    assert.equal(found.source, "ductus");
    assert.ok(
      vscode.languages
        .getDiagnostics()
        .every(([, list]) => list.every((d) => d.source !== "ductus" || !String(d.code).startsWith("TS"))),
      "no TypeScript findings from ductus (VS Code's own server shows those)",
    );
    await edit("66610", "6661");
    await waitFor("DUC122 gone", () => !duc122());
  });

  test("Debug from the Test Explorer stops at a breakpoint in the code a test calls", async () => {
    const code = vscode.Uri.file(at("interfaces/adt-in/lab.ts"));
    const breakpoint = new vscode.SourceBreakpoint(new vscode.Location(code, new vscode.Position(17, 0)));
    vscode.debug.addBreakpoints([breakpoint]);
    stops.length = 0;
    try {
      const item = api.tests.find(root, "adt-in", VISITS)!;
      await run([item], true);
      assert.deepEqual(stops[0], { file: code.fsPath, line: 18 }, "stopped at lab.ts:18");
      assert.equal(api.tests.results.get(item.id)?.state, "passed", "then the test went on and passed");
      await waitFor("the debug session's end", () => !vscode.debug.activeDebugSession);
    } finally {
      vscode.debug.removeBreakpoints([breakpoint]);
    }
  });

  test("debugging works without launch.json: the Ductus configuration with lab.ts open", async () => {
    // `ductus test` (run above) wrote one, as it always does: gone, to show it isn't needed.
    fs.rmSync(at(".vscode/launch.json"), { force: true });
    const code = vscode.Uri.file(at("interfaces/adt-in/lab.ts"));
    await vscode.window.showTextDocument(code);
    const breakpoint = new vscode.SourceBreakpoint(new vscode.Location(code, new vscode.Position(17, 0)));
    vscode.debug.addBreakpoints([breakpoint]);
    stops.length = 0;
    try {
      // This configuration's own session (the one it becomes: VS Code's JavaScript debugger).
      let ours: vscode.DebugSession | undefined;
      const started = vscode.debug.onDidStartDebugSession((s) => {
        if (!s.parentSession && s.name === "Ductus: debug these tests") ours ??= s;
      });
      const ended = new Promise<void>((resolve) => {
        const d = vscode.debug.onDidTerminateDebugSession((s) => {
          if (s === ours) {
            d.dispose();
            started.dispose();
            resolve();
          }
        });
      });
      const folder = vscode.workspace.workspaceFolders![0];
      const ok = await vscode.debug.startDebugging(folder, {
        type: "ductus",
        request: "launch",
        name: "Ductus: debug these tests",
        command: "test",
        args: ["--test", VISITS],
      });
      assert.ok(ok, "started");
      await ended;
      assert.deepEqual(stops[0], { file: code.fsPath, line: 18 });
    } finally {
      vscode.debug.removeBreakpoints([breakpoint]);
    }
  });
});
