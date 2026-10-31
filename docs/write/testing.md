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
the code and checks the configuration against that environment. `--interface adt-in` runs one
interface's tests.

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
  ([nuance](../nuances.md#fixtures-can-be-read-only-inside-a-test-level-1)).
- **With `split: records`**, `input()` gives the fixture's **first** record, as the engine gives
  `transform` one record per message. For a file of several rows, parse it yourself and pick:
  `parseCsv(fixture("census_20261031.csv"))[1]`
  ([nuance](../nuances.md#with-split-records-input-gives-a-fixtures-first-record-level-2)).
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

## Beyond unit tests

- **`ductus validate`** type-checks every interface and checks its configuration and resources
  for one environment: run it for each environment you deploy to.
- **`ductus replay`** runs a real period's stored messages through a changed version of the code
  and compares every destination's output with what the installed version sent, before you
  deploy: the test against real traffic that fixtures can't be
  ([replay](../operate/resend.md#replay-a-period-before-a-change)).
- **`ductus gen` and `ductus sink`** send synthetic ADT and receive it, to try a whole project on
  your laptop with `ductus dev` ([first interface](../first-interface.md)).
