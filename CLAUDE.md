# Ductus: engineering guide

Code-first healthcare integration engine: Rust runtime, embedded V8 for TypeScript interface code, PostgreSQL as store and queue. Open core (Apache-2.0). This repo is the open core; commercial features and planning live in the private `ductus-enterprise` repo (`planning/` has the vision, `decisions.md`, `roadmap.md`, `STATUS.md`).

**Read first:** `docs/architecture.md` (how it works + invariants), then `../ductus-enterprise/planning/STATUS.md` (where we are, what's next).

## Non-negotiables

- The invariants in `docs/architecture.md` (persist before ACK, at-least-once, one owner per lane, expand-only migrations, no credentials in code). If a change seems to need breaking one, stop and discuss.
- **Every change ships with tests.** Unit tests next to the code; DB tests in `crates/ductus-store/tests`; script/SDK tests in `crates/ductus-script/tests` and `sdk/tests/*.test.ts` (run inside the real engine); end-to-end in `scripts/demo-*.sh` (each ends with PASS/FAIL and runs in CI).
- **Before every commit:** `scripts/dev/check.sh` must end with `ALL CHECKS PASSED` (fmt, build incl. the binary, clippy `-D warnings`, all tests). For runtime changes also run `scripts/dev/demos.sh`.
- Commits: `git commit -s` (DCO), message explains what and why, ends with the Co-Authored-By trailer. Push to `main` (solo project, CI on every push).
- Keep `../ductus-enterprise/planning/` current: decisions in `decisions.md`, progress in `roadmap.md` and `STATUS.md`.

## Environment

- Windows host; **build and run everything in WSL2 Ubuntu** (`wsl -d Ubuntu`). Repo at `/mnt/d/Projects/integration_engine/ductus`. `CARGO_TARGET_DIR=$HOME/target` (building on `/mnt/d` is slow); binary at `~/target/release/ductus`.
- Postgres 18 in WSL: `postgres://todd:ductus@localhost/ductus_dev`.
- From PowerShell, multi-line or `$`-heavy shell commands get mangled: write a script file and run `wsl -d Ubuntu -- bash /mnt/c/...script.sh`.
- `/tmp` in WSL is tmpfs: don't benchmark disk there.

## Code conventions

- Rust 2024, `rustfmt.toml` (120 cols). Errors: `anyhow` with `.context(...)` that says what was being done. Comments explain *why*; doc comments on public items.
- User-facing messages are plain and actionable ("message has no PID segment; add it first with msg.add("PID")").
- Config: `serde(deny_unknown_fields)`; validate in `InterfaceConfig::validate`; new options get defaults that are safe.
- SDK (TypeScript): HL7 numbering is 1-based; missing reads as `""`; never throw on reads. Keep `sdk/src/typegen.ts` declarations in sync with the SDK's public API (that's what VS Code shows).

## Gotchas already solved (don't rediscover)

- **deno_core 0.412**: scopes via `deno_core::scope!(scope, &mut runtime)`; try/catch via `v8::tc_scope!(let scope, scope)`. `deno_error` must be pinned `=0.7.1`. Ops: `#[op2]` + `#[string]` for `Option<String>` returns, `#[serde]` for JSON. Extensions via `deno_core::extension!` with `esm = [dir "src", "file.js"]`.
- `call_with_args`/`resolve` return futures that **don't borrow the runtime**; drive the event loop separately (`poll_event_loop`). That's how concurrent async transforms work.
- Module specifiers must be canonical paths (`file_url` canonicalizes); `..` in paths broke module identity once.
- TypeScript transpile removes unused imports: tests that check import behavior must *use* the import.
- **sqlx 0.9**: dynamic SQL needs `sqlx::AssertSqlSafe(...)`; migrations must never be edited after they're applied (add a new one).
- `serde_yaml_ng`: enums in YAML maps need `with = "serde_yaml_ng::with::singleton_map"` (or `_recursive`).
- `${...}` in YAML flow style breaks parsing: always load project YAML through `ductus_core::env::parse_yaml` (protects references first).
- chacha20poly1305 0.11 changed its RNG API: we use `getrandom` directly.
- quick-xml's API churns: XML key extraction uses our own small reader (`runtime/src/keys.rs`), deliberately matching the SDK's `parseXml` rules.
- `cargo test` doesn't rebuild the `ductus` binary: `check.sh` builds it explicitly (a stale binary once hid a migration bug).
- TypeScript 6 is vendored (`vendor/typescript`) for type checking because TS 7 is a native Go binary that can't run in V8.

## Where things are

```
crates/ductus-core     config, formats, environments, secrets, resources
crates/ductus-hl7      HL7 field lookup + ACKs (ingest)
crates/ductus-mllp     MLLP framing/listener/client
crates/ductus-store    Postgres store + migrations (crates/ductus-store/migrations)
crates/ductus-script   embedded V8, module loader, ops, type checker (+ tests/script.rs)
crates/ductus-runtime  engine: lib.rs (startup), ingest.rs, http.rs, worker.rs, destination.rs, keys.rs, services.rs, limiter.rs
crates/ductus-cli      the `ductus` command (project.rs: test/validate/types/secret; tools.rs: gen/sink/status)
sdk/src                TypeScript SDK (embedded via include_str! in ductus-script/src/loader.rs: add new files there)
sdk/tests              SDK tests (TypeScript)
scripts/               demos (end-to-end, PASS/FAIL) and scripts/dev (check.sh, demos.sh)
docs/                  architecture.md, shared-pool.md
examples/adt-relay     example project
vendor/typescript      TypeScript 6.0.3 compiler for `ductus validate`
```
