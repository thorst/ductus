//! `ductus upgrade --check`: run with the new version's binary before installing it, reading only.
//! What its start would do to the database (migrations, and how big the tables they work through
//! are now), which engines run on that database and on which versions, whether the project still
//! validates and passes its tests with the new SDK and type checker, and what the new version
//! deprecates. Exits 1 on anything that would refuse a start or fail the project.

use anyhow::Result;
use ductus_store::Store;
use ductus_store::migrations::{self, Plan};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

const VERSION: &str = env!("CARGO_PKG_VERSION");

pub struct Options {
    pub root: PathBuf,
    pub env: String,
    pub database_url: String,
    pub json: bool,
    /// `--replay 1d`: replay that much of the latest traffic too (`ductus replay`).
    pub replay: Option<String>,
}

#[derive(Clone, Copy, PartialEq)]
enum Level {
    Ok,
    Warning,
    Problem,
    Note,
}

impl Level {
    fn mark(self) -> &'static str {
        match self {
            Level::Ok => "✓",
            Level::Warning => "⚠",
            Level::Problem => "✗",
            Level::Note => "note:",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Level::Ok => "ok",
            Level::Warning => "warning",
            Level::Problem => "problem",
            Level::Note => "note",
        }
    }
}

/// The checks as they're made: printed at once as text, or kept for one JSON document.
struct Report {
    json: bool,
    section: &'static str,
    checks: Vec<Value>,
    details: serde_json::Map<String, Value>,
    problems: usize,
}

impl Report {
    fn section(&mut self, name: &'static str, title: &str) {
        self.section = name;
        if !self.json {
            println!("\n{title}");
        }
    }

    /// One check, with lines under it (a table's size, a failing test's output).
    fn add(&mut self, level: Level, text: impl Into<String>, more: Vec<String>) {
        let text = text.into();
        if level == Level::Problem {
            self.problems += 1;
        }
        if self.json {
            self.checks.push(json!({ "section": self.section, "level": level.name(), "text": text, "more": more }));
        } else {
            println!("{} {text}", level.mark());
            for line in more {
                println!("    {line}");
            }
        }
    }

    fn detail(&mut self, key: &str, value: Value) {
        self.details.insert(key.to_string(), value);
    }
}

pub async fn check(options: Options) -> Result<bool> {
    let started = Instant::now();
    let mut report =
        Report { json: options.json, section: "", checks: Vec::new(), details: serde_json::Map::new(), problems: 0 };
    if !report.json {
        println!("ductus upgrade --check: what version {VERSION} would do here (nothing is changed)");
    }

    report.section("database", "The database");
    match Store::connect_read_only(&options.database_url).await {
        Err(e) => report.add(Level::Problem, format!("database: {e:#}"), Vec::new()),
        Ok(store) => {
            database(&store, &mut report).await;
            report.section("engines", "Engines on this database");
            engines(&store, &mut report).await;
        }
    }

    report.section("project", &format!("The project ({}, environment {})", options.root.display(), options.env));
    project(&options, &mut report);

    report.section("versions", "Between versions");
    report.add(Level::Ok, "required stops: none (versions before 1.0 upgrade to any later one)", Vec::new());
    match ductus_core::deprecated::find(ductus_core::deprecated::DEPRECATED, &options.root) {
        Err(e) => {
            report.add(Level::Warning, format!("deprecated settings: couldn't read the project: {e:#}"), Vec::new())
        }
        Ok(found) if found.is_empty() => report.add(Level::Ok, "deprecated settings: none used", Vec::new()),
        Ok(found) => {
            for f in &found {
                report.add(
                    Level::Warning,
                    format!(
                        "{} `{}`: deprecated since {}, removed at the next major version: {}",
                        f.file.display(),
                        f.at,
                        f.since,
                        f.instead
                    ),
                    Vec::new(),
                );
            }
            let found: Vec<Value> = found
                .iter()
                .map(|f| json!({ "file": f.file, "at": f.at, "since": f.since, "instead": f.instead }))
                .collect();
            report.detail("deprecated", Value::Array(found));
        }
    }

    if let Some(since) = &options.replay {
        report.section("replay", &format!("The last {since} replayed through {VERSION} (nothing is sent)"));
        replay(&options, since, &mut report);
    }

    let took = started.elapsed().as_secs_f64();
    let ok = report.problems == 0;
    if report.json {
        let mut document = json!({
            "version": VERSION,
            "ok": ok,
            "problems": report.problems,
            "seconds": (took * 100.0).round() / 100.0,
            "checks": report.checks,
        });
        document.as_object_mut().unwrap().extend(report.details);
        println!("{}", serde_json::to_string_pretty(&document)?);
    } else if ok {
        println!("\nready to upgrade to {VERSION} (checked in {took:.1} s)");
    } else {
        println!("\n{} problem(s): fix them before upgrading (checked in {took:.1} s)", report.problems);
    }
    Ok(ok)
}

