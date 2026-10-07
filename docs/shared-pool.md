# Writing interfaces for the shared pool

By default, interfaces run in a **shared pool**. A few worker threads (one per CPU core) each run one JavaScript engine (V8 isolate), and that engine hosts many interfaces' code. This is how a server can run 1,000+ interfaces without 1,000+ copies of everything.

Interfaces in the same isolate share one JavaScript global scope. Ordinary interface code is already safe; this page explains the three things that are not, and how to avoid them.

`ductus validate` warns about all three (codes `DX1`–`DX3` below).

## What's already safe

**Your module's variables are private.** Each interface file is an ES module. Top-level `const`, `let`, `function` and `class` declarations belong to that module only. Two interfaces can both declare `let count = 0` and never see each other's value.

```ts
// interfaces/adt-in/transform.ts
let processed = 0;                 // private to adt-in: safe
const facilityMap = new Map([["A", "HOSP-A"]]);

export function transform(msg: Message) {
  processed++;
  ...
}
```

**Typos can't create globals.** Modules run in strict mode, so assigning to an undeclared name (`totl = 1` instead of `total = 1`) throws an error instead of silently creating a global variable. The message is quarantined with the error, and the bug shows up right away.

**Each message gets fresh objects.** The `msg` passed to `transform` is new for every message. Nothing carries over between messages unless you store it yourself.

## What to avoid

### DX1: Writing to the global scope

```ts
globalThis.lastMrn = msg.PID[3][1];          // ✗ every interface in the pool sees this
(globalThis as any).cache = new Map();       // ✗
Object.assign(globalThis, { helper });       // ✗
```

Another interface using the same name would read or overwrite your value. Use a module-level variable instead; it's just as persistent and private to you:

```ts
let lastMrn = "";                            // ✓ module scope
const cache = new Map<string, string>();     // ✓
```

### DX2: Changing built-in prototypes

```ts
String.prototype.shout = function () { ... };   // ✗ changes strings for every interface
Array.prototype.last = ...;                     // ✗
```

Write a plain function instead:

```ts
const shout = (s: string) => s.toUpperCase();   // ✓
```

### DX3: Mutable state in a shared library file

A file outside any interface folder (for example `lib/patient.ts`) that several interfaces import is loaded **once per isolate**, and every importing interface shares that one instance. A `let` or a mutable object at its top level is shared state:

```ts
// lib/counters.ts
let total = 0;                               // ✗ shared by every interface that imports this
export const bump = () => ++total;
```

Keep shared libraries **pure**: export constants and functions that work only on their arguments.

```ts
// lib/patient.ts
export const normalizeMrn = (mrn: string) => mrn.replace(/^0+/, "");   // ✓
export const SEX_CODES = Object.freeze({ M: "male", F: "female" });     // ✓ frozen constant
```

If an interface needs a cache, create it in the interface's own module and pass it to the library function.

Note: `const` alone doesn't make an object immutable (`const m = new Map()` can still be modified). `DX3` flags only top-level `let`/`var`; mutating a `const` Map or object in a shared library is still shared state. Treat shared libraries as read-only.

## Endless loops and memory blow-ups

Interfaces on one thread take turns, so code that never pauses would hold up all of them. The engine stops it:

- **Time:** each run of your code may last `timeout` without pausing (default `5s`; set it in `interface.yaml`, e.g. `timeout: 500ms`). Waiting on `db()` or `fetch()` doesn't count. Past it, the code is stopped and the message quarantined (`transform stopped: the script ran longer than its 5s timeout…`). The other interfaces on the thread waited at most that long.
- **Memory:** each isolate has a heap limit (`isolates.heap_mb` in `ductus.yaml`, default 512). Code that fills it is stopped and its message quarantined; the process keeps running.
- **Stopped interface:** three timeouts in a row, or one heap overflow, stop the interface. New messages are still received and stored; they wait (pending) until the code is fixed and the engine restarted. The log says `interface stopped` with the reason.

A loop in a shared library file, resumed after an `await`, can't be traced to one interface (no interface file is on the stack): every transform running on that thread is retried. Keep heavy work in the interface's own files, or give the interface its own isolate.

## When to use a dedicated isolate

Give an interface its own threads and isolate when it:

- carries very high volume (for example a large Epic ADT feed) and needs parallel lanes,
- is critical enough that you want it fully separated from other code, or
- uses third-party code you can't vouch for.

```yaml
# interface.yaml
isolation: dedicated
workers: 8
```

Dedicated interfaces never share a global scope with anything else.
