// The parts of the extension that don't need VS Code: run by `npm run check` under plain Node.
import assert from "node:assert/strict";
import { test } from "node:test";
import {
  LineBuffer,
  atLeast,
  deployArgs,
  deployLines,
  deploySummary,
  lacks,
  parseDeploy,
  parseEngines,
  parseReleases,
  parseSession,
  releaseFile,
  parseDevEngine,
  parseReload,
  parseTestLine,
  parseValidate,
  parseVersion,
  plan,
  problemsByFile,
  reloadSummary,
  testArgs,
  type ValidateDocument,
} from "../../src/lines.ts";

test("ductus.engines: names to URLs in order; a wrong entry left out and said", () => {
  assert.deepEqual(parseEngines({ test: "https://ductus-test:7700/", prod: " http://10.0.0.5:7700 " }), {
    engines: [
      { name: "test", url: "https://ductus-test:7700" },
      { name: "prod", url: "http://10.0.0.5:7700" },
    ],
    problems: [],
  });
  const wrong = parseEngines({ test: "ductus-test:7700", prod: 7700 });
  assert.deepEqual(wrong.engines, []);
  assert.equal(wrong.problems.length, 2);
  assert.match(wrong.problems[0], /test's URL isn't one/);
  assert.deepEqual(parseEngines(undefined), { engines: [], problems: [] });
  assert.match(parseEngines(["https://x"]).problems[0], /a map of names to URLs/);
});