/// The migrations a start would apply, against what the database has.
async fn database(store: &Store, report: &mut Report) {
    let embedded = match migrations::embedded() {
        Ok(m) => m,
        Err(e) => return report.add(Level::Problem, format!("this binary's migrations: {e:#}"), Vec::new()),
    };
    let applied = match store.applied_migrations().await {
        Ok(a) => a,
        Err(e) => return report.add(Level::Problem, format!("database: {e:#}"), Vec::new()),
    };
    let plan = migrations::plan(embedded, applied);
    let mut pending = Vec::new();
    if plan.new_database {
        report.add(
            Level::Ok,
            format!("a new database: the first start creates every table ({} migrations)", plan.pending.len()),
            Vec::new(),
        );
    } else if plan.pending.is_empty() {
        report.add(Level::Ok, "migrations: nothing to apply", Vec::new());
    } else {
        report.add(
            Level::Ok,
            format!(
                "migrations: the first start applies {} (they only add: the version running now keeps working on them)",
                plan.pending.len()
            ),
            Vec::new(),
        );
    }
    for m in &plan.pending {
        // The tables it works through, with their size now: what decides whether it takes
        // seconds or hours (and holds up storing meanwhile).
        let mut heavy = Vec::new();
        let mut lines = Vec::new();
        for h in &m.heavy {
            let bytes = store.table_bytes(&h.table).await.ok().flatten();
            let size = bytes.map(|b| format!(" ({})", crate::admin::size(b))).unwrap_or_default();
            lines.push(format!("{}{size}: {}", h.table, h.how));
            heavy.push(json!({ "table": h.table, "how": h.how, "bytes": bytes }));
        }
        if !plan.new_database {
            let level = if lines.is_empty() { Level::Ok } else { Level::Warning };
            report.add(level, format!("{:04} {}", m.version, m.what), lines);
        }
        pending.push(json!({ "version": m.version, "description": m.description, "what": m.what, "heavy": heavy }));
    }
    if !plan.newer.is_empty() {
        report.add(
            Level::Ok,
            format!(
                "applied by a newer version than this one: {} (fine: migrations only add, so this version runs on them)",
                versions(&plan.newer)
            ),
            Vec::new(),
        );
    }
    for a in &plan.failed {
        report.add(
            Level::Problem,
            format!(
                "migration {:04} ({}) failed part-way at an earlier start: every start refuses until it's repaired (ask for help before touching `_sqlx_migrations`)",
                a.version, a.description
            ),
            Vec::new(),
        );
    }
    for a in &plan.edited {
        report.add(
            Level::Problem,
            format!(
                "migration {:04} ({}) was applied from a different file than this version carries: a start refuses. Was this database migrated by a development build?",
                a.version, a.description
            ),
            Vec::new(),
        );
    }
    report.detail("migrations", migrations_json(&plan, pending));
}

fn versions(applied: &[migrations::Applied]) -> String {
    applied.iter().map(|a| format!("{:04}", a.version)).collect::<Vec<_>>().join(", ")
}

fn migrations_json(plan: &Plan, pending: Vec<Value>) -> Value {
    let list = |applied: &[migrations::Applied]| -> Vec<Value> {
        applied.iter().map(|a| json!({ "version": a.version, "description": a.description })).collect()
    };
    json!({
        "new_database": plan.new_database,
        "pending": pending,
        "newer": list(&plan.newer),
        "edited": list(&plan.edited),
        "failed": list(&plan.failed),
    })
}

/// Who runs on this database now (0.6.2 recorded them), and on which version.
async fn engines(store: &Store, report: &mut Report) {
    if !store.has_table("engines").await.unwrap_or(false) {
        report.add(
            Level::Note,
            "engines before 0.6.2 aren't recorded in the database: look on each server for running ones",
            Vec::new(),
        );
        return;
    }
    let live = match store.live_engines().await {
        Ok(l) => l,
        Err(e) => return report.add(Level::Warning, format!("engines: {e:#}"), Vec::new()),
    };
    if live.is_empty() {
        report.add(Level::Ok, "none running now", Vec::new());
    }
    for e in &live {
        report.add(
            Level::Ok,
            format!(
                "{} on {} (pid {}): version {}, environment {}, since {}, runs {} interface(s)",
                e.instance,
                e.host,
                e.pid,
                e.version,
                e.environment,
                e.started_at.format("%Y-%m-%d %H:%M UTC"),
                e.interfaces.len()
            ),
            Vec::new(),
        );
    }
    let older: std::collections::BTreeSet<&str> =
        live.iter().map(|e| e.version.as_str()).filter(|v| *v != VERSION).collect();
    if live.len() > 1 && !older.is_empty() {
        report.add(
            Level::Note,
            format!(
                "an engine upgraded alone leaves the others on {}: they keep working on the new tables (migrations only add); upgrade them too",
                older.into_iter().collect::<Vec<_>>().join(", ")
            ),
            Vec::new(),
        );
    }
    let engines: Vec<Value> = live
        .iter()
        .map(|e| {
            json!({
                "instance": e.instance, "host": e.host, "pid": e.pid, "version": e.version,
                "environment": e.environment, "started_at": e.started_at.to_rfc3339(), "interfaces": e.interfaces,
            })
        })
        .collect();
    report.detail("engines", Value::Array(engines));
}

