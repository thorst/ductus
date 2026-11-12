# Where things are

A map of the repo by crate and file. `Grep` finds a symbol faster; this says what each file is *for*.

```
crates/ductus-core     config, yaml (loading project files with places: `Located`, `locate`, `Problem`), pgtls (libpq's sslmode, a URL's TLS parameters, `Security` in words), allow (`allow:` lists: parse, match, cover `about.from.hosts`), tls (`tls:` on listeners, client subject patterns, `ConnectTls` on outbound connections, checks), alerts (`alerts:` settings, rules, owners as recipients, the alert state machine, notification text), formats, environments, secrets, password (argon2id, policy), resources (databases, sftp, apis, ldap; `owner`), ldap (`auth.ldap`, filters, groups → role), retention, docs (company-defined
                       documentation fields: docs-schema.yaml, `custom:` values, checks, the snapshot), monitor
                       (`monitor:` expectations and their time rules), hotspots (backlog by lane and worker), about (systems,
                       host/subnet matching), impact, owners (report + in-place reassign), schedule timing, ports,
                       settings (ductus.yaml), deprecated (settings going away, found in a project)
crates/ductus-hl7      HL7 field lookup + ACKs (ingest)
crates/ductus-mllp     MLLP framing/listener/client
crates/ductus-store    Postgres store + migrations (crates/ductus-store/migrations; migrations.rs: what each does, applied vs
                       carried, table sizes for `upgrade --check`); tls.rs (pg_stat_ssl, refusals in words, `connect_url`); testing.rs (`TestUsers`, `PgTls`); retention.rs (tiers, changes, drops, held area); activity.rs (last activity, pauses);
                       actions.rs (resend, edited copies, message_actions, the quarantine list); users.rs (accounts,
                       sign-in bookkeeping, sessions, appearance); api.rs (tokens, sessions, `Principal`, the message browser);
                       audit.rs (the audit log: `Audit`, `By`, reads, trimming); alerts.rs (alert state, events, the outbox);
                       certificates.rs (certificate_status, doctor_runs); directory.rs (directory accounts: save, refuse, who to check);
                       engines.rs (the engine lock, interface claims, `interface_applied`); lanes.rs (lane generations: read,
                       written, retired, taken back; the key hash a resend uses); releases.rs (releases stored by folder, their checks)
crates/ductus-script   embedded V8, module loader, ops, runaway guard (guard.rs), type checker, errors.rs (an error from interface code as people read it) (+ tests/script.rs)
crates/ductus-runtime  engine: lib.rs (startup, its phases timed), code.rs (code versions), git.rs (the project's commit
                       and uncommitted files, gix; an interface's commits; HEAD's ancestry), deployment.rs (who and why; recording each start's deployment; the folder-mode warning), release.rs (a release's files from git, its id, the manifest, the archive), history.rs (one timeline per interface: git, deployments, audit, documentation), ownership.rs (one engine per interface: lock, claims, the
                       watch, refusal texts), lanes.rs (lane generations: the start's decision, which lanes a worker reads
                       and which are held, drained old lanes, retiring, a failed handover's take-back), control.rs (engine state for the API: health, queues, drain, pauses),
                       stats.rs (counts and timings in memory: histograms, Prometheus text format),
                       activity.rs (last activity and pauses, kept in the database across restarts),
                       gate.rs (drain gate), retention.rs (applies retention, runs changes, drops partitions),
                       alerts.rs (judging every 15 s, the outbox sender, SMTP and webhooks, the heartbeat),
                       certificates.rs (the engine's own connection checks on `doctor.schedule`, what has a certificate),
                       ldap.rs (directory sign-in: connect, find, bind, groups; the refresh task; tests/ldap.rs against slapd),
                       api/ (REST API: status, messages, actions (resend, edit, quarantine), alerts, reports (impact, owners + reassign
                       preview, doctor), retention, releases (stored, listed, checked by running `release check`), tokens, users, me (own account), metrics, hotspots,
                       session (browser sign-in), ui (serves the embedded web UI; build.rs embeds ui/dist)),
                       project.rs (what code uses, via the type checker), ingest.rs (also the receiving
                       side of routes: accept_routed), http.rs, files.rs + split.rs (file/SFTP sources), sftp.rs,
                       schedule.rs, worker.rs, health.rs (stopped interfaces), destination.rs, keys.rs,
                       allow.rs (who may connect: admissions per listener, the one task keeping host names
                       resolved, TCP keepalive), systemd.rs (every listener's socket: kept in systemd's
                       store across restarts and crashes, passed on by a handover; READY/STOPPING/MAINPID),
                       handover.rs (`systemctl reload`: the control socket, the successor, the takeover),
                       replay.rs + replay/report.rs (`ductus replay`: the code over a period's originals, sending
                       nothing; the comparison with the record, kept outputs and another version; the report),
                       tls.rs (our listeners' TLS: certificates loaded and reloaded, client certificates,
                       the HTTP `Listener`, the MLLP handshake, refusals; `self_signed`; outbound: `Trust`,
                       cached client settings, the MLLP TLS client, reqwest clients, refusals in words),
                       services.rs + mssql.rs (DB calls), limiter.rs, doctor.rs (connection checks: also SMTP STARTTLS,
                       the certificate seen, `Mode::Certificates`, `judge_chain`), cert_show.rs (`ductus cert show`'s judgement);
                       examples/cert-server.rs (TLS servers for demos), examples/tls-bench.rs (bench-tls.sh)
crates/ductus-cli      the `ductus` command (init.rs: `init`, a new project; dev.rs: `dev`, the engine with an embedded Postgres; project.rs: test/validate/ports/impact/types/secret; spread.rs: interfaces spread over threads, results in order; findings.rs: validate's findings, text or JSON, and their codes; new.rs: `new interface`;
                       admin.rs: `user`, `token`, `drain`, `resume`, `pause`, `retention`, `hotspots`;
                       doctor.rs: runs `doctor` (what to check is runtime's `doctor::plan`); owners.rs: `owners` report and reassign;
                       tools.rs: gen/sink (both `--tls`)/status; sinks.rs: `sink --smtp`/`--http`; cert.rs: `cert request|show|self-signed`;
                       upgrade.rs: `upgrade --check` (runs `validate`/`test --read-only` as child processes for the report);
                       replay.rs: `replay` (the other version as a child, `--emit`; `--baseline` from a Git revision);
                       history.rs: `history` (the project's files now + the database, read-only);
                       release.rs: `release` (refusals, versions from an unpacked copy, tests as a child) and `release check` (the gate);
                       tests/cli.rs runs the binary on temp projects)
ui/                    web UI (React + Vite; README.md): src/api (typed client, queries, live updates), src/pages,
                       src/components, src/lib (format, hl7, health, theme, openapi: the API reference's reading of the
                       document); e2e/ (Playwright, run by scripts/demo-ui.sh)
sdk/src                TypeScript SDK (embedded via include_str! in ductus-script/src/loader.rs: add new files there)
sdk/tests              SDK tests (TypeScript)
scripts/               demos (end-to-end, PASS/FAIL) and scripts/dev (check.sh, demos.sh)
Dockerfile, packaging/ the image, the .deb/.rpm and the portable .tar.gz from one engine build (packaging/README.md: layout,
                       upgrades; packaging/archive/README.md goes in both archives; windows/archive.ps1 makes the zip);
                       demo-docker.sh, demo-packages.sh (systemd in containers) run in CI's `packaging` job
docs/                  architecture.md, nuances/ (one page per subject, index.md by level), schema.md (every table/column; a store test fails if one
                       isn't described), shared-pool.md
examples/adt-relay     example project
examples/guides        the docs' "Write interfaces" project (routes, mappings, FHIR, X12, CSV, XML, lookups, tests)
vendor/typescript      TypeScript 6.0.3 compiler for `ductus validate`
```
