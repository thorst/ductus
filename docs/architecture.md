# Ductus architecture

How the engine works, and the rules that keep it correct. Read this before changing the runtime.

## The shape of it

```
 sources                 store (Postgres)              workers                       destinations
 ───────                 ────────────────              ───────                       ────────────
 MLLP listener ─┐                                      pool thread 0..N  ─┐          MLLP client
 HTTP (queue)  ─┼─► Intake.accept ─► messages table ─► (shared V8 isolate) ├─► Sender ─ HTTP client
 (file, SFTP,  ─┘   persist, then   (day partitions,   dedicated threads ─┘          File
  schedule: v0.4)   acknowledge      = the queue)      (own isolate)
                                                        │
 HTTP (respond) ───────────────► channel ──────────────►┘ transform(req) → HTTP response (no queue)
```

One binary (`ductus`) plus PostgreSQL. No other services.

## Invariants (never break these)

1. **Persist before acknowledge.** A source only ACKs (MLLP `AA`, HTTP `202`) after the message row is committed. If storing fails, the sender gets a retryable error (`AE`, `503`). Nothing acknowledged is ever lost by an engine crash.
2. **At-least-once delivery.** A message stays `pending` until its destination confirms. A crash after sending but before recording `sent` re-sends it on restart; dedupe at ingest and idempotent receivers cover this. Sent statuses are flushed in batches of ≤50 and always before a retry back-off, so the re-send window is small.
3. **One owner per lane = per-key order.** The ordering key (e.g. MRN) hashes (FNV-1a, stable forever: `ductus_core::partition_of`) to one of N virtual partitions ("lanes"). Each lane is owned by exactly one worker slot, which processes its pending messages strictly by id. A failed message blocks its slot's lanes until it succeeds or is quarantined: that's the price of ordering.
4. **Migrations are expand-only.** Never drop or rename a column/table in a migration that an older binary still uses. Older binaries tolerate unknown newer migrations (`set_ignore_missing`), so rollback = reinstall the previous binary.
5. **No credentials in interface code.** Secrets come from `secrets/<env>.json` (encrypted) via `${secret:NAME}` in YAML or `secret()` in code; connections are named resources (`resources.yaml`).
6. **Interface code is sandboxed from the network and filesystem** except through engine services (`db()`, `env()`, `secret()`; HTTP fetch is planned). Remote module imports are refused.

## Crates

| crate | role |
|---|---|
| `ductus-core` | `interface.yaml` (`config.rs`), formats (`format.rs`), environments + `${VAR}` substitution (`env.rs`), encrypted secrets (`secrets.rs`), `resources.yaml` (`resources.rs`), lane hashing, port-conflict checks |
| `ductus-hl7` | HL7v2 essentials in Rust: field lookup by path, ACK generation (ingest only; interface code uses the SDK) |
| `ductus-mllp` | MLLP framing, listener (in order per connection), reconnecting client (with or without waiting for ACK) |
| `ductus-store` | Postgres: migrations, `messages` (day-partitioned; also the work queue), dedupe keys, status updates |
| `ductus-script` | Embedded V8 (deno_core): module loader (local files + embedded `@ductus/sdk`), multi-interface isolates, async calls (`begin`/`settle`), native ops (`Services` trait), embedded TypeScript 6 type checker + shared-pool lints |
| `ductus-runtime` | The engine: planning threads, `Intake` (ingest for any source), MLLP + HTTP servers, workers, destinations, rate limiter, format-aware keys, engine services (DB pools) |
| `ductus-cli` (`ductus`) | `run`, `test`, `validate`, `types`, `secret`, `status`, `gen`, `sink` |
| `sdk/` | The TypeScript SDK, compiled into the binary: HL7 model, formats (CSV/fixed/XML), HTTP request/response, services, test framework, typings generator |

## Message lifecycle

`messages.status`: `0 pending`, `1 sent`, `2 filtered` (dropped on purpose: `return null` / `kill(reason)`, or over a rate limit with `drop`), `3 quarantined` (needs a human), `4 duplicate` (dedupe hit; stored for audit). `last_error` holds the reason.

Worker per message: transform → outcome:
- deliver → rate limit → send → `sent` (batched)
- `Filter(reason)` → `filtered`
- `Quarantine(reason)` (transform threw, `quarantine()`, AR, 4xx) → `quarantined`
- `Retry` (error text contains `[ductus:transient]`, e.g. DB unreachable) → back off 1s→60s, retry the same message (order kept)
Delivery failures: transient → back off and retry (forever unless `retry.max_attempts`); permanent → quarantine.

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
- Native ops (`ops.rs` / `ops.js`): `op_ductus_db_query/execute/variable/secret`, exposed to the SDK as `globalThis.__ductus_ops`. The runtime implements the `Services` trait (`services.rs`); DB queries are wrapped as `WITH ductus_q AS (<sql>) SELECT json_agg(...)` so Postgres produces JSON; parameters are always bound.

## Storage

- `messages`: partitioned by day on `received_at`; partial index `(interface, partition_key, id) WHERE status = 0` is the queue. Partitions created 3 days ahead, hourly. Retention = drop partitions (v1.3).
- `dedupe_keys(interface, key, seen_at)`: checked and claimed in the same transaction as the message insert (one commit).
- `durability: relaxed` sets `synchronous_commit = off` for that insert only.

## Performance notes (measured, WSL, 2.5 ms fsync disk)

- One sender connection is bounded by one durable commit per ACK: ~443 msg/s strict, ~868/s relaxed; 16 connections ~2,100/s (group commit).
- SDK: ~14.8k realistic ORU transforms/s per thread. Embedded V8 call overhead is negligible.
- 1,001 interfaces: ready in 0.4 s, ~111 MB, 0 idle CPU.
