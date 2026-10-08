# Ductus architecture

How the engine works, and the rules that keep it correct. Read this before changing the runtime. What interface developers need to know about the resulting behaviour is in [nuances.md](nuances.md); the tables are described in [schema.md](schema.md) (for transparency, not an API).

## The shape of it

```
 sources                 store (Postgres)              workers                       destinations
 ───────                 ────────────────              ───────                       ────────────
 MLLP listener ─┐                                      pool thread 0..N  ─┐          MLLP client
 HTTP (queue)  ─┤   Intake          messages table ─► (shared V8 isolate) ├─► Sender ─ HTTP client
 File / SFTP   ─┼─► persist, then ─► (day partitions,   dedicated threads ─┘          File
 Schedule      ─┘   acknowledge      = the queue)      (own isolate)                 SFTP
                                                        │
 HTTP (respond) ───────────────► channel ──────────────►┘ transform(req) → HTTP response (no queue)
```

One binary (`ductus`) plus PostgreSQL. No other services. The engine also serves its REST API (`/api/v1`, default `127.0.0.1:7700`), which is how the UI, scripts and `ductus drain` see and steer it.

## Invariants (never break these)

1. **Persist before acknowledge.** A source only ACKs (MLLP `AA`, HTTP `202`) after the message row is committed. If storing fails, the sender gets a retryable error (`AE`, `503`). Nothing acknowledged is ever lost by an engine crash. For files, "acknowledge" is moving or deleting the file: that happens only after every record in it is committed.
2. **At-least-once delivery.** A delivery stays `pending` until its destination confirms. A crash after sending but before recording `sent` re-sends it on restart; dedupe at ingest and idempotent receivers cover this. Sent statuses are flushed in batches of ≤50 and always before a retry back-off, so the re-send window is small.
3. **One owner per lane and destination = per-key order.** The ordering key (e.g. MRN) hashes (FNV-1a, stable forever: `ductus_core::partition_of`) to one of N virtual partitions ("lanes"). For each destination, each lane is owned by exactly one worker (slot), which processes its pending deliveries strictly by message id. A failed message blocks its slot's lanes *for that destination* until it succeeds or is quarantined: that's the price of ordering.
4. **Migrations are expand-only.** Never drop or rename a column/table in a migration that an older binary still uses. Older binaries tolerate unknown newer migrations (`set_ignore_missing`), so rollback = reinstall the previous binary.
5. **No credentials in interface code.** Secrets come from `secrets/<env>.json` (encrypted) via `${secret:NAME}` in YAML or `secret()` in code; connections are named resources (`resources.yaml`).
6. **Interface code is sandboxed from the network and filesystem** except through engine services (`db()`, `fetch()`, `env()`, `secret()`). Remote module imports are refused.

## Crates

