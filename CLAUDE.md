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
- Calling `wsl` from the Git Bash tool mangles `/mnt/...` paths: prefix `MSYS_NO_PATHCONV=1` (or use PowerShell). Don't put the text "ductus run" in a wrapper command line: `demos.sh` does `pkill -f 'ductus (run|...)'` and kills the wrapper.
- Demo waits must re-evaluate their condition: `wait_for 15 '[ "$(count $OUT)" -ge 3 ]'` (quoted, `eval`ed). Passing `"$(...)"` unquoted freezes the value and the wait silently runs to its timeout.
- Logs are plain text when stdout isn't a terminal (`with_ansi`), so demos can grep fields like `started interfaces=`.
- SFTP tests use a throwaway user-level `sshd` (openssh-server installed in WSL; its system service is disabled) on 127.0.0.1:2222; see `scripts/demo-files.sh`. Clean up child processes with a `trap` whose commands each end in `|| true` (`set -e` applies inside traps).
- CI "could not find native static library `rusty_v8`" on Build (Lint passes) = a stale rust-cache restored as a partial match. Fix: `gh cache delete --all`, then `gh run rerun <id>`. Always check the run after pushing.
- **WSL stops the distro** (Docker and containers with it, exit 255, nothing in the logs) once no `wsl` command has run for a while. Not a SQL Server crash. The local SQL Server container (`scripts/dev/sqlserver.sh`, Docker Engine installed in WSL) has `--restart unless-stopped`; check.sh starts it and waits for a real login before the tests. SQL Server tests use `DUCTUS_TEST_SQLSERVER=user:password@host:port`.
- tiberius 0.13 (bb8-tiberius is stuck on 0.12, so `mssql.rs` has its own pool). SQL Server 2025 image not tried since the WSL finding; tests and CI use 2022.
- **Runaway guard (`ductus-script/src/guard.rs`)**: terminate only from inside an `IsolateHandle::request_interrupt` callback (runs on the isolate's thread; check the call generation first), never `terminate_execution` from the watchdog thread (a finished call's successor would be killed). Reading the stack there needs a context: the main context is kept in an isolate slot (`MainContext`) and entered with `v8::scope_with_context!`. After termination, `cancel_terminate_execution` (deno_core re-arms termination in `exception_to_err`). A termination during the event loop **drops all queued microtasks**, so every in-flight async transform on that isolate hangs: fail them all (worker's `interrupted` watch). `Isolate::settle` stringifies errors: use `drive` + `output` where a typed error (`Runaway`) must survive.
- **Sockets belong to the runtime that opened them.** The sqlx pool is shared with intake: worker threads must run store calls on the main runtime (`Worker::db`), or a busy V8 thread delays other interfaces' ACKs.
- tracing's plain log format puts the message before the fields: grep `interface stopped.*interface=x`, not the other way round.
- russh 0.64: `client::Handler::check_server_key(&PublicKeyOrCertificate)`; fingerprints via `key.fingerprint(HashAlg::Sha256)` print like `ssh-keygen -lf`. SFTP `rename` won't overwrite: remove the target first.

## Where things are

```
crates/ductus-core     config, formats, environments, secrets, resources, schedule timing, ports, settings (ductus.yaml)
crates/ductus-hl7      HL7 field lookup + ACKs (ingest)
crates/ductus-mllp     MLLP framing/listener/client
crates/ductus-store    Postgres store + migrations (crates/ductus-store/migrations)
crates/ductus-script   embedded V8, module loader, ops, runaway guard (guard.rs), type checker (+ tests/script.rs)
crates/ductus-runtime  engine: lib.rs (startup), ingest.rs, http.rs, files.rs + split.rs (file/SFTP sources), sftp.rs,
                       schedule.rs, worker.rs, health.rs (stopped interfaces), destination.rs, keys.rs,
                       services.rs + mssql.rs (DB calls), limiter.rs
crates/ductus-cli      the `ductus` command (project.rs: test/validate/ports/types/secret; new.rs: `new interface`;
                       tools.rs: gen/sink/status;
                       tests/cli.rs runs the binary on temp projects)
sdk/src                TypeScript SDK (embedded via include_str! in ductus-script/src/loader.rs: add new files there)
sdk/tests              SDK tests (TypeScript)
scripts/               demos (end-to-end, PASS/FAIL) and scripts/dev (check.sh, demos.sh)
docs/                  architecture.md, shared-pool.md
examples/adt-relay     example project
vendor/typescript      TypeScript 6.0.3 compiler for `ductus validate`
```
