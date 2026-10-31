# Nuances: writing transforms

What could surprise you here, why it is so, and what to do. Each entry has a level, 1 (day one) to 5 (expert): the [index](index.md) lists them all by level.

## Missing fields read as "" and never throw (level 1)
- **What:** `msg.PID[30][1]` on a message without it is `""`. This avoids null checks everywhere, but a typo in a position also quietly gives `""`.
- **What to do:** test with real sample messages (`ductus test`) and assert on the fields you map.

## `return null` filters; throwing quarantines (level 1)
- **What:** `null` → filtered (kept for audit, never delivered); `kill("reason")` → filtered with a reason; a thrown error or `quarantine("reason")` → quarantined for a person to look at.

## Imports used only for their side effects are removed (level 2)
- **What:** TypeScript drops `import { x } from "./m.ts"` if `x` is never used, so code that runs when `m.ts` loads won't run.
- **What to do:** write `import "./m.ts";` when you need a module only for what it does on load.

## Copy what you change, not the whole thing through JSON (level 2)
- **What:** `JSON.parse(JSON.stringify(x))` builds the whole object twice, as text and again as objects, for every message. It also changes what it copies: dates become strings, `undefined` and functions disappear, a `Map` becomes `{}`.
- **What to do:** you rarely need a copy: each destination's function already gets the message as received (see [Each destination runs its own function on the original message](destinations.md#each-destination-runs-its-own-function-on-the-original-message-level-2)), so change it in place. For an object from `db()` or `fetch()`, copy only what you change: `{ ...row, name: row.name.trim() }`.

## One query for many values, not one query per value (level 3)
- **What:** each `db()` call is a round trip to the database and a step from your code into the engine and back. A lookup inside a loop (one query per OBX, per row of a file) adds one round trip per item, and the transform waits for each in turn.
- **What to do:** collect the values first and ask once. An array parameter arrives in Postgres as `jsonb`: `db("lab").query("SELECT code, name FROM tests WHERE code IN (SELECT jsonb_array_elements_text($1))", [codes])`, then look the answers up in a `Map`. Lookups that don't depend on each other can also run together (see [In respond mode, calls run one after another unless you await them together](monitoring.md#in-respond-mode-calls-run-one-after-another-unless-you-await-them-together-level-2)).

## Short-lived objects are cheap; what you keep is what costs (level 4)
- **What:** the JavaScript engine (V8) frees memory by itself, in two ways. Objects made while handling one message and dropped at the end are collected often and quickly. Objects still referenced later (a module-level array or `Map` that keeps growing, results kept "for next time") move to a second area that is collected less often, with longer pauses that delay every interface on that thread, and they count toward `isolates.heap_mb` until the engine restarts.
- **Why:** this is how V8 works, as Java's and .NET's collectors do. Rust code in the engine has no collector; only your TypeScript does.
- **What to do:** make what you need inside the transform and let it go. Don't avoid creating objects to "save memory": per-message garbage is the cheap kind. Keep anything that outlives a message bounded (see [Module-level caches live until the engine restarts](shared-pool.md#module-level-caches-live-until-the-engine-restarts-level-2)).

## Loops over thousands of items: give objects one shape (level 4)
- **What:** V8 makes property access fast by remembering how objects of the same shape (the same properties, added in the same order) are laid out. Objects built in different ways, or with properties added later or `delete`d, take a slower path at the places that read them.
- **When it matters:** only in code that touches many objects per message: a file of 10,000 rows, a result with hundreds of OBX. A message's handful of objects never notices.
- **What to do:** build such objects in one place with every property, in the same order (`{ mrn, name, dob }` with `""` for missing ones), and set a property to `""` or `undefined` rather than `delete`.

## Fixtures can be read only inside a test (level 1)
- **What:** `fixture()`, `hl7()` and `input()` work inside `test(...)` (or a function a test calls), not at the top of a test file: `ductus test` hands the interface's fixtures over after the test files have loaded. Read at the top, they fail with "fixture "x" read when the test file loads: fixtures can be read only inside a test".
- **What to do:** read fixtures in each test, or write a small function (`const row = (n) => parseCsv(fixture("census.csv"))[n]`) and call it from the tests.

## With `split: records`, `input()` gives a fixture's first record (level 2)
- **What:** on an interface whose file source has `split: records`, the engine gives `transform` one record per message, and `input("file.csv")` in a test does the same: it parses the fixture as the interface's format and returns its **first** record, not the list. A test that indexes `input(...)[1]` gets `undefined`.
- **What to do:** keep one record per fixture file, or parse a file of several yourself and pick one: `parseCsv(fixture("census_20261031.csv"))[1]` (`parseFixed` for fixed-width).

## A quoted number in an environment file is text (level 1)
- **What:** a setting that's only a reference keeps the variable's type, so `port: ${LAB_PORT}` with `LAB_PORT: "6670"` in `environments/dev.yaml` is the text "6670", and the interface is refused: "invalid type: string "6670", expected u16". `LAB_PORT: 6670` is the number.
- **What to do:** write numbers in environment files without quotes. Quote only what YAML would otherwise read wrongly (a value starting with `*`, `&`, `{` or `[`; `yes`/`no` meant as text; a time like `07:00`).