| crate | role |
|---|---|
| `ductus-core` | `interface.yaml` (`config.rs`), formats (`format.rs`), environments + `${VAR}` substitution (`env.rs`), encrypted secrets (`secrets.rs`), `resources.yaml` (`resources.rs`: Postgres and SQL Server databases, SFTP servers, HTTP APIs; each may name its `application`/`vendor`), `about:` and host/subnet matching (`about.rs`), `ductus impact` and the `about:` completeness check (`impact.rs`), owners and their in-place reassignment (`owners.rs`: edits `owner:` values in the YAML text, then checks the result parses to exactly the intended change), schedule timing (`schedule.rs`), lane hashing, port-conflict checks, listeners and outbound connections (`ports.rs`) |
| `ductus-hl7` | HL7v2 essentials in Rust: field lookup by path, ACK generation (ingest only; interface code uses the SDK) |
| `ductus-mllp` | MLLP framing, listener (in order per connection), reconnecting client (with or without waiting for ACK) |
| `ductus-store` | Postgres: migrations, `messages` (day-partitioned, overall status), `deliveries` (status per destination; each destination's work queue), dedupe keys, reconcile at startup; for the API (`api.rs`): tokens (hashed), the message browser, queue counts |
| `ductus-script` | Embedded V8 (deno_core): module loader (local files + embedded `@ductus/sdk`), multi-interface isolates, async calls (`begin`/`settle`), runaway guard (`guard.rs`: time budgets, heap limit), native ops (`Services` trait), embedded TypeScript 6 type checker + shared-pool lints, and `analyze` (the `db("...")`/`api("...")` calls code makes, resolved through import aliases by the checker, plus the import graph, so validate and `impact` know what each interface uses) |
| `ductus-runtime` | The engine: planning threads, `Intake` (ingest for any source, single or batched), MLLP + HTTP servers, file/SFTP watcher (`files.rs`) and record splitter (`split.rs`), SFTP client (`sftp.rs`), schedules (`schedule.rs`), workers, destinations, rate limiter, format-aware keys, engine services (Postgres pools; SQL Server pool in `mssql.rs`), connection checks for `ductus doctor` (`doctor.rs`: what to check from the project (`plan`), then DNS, TCP to every address with the source address, TLS through the same platform verifier as the HTTP client, then each protocol's login through the engine's own connection code), what code uses (`project.rs`: the type checker's `db`/`api` references, shared by validate, impact, doctor and the API), the drain gate (`gate.rs`), the running engine as the API sees it (`control.rs`: interfaces, health, queue monitor, drain), and the REST API (`api/`: axum + utoipa) |
| `ductus-cli` (`ductus`) | `run`, `test`, `validate`, `ports`, `impact`, `doctor`, `owners` (`owners.rs`), `token`, `drain`, `resume` (`admin.rs`: tokens straight in the database; drain/resume through the API), `new interface` (`new.rs`: templates per source/format; ports from `ductus.yaml` ranges, free in every environment), `types`, `secret`, `status`, `gen`, `sink` (tests in `tests/cli.rs` run the real binary) |
| `sdk/` | The TypeScript SDK, compiled into the binary: HL7 model, formats (CSV/fixed/XML), HTTP request/response, services, test framework, typings generator |

## Message lifecycle

Statuses: `0 pending`, `1 sent`, `2 filtered` (dropped on purpose: `return null` / `kill(reason)`, over a rate limit with `drop`, or its destination was removed), `3 quarantined` (needs a human), `4 duplicate` (dedupe hit; stored for audit, never queued). `last_error` holds the reason.

### Destinations and deliveries

- An interface has one destination (`destination:`, named `default`) or several (`destinations:` by name). Each destination has its own function (`transform:`, default `transform`), output format, `retry`, `rate_limit` (its own allowance) and `save_output`.
- **`deliveries`** (day-partitioned like `messages`): one row per queued message and destination, inserted **in the same statement** as the message (so still one commit per ACK). Its partial index `(interface, destination, partition_key, message_id) WHERE status = 0` is each destination's queue. Rows carry `status`, `attempts`, `last_error` and, with `save_output`, `output`.
- **One worker per (slot, destination).** Each runs the destination's function on the original message (with `meta`: `id`, `receivedAt`, `destination`), then delivers. A destination that's down blocks only its own lanes; the others keep going. Ingest wakes every destination's worker for the lane's slot (`Wake`).
- **`messages.status` is the overall status**: pending until no delivery is pending; then quarantined if any is, else sent if any is, else filtered. `Store::settle` sets it in a statement of its own **after** the delivery update commits: of two destinations finishing a message at once, the later always sees both done (in one statement, each could see the other pending: write skew). Older binaries read only `messages.status`, so rolling back still works.
- **Reconcile at startup** (`Store::reconcile`, all interfaces in four queries): deliveries for pending messages that lack one (stored by an older version, or a new destination); pending deliveries to removed destinations → filtered; pending deliveries whose message an older version finished → its status; then settle.
- The script host loads an interface module **once** and binds one entry per destination (`Isolate::add_functions` → `FunctionId`s); runaways are attributed per interface (`InterfaceId`), so every in-flight call of that interface takes the blame.

Worker per delivery: function → outcome:
- deliver → rate limit → send → `sent` (batched)
- `Filter(reason)` → `filtered`
- `Quarantine(reason)` (transform threw, `quarantine()`, AR, 4xx) → `quarantined`
- `Retry` (error text contains `[ductus:transient]`, e.g. DB unreachable) → back off 1s→60s, retry the same message (order kept)
- `Runaway` (the code ran past its `timeout` without pausing, or filled the isolate's heap) → `quarantined`; three timeouts in a row or one heap overflow **stop the interface** (`health.rs`): its messages stay `pending` until the code is fixed and the engine restarted; respond-mode requests get 503
Delivery failures: transient → back off and retry (forever unless `retry.max_attempts`); permanent → quarantine.

### Files (file and SFTP sources)

- One `FileWatcher` per interface over a `Backend` (local folder or SFTP connection). Each poll lists the folder; a file is ready when its size and time are unchanged since the last poll (or its `marker` exists). Dot-files are skipped. Ready files go oldest first, one at a time; a failure stops the poll so later files wait (file order kept).
- `split: records` streams the file through `split::Records` (256 KB buffer, never the whole file): HL7 per MSH, CSV per record with the header prepended, others per line. Records go to `Intake::accept_batch` 500 at a time (or 4 MB): **one transaction stores the records and the file's progress** (`source_state`: size, time, byte offset, records, done).
- After a crash, the saved offset is where reading resumes (same size and time) or the file is read again from the start (it changed). `done: true` is saved with the last batch, so a crash before the move doesn't read the file again. Then move/delete, then the progress row is deleted.
- Unsplit files (one message each) are capped at 256 MB; bigger ones are left in place with an error.

### Schedules

- `schedule::run` sleeps until the next due time (re-checking the clock every minute), then `Intake::accept_scheduled` stores a message `{"due", "previous"}` **and** the schedule state (`anchor`, `previous`) in one transaction, unless a run of this interface is still pending (then the run is skipped). The worker runs `transform` on it like any message; the transform's input format is JSON and `format`/`output` describe what it produces.
- `ductus_core::schedule::Timing`: `every` runs at start then every interval; `cron` (croner, server local time) waits for its next time. Missed runs collapse into one run on start (`anchor_after`).

## REST API and drain mode

- **`/api/v1`** (`ductus-runtime/src/api/`): axum, one listener (`api.listen` in ductus.yaml, default `127.0.0.1:7700`, `off` to disable; `--api-listen` overrides). Routes are declared with `utoipa_axum::routes!`, which builds the router and the OpenAPI document (`/api/v1/openapi.json`) from the same handler annotations: the spec can't drift from the code. The UI will use only this API.
- **Tokens:** `Authorization: Bearer dux_<64 hex>`. Stored as SHA-256 hashes (`api_tokens`); every call looks the hash up (an index hit), so revoking works at once. Roles: viewer (read), operator (+ drain, connection checks), admin (+ tokens). The first token is made on the server with `ductus token create`, which writes to the database directly.
- **What it serves:** engine info and `/health` (no token: for load balancers); interfaces with state (running/stopped by the runaway guard), drain gate and each destination's pending/quarantined counts; the message browser (an interface's messages newest first, paged by id; one message with payload and deliveries); drain status, start and stop; `impact`, `owners` and `doctor` as JSON (the same code as the CLI; doctor skips listening ports, which the engine itself holds); token management; `/events`, Server-Sent Events with the same counts whenever they change.
- **Queue counts** come from the partial indexes (`deliveries_pending`, `deliveries_quarantined`): their cost grows with the backlog, not with what's kept (about 10 ms for 15k pending here). One **monitor** task (`control.rs`) counts every 2 s, only while someone is subscribed to `/events` or a drain is running, and shares the result with every subscriber.
- **Drain:** each interface has a `Gate` (`gate.rs`). Draining closes them all: `Intake` refuses to store (`gate::Closed`), MLLP listeners close their socket and connections (`ductus_mllp::serve_until`), queued HTTP answers 503 + `Retry-After`, file watchers and schedules wait for the gate. Every store holds a counted `Pass`; the monitor declares the engine **drained** once every gate is closed, no pass is in flight, and none of this engine's destinations has a pending delivery. With `stop`, the engine then exits (`Engine::shutdown`). Resume opens the gates; MLLP listeners bind again.

## Threads and isolates

- **Shared pool** (default `isolation: shared`): `--threads` (default = cores) OS threads, each a tokio current-thread runtime + `LocalSet` + **one** V8 isolate hosting many interfaces. Each interface is one async task (`Worker`); shared interfaces have one slot (all lanes). See `docs/shared-pool.md` for the scoping rules interface authors follow.
- **Dedicated** (`isolation: dedicated`, `workers: N`): N threads, each with its own isolate, lanes split round-robin.
- V8 isolates can't move between threads; the isolate is `Rc<RefCell<Isolate>>` per thread and is **only borrowed for synchronous calls, never across an await**.
- **Async transforms**: `Isolate::begin` returns `Call::Done` (sync transform) or `Call::Pending` (a future not borrowing the isolate). A per-thread `drive_event_loop` task polls the JS event loop when kicked, so many interfaces can await DB calls concurrently on one thread. `settle` converts the result.
- Wake-ups: ingest calls `notify_one` on the owning slot's `Notify` (permit kept if busy); a 30 s idle poll is only a safety net.
- **Store calls from workers run on the main runtime** (`Worker::db`). The Postgres pool is shared with intake, and a connection's socket is served by the runtime that opened it: a connection opened on a worker thread stalled intake's commits (the senders' ACKs) whenever that thread was busy in V8. Measured in the runaway demo: one sender's 200 ACKs took 4.0 s instead of 0.9 s; a pass-through run 3.3 s → 0.5 s.

### Runaway scripts (`ductus-script/src/guard.rs`)

- Every call into V8 is **armed** with a deadline: the interface's `timeout` (default 5 s, 10 ms–10 min) for its `transform`, the module's for loading it. A single `script-watchdog` thread checks all isolates every 10 ms and, past a deadline, calls `IsolateHandle::request_interrupt`. The interrupt runs **on the isolate's thread inside the running code**: it checks the call is still the one armed (generation counter), reads the JS stack to find whose code it is (module URL, then folder), and calls `terminate_execution`. `disarm` then cancels the termination; the isolate stays usable. Terminating only from inside the interrupt means a call that finished in the meantime can never be hit.
- The event loop runs continuations of *any* interface, so it's armed with the **shortest** budget on the isolate; when it passes, the interrupt extends the deadline to the budget of the interface found on the stack (or the longest, if none is found).
- Heap: each isolate is created with `heap_limits(0, isolates.heap_mb)` (`ductus.yaml`, default 512). Near the limit V8 calls our callback: we raise the limit by a quarter (or the process dies) and stop the running call the same way, then run a full GC.
- **A termination in the event loop drops every queued continuation** (V8 clears the microtask queue), not just the runaway's: every async transform in flight on that thread will never settle. The worker's `drive_event_loop` signals them all (`watch`): the culprit gets the `Runaway`, the others a transient error and are retried.
- Limits: code in a shared module resumed after an `await` has no interface file on its stack, so it can't be attributed: all in-flight transforms are retried and it can recur. While runaway code runs, its thread's other interfaces wait up to `timeout`.

## SDK ↔ engine protocol

- Each interface is loaded through a generated glue module: `export const run = (raw) => __invoke(entry.transform, raw, formats)` where `formats` = `InterfaceConfig::format_spec()` (`{"input", "output", "http"}`).
- `__invoke` decodes the payload by format, calls `transform`, serializes the result. Special results: `null` → filtered; `kill(reason)` → string prefixed `ductus_script::KILL`; errors containing `QUARANTINE` / `TRANSIENT` markers are classified by the worker.
- Respond-mode HTTP: the engine passes a JSON envelope (method, path, params, query, headers, body); the SDK returns a JSON envelope (status, headers, body).
- Native ops (`ops.rs` / `ops.js`): `op_ductus_db_query/execute/fetch/variable/secret`, exposed to the SDK as `globalThis.__ductus_ops`. The runtime implements the `Services` trait (`services.rs`); parameters are always bound.
  - Postgres: queries are wrapped as `WITH ductus_q AS (<sql>) SELECT json_agg(...)` so Postgres produces the JSON.
  - SQL Server (`mssql.rs`, tiberius): `$1` → `@P1` outside literals/quoted names/comments; rows are converted in Rust to match Postgres's JSON (numbers, ISO 8601 times, `\x` hex binary, upper-case GUIDs; unnamed columns `column<N>`); first result set only. Own small pool: a connection goes back only after a call that succeeded (a failed one may be mid-response or inside an open transaction); idle > 5 min is closed (firewalls drop quiet connections); Azure/AG routing is followed once; named instances via SQL Browser.
  - `fetch()` (`services.rs` `http_fetch`, one shared reqwest client): the engine returns every status as a response and marks only network errors/timeouts transient; the SDK turns 5xx/408/429 into transient errors unless `retry: false`. Responses capped at 16 MB; messages never show the query string or URL password.
  - `api(name)` uses the same op with `api` set and a path as `url`; the engine (`resolve_api`) puts the path under the API's URL, adds its headers (the code's win) and its timeout, so tokens stay in `resources.yaml` and never enter V8. HTTP destinations with `api:` are resolved the same way when their sender is built.
  - Under `ductus test` the SDK swaps the ops for mocks (`mock.db/fetch/env/secret`, reset before each test; unmocked calls fail with how to mock them), and tests may be async (`run_tests` drives the event loop).
  - Pools: one shared pool per database resource (`max_connections`). A dedicated interface can have its own (`dedicated_pools: { patient_db: 4 }`), created once and shared by its worker threads; its `db()` calls use it before the shared one (a bulkhead: a busy interface can't starve the others). Shared-pool interfaces can't: the calling interface isn't known inside a shared isolate.
  - DB and HTTP calls are spawned on the **main multi-thread runtime**, not the worker thread's: a socket is served by the runtime that opened it, and a worker thread busy in V8 would delay its replies.
  - Temporary (message waits and retries): connection/TLS errors, pool timeouts, deadlocks, failovers, **login failures and missing databases** (fixed by fixing config; quarantining every message would be worse). Everything else (SQL errors) is permanent → quarantine.

## Storage

- `messages`: partitioned by day on `received_at`; partial index `(interface, partition_key, id) WHERE status = 0` (the queue of older versions; still used to find pending messages). `deliveries` gets a partition for every `messages` partition, with the same bounds (copied from the catalog). Partitions created 3 days ahead, hourly, in two statements (`messages`, then `deliveries`): one transaction holding both table locks deadlocked with status settling, which reads `deliveries` then updates `messages` (migration 0006; reproduced 8/8 by `partition_maintenance_never_deadlocks_with_settling`). Each waits at most 2 s for its lock (`lock_timeout`), then gives up until the next hour: a partition DDL waiting behind a long query makes every later insert queue behind it (measured: an insert waited the whole 4 s of a "report"; `partition_maintenance_never_holds_up_inserts`). Startup retries for up to a minute. **Last resort:** when tomorrow's partitions are still missing (every hourly try for two days was blocked), maintenance waits for the lock however long it takes (`lock_timeout = 0`, logged as a warning): new messages wait behind it for as long as the blocking query runs, which beats having nowhere to store them at midnight (`partition_maintenance_waits_when_the_day_is_close`). With nothing to create, maintenance takes no lock at all. **Locking rules** and the stress test that checks them (`everything_at_once_without_lock_errors`: inserts with colliding dedupe keys, a batch writer, three destinations draining, partitions created, keys purged, all at once; it catches the old deadlock 3/3): see `decisions.md` "Locking" in the planning repo, and CLAUDE.md. Retention = drop partitions (v0.5 step 5).
- `dedupe_keys(interface, key, seen_at)`: checked and claimed in the same transaction as the message insert (one commit).
- `durability: relaxed` sets `synchronous_commit = off` for that insert only.
- `source_state(interface, key, value jsonb)`: what sources remember (file progress `file:<name>`, `schedule`). Always written in the same transaction as the messages it accounts for.
- Batched inserts (`Store::insert_batch`) use one `INSERT ... SELECT FROM unnest(...) WITH ORDINALITY ORDER BY n` (ids keep the batch's order) and claim dedupe keys in bulk (repeats inside the batch are duplicates after the first).

## Performance notes (measured, WSL, 2.5 ms fsync disk)

- One sender connection is bounded by one durable commit per ACK: ~443 msg/s strict, ~868/s relaxed; 16 connections ~2,100/s (group commit).
- SDK: ~14.8k realistic ORU transforms/s per thread. Embedded V8 call overhead is negligible.
- 1,001 interfaces: ready in 0.4 s, ~111 MB, 0 idle CPU.
- File ingest (`split: records`, `durability: relaxed`): ~100k CSV rows/s (300,000 rows, 13 MB, 500 per transaction); ~155k before status per destination (a delivery row costs about as much to insert as the message). Strict MLLP ingest (16 connections) unchanged: ~3,100/s either way, it's bound by the commit flush.
- Fan-out (`scripts/bench-fanout.sh`, 6,000 messages, 16 connections, strict, file destinations): 1 destination 3,125 ACKs/s, all delivered in 3.9 s; 5 destinations 3,081/s, 3.6 s (8,444 deliveries/s); 25 destinations 1,710/s, 8.0 s (18,776 deliveries/s). Ingest holds up to 5 destinations; at 25 the drain's status writes (~19k/s) compete with ingest for the same database. A single destination drains at ~1,500/s: a shared interface has one worker per destination (`isolation: dedicated` with `workers: N` for more).
