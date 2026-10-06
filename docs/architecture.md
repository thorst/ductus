# Ductus architecture

How the engine works, and the rules that keep it correct. Read this before changing the runtime.

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

One binary (`ductus`) plus PostgreSQL. No other services.

## Invariants (never break these)

1. **Persist before acknowledge.** A source only ACKs (MLLP `AA`, HTTP `202`) after the message row is committed. If storing fails, the sender gets a retryable error (`AE`, `503`). Nothing acknowledged is ever lost by an engine crash. For files, "acknowledge" is moving or deleting the file: that happens only after every record in it is committed.
2. **At-least-once delivery.** A message stays `pending` until its destination confirms. A crash after sending but before recording `sent` re-sends it on restart; dedupe at ingest and idempotent receivers cover this. Sent statuses are flushed in batches of ≤50 and always before a retry back-off, so the re-send window is small.
3. **One owner per lane = per-key order.** The ordering key (e.g. MRN) hashes (FNV-1a, stable forever: `ductus_core::partition_of`) to one of N virtual partitions ("lanes"). Each lane is owned by exactly one worker slot, which processes its pending messages strictly by id. A failed message blocks its slot's lanes until it succeeds or is quarantined: that's the price of ordering.
4. **Migrations are expand-only.** Never drop or rename a column/table in a migration that an older binary still uses. Older binaries tolerate unknown newer migrations (`set_ignore_missing`), so rollback = reinstall the previous binary.
5. **No credentials in interface code.** Secrets come from `secrets/<env>.json` (encrypted) via `${secret:NAME}` in YAML or `secret()` in code; connections are named resources (`resources.yaml`).
6. **Interface code is sandboxed from the network and filesystem** except through engine services (`db()`, `fetch()`, `env()`, `secret()`). Remote module imports are refused.

## Crates

| crate | role |
|---|---|
| `ductus-core` | `interface.yaml` (`config.rs`), formats (`format.rs`), environments + `${VAR}` substitution (`env.rs`), encrypted secrets (`secrets.rs`), `resources.yaml` (`resources.rs`: Postgres and SQL Server databases, SFTP servers), schedule timing (`schedule.rs`), lane hashing, port-conflict checks, listeners and outbound connections (`ports.rs`) |
| `ductus-hl7` | HL7v2 essentials in Rust: field lookup by path, ACK generation (ingest only; interface code uses the SDK) |
| `ductus-mllp` | MLLP framing, listener (in order per connection), reconnecting client (with or without waiting for ACK) |
| `ductus-store` | Postgres: migrations, `messages` (day-partitioned; also the work queue), dedupe keys, status updates |
| `ductus-script` | Embedded V8 (deno_core): module loader (local files + embedded `@ductus/sdk`), multi-interface isolates, async calls (`begin`/`settle`), native ops (`Services` trait), embedded TypeScript 6 type checker + shared-pool lints |
| `ductus-runtime` | The engine: planning threads, `Intake` (ingest for any source, single or batched), MLLP + HTTP servers, file/SFTP watcher (`files.rs`) and record splitter (`split.rs`), SFTP client (`sftp.rs`), schedules (`schedule.rs`), workers, destinations, rate limiter, format-aware keys, engine services (Postgres pools; SQL Server pool in `mssql.rs`) |
| `ductus-cli` (`ductus`) | `run`, `test`, `validate`, `ports`, `types`, `secret`, `status`, `gen`, `sink` (tests in `tests/cli.rs` run the real binary) |
| `sdk/` | The TypeScript SDK, compiled into the binary: HL7 model, formats (CSV/fixed/XML), HTTP request/response, services, test framework, typings generator |

## Message lifecycle

`messages.status`: `0 pending`, `1 sent`, `2 filtered` (dropped on purpose: `return null` / `kill(reason)`, or over a rate limit with `drop`), `3 quarantined` (needs a human), `4 duplicate` (dedupe hit; stored for audit). `last_error` holds the reason.

Worker per message: transform → outcome:
- deliver → rate limit → send → `sent` (batched)
- `Filter(reason)` → `filtered`
- `Quarantine(reason)` (transform threw, `quarantine()`, AR, 4xx) → `quarantined`
- `Retry` (error text contains `[ductus:transient]`, e.g. DB unreachable) → back off 1s→60s, retry the same message (order kept)
Delivery failures: transient → back off and retry (forever unless `retry.max_attempts`); permanent → quarantine.

### Files (file and SFTP sources)

- One `FileWatcher` per interface over a `Backend` (local folder or SFTP connection). Each poll lists the folder; a file is ready when its size and time are unchanged since the last poll (or its `marker` exists). Dot-files are skipped. Ready files go oldest first, one at a time; a failure stops the poll so later files wait (file order kept).
- `split: records` streams the file through `split::Records` (256 KB buffer, never the whole file): HL7 per MSH, CSV per record with the header prepended, others per line. Records go to `Intake::accept_batch` 500 at a time (or 4 MB): **one transaction stores the records and the file's progress** (`source_state`: size, time, byte offset, records, done).
- After a crash, the saved offset is where reading resumes (same size and time) or the file is read again from the start (it changed). `done: true` is saved with the last batch, so a crash before the move doesn't read the file again. Then move/delete, then the progress row is deleted.
- Unsplit files (one message each) are capped at 256 MB; bigger ones are left in place with an error.

### Schedules

