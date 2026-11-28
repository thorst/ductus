// The extension in a real VS Code on a copy of the guides' project (run.ts): ductus found, the
// tests discovered at their lines, a run's results, a failure at its place with both values,
// Problems on save, a breakpoint hit from the Test Explorer's Debug, a debug configuration with no
// launch.json, Deploy to dev against a real `ductus dev`, the History view and Debug this message
// (a message that dev stored, through the web UI's link) against it, Load test this interface
// (`ductus bench interface`), and Promote… across two
// engines in release mode (scripts/demo-vscode.sh starts them). Lines starting `measure:` are read by
// scripts/demo-vscode.sh.
import * as assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import * as fs from "node:fs";
import * as net from "node:net";
import * as path from "node:path";
import * as vscode from "vscode";
import type { Deployed } from "../../src/dev";
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

/** A port nothing listens on now. */
function freePort(): Promise<number> {
  return new Promise((resolve) => {
    const server = net.createServer().listen(0, "127.0.0.1", () => {
      const { port } = server.address() as net.AddressInfo;
      server.close(() => resolve(port));
    });
  });
}

/** One HL7 message over MLLP; resolves with the ACK (sent once the engine has stored it). */
function sendMllp(port: number, message: string): Promise<string> {
  return new Promise((resolve, reject) => {
    const socket = net.connect(port, "127.0.0.1", () => socket.write(`\x0b${message}\x1c\r`));
    let got = "";
    socket.setEncoding("utf8");
    socket.on("data", (d) => {
      got += d;
      if (got.includes("\x1c\r")) {
        socket.end();
        resolve(got);
      }
    });
    socket.on("error", reject);
    socket.setTimeout(30_000, () => {
      socket.destroy();
      reject(new Error(`no ACK from 127.0.0.1:${port} in 30 s`));
    });
  });
}

/**
 * The `ductus dev` the Deploy to dev test starts in a project of its own: the History and Debug
 * this message test after it uses it, then stops it.
 */
let devEngine:
  { project: string; mllp: number[]; said: () => string; ended: () => boolean; stop: () => Promise<void> } | undefined;

suiteTeardown(async () => {
  await devEngine?.stop();
});

/** Every `stopped` event any debug session sends, with its top frame's file and line. */
const stops: { file: string; line: number }[] = [];
const sessions = new Set<string>();

function run(items: vscode.TestItem[], debug = false) {
  const tokens = new vscode.CancellationTokenSource();
  return api.tests.run(new vscode.TestRunRequest(items), tokens.token, debug);
}

