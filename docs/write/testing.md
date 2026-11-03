# Testing with fixtures

`ductus test` runs every interface's `*.test.ts` files inside the real engine's JavaScript
runtime, with the SDK the engine uses, and no database, network or running engine. Tests are how
you know a mapping does what the receiver's spec says before a message reaches them, and how the
next person changing it knows they didn't break it.

```text
$ ductus test
adt-in
  ✓ the lab gets visits, with its own patient class and no Z segments (1ms)
  ✓ the lab doesn't get patient updates (A31) (0ms)
  ✓ test patients are dropped with a reason (0ms)
  ✓ an unknown patient class is quarantined, not guessed (1ms)
  ✓ billing gets admissions and discharges only (0ms)
adt-to-billing
  ✓ an admission becomes an encounter, with the attending's NPI (1ms)
  ✓ the NPI is looked up once an hour, not once a message (0ms)
  ✓ a provider the database doesn't know is quarantined (1ms)
…
17 passed, 0 failed
```

It exits 1 when a test fails, so it belongs in the checks every change goes through (a pull
request's CI, a script before deploying), next to `ductus validate --env prod`, which type-checks
the code and checks the configuration against that environment.

| Option | Runs |
|---|---|
| `--interface adt-in` | one interface's tests |
| `--filter "A31"` | the tests whose name contains the text, in every interface (or the one `--interface` names); case matters. A filter that matches nothing exits 1, so a typo in a CI script can't pass by running nothing ([nuance](../nuances/transforms.md#a-test-filter-matches-part-of-a-name-level-1)) |
| `--list` | nothing: it lists the tests with where each is, and takes `--interface` and `--filter` too |
| `--json` | the same, as JSON Lines for an editor or CI ([below](#for-editors-and-ci-json)) |

```text
$ ductus test --list --filter billing
adt-in
  billing gets admissions and discharges only  (interfaces/adt-in/route.test.ts:26:1)
remits-in
  the CSV the billing office gets  (interfaces/remits-in/remits.test.ts:21:1)

2 test(s)
```

## When a test fails

A failed test says where it is, where it failed and why. Here a mapping's expected patient class
was mistyped as `I`:

```text
$ ductus test --interface adt-in
adt-in
  ✗ the lab gets visits, with its own patient class and no Z segments  (interfaces/adt-in/route.test.ts:4:1)
      expected "I", got "IN"
      at interfaces/adt-in/route.test.ts:6:28
  ✓ the lab doesn't get patient updates (A31) (0ms)
  ✓ test patients are dropped with a reason (0ms)
  ✓ an unknown patient class is quarantined, not guessed (1ms)
  ✓ billing gets admissions and discharges only (0ms)

4 passed, 1 failed
```

- **The test's place** follows its name: `route.test.ts:4:1` is the `test(...)` call.
- **A failed `expect`** gives both values and the line and column of the `expect` that failed
  (`6:28`: the `toBe` on line 6), also after an `await`. A failed `toReject` points at the
  `await` of it.
- **An error thrown by the code under test** (not an `expect`) shows its stack trace on your
  `.ts` lines, with the file and line it was thrown from at the end of its first line:
  `Error: no PID (interfaces/adt/route.ts:5:11)`, then `at transform (interfaces/adt/route.ts:5:11)`
  and the test's line that called it. The SDK's and the engine's own frames are left out. The
  engine quarantines a message with the same first line, so the Quarantine page says where too
  ([nuance](../nuances/transforms.md#a-thrown-errors-reason-says-where-it-was-thrown-level-1)).

## Seeing what the code did: `console`

`console.log` in a test, or in the code it runs, is the quick way to see a value. Each line is
shown under the test that wrote it, whether it passed or failed, marked `│` so it reads apart
from `ductus test`'s own text, and before the failure it led to:

```text
$ ductus test
adt-in
  ✓ the lab gets the visit (1ms)
      │ class I for visit V9001
      │ sent {"segments":["MSH","PV1"]}
  ✗ a visit without a class is still sent  (interfaces/adt-in/route.test.ts:12:1)
      │ class  for visit V9001
      │ no patient class 41
      expected "IN", got ""
      at interfaces/adt-in/route.test.ts:15:35

1 passed, 1 failed
```

- **Every level is shown** (`log`, `info`, `debug`, `warn`, `error`), whatever the environment's
  [`console:`](../configure/environments.md#console-lines-from-your-code) says: that setting is
  for the engine's log.
- **Values**: text as is, numbers and `null` as written, objects and arrays as one line of JSON,
  an error as its stack, an HL7 message as its text; values are joined with a space. `%s`-style
  placeholders aren't filled in.
- **Lines written while the test files load** (at the top of a file, outside any function) are
  shown first, under `while loading`. A line a test's promise writes after the test ended (one
  it didn't `await`) lands under the next test
  ([nuance](../nuances/transforms.md#console-in-tests-each-line-under-the-test-that-wrote-it-level-1)).
- **Remove them when you're done**: in the engine, a `console.log` in the code runs for every
  message, and `ductus validate` warns about it for environments that would log it.

## A test

```ts
// interfaces/adt-in/route.test.ts, in part
test("the lab gets visits, with its own patient class and no Z segments", () => {
  const out = toLab(hl7("a01.hl7")) as Message;
  expect(out.get("PV1-2")).toBe("IN");
  expect(out.get("MSH-5")).toBe("LAB");
  expect(out.segmentNames).toEqual(["MSH", "EVN", "PID", "PV1"]);
});
```

- **A test file sits next to the code** (`route.test.ts` beside `route.ts`), imports the functions
  it tests, and registers tests with `test(name, fn)`. The name is a sentence saying what should
  happen: it's what a failing run prints.
- **Tests run one at a time**, in the order written; an `async` test is awaited.
- **Name what the receiver cares about**: a test per rule in the spec ("the lab doesn't get
  A31s"), not one per function.

## Fixtures: real messages, without the patient

A test's messages are files in the interface's `fixtures/` folder:

| Read with | Gives |
|---|---|
| `fixture("a01.hl7")` | the file's text |
| `hl7("a01.hl7")` | the file parsed as an HL7 message |
| `input("order.xml")` | the file parsed **the way the engine parses this interface's input** (its `format`), ready to pass to `transform` |

- **Start from real messages.** Take a few of each kind the sender sends (each event type, the
  odd ones: a merge, a result with 40 OBX, a name with an apostrophe) from the sender's test
  system or a message the engine has stored.
- **Never commit a real patient.** Fixtures live in Git, read by everyone with the repository.
  Replace names, MRNs, dates of birth, addresses and identifiers with made-up ones before saving
  the file; keep the structure (how many repetitions, which fields are empty, the sender's
  quirks), which is what the test needs.
- **Fixtures can be read only inside a test**, not when the test file loads: put
  `hl7("a01.hl7")` inside `test(...)`, or in a function the test calls
  ([nuance](../nuances/transforms.md#fixtures-can-be-read-only-inside-a-test-level-1)).
- **With `split: records`**, `input()` gives the fixture's **first** record, as the engine gives
  `transform` one record per message. For a file of several rows, parse it yourself and pick:
  `parseCsv(fixture("census_20261031.csv"))[1]`
  ([nuance](../nuances/transforms.md#with-split-records-input-gives-a-fixtures-first-record-level-2)).
- **Change a fixture in the test** for a variation instead of keeping a file per case:

```ts
// interfaces/adt-in/route.test.ts, in part
test("an unknown patient class is quarantined, not guessed", () => {
  const msg = hl7("a01.hl7");
  msg.set("PV1-2", "X");
  expect(() => toLab(msg)).toThrow('unknown patient class "X"');
});
```

## Checking results

| Assertion | Passes when |
|---|---|
| `expect(x).toBe(y)` | `x` is `y` (the same value: strings, numbers, `true`) |
| `expect(x).toEqual(y)` | `x` and `y` have the same contents (objects and arrays, deeply) |
| `expect(x).toBeNull()` | `x` is `null`: the destination filters the message |
| `expect(x).toBeTruthy()` | `x` is anything but `null`, `undefined`, `false`, `0` or `""` |
| `expect(x).toContain(y)` | a string contains `y`, or an array has it |
| `expect(() => f()).toThrow("text")` | calling it throws an error whose message contains the text: a quarantine |
| `await expect(promise).toReject("text")` | the same for `async` code |
| `expect(x).not.toBe(y)` | any of the above, reversed |

An HL7 field passed to `expect` is compared by its text, so `expect(msg.PID[5]).toBe("SMITH^ANNA^M")`
works.

### Compare the whole result

For a mapping, compare the whole output with `toEqual`, not a field or two: a row added to the
mapping, or a field that's suddenly empty, then fails the test.

```ts
// interfaces/adt-to-billing/billing.test.ts, in part
test("an admission becomes an encounter, with the attending's NPI", async () => {
  const calls = mock.db("provider_db", () => [{ npi: "1093817465" }]);
  const out = await transform(hl7("a01.hl7"), meta);
  expect(out).toEqual({
    patient: { mrn: "123456", family: "SMITH", given: "ANNA", birthDate: "19800214" },
    visit: { number: "V9001", unit: "4W", admitted: "20261031083000-0500" },
    event: "admit",
    attendingNpi: "1093817465",
    sourceMessage: 41,
  });
  expect(calls[0].params).toEqual(["1234"]);
});
```

### Filters, kills and quarantines

- **Filtered**: `expect(toLab(hl7("a31.hl7"))).toBeNull()`.
- **Killed with a reason**: the result is a `Kill`; check its reason:

```ts
// interfaces/adt-in/route.test.ts, in part
test("test patients are dropped with a reason", () => {
  const out = toLab(hl7("test-patient.hl7"));
  expect(out instanceof Kill && out.reason).toBe("test patient");
});
```

- **Quarantined**: `toThrow` (or `toReject` for async code) with part of the reason, as above.
  Test every quarantine you wrote: it's the behaviour someone relies on at 3 a.m.

### `meta`

A function that reads `meta` gets one from the test: a plain object with the fields it uses.

```ts
// interfaces/adt-to-billing/billing.test.ts, in part
const meta: Meta = { id: 41, receivedAt: "2026-10-31T13:30:00Z", destination: "default", origin: 7 };
```

## Mocks: databases, APIs, variables and secrets

Tests never reach a real database or API. A call your test didn't mock fails with what to add
("db("provider_db") is not mocked in this test: add mock.db("provider_db", (sql, params) =>
[/* rows */])"), so a test can't quietly depend on a system being up.

| Mock | Answers | Returns |
|---|---|---|
| `mock.db("name", rows)` or `mock.db("name", (sql, params) => rows)` | `db("name").query/one/execute` (`execute` takes a number, or rows' count) | the calls made: `{ sql, params }` |
| `mock.api("name", { status, body })` or a function of the request | `api("name")` calls; the request's `url` is the path | the requests made |
| `mock.fetch({ status, body })` or a function | `fetch()` calls | the requests made |
| `mock.env({ NAME: "value" })` | `env("NAME")` | |
| `mock.secret({ NAME: "value" })` | `secret("NAME")` | |

- **Mocks last one test**: each test starts with none, so tests don't depend on each other's.
  What your own code keeps (a module-level cache) does last, as it would in the engine: the
  caching test above uses a provider id no other test uses.
- **Check what was asked**, not only what came back: `calls[0].params` is the MRN your code
  sent; `calls.length` shows a lookup made once, not per message.
- **Answer like the real system**, including its failures: a 404, an empty result, the odd shape
  it sends for a merged patient.

```ts
// interfaces/adt-to-billing/empi.test.ts, in part
test("an MRN the EMPI doesn't know is quarantined", async () => {
  mock.api("empi", { status: 404 });
  await expect(enterpriseId("999")).toReject("the EMPI doesn't know MRN 999");
});
```

A mocked temporary failure (a 503) makes your code's call throw the same temporary error the
engine would wait on and retry; tests usually check the answers your code decides on instead.

## For editors and CI: JSON

`ductus test --json` and `ductus validate --json` give tools what the text gives people: an
editor puts a failure or a problem at its line, a CI system annotates the pull request. Both keep
their shape across versions: fields and codes are only ever added, never renamed, removed or reused,
so a tool should ignore fields it doesn't know
([nuance](../nuances/transforms.md#the---json-outputs-only-ever-gain-fields-level-3)).

### `ductus test --json`

JSON Lines: one object per line, one line per test as each interface's tests finish, and a
summary last. The exit code is the same as without `--json`.

```text
$ ductus test --interface adt-in --filter "visits" --json
{"type":"test","interface":"adt-in","name":"the lab gets visits, with its own patient class and no Z segments","file":"interfaces/adt-in/route.test.ts","line":4,"column":1,"error":"expected \"I\", got \"IN\"","failure":{"file":"interfaces/adt-in/route.test.ts","line":6,"column":28},"expected":"\"I\"","actual":"\"IN\"","ok":false,"ms":3}
{"type":"summary","passed":0,"failed":1,"ms":36}
```

The tests above that log ([`console`](#seeing-what-the-code-did-console)) carry what they wrote:

```text
$ ductus test --json
{"type":"test","interface":"adt-in","name":"the lab gets the visit","file":"interfaces/adt-in/route.test.ts","line":6,"column":1,"output":"class I for visit V9001\nsent {\"segments\":[\"MSH\",\"PV1\"]}\n","ok":true,"ms":1}
{"type":"test","interface":"adt-in","name":"a visit without a class is still sent","file":"interfaces/adt-in/route.test.ts","line":12,"column":1,"output":"class  for visit V9001\nno patient class 41\n","error":"expected \"IN\", got \"\"","failure":{"file":"interfaces/adt-in/route.test.ts","line":15,"column":35},"expected":"\"IN\"","actual":"\"\"","ok":false,"ms":1}
{"type":"summary","passed":1,"failed":1,"ms":25}
```

| Field | On | What it is |
|---|---|---|
| `type` | every line | `test`, `output` (what an interface's test files wrote to `console` while loading, before its first test line), `load` (an interface whose test files didn't load: `interface`, `error` and `output`) or `summary` |
| `interface` | `test`, `output`, `load` | the interface the test file belongs to |
| `output` | `test`, `output`, `load`, when something was written | the `console` lines, each ending in `\n`; absent when nothing was written. Nothing else reaches stdout or stderr with `--json` |
| `name` | `test` | the name given to `test(...)` |
| `file`, `line`, `column` | `test` | where the `test(...)` call is: the file relative to the project, with `/`; line and column from 1, on the `.ts` as written |
| `ok`, `ms` | `test`, when run | whether it passed, and how long it took |
| `error` | a failed `test`, `load` | what failed: an `expect`'s message, or a thrown error's stack as the text output shows it |
| `failure` | a failed `test` | `{ file, line, column }` of where it failed: the `expect`, or the first frame of a thrown error in project code (which may be in the code under test) |
| `expected`, `actual` | a failed `toBe`, `toEqual` or `toBeNull` | both values as text, objects and arrays as indented JSON, for a side-by-side view |
| `passed`, `failed`, `ms` | `summary` | the counts (a `load` line counts as one failed) and the run's time |
| `tests` | `summary` with `--list` | how many tests were listed |

With `--list`, test lines have no `ok`, `ms` or failure: nothing ran.

```text
$ ductus test --list --interface adt-in --json
{"type":"test","interface":"adt-in","name":"the lab gets visits, with its own patient class and no Z segments","file":"interfaces/adt-in/route.test.ts","line":4,"column":1}
{"type":"test","interface":"adt-in","name":"the lab doesn't get patient updates (A31)","file":"interfaces/adt-in/route.test.ts","line":11,"column":1}
{"type":"test","interface":"adt-in","name":"test patients are dropped with a reason","file":"interfaces/adt-in/route.test.ts","line":15,"column":1}
{"type":"test","interface":"adt-in","name":"an unknown patient class is quarantined, not guessed","file":"interfaces/adt-in/route.test.ts","line":20,"column":1}
{"type":"test","interface":"adt-in","name":"billing gets admissions and discharges only","file":"interfaces/adt-in/route.test.ts","line":26,"column":1}
{"type":"summary","tests":5}
```

### `ductus validate --json`

One JSON document, after every check has run (validate takes about a second on a project of a few
interfaces: most of it is the TypeScript type check). The text output is unchanged without
`--json`. Here `adt-in`'s port was mistyped as 66610 and the secrets weren't set:

```text
$ ductus validate --json
{
  "environment": "dev",
  "valid": false,
  "errors": 2,
  "warnings": 1,
  "findings": [
    {
      "severity": "error",
      "code": "DUC111",
      "source": "ductus",
      "message": "resources: in ./resources.yaml: missing in environment `dev`: secret:BILLING_TOKEN, secret:EMPI_TOKEN, secret:FHIR_TOKEN, secret:PROVIDER_DB_PASSWORD",
      "file": "resources.yaml",
      "line": 12,
      "column": 39,
      "end_line": 12,
      "end_column": 62
    },
    {
      "severity": "error",
      "code": "DUC122",
      "source": "ductus",
      "message": "adt-in: in ./interfaces/adt-in/interface.yaml: invalid value: integer `66610`, expected u16",
      "file": "interfaces/adt-in/interface.yaml",
      "line": 6,
      "column": 19,
      "end_line": 6,
      "end_column": 23,
      "interface": "adt-in"
    },
    {
      "severity": "warning",
      "code": "DUC204",
      "source": "ductus",
      "message": "adt-to-billing: has `source: internal` but no interface routes to it (`destination: { interface: adt-to-billing }`)",
      "file": "interfaces/adt-to-billing/interface.yaml",
      "line": 6,
      "column": 1,
      "end_line": 6,
      "end_column": 7,
      "interface": "adt-to-billing",
      "hint": "route to it from another interface: `destination: { interface: adt-to-billing }`"
    },
    {
      "severity": "note",
      "code": "DUC390",
      "source": "ductus",
      "message": "not checked against a database (set DUCTUS_DATABASE_URL): lanes that would strand waiting messages, retention changes",
      "hint": "set DUCTUS_DATABASE_URL (or --database-url)"
    }
  ]
}
```

(`adt-to-billing` gets the warning because `adt-in`, which routes to it, didn't load.)

| Field | What it is |
|---|---|
| `environment`, `valid`, `errors`, `warnings` | the environment checked; whether the project is valid (no errors: exit 0); the counts |
| `severity` | `error` (makes the project invalid), `warning`, or `note` (what wasn't checked) |
| `code` | stable: `DUC…` for Ductus's checks, `DX1`–`DX4` for its checks of your code, `TS…` for TypeScript's errors; every one is in the [reference](../reference/validate.md) |
| `source` | `typescript` for TypeScript's own errors, `ductus` for the rest: an editor that already runs TypeScript (VS Code does) shows only `ductus` ones to avoid showing errors twice |
| `message` | what the text output prints for it, without the ✗ or ⚠ |
| `file` | relative to the project, with `/`; absent for findings about no one file (the database) |
| `line`, `column`, `end_line`, `end_column` | from 1, columns counted in characters: the key in YAML (`port`), the code in TypeScript, the `${...}` that has no value, or where the YAML parser stopped. Absent when the file doesn't have even the first part of the key (a missing `about:`) ([nuance](../nuances/transforms.md#a-places-precision-in-validate---json-depends-on-the-file-level-2)) |
| `interface` | the interface it's about |
| `hint` | what to do, when the message says (already part of `message`) |

## Beyond unit tests

- **`ductus validate`** type-checks every interface and checks its configuration and resources
  for one environment: run it for each environment you deploy to.
- **`ductus replay`** runs a real period's stored messages through a changed version of the code
  and compares every destination's output with what the installed version sent, before you
  deploy: the test against real traffic that fixtures can't be
  ([replay](../operate/resend.md#replay-a-period-before-a-change)).
- **`ductus gen` and `ductus sink`** send synthetic ADT and receive it, to try a whole project on
  your laptop with `ductus dev` ([first interface](../first-interface.md)).