- `schedule::run` sleeps until the next due time (re-checking the clock every minute), then `Intake::accept_scheduled` stores a message `{"due", "previous"}` **and** the schedule state (`anchor`, `previous`) in one transaction, unless a run of this interface is still pending (then the run is skipped). The worker runs `transform` on it like any message; the transform's input format is JSON and `format`/`output` describe what it produces.
- `ductus_core::schedule::Timing`: `every` runs at start then every interval; `cron` (croner, server local time) waits for its next time. Missed runs collapse into one run on start (`anchor_after`).

## Threads and isolates

- **Shared pool** (default `isolation: shared`): `--threads` (default = cores) OS threads, each a tokio current-thread runtime + `LocalSet` + **one** V8 isolate hosting many interfaces. Each interface is one async task (`Worker`); shared interfaces have one slot (all lanes). See `docs/shared-pool.md` for the scoping rules interface authors follow.
- **Dedicated** (`isolation: dedicated`, `workers: N`): N threads, each with its own isolate, lanes split round-robin.
- V8 isolates can't move between threads; the isolate is `Rc<RefCell<Isolate>>` per thread and is **only borrowed for synchronous calls, never across an await**.
- **Async transforms**: `Isolate::begin` returns `Call::Done` (sync transform) or `Call::Pending` (a future not borrowing the isolate). A per-thread `drive_event_loop` task polls the JS event loop when kicked, so many interfaces can await DB calls concurrently on one thread. `settle` converts the result.
- Wake-ups: ingest calls `notify_one` on the owning slot's `Notify` (permit kept if busy); a 30 s idle poll is only a safety net.

## SDK ↔ engine protocol

- Each interface is loaded through a generated glue module: `export const run = (raw) => __invoke(entry.transform, raw, formats)` where `formats` = `InterfaceConfig::format_spec()` (`{"input", "output", "http"}`).
- `__invoke` decodes the payload by format, calls `transform`, serializes the result. Special results: `null` → filtered; `kill(reason)` → string prefixed `ductus_script::KILL`; errors containing `QUARANTINE` / `TRANSIENT` markers are classified by the worker.
- Respond-mode HTTP: the engine passes a JSON envelope (method, path, params, query, headers, body); the SDK returns a JSON envelope (status, headers, body).
- Native ops (`ops.rs` / `ops.js`): `op_ductus_db_query/execute/fetch/variable/secret`, exposed to the SDK as `globalThis.__ductus_ops`. The runtime implements the `Services` trait (`services.rs`); parameters are always bound.
  - Postgres: queries are wrapped as `WITH ductus_q AS (<sql>) SELECT json_agg(...)` so Postgres produces the JSON.
  - SQL Server (`mssql.rs`, tiberius): `$1` → `@P1` outside literals/quoted names/comments; rows are converted in Rust to match Postgres's JSON (numbers, ISO 8601 times, `\x` hex binary, upper-case GUIDs; unnamed columns `column<N>`); first result set only. Own small pool: a connection goes back only after a call that succeeded (a failed one may be mid-response or inside an open transaction); idle > 5 min is closed (firewalls drop quiet connections); Azure/AG routing is followed once; named instances via SQL Browser.
  - `fetch()` (`services.rs` `http_fetch`, one shared reqwest client): the engine returns every status as a response and marks only network errors/timeouts transient; the SDK turns 5xx/408/429 into transient errors unless `retry: false`. Responses capped at 16 MB; messages never show the query string or URL password.
  - Under `ductus test` the SDK swaps the ops for mocks (`mock.db/fetch/env/secret`, reset before each test; unmocked calls fail with how to mock them), and tests may be async (`run_tests` drives the event loop).
  - Pools: one shared pool per database resource (`max_connections`). A dedicated interface can have its own (`dedicated_pools: { patient_db: 4 }`), created once and shared by its worker threads; its `db()` calls use it before the shared one (a bulkhead: a busy interface can't starve the others). Shared-pool interfaces can't: the calling interface isn't known inside a shared isolate.
  - DB and HTTP calls are spawned on the **main multi-thread runtime**, not the worker thread's: a socket is served by the runtime that opened it, and a worker thread busy in V8 would delay its replies.
  - Temporary (message waits and retries): connection/TLS errors, pool timeouts, deadlocks, failovers, **login failures and missing databases** (fixed by fixing config; quarantining every message would be worse). Everything else (SQL errors) is permanent → quarantine.

## Storage

- `messages`: partitioned by day on `received_at`; partial index `(interface, partition_key, id) WHERE status = 0` is the queue. Partitions created 3 days ahead, hourly. Retention = drop partitions (v1.3).
- `dedupe_keys(interface, key, seen_at)`: checked and claimed in the same transaction as the message insert (one commit).
- `durability: relaxed` sets `synchronous_commit = off` for that insert only.
- `source_state(interface, key, value jsonb)`: what sources remember (file progress `file:<name>`, `schedule`). Always written in the same transaction as the messages it accounts for.
- Batched inserts (`Store::insert_batch`) use one `INSERT ... SELECT FROM unnest(...) WITH ORDINALITY ORDER BY n` (ids keep the batch's order) and claim dedupe keys in bulk (repeats inside the batch are duplicates after the first).

## Performance notes (measured, WSL, 2.5 ms fsync disk)

- One sender connection is bounded by one durable commit per ACK: ~443 msg/s strict, ~868/s relaxed; 16 connections ~2,100/s (group commit).
- SDK: ~14.8k realistic ORU transforms/s per thread. Embedded V8 call overhead is negligible.
- 1,001 interfaces: ready in 0.4 s, ~111 MB, 0 idle CPU.
- File ingest (`split: records`, `durability: relaxed`): ~148k CSV rows/s (300,000 rows, 13 MB, 500 per transaction).
