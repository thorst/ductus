# Ductus: engineering guide

Code-first healthcare integration engine: Rust runtime, embedded V8 for TypeScript interface code, PostgreSQL as store and queue. Open core (Apache-2.0). This repo is the open core; commercial features and planning live in the private `ductus-enterprise` repo (`planning/STATUS.md` says where work stands).

**Keep this file short.** It's read at the start of every session. A new lesson goes in its area's file under `dev-guide/` (table below), not here; only a rule that applies to almost every change belongs in this file.

**Read first:** `../ductus-enterprise/planning/STATUS.md`, then the `dev-guide/` file for each area the step touches, and the section of `docs/architecture.md` it touches (find it with `grep -n '^#' docs/architecture.md`; don't read the whole file).

## Non-negotiables

- The invariants in `docs/architecture.md` (persist before ACK, at-least-once, one owner per lane and one engine per interface, expand-only migrations, no credentials in code). If a change seems to need breaking one, stop and discuss.
- **Every change ships with tests.** Unit tests next to the code; DB tests in `crates/ductus-store/tests`; script/SDK tests in `crates/ductus-script/tests` and `sdk/tests/*.test.ts` (run inside the real engine); end-to-end in `scripts/demo-*.sh` (each ends with PASS/FAIL and runs in CI); the web UI's in `ui/src/**/*.test.ts` (Vitest) and `ui/e2e` (Playwright in a real browser against a real engine, run by `scripts/demo-ui.sh`).
- **Before every commit:** `scripts/dev/check.sh` must end with `ALL CHECKS PASSED` (the web UI's format/types/lint/tests/build, fmt, build incl. the binary, the API document the UI is typed from, clippy `-D warnings`, all tests). For runtime or UI changes also run `scripts/dev/demos.sh`.
- Commits: `git commit -s` (DCO), message explains what and why, ends with the Co-Authored-By trailer. Push to `main` (solo project, CI on every push); then check the run went green.
- **Nuances get documented, not hidden.** Any behaviour that could surprise an interface developer (limits, failure semantics, things that run twice) goes into its section's page in `docs/nuances/` in the same commit, linked from the guide page it affects (`every_nuance_is_linked_from_a_guide`), with what/why/what to do and a level (1 foundations … 5 expert; these become the certification tracks).
- **Docs change in the same commit as the behaviour.** A change to a setting, command, API operation or anything someone could notice updates its doc comment (the reference), the guide page that explains it and, when it could surprise, a nuance. A page left saying the old thing is a bug like a failing test. The bar and the rules are `docs/STYLE.md` (what, why, when not, default, example; commands shown were run, outputs are what they printed). Details: `dev-guide/api-ui-docs-config.md`.

## Environment

- Windows host; **build and run everything in WSL2 Ubuntu** (`wsl -d Ubuntu`). Repo at `/mnt/d/Projects/integration_engine/ductus`. `CARGO_TARGET_DIR=$HOME/target` (building on `/mnt/d` is slow); binary at `~/target/release/ductus`.
- Postgres 18 in WSL. The binary has **no default database**: scripts get theirs from `scripts/dev/db.sh` (a personal login in `scripts/dev/db.local.sh`, not in Git); at a prompt, export `DUCTUS_DATABASE_URL` yourself. `ductus dev` needs none (its own Postgres).
- From PowerShell, multi-line or `$`-heavy shell commands get mangled: write a script file and run `wsl -d Ubuntu -- bash /mnt/c/...script.sh`. `/tmp` in WSL is tmpfs: don't benchmark disk there.
- WSL's disk image is on D: and only grows; the VM gets killed under memory pressure (`wsl --shutdown` and `CARGO_BUILD_JOBS=4` before a long check.sh). Details, compaction steps and every other machine/CI gotcha: `dev-guide/scripts-ci-wsl.md`.

## Code conventions

- Rust 2024, `rustfmt.toml` (120 cols). Errors: `anyhow` with `.context(...)` that says what was being done. Comments explain *why*; doc comments on public items.
- User-facing messages are plain and actionable ("message has no PID segment; add it first with msg.add("PID")").
- Config: `serde(deny_unknown_fields)`; validate in `InterfaceConfig::validate`; new options get defaults that are safe.
- SDK (TypeScript): HL7 numbering is 1-based; missing reads as `""`; never throw on reads. Keep `sdk/src/typegen.ts` declarations in sync with the SDK's public API (that's what VS Code shows).

## Rules that catch most changes

- **A new migration**: starts with its `-- Upgrade:` / `-- Heavy:` header, is never edited once applied (not even a comment), needs a rebuild to be picked up, and its tables/columns go in `docs/schema.md`. New database work goes into the stress test. → `dev-guide/database.md`
- **A new API operation**: a row in `ACCESS` (`api/mod.rs`), its own `operation_id` if its handler's name exists elsewhere, `#[into_params(parameter_in = Query)]` on query structs; check.sh regenerates the UI's types and asks for a second run. → `dev-guide/api-ui-docs-config.md`
- **A new setting, CLI option, metric, audit action, Event Log ID or `validate` finding**: each has a registry and a test that fails without it (doc comment to the bar, `api::METRICS`, `AUDIT_ACTIONS` + schema.md + the UI's `ACTIONS`, `service/messages.rs`, a `DUC` code in `findings::CODES`). → the area's file
- **Any Cargo.lock or `ui/package-lock.json` change**: run `bash scripts/dev/dependencies.sh` (`the_libraries_page_is_current`).
- **Anything new in the engine's start**: if the start doesn't need its result, it runs beside the start, never in its path (Todd's rule); anything that changes an interface's rows goes after the claim. → `dev-guide/lifecycle-and-packaging.md`, `dev-guide/database.md`
- **Tests and demos** delete the accounts they make (`test-user-*`), use interface names of their own, and never run two engines with the same names at once. → `dev-guide/people-audit-alerts.md`, `dev-guide/database.md`
- The package of `crates/ductus-cli` is named `ductus`: `cargo test -p ductus --test cli`.

## Area notes (`dev-guide/`)

Read the file for each area a step touches before changing it; each note there already cost someone a debugging session.

| Touching | Read |
|---|---|
| store, migrations, SQL, partitions, retention, locking, engines sharing a database, start-up statements | `dev-guide/database.md` |
| V8/deno_core, module loader, ops, runaway guard, type checker, `console`, inspector/debugging, `validate`/`test` | `dev-guide/script-runtime.md` |
| TLS (listeners, outbound, Postgres, SQL Server), `allow:`, certificates, LDAP/directory, SFTP | `dev-guide/connections-and-tls.md` |
| users and test accounts, audit log, alerts, owners | `dev-guide/people-audit-alerts.md` |
| `ductus dev`, Windows (service, MSI, native build), systemd, handover, replay, code versions, history, releases, the release job | `dev-guide/lifecycle-and-packaging.md` |
| bash scripts and demos, CI, WSL, comparing against an older build | `dev-guide/scripts-ci-wsl.md` |
| REST API/OpenAPI, web UI and Playwright, docs reference and site, YAML/config parsing | `dev-guide/api-ui-docs-config.md` |
| which file does what | `dev-guide/where-things-are.md` |
