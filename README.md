# Ductus

**A code-first integration engine for healthcare.** Write interfaces in TypeScript, run them on a fast Rust runtime with PostgreSQL as the store and queue, and operate them with monitoring, replay and data lifecycle tools built in.

One binary plus PostgreSQL. No message broker, no JVM, no Node.js.

> Early development (v0.4 released, v0.5 in progress). Not production-ready yet.

## What an interface looks like

Each interface is a folder: config in `interface.yaml`, logic in TypeScript, tests next to it.

`interfaces/adt-in/interface.yaml`:

```yaml
name: adt-in
source:
  mllp: { port: 6661 }
script: transform.ts
ordering:
  key: PID-3.1          # messages for the same MRN stay in order
destinations:           # each gets every message, with its own queue, retries and status
  lab:
    mllp: { host: 10.0.0.5, port: 6662 }
  billing:
    http: { url: "https://billing.example.org/api/charges" }
    transform: toBilling
    output: json
```

`interfaces/adt-in/transform.ts`:

```ts
import { db, kill, type Message, type Meta } from "@ductus/sdk";

export function transform(msg: Message) {
  if (msg.MSH[9][1] !== "ADT") return null;      // filter
  msg.PID[5][1] = msg.PID[5][1].toUpperCase();   // PID-5.1, HL7 numbering
  for (const obx of msg.OBX) obx[5][1] = obx[5][1].trim();
  return msg;
}

export async function toBilling(msg: Message, meta: Meta) {
  const account = await db("billing_db").one("SELECT id FROM accounts WHERE mrn = $1", [msg.PID[3][1]]);
  if (!account) return kill("not billable");
  return { id: meta.id, account: account.id, event: msg.MSH[9][2] };   // meta.id: stable idempotency key
}
```

Hovering `msg.PID[5]` in VS Code shows **PID-5 Patient Name (XPN)**. `ductus validate` type-checks everything with an embedded TypeScript compiler.

`interfaces/adt-in/transform.test.ts`:

```ts
import { expect, hl7, mock, test } from "@ductus/sdk";
import { toBilling, transform } from "./transform.ts";

test("filters non-ADT", () => {
  expect(transform(hl7("orm-o01.hl7"))).toBeNull();   // fixtures/orm-o01.hl7
});

test("looks up the account", async () => {
  mock.db("billing_db", [{ id: 42 }]);                 // no database needed in tests
  const out = await toBilling(hl7("adt-a01.hl7"), { id: 1, receivedAt: "", destination: "billing" });
  expect(out).toEqual({ id: 1, account: 42, event: "A01" });
});
```

## What it does today

- **Sources:** MLLP, HTTP (queue a message, or answer the request directly as a REST endpoint), local folders and SFTP (crash-safe streaming of large files, record by record), schedules (`every: 15m` or cron).
- **Destinations:** MLLP, HTTP, files, SFTP. Several per interface, each independent: one vendor being down holds up only its own queue.
- **Routing between interfaces:** `destination: { interface: lab }` puts the message straight into another interface's queue, in the same transaction that marks it sent: no network hop, exactly once even across a crash. Chain and fan out by structure or ownership; `ductus validate` refuses loops and `ductus impact` follows the routes.
- **Formats:** HL7 v2 (lazy parsing, positional syntax), JSON, XML, CSV, fixed-width, text.
- **Interface code:** TypeScript in embedded V8, with `db()` (PostgreSQL and SQL Server), `api()` (named HTTP APIs whose tokens the engine adds), `fetch()`, `env()` and encrypted `secret()`. No direct network or file access; no credentials in code.
- **Know what connects to what:** each interface names the applications and vendors on each side (`about:`), resources name theirs, and `ductus impact --vendor "Epic Systems"` lists every interface affected, including through databases and APIs its code uses. `ductus owners` lists who to call about each interface and destination, and `ductus owners reassign` moves ownership when someone leaves.
- **Find out why a connection doesn't work before messages fail:** `ductus doctor` checks every connection from the server it runs on (DNS, TCP with the source address a firewall request needs, TLS certificate and expiry, each system's login) and says in plain words what a failure means. `--report` saves it for the network team or vendor.
- **Reliability:** persist before ACK, per-key ordering, dedupe, rate limits, retries with back-off, quarantine. A runaway script (endless loop, memory blow-up) is stopped without affecting its neighbours.
- **Environments:** one project, per-environment variables and encrypted secrets, named resources (`resources.yaml`).

```yaml
isolation: dedicated     # own threads + V8 isolate (default: shared pool) - see docs/shared-pool.md
workers: 4               # parallel MRN lanes, dedicated only
timeout: 5s              # longest the code may run without pausing
dedupe:
  keys: [MSH-10]         # or business keys, e.g. [PID-3.1, EVN-2]
  window: 24h
rate_limit:
  per_minute: 100        # e.g. a device that can't take bursts
  over_limit: wait       # or drop
retry:
  max_attempts: 10       # default: retry until delivered, backing off 1s -> 60s
durability: strict       # or relaxed: ACK before the disk flush, for much higher throughput
```