test("a token may deploy, or fetch releases, as its role and permissions say", () => {
  const session = (role: string, permissions: string[]) =>
    parseSession(JSON.stringify({ kind: "token", name: "ci", role, permissions, ends_at: null }))!;
  assert.equal(lacks(session("operator", ["deploy"]), "deploy"), undefined);
  assert.equal(lacks(session("admin", ["payloads", "audit", "deploy", "download"]), "deploy"), undefined);
  assert.match(lacks(session("viewer", ["download"]), "deploy")!, /viewer's token/);
  assert.match(lacks(session("operator", []), "deploy")!, /lacks the `deploy` permission/);
  // Fetching: `download`, or an operator's `deploy`.
  assert.equal(lacks(session("viewer", ["download"]), "download"), undefined);
  assert.equal(lacks(session("operator", ["deploy"]), "download"), undefined);
  assert.match(lacks(session("viewer", []), "download")!, /`download` permission/);
  assert.equal(parseSession('{"error": "sign in"}'), undefined);
});

test("release files and stored releases are read; anything else isn't one", () => {
  assert.deepEqual(releaseFile("integrations-0a1b2c3d4e5f.tar.gz"), { project: "integrations", short: "0a1b2c3d4e5f" });
  assert.deepEqual(releaseFile("my-project-0a1b2c3d4e5f.tar.gz"), { project: "my-project", short: "0a1b2c3d4e5f" });
  assert.equal(releaseFile("integrations-0a1b.tar.gz"), undefined);
  assert.equal(releaseFile("notes.txt"), undefined);
  const listed = parseReleases(
    JSON.stringify([
      { id: "0a1b2c3d4e5f99", short: "0a1b2c3d4e5f", project: "integrations", running: true },
      { nope: 1 },
    ]),
  );
  assert.equal(listed?.length, 1);
  assert.equal(parseReleases('{"error": "x"}'), undefined);
});

test("ductus deploy's arguments: a file or an id, --from, the reason; never a token", () => {
  assert.deepEqual(
    deployArgs({ release: "/p/.ductus/releases/x-0a1b2c3d4e5f.tar.gz", to: "https://t:7700", reason: " why " }),
    ["deploy", "/p/.ductus/releases/x-0a1b2c3d4e5f.tar.gz", "--to", "https://t:7700", "-m", "why", "--json"],
  );
  assert.deepEqual(deployArgs({ release: "0a1b2c3d4e5f", from: "https://t:7700", to: "https://p:7700", reason: "" }), [
    "deploy",
    "0a1b2c3d4e5f",
    "--to",
    "https://p:7700",
    "--from",
    "https://t:7700",
    "--json",
  ]);
});

test("a deploy's report: the outcome in one line, the gate and what changed in the output", () => {
  const ok = parseDeploy(
    JSON.stringify({
      ok: true,
      outcome: "applied in place: 1 interface took its new code between two messages; the others never paused",
      short: "0a1b2c3d4e5f",
      project: "integrations",
      environment: "prod",
      applied: "in_place",
      replaces: {},
      gate: {
        reused: false,
        why_not_reused: "no check of it for prod",
        check: {
          ok: true,
          seconds: 1.2,
          report: {
            checks: [
              { level: "warning", text: "no database: not checked", more: ["a"] },
              { level: "ok", text: "fine" },
            ],
          },
        },
      },
      plan: { changes: [{ interface: "adt", what: ["code"] }] },
      steps: ["adt: code → bbbbbbbbbbbb, applied in place (3 ms)"],
      changes: [{ interface: "adt", from: "aaaaaaaaaaaa", to: "bbbbbbbbbbbb" }],
    }),
  )!;
  assert.equal(deploySummary(ok), "adt (in place)");
  const lines = deployLines(ok);
  assert.equal(lines[0], "release 0a1b2c3d4e5f (project integrations) to `prod`");
  assert.ok(lines.includes("gate: passed in 1.2 s (no check of it for prod)"), lines.join("\n"));
  assert.ok(lines.includes("  ⚠ no database: not checked") && lines.includes("      a"), lines.join("\n"));
  assert.ok(!lines.some((l) => l.includes("fine")), "only problems and warnings");
  assert.ok(lines.includes("· adt: code (in place)") && lines.includes("  adt: aaaaaaaaaaaa → bbbbbbbbbbbb"));

  // Refused: the first reason, a rule before someone else's work before the gate.
  const rules = parseDeploy(
    JSON.stringify({
      ok: false,
      outcome: "refused by this environment's rules (`rules` says why)",
      rules: ["this environment takes only releases deployed to test first"],
    }),
  )!;
  assert.equal(deploySummary(rules), "this environment takes only releases deployed to test first");
  assert.ok(deployLines(rules).includes("✗ this environment takes only releases deployed to test first"));
  const undo = parseDeploy(
    JSON.stringify({ ok: false, outcome: "refused: …", replaces: { adt: "replaces 1234 (by ann)" } }),
  )!;
  assert.equal(deploySummary(undo), "it would undo adt: replaces 1234 (by ann)");
  assert.ok(deployLines(undo).includes("✗ would undo adt: replaces 1234 (by ann)"));
  const gate = parseDeploy(
    JSON.stringify({
      ok: false,
      outcome: "the gate found problems",
      gate: {
        check: {
          ok: false,
          report: {
            checks: [
              { level: "warning", text: "w" },
              { level: "problem", text: "adt: 1 test failed" },
            ],
          },
        },
      },
    }),
  )!;
  assert.equal(deploySummary(gate), "adt: 1 test failed");
  assert.equal(parseDeploy("not json"), undefined);
});

test("dev-engine.json is read; anything else isn't a dev engine", () => {
  assert.deepEqual(parseDevEngine('{\n  "api": "http://127.0.0.1:7700/",\n  "pid": 4242\n}\n'), {
    api: "http://127.0.0.1:7700",
    pid: 4242,
  });
  assert.equal(parseDevEngine(""), undefined);
  assert.equal(parseDevEngine('{"api": "http://127.0.0.1:7700"}'), undefined);
  assert.equal(parseDevEngine("{half"), undefined);
});

test("a reload's report: what took the change, or why not, in one line", () => {
  const inPlace = parseReload(
    JSON.stringify({
      ok: true,
      applied: "in_place",
      outcome: "applied in place: adt-in's code",
      steps: ["adt-in: new code between two messages"],
      changes: [
        { interface: "adt-in", from: "0a1b2c3d4e5f", to: "5f4e3d2c1b0a" },
        { interface: "lab-out", from: null, to: "abcdefabcdef" },
      ],
      warnings: {},
      took_ms: 84,
    }),
  );
  assert.ok(inPlace);
  assert.equal(reloadSummary(inPlace), "adt-in, lab-out (new) in 84 ms");

  const nothing = parseReload('{"ok":true,"applied":"nothing","outcome":"nothing changed: the folder is what runs"}');
  assert.ok(nothing);
  assert.equal(reloadSummary(nothing), "nothing to deploy: dev runs the folder as it is");

  const refused = parseReload(
    JSON.stringify({
      ok: false,
      applied: "refused",
      outcome:
        "refused: only a new engine applies it (ductus.yaml changed), and `ductus dev` doesn't hand over: restart it\nNothing changed: every interface runs what it ran.",
    }),
  );
  assert.ok(refused);
  assert.equal(
    reloadSummary(refused),
    "only a new engine applies it (ductus.yaml changed), and `ductus dev` doesn't hand over: restart it",
  );

  const warned = parseReload(
    JSON.stringify({
      ok: true,
      applied: "in_place",
      outcome: "x",
      changes: [{ interface: "adt-in", from: "a", to: "b" }],
      warnings: { "adt-in": "sam's change" },
    }),
  );
  assert.ok(warned);
  assert.equal(reloadSummary(warned), "adt-in (⚠ 1 over someone else's change: see the output)");

  // An error body (403 without the permission) isn't a report.
  assert.equal(parseReload('{"error":"needs the deploy permission"}'), undefined);
  assert.equal(parseReload("<html>"), undefined);
});

test("test --json lines are read; anything else is not ours", () => {
  // As docs/write/testing.md shows them.
  const failed = parseTestLine(
    '{"type":"test","interface":"adt-in","name":"the lab gets visits","file":"interfaces/adt-in/lab.test.ts","line":4,"column":1,"error":"expected \\"I\\", got \\"IN\\"","failure":{"file":"interfaces/adt-in/lab.test.ts","line":6,"column":28},"expected":"\\"I\\"","actual":"\\"IN\\"","ok":false,"ms":2}',
  );
  assert.equal(failed?.type, "test");
  assert.deepEqual(failed?.type === "test" && failed.failure, {
    file: "interfaces/adt-in/lab.test.ts",
    line: 6,
    column: 28,
  });
  assert.equal(parseTestLine('{"type":"summary","passed":0,"failed":1,"ms":40}')?.type, "summary");
  assert.equal(parseTestLine('{"type":"test","name":"x","interface":"a","later":"a field added"}')?.type, "test");
  assert.equal(parseTestLine("adt-in: waiting for a debugger"), undefined);
  assert.equal(parseTestLine('{"type":"something new"}'), undefined);
  assert.equal(parseTestLine("{not json"), undefined);
});

test("lines are split across chunks, \\r\\n too, the last without its newline", () => {
  const b = new LineBuffer();
  assert.deepEqual(b.push('{"a":'), []);
  assert.deepEqual(b.push('1}\r\n{"b":2}\n{"c"'), ['{"a":1}', '{"b":2}']);
  assert.deepEqual(b.push(":3}"), []);
  assert.deepEqual(b.end(), ['{"c":3}']);
  assert.deepEqual(b.end(), []);
});

test("Problems get Ductus's findings only, by file; notes about no file are left out", () => {
  const document: ValidateDocument = {
    environment: "dev",
    valid: false,
    errors: 3,
    warnings: 0,
    findings: [
      { severity: "error", code: "TS2322", source: "typescript", message: "x", file: "interfaces/a/a.ts", line: 1 },
      { severity: "error", code: "DX1", source: "ductus", message: "y", file: "interfaces/a/a.ts", line: 2 },
      { severity: "error", code: "DUC122", source: "ductus", message: "z", file: "interfaces/a/interface.yaml" },
      { severity: "note", code: "DUC390", source: "ductus", message: "not checked against a database" },
      { severity: "error", code: "DUC300", source: "ductus", message: "about the database" },
    ],
  };
  const byFile = problemsByFile(document);
  assert.deepEqual(
    [...byFile].map(([f, list]) => [f, list.map((x) => x.code)]),
    [
      ["interfaces/a/a.ts", ["DX1"]],
      ["interfaces/a/interface.yaml", ["DUC122"]],
      ["interfaces", ["DUC300"]],
    ],
  );
  assert.equal(parseValidate('{"environment":"dev","findings":[]}')?.environment, "dev");
  assert.equal(parseValidate("Error: no interfaces/ folder"), undefined);
});

test("the version: a pre-release counts as its release", () => {
  assert.deepEqual(parseVersion("ductus 0.7.0-dev"), [0, 7, 0]);
  assert.equal(parseVersion("ductus"), undefined);
  assert.ok(atLeast([0, 7, 0], [0, 7, 0]));
  assert.ok(atLeast([0, 10, 0], [0, 7, 0]));
  assert.ok(atLeast([1, 0, 0], [0, 7, 3]));
  assert.ok(!atLeast([0, 6, 9], [0, 7, 0]));
});

test("a choice becomes the fewest ductus test runs", () => {
  const known = new Map([
    [
      "/p",
      new Map([
        ["adt-in", ["a", "b", "c"]],
        ["lab", ["x"]],
        ["census", ["y"]],
      ]),
    ],
  ]);
  // The whole project: one run with nothing named (tests added since the last listing run too).
  assert.deepEqual(plan([{ root: "/p" }], [], known, false), [{ root: "/p", interfaces: [], tests: [] }]);
  // Interfaces whole together; one interface's single tests on their own (`--test` is per name).
  assert.deepEqual(
    plan(
      [
        { root: "/p", interface: "lab" },
        { root: "/p", interface: "census" },
        { root: "/p", interface: "adt-in", name: "a" },
        { root: "/p", interface: "adt-in", name: "c" },
      ],
      [],
      known,
      false,
    ),
    [
      { root: "/p", interfaces: ["lab", "census"], tests: [] },
      { root: "/p", interfaces: ["adt-in"], tests: ["a", "c"] },
    ],
  );
  // An exclusion inside an interface turns it into the tests left.
  assert.deepEqual(
    plan([{ root: "/p", interface: "adt-in" }], [{ root: "/p", interface: "adt-in", name: "b" }], known, false),
    [{ root: "/p", interfaces: ["adt-in"], tests: ["a", "c"] }],
  );
  // ... and inside a project, the interfaces left whole and the rest by test.
  assert.deepEqual(plan([{ root: "/p" }], [{ root: "/p", interface: "lab" }], known, false), [
    { root: "/p", interfaces: ["adt-in", "census"], tests: [] },
  ]);
  // Debugging is one interface a run (`test --inspect` runs one).
  assert.deepEqual(plan([{ root: "/p" }], [], known, true), [
    { root: "/p", interfaces: ["adt-in"], tests: [] },
    { root: "/p", interfaces: ["lab"], tests: [] },
    { root: "/p", interfaces: ["census"], tests: [] },
  ]);
  // A test chosen inside an interface chosen whole adds nothing.
  assert.deepEqual(
    plan(
      [
        { root: "/p", interface: "lab" },
        { root: "/p", interface: "lab", name: "x" },
      ],
      [],
      known,
      false,
    ),
    [{ root: "/p", interfaces: ["lab"], tests: [] }],
  );
});

test("ductus test's arguments", () => {
  assert.deepEqual(testArgs({ root: "/p", interfaces: ["adt-in"], tests: ["a b"] }, "dev", "127.0.0.1:4000"), [
    "test",
    "--json",
    "--env",
    "dev",
    "--inspect=127.0.0.1:4000",
    "--interface",
    "adt-in",
    "--test",
    "a b",
  ]);
});
