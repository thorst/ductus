// The parts of the extension that don't need VS Code: run by `npm run check` under plain Node.
import assert from "node:assert/strict";
import { test } from "node:test";
import {
  LineBuffer,
  atLeast,
  parseTestLine,
  parseValidate,
  parseVersion,
  plan,
  problemsByFile,
  testArgs,
  type ValidateDocument,
} from "../../src/lines.ts";

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