/// `ductus validate` (against the database) and `ductus test`, as this binary: the new SDK, type
/// checker and config rules. Each runs as its own process, as a person would run it, so its
/// output can be kept whole for the report.
fn project(options: &Options, report: &mut Report) {
    let mut results = serde_json::Map::new();
    for (name, args) in [("validate", ["validate", "--root"]), ("test", ["test", "--root"])] {
        let run = run_self(&args, options);
        let (ok, output) = match run {
            Ok(r) => r,
            Err(e) => (false, format!("couldn't run `ductus {name}`: {e:#}")),
        };
        let last = output.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
        if ok {
            // Its warnings and notes still matter for the upgrade (a database that isn't
            // verified, retention changes the next start begins).
            let worth: Vec<String> =
                output.lines().filter(|l| l.starts_with('⚠') || l.starts_with("note:")).map(str::to_string).collect();
            report.add(Level::Ok, format!("{name}: {last}"), worth);
        } else {
            report.add(Level::Problem, format!("{name}: {last}"), output.lines().map(str::to_string).collect());
        }
        results.insert(name.to_string(), json!({ "ok": ok, "output": output }));
    }
    report.detail("project", Value::Object(results));
}

/// `ductus replay --since <since> --json` as this binary: against the installed version where there
/// is one, else the record and kept outputs. Its differences are problems; a full report is one
/// command away.
fn replay(options: &Options, since: &str, report: &mut Report) {
    let mut command = Command::new(match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => return report.add(Level::Problem, format!("replay: {e}"), Vec::new()),
    });
    command.args(["replay", "--json", "--since", since, "--root"]).arg(&options.root).args(["--env", &options.env]);
    command.env("DUCTUS_DATABASE_URL", &options.database_url);
    let out = match command.output() {
        Ok(out) => out,
        Err(e) => return report.add(Level::Problem, format!("couldn't run `ductus replay`: {e}"), Vec::new()),
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let Ok(document) = serde_json::from_str::<Value>(&stdout) else {
        let said = String::from_utf8_lossy(&out.stderr);
        let last = said.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
        return report.add(Level::Problem, format!("replay: {last}"), said.lines().map(str::to_string).collect());
    };
    let count = |key: &str| document[key].as_u64().unwrap_or(0);
    let against = document["other"]["label"].as_str().map_or("the record and kept outputs".to_string(), str::to_string);
    let notes: Vec<String> = document["notes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|n| n.as_str())
        .map(|n| format!("note: {n}"))
        .collect();
    let mut differences = Vec::new();
    for interface in document["interfaces"].as_array().into_iter().flatten() {
        if let Some(why) = interface["skipped"].as_str().filter(|s| s.starts_with("its code")) {
            differences.push(format!("{}: {why}", interface["name"].as_str().unwrap_or("")));
        }
        for destination in interface["destinations"].as_array().into_iter().flatten() {
            for group in destination["differences"].as_array().into_iter().flatten().filter(|g| g["drift"] != true) {
                differences.push(format!(
                    "{} → {}: {} differs in {} message(s) (vs {})",
                    interface["name"].as_str().unwrap_or(""),
                    destination["name"].as_str().unwrap_or(""),
                    group["what"].as_str().unwrap_or(""),
                    group["count"],
                    group["against"].as_str().unwrap_or("")
                ));
            }
        }
    }
    let same = match count("same_decision") {
        0 => String::new(),
        n => format!(", {n} same decision (outputs not compared)"),
    };
    let summary = format!(
        "replay against {against}: {} message(s), {} comparison(s), {} identical{same}, {} different",
        count("messages"),
        count("compared"),
        count("identical"),
        count("different")
    );
    if document["ok"] == true {
        report.add(Level::Ok, summary, notes);
    } else {
        differences.push(format!("the page with every difference: ductus replay --since {since} --report replay.html"));
        report.add(Level::Problem, summary, differences.into_iter().chain(notes).collect());
    }
    // Decisions alone can't show a changed field: said as a warning, with the fix, not as a pass.
    if let Some(warning) = document["outputs_not_compared"].as_str() {
        report.add(Level::Warning, format!("replay: {warning}"), Vec::new());
    }
    if count("not_comparable") + count("drift") > 0 {
        report.add(
            Level::Warning,
            format!(
                "replay: {} not comparable (writes it doesn't make, lookups that failed), {} drift (the installed version does the same)",
                count("not_comparable"),
                count("drift")
            ),
            Vec::new(),
        );
    }
    report.detail("replay", document);
}

/// This binary with `args`, the project and environment: (exit 0, stdout and stderr).
fn run_self(args: &[&str], options: &Options) -> Result<(bool, String)> {
    let mut command = Command::new(std::env::current_exe()?);
    command.args(args).arg(&options.root).args(["--env", &options.env, "--read-only"]);
    // The URL goes in the environment: on a command line, other users could read its password.
    command.env("DUCTUS_DATABASE_URL", &options.database_url);
    if std::env::var_os("RUST_LOG").is_none() {
        command.env("RUST_LOG", "warn,rustls_platform_verifier=off,tiberius=error");
    }
    let out = command.output()?;
    Ok((
        out.status.success(),
        format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)),
    ))
}
