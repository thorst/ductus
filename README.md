# Ductus

**A code-first integration engine for healthcare.** Write interfaces in TypeScript, run them on a fast Rust runtime, and operate them with monitoring, replay and data lifecycle tools built in.

> Early development (v0.2). Not production-ready.

## What an interface looks like

`interfaces/adt-in/interface.yaml`:

```yaml
name: adt-in
source:
  mllp: { port: 6661 }
script: transform.ts
ordering:
  key: PID-3.1        # messages for the same MRN stay in order
destination:
  mllp: { host: 127.0.0.1, port: 6662 }
```

`interfaces/adt-in/transform.ts`:

```ts
import type { Message } from "@ductus/sdk";

export function transform(msg: Message) {
  if (msg.MSH[9][1] !== "ADT") return null;      // filter
  msg.PID[5][1] = msg.PID[5][1].toUpperCase();   // PID-5.1, HL7 numbering
  for (const obx of msg.OBX) obx[5][1] = obx[5][1].trim();
  return msg;
}
```

Hovering `msg.PID[5]` in VS Code shows **PID-5 Patient Name (XPN)**.

`interfaces/adt-in/transform.test.ts`:

```ts
import { expect, hl7, test } from "@ductus/sdk";
import { transform } from "./transform.ts";

test("filters non-ADT", () => {
  expect(transform(hl7("orm-o01.hl7"))).toBeNull();   // fixtures/orm-o01.hl7
});
```

## Reliability options

```yaml
isolation: dedicated     # own threads + V8 isolate (default: shared pool) - see docs/shared-pool.md
workers: 4               # parallel MRN lanes, dedicated only
dedupe:
  keys: [MSH-10]         # or business keys, e.g. [PID-3.1, EVN-2]
  window: 24h
rate_limit:
  per_minute: 100        # e.g. a device that can't take bursts
  burst: 500             # average mode (token bucket); or mode: hard
  over_limit: wait       # or drop
retry:
  max_attempts: 10       # default: retry until delivered, backing off 1s -> 60s
destination:
  mllp:
    host: 10.0.0.5
    port: 6662
    ack: wait            # or none: send and move on
    on_reject: quarantine  # AR handling; or retry. AE always retries.
```

## Commands

| Command | What it does |
|---|---|
| `ductus run` | Run every interface in the project |
| `ductus test` | Run each interface's `*.test.ts` against its `fixtures/` |
| `ductus validate` | Check configs, code loading and TypeScript types (exits 1 on problems) |
| `ductus types` | Write editor typings and `tsconfig.json` for VS Code |
| `ductus status` | Message counts by interface and status |
| `ductus gen` / `ductus sink` | Synthetic HL7 traffic and an order-verifying receiver |

## Guarantees so far

- Messages are stored in PostgreSQL **before** they're acknowledged; a crash loses nothing.
- Messages with the same ordering key (e.g. MRN) are delivered in order, across restarts.
- Delivery is at-least-once: a crash can cause a rare duplicate (deduplication arrives in v0.3).

`scripts/demo-crash-recovery.sh` proves the first two by killing the engine mid-stream.

## Building

Requires Rust (stable) and PostgreSQL. TypeScript type checking uses an embedded copy of the TypeScript 6 compiler (`vendor/typescript`, Apache-2.0); no Node.js needed.

```
cargo build --release
DUCTUS_TEST_DATABASE_URL=postgres://user:pass@localhost/db cargo test
```

## License

Apache-2.0. See [LICENSING.md](LICENSING.md) for what's open source, what's commercial, and our commitments.