## Performance

Measured on a development laptop (WSL2, a slow disk at ~2.5 ms per flush), not a server, so treat these as a floor. For scale: a large hospital network's ADT feed of **1 million messages/day averages about 12 messages/second**, with peaks around 10x that.

| What | Measured |
|---|---|
| MLLP ingest, 16 sender connections, each message flushed to disk before its ACK | ~3,100 messages/s |
| Same, one sender connection (one disk flush per ACK) | ~440 messages/s strict, ~870/s relaxed |
| Fan-out: 6,000 messages to 5 destinations each | 8,400 deliveries/s, ingest unchanged |
| Fan-out: 6,000 messages to 25 destinations each | 18,800 deliveries/s |
| Large file ingest (CSV split into records, relaxed durability) | ~100,000 records/s |
| Realistic HL7 ORU transform in TypeScript | ~14,800 transforms/s per CPU thread |
| Startup, 1,001 interfaces (every interface's TypeScript loaded and compiled) | 0.4 s to ready, 111 MB memory, no CPU when idle |
| Startup, 301 interfaces, including checking stored deliveries against the config | 0.4 s to ready |

Fast startup is what makes upgrades and restarts uneventful: senders reconnect to a ready engine within a second, and nothing they sent is lost meanwhile. Details and how each was measured: [docs/architecture.md](docs/architecture.md#performance-notes-measured-wsl-25-ms-fsync-disk). Numbers on real server hardware are still to come.

### Scaling one interface

Most interfaces never need to: one destination of an interface in the shared pool drained about 1,500 messages/s in the measurements above, against an average of 12/s for a 1M/day feed. When one does, in this order:

1. **Set `ordering.key`** (e.g. `PID-3.1`). Without one, the interface is a single ordered lane and no number of workers can split it; with one, it has 64 lanes and only the same patient's messages wait on each other.
2. **`isolation: dedicated`**: its own threads and V8 isolate, away from its neighbours, with 4 workers by default.
3. **Raise `workers`**, up to about the number of CPU cores. Lanes are spread evenly over them.
4. **`dedicated_pools`** if it does database lookups, so it has connections of its own.

Past that, the limit is PostgreSQL, and mostly its disk: Ductus spends little CPU and memory per message, and every acknowledged message waits for one durable commit. A disk that flushes in 0.2 ms instead of 2.5 ms raises what one sender connection can do about tenfold. So: fast storage for Postgres's write-ahead log (NVMe, or a cloud disk with low write latency), more sender connections (commits are grouped), and `durability: relaxed` for feeds where losing the last fraction of a second on a database server crash is acceptable (an engine crash alone loses nothing either way). Tuning guidance and a built-in check are planned.

## Commands

| Command | What it does |
|---|---|
| `ductus run` | Run every interface in the project |
| `ductus new interface <name>` | A working interface (config, code, test, sample) on a free port |
| `ductus test` | Run each interface's `*.test.ts`, with mocks for databases and APIs |
| `ductus validate` | Check configs, code loading and TypeScript types for an environment (exits 1 on problems) |
| `ductus ports` | Listening ports and routes, outbound hosts (for firewall requests), conflicts |
| `ductus impact` | Which interfaces a vendor, application or host/subnet touches, how, and who owns them (vendor breach, VPN change) |
| `ductus doctor` | Check every connection from this server: DNS, TCP, TLS, logins, folders, ports; failures explained; `--report` to hand on |
| `ductus owners` | Who owns which interface and destination; `reassign` changes owners in place |
| `ductus secret` | Manage encrypted per-environment secrets |
| `ductus types` | Write editor typings and `tsconfig.json` for VS Code |
| `ductus status` | Message counts by interface, status and destination |
| `ductus gen` / `ductus sink` | Synthetic HL7 traffic and an order-verifying receiver |

## Guarantees

- Messages are stored in PostgreSQL **before** they're acknowledged (files are moved only after every record is stored). An engine crash loses nothing.
- Messages with the same ordering key (e.g. MRN) are delivered in order to each destination, across restarts.
- Delivery is at-least-once: a crash between sending and recording it can resend one message. Dedupe at ingest and `meta.id` (a stable key for idempotent receivers) cover this.
- Upgrades are in place and rollback is reinstalling the previous binary: database migrations only ever add.

Every guarantee has an end-to-end demo in `scripts/` that kills the engine mid-stream and checks the result; they all run in CI. Behaviour worth planning around is collected in [docs/nuances.md](docs/nuances.md).

## Building

Requires Rust (stable) and PostgreSQL. TypeScript type checking uses an embedded copy of the TypeScript 6 compiler (`vendor/typescript`, Apache-2.0); no Node.js needed.

```
cargo build --release
DUCTUS_TEST_DATABASE_URL=postgres://user:pass@localhost/db cargo test
```

## License

Apache-2.0. See [LICENSING.md](LICENSING.md) for what's open source, what's commercial, and our commitments.