suiteSetup(async () => {
  const extension = vscode.extensions.getExtension<Api>("getductus.ductus");
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

  test("Load test this interface: the bench's report, and what limits it", async () => {
    const done = await api.load.run(root, "results-to-fhir", ["--seconds", "1"]);
    assert.ok("limit" in done, `ran: ${JSON.stringify(done)}`);
    assert.match(done.limit, /^The code is the limit: [\d,]+ msg\/s a thread/);
    const failed = await api.load.run(root, "no-such-interface");
    assert.ok("why" in failed && failed.why.includes("`no-such-interface` isn't an interface"), JSON.stringify(failed));
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

  test("interface.yaml and its code as one: a lens on the function, F12 from `script:`, the path a link", async () => {
    await waitFor("validate's uses", () => (api.problems.uses.get(root) ?? []).length > 0);
    const code = vscode.Uri.file(at("interfaces/adt-in/lab.ts"));
    const yaml = vscode.Uri.file(at("interfaces/adt-in/interface.yaml"));
    // VS Code asks providers only about documents it has open ("Illegal argument" otherwise).
    const labDocument = await vscode.workspace.openTextDocument(code);
    await vscode.workspace.openTextDocument(yaml);
    const ours0 = performance.now();
    api.wiring.provideCodeLenses(labDocument);
    console.log(`measure: our lenses for lab.ts ${(performance.now() - ours0).toFixed(2)} ms`);
    const started = Date.now();
    const lenses = await vscode.commands.executeCommand<vscode.CodeLens[]>("vscode.executeCodeLensProvider", code);
    // VS Code's command waits for every provider, TypeScript's own reference lenses included.
    console.log(`measure: every provider's lenses for lab.ts ${Date.now() - started} ms`);
    const ours = lenses.filter((l) => l.command?.command === "editor.action.showReferences");
    assert.deepEqual(
      ours.map((l) => [l.range.start.line, l.command?.title]),
      [[9, "used by adt-in → lab"]],
      "above `export function transform`, line 10 from 1",
    );
    const [, , places] = ours[0].command!.arguments as [vscode.Uri, vscode.Position, vscode.Location[]];
    assert.deepEqual(
      places.map((p) => [p.uri.fsPath, p.range.start.line]),
      [[yaml.fsPath, 11]],
      "peeks `script: lab.ts`",
    );

    // F12 on `lab.ts` in `script: lab.ts` (line 12, the value from column 13).
    const found = await vscode.commands.executeCommand<(vscode.Location | vscode.LocationLink)[]>(
      "vscode.executeDefinitionProvider",
      yaml,
      new vscode.Position(11, 14),
    );
    const target = found.map((f) =>
      "targetUri" in f ? { uri: f.targetUri, range: f.targetRange } : { uri: f.uri, range: f.range },
    );
    assert.deepEqual(
      target.map(({ uri, range }) => [uri.fsPath, range.start.line, range.start.character]),
      [[code.fsPath, 9, 16]],
      "the function's name",
    );

    const links = await vscode.commands.executeCommand<vscode.DocumentLink[]>("vscode.executeLinkProvider", yaml);
    const files = links.filter((l) => l.target?.scheme === "file").map((l) => [l.range.start.line, l.target!.fsPath]);
    assert.deepEqual(files, [
      [11, code.fsPath],
      [15, at("interfaces/adt-in/billing.ts")],
    ]);
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

  test("Deploy to dev: a running ductus dev takes a change in place, by the command and on save", async function () {
    this.timeout(240_000);
    const binary = await api.binary.get();
    assert.ok(binary);
    const port = await freePort();
    // The guides' project needs its environment's secrets to run: a small project of its own in
    // the workspace (as scripts/demo-debug.sh does), `adt` and `other` beside it.
    const project = at("dev-project");
    const mllp = [await freePort(), await freePort()];
    for (const [i, name] of ["adt", "other"].entries()) {
      fs.mkdirSync(path.join(project, "interfaces", name), { recursive: true });
      fs.writeFileSync(
        path.join(project, "interfaces", name, "interface.yaml"),
        `name: ${name}\nsource: { mllp: { host: 127.0.0.1, port: ${mllp[i]} } }\ndestinations: { out: { file: { dir: out }, script: route.ts } }\n`,
      );
      fs.writeFileSync(
        path.join(project, "interfaces", name, "route.ts"),
        'import type { Message } from "@ductus/sdk";\n\nexport function transform(msg: Message) {\n  msg.set("MSH-5", "LAB");\n  return msg;\n}\n',
      );
    }
    await api.projects.scan();
    assert.ok(api.projects.roots.includes(project), "a project of its own");
    const dev = spawn(binary.path, ["dev", "--threads", "1", "--api-listen", `127.0.0.1:${port}`], {
      cwd: project,
      stdio: ["ignore", "pipe", "pipe"],
    });
    let said = "";
    dev.stdout.on("data", (d) => (said += d));
    dev.stderr.on("data", (d) => (said += d));
    let ended = false;
    const exited = new Promise<number | null>((resolve) =>
      dev.on("exit", (code) => {
        ended = true;
        resolve(code);
      }),
    );
    devEngine = {
      project,
      mllp,
      said: () => said,
      ended: () => ended,
      stop: async () => {
        devEngine = undefined;
        dev.kill("SIGTERM");
        await exited;
      },
    };
    const found = path.join(project, ".ductus/dev-engine.json");
    try {
      await waitFor("ductus dev's dev-engine.json", () => fs.existsSync(found) || ended, 180_000);
      assert.ok(!ended, `ductus dev ended:\n${said}`);
      const code = path.join(project, "interfaces/adt/route.ts");
      const original = fs.readFileSync(code, "utf8");
      try {
        // Changed in the editor as a person would (writing it behind VS Code's back makes the
        // editor's next save fail: "File Modified Since").
        const document = await vscode.workspace.openTextDocument(vscode.Uri.file(code));
        await vscode.window.showTextDocument(document);
        const append = async (line: string) => {
          const edit = new vscode.WorkspaceEdit();
          edit.insert(document.uri, document.positionAt(document.getText().length), `${line}\n`);
          assert.ok(await vscode.workspace.applyEdit(edit));
          assert.ok(await document.save());
        };
        // The command (as the editor title's button runs it).
        await append("// deployed by the command");
        const started = Date.now();
        const deployed = await api.dev.deploy(project);
        console.log(`measure: Deploy to dev of one interface ${Date.now() - started} ms`);
        assert.equal(deployed.error, undefined, said);
        assert.equal(deployed.report?.applied, "in_place", JSON.stringify(deployed.report));
        assert.deepEqual(
          deployed.report?.changes?.map((c) => c.interface),
          ["adt"],
          "adt alone",
        );

        // On save, with the setting.
        await vscode.workspace.getConfiguration("ductus").update("deployToDevOnSave", true);
        const next = new Promise<Deployed>((resolve) => {
          const d = api.dev.onDidDeploy((e) => {
            d.dispose();
            resolve(e);
          });
        });
        await append("// deployed on save");
        const onSave = await next;
        assert.equal(onSave.report?.applied, "in_place", JSON.stringify(onSave));
      } finally {
        await vscode.workspace.getConfiguration("ductus").update("deployToDevOnSave", undefined);
        fs.writeFileSync(code, original);
      }
    } catch (e) {
      // Left running for the next test only when this one passed.
      await devEngine?.stop();
      throw e;
    }
  });

  test("History and Debug this message: dev's reloads listed, a stored message stopped at a breakpoint", async function () {
    this.timeout(180_000);
    assert.ok(devEngine && !devEngine.ended(), "the Deploy to dev test's ductus dev runs");
    const { project, mllp } = devEngine;
    const route = vscode.Uri.file(path.join(project, "interfaces/adt/route.ts"));
    const breakpoint = new vscode.SourceBreakpoint(new vscode.Location(route, new vscode.Position(3, 0)));
    try {
      // The History view, from the running dev (where it reads while dev runs): the test before's
      // two deploys are reloads there.
      let started = Date.now();
      const page = await api.history.show(project, "adt");
      console.log(`measure: History of an interface from ductus dev ${Date.now() - started} ms`);
      assert.ok("entries" in page, JSON.stringify(page));
      const reloads = page.entries.filter((e) => e.deployment?.how === "reload");
      assert.ok(reloads.length >= 2, page.entries.map((e) => `${e.kind}: ${e.what}`).join("\n"));
      assert.equal(api.history.shownEntries.length, page.entries.length, "the view shows the page");
      // The folder's own, `ductus history --json`: not in git here, said in its notes.
      started = Date.now();
      const folder = await api.history.show(project, "adt", { kind: "folder" });
      console.log(`measure: History of an interface from the folder ${Date.now() - started} ms`);
      assert.ok("entries" in folder, JSON.stringify(folder));
      assert.ok(
        folder.notes.some((n) => /git/.test(n)),
        folder.notes.join("\n"),
      );

      // A message dev stored, debugged as the web UI's link opens it.
      const ack = await sendMllp(
        mllp[0],
        "MSH|^~\\&|ADT|HOSP|LAB|HOSP|20261120120000||ADT^A01|VSCODE1|P|2.5\rPID|1||100001^^^MRN||DOE^JANE\r",
      );
      assert.match(ack, /MSA\|AA/, ack);
      const found = await api.dev.find(project);
      assert.ok("api" in found, JSON.stringify(found));
      const listed = await fetch(`${found.api}/api/v1/interfaces/adt/messages?limit=1`, {
        headers: { authorization: `Bearer ${found.token}` },
      });
      const id = ((await listed.json()) as { messages: { id: number }[] }).messages[0]?.id;
      assert.ok(id, "the message is stored");

      vscode.debug.addBreakpoints([breakpoint]);
      stops.length = 0;
      const name = `Ductus: debug message ${id}`;
      let ours: vscode.DebugSession | undefined;
      const begun = vscode.debug.onDidStartDebugSession((s) => {
        if (!s.parentSession && s.name === name) ours ??= s;
      });
      const ended = new Promise<void>((resolve) => {
        const d = vscode.debug.onDidTerminateDebugSession((s) => {
          if (s === ours) {
            d.dispose();
            begun.dispose();
            resolve();
          }
        });
      });
      // The link names the engine as the browser reached it: `localhost` is dev's 127.0.0.1.
      const link = new URLSearchParams({
        engine: found.api.replace("127.0.0.1", "localhost"),
        interface: "adt",
        message: String(id),
      });
      started = Date.now();
      assert.ok(await api.messages.fromLink(link.toString()), "the debugger started");
      await ended;
      console.log(`measure: Debug this message, the link to the session's end ${Date.now() - started} ms`);
      assert.deepEqual(stops[0], { file: route.fsPath, line: 4 }, "stopped at route.ts:4");
    } finally {
      vscode.debug.removeBreakpoints([breakpoint]);
      await devEngine?.stop();
    }
    assert.ok(!fs.existsSync(path.join(project, ".ductus/dev-engine.json")), "dev-engine.json goes when dev stops");
    // No dev engine: said, nothing else.
    const none = await api.dev.deploy(project, false);
    assert.match(none.error ?? "", /no `ductus dev` is running/);
  });

  test("Promote…: a release file to test, then from test to prod; each token asked once, checked first", async function () {
    this.timeout(300_000);
    // Two engines in release mode (test, and prod requiring test first), started by
    // scripts/demo-vscode.sh with a deploy token on each and a viewer's on prod.
    const given = JSON.parse(process.env.DUCTUS_E2E_ENGINES || "null") as {
      test: { url: string; token: string };
      prod: { url: string; token: string; viewer: string };
    } | null;
    assert.ok(given, "DUCTUS_E2E_ENGINES (scripts/demo-vscode.sh starts the engines)");
    const binary = await api.binary.get();
    assert.ok(binary);

    // A project of its own (its folder's name is the release's project), its port per environment.
    const project = at("promote-project");
    const feed = "vscode-promote-feed";
    fs.mkdirSync(path.join(project, "interfaces", feed), { recursive: true });
    fs.mkdirSync(path.join(project, "environments"), { recursive: true });
    fs.writeFileSync(
      path.join(project, "interfaces", feed, "interface.yaml"),
      `name: ${feed}\nsource: { mllp: { host: 127.0.0.1, port: \${MLLP_PORT} } }\ndestinations: { out: { file: { dir: out }, script: route.ts } }\n`,
    );
    fs.writeFileSync(
      path.join(project, "interfaces", feed, "route.ts"),
      'import type { Message } from "@ductus/sdk";\n\nexport function transform(msg: Message) {\n  msg.set("MSH-5", "LAB");\n  return msg;\n}\n',
    );
    for (const environment of ["dev", "test", "prod"]) {
      fs.writeFileSync(
        path.join(project, "environments", `${environment}.yaml`),
        `variables:\n  MLLP_PORT: ${await freePort()}\n`,
      );
    }
    await api.projects.scan();
    // `ductus release`, as a developer runs it in the terminal (no database needed).
    const environment = { ...process.env };
    delete environment.DUCTUS_DATABASE_URL;
    const made = spawnSync(binary.path, ["release", "-m", "the feed"], {
      cwd: project,
      env: environment,
      encoding: "utf8",
    });
    assert.equal(made.status, 0, `${made.stdout}${made.stderr}`);
    const files = fs.readdirSync(path.join(project, ".ductus/releases"));
    assert.equal(files.length, 1, files.join(", "));
    const file = path.join(project, ".ductus/releases", files[0]);

    // The engines by name, and the token prompt answered here (prod: a viewer's first).
    const settings = vscode.workspace.getConfiguration("ductus");
    await settings.update("engines", { test: given.test.url, prod: given.prod.url });
    const asked: string[] = [];
    const answers = new Map([
      [given.test.url, [given.test.token]],
      [given.prod.url, [given.prod.viewer, given.prod.token]],
    ]);
    const prompt = api.engines.ask;
    api.engines.ask = async (engine, need, why) => {
      asked.push(`${engine.name} ${need}${why ? `: ${why}` : ""}`);
      return answers.get(engine.url)?.shift();
    };
    try {
      const engines = api.engines.list();
      const test = engines.find((e) => e.name === "test")!;
      const prod = engines.find((e) => e.name === "prod")!;
      assert.ok(test && prod, JSON.stringify(engines));

      // 1. The file straight to prod: refused by prod's rules. The viewer's token was turned away
      //    before anything ran, and asked for again.
      const refused = await api.promote.promote({ root: project, release: file, to: prod, reason: "the feed" });
      assert.equal(refused.report?.ok, false, JSON.stringify(refused));
      assert.match(refused.report?.rules?.[0] ?? "", /deployed to test first/);
      assert.equal(asked.length, 2, asked.join("\n"));
      assert.match(asked[1], /^prod deploy: .*viewer's token/);

      // 2. The file to test: deployed, its record written into the file.
      let started = Date.now();
      const toTest = await api.promote.promote({ root: project, release: file, to: test, reason: "the feed" });
      console.log(`measure: Promote a release file to test ${Date.now() - started} ms`);
      assert.equal(toTest.report?.ok, true, JSON.stringify(toTest));
      assert.equal(toTest.report?.environment, "test");
      assert.deepEqual(asked.slice(2), ["test deploy"]);
      const short = toTest.report!.short!;

      // Test's releases, as the picker lists them: this project's.
      const signed = await api.engines.signIn(test, "download");
      assert.ok(signed, "test's token kept");
      const listed = await api.engines.get(signed, "/releases?limit=50");
      assert.ok("body" in listed && listed.body.includes(`"project":"promote-project"`), JSON.stringify(listed));

      // 3. From test to prod by its id: both tokens kept (nothing asked), prod's rules met.
      started = Date.now();
      const toProd = await api.promote.promote({
        root: project,
        release: short,
        from: test,
        to: prod,
        reason: "tested in test",
      });
      console.log(`measure: Promote from test to prod ${Date.now() - started} ms`);
      assert.equal(toProd.report?.ok, true, JSON.stringify(toProd));
      assert.equal(toProd.report?.environment, "prod");
      assert.equal(toProd.promotion?.from, test.url);
      assert.equal(asked.length, 3, `nothing more asked: ${asked.join("\n")}`);

      // 4. Forgotten: the next use asks again.
      assert.ok(await api.engines.forget(prod));
      assert.ok(!(await api.engines.forget(prod)), "gone");
    } finally {
      api.engines.ask = prompt;
      await settings.update("engines", undefined);
    }
  });
});
