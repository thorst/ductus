//! `ductus release` and `ductus release check` (0.7.4.3.2): one exact, checksummed artifact of the
//! project (`ductus_runtime::release`), and the gate it must pass for an environment before
//! anything deploys it.
//!
//! `ductus release` takes the project's files from git's HEAD (or, with `--uncommitted`, as they
//! are in the folder), refuses what a release must not carry (uncommitted files unless asked, a
//! private key, code imported from outside the project, a link), works out every interface's code
//! version and the project version from **an unpacked copy** (so the manifest describes exactly
//! what the release carries), runs the tests there, and writes the tar.gz.
//!
//! `ductus release check` unpacks a release into a temporary folder and runs, as child processes
//! of this binary, `validate --read-only` against the environment's database and the tests of the
//! interfaces the release changes there. Nothing is written: not the project, not the database;
//! the temporary folder is removed.

use crate::upgrade::{Level, Report};
use anyhow::{Context, Result, bail};
use ductus_core::Environment;
use ductus_runtime::code::{self, Version};
use ductus_runtime::release::{self, Bundle, Manifest};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

const VERSION: &str = env!("CARGO_PKG_VERSION");
/// How many files a refusal names before "and N more".
const NAMED: usize = 20;

pub struct Make {
    pub root: PathBuf,
    pub env: String,
    pub reason: Option<String>,
    pub uncommitted: bool,
    pub over: Option<String>,
    pub branch: Option<String>,
    pub no_tests: bool,
    pub out: Option<PathBuf>,
    pub json: bool,
}

/// `ductus release`.
pub async fn make(args: Make) -> Result<()> {
    let started = Instant::now();
    let root = std::fs::canonicalize(&args.root).with_context(|| format!("finding {}", args.root.display()))?;
    if !root.join("interfaces").is_dir() {
        bail!("{} isn't a project (no `interfaces/` folder): give the project with --root", args.root.display());
    }
    let over = args.over.as_deref().map(base_release).transpose()?;
    let mut picked = {
        let (root, uncommitted) = (root.clone(), args.uncommitted);
        tokio::task::spawn_blocking(move || release::pick(&root, uncommitted)).await??
    };
    if let Some(given) = args.branch.as_deref().map(str::trim) {
        match picked.git.as_mut() {
            None => {
                bail!("`--branch {given}`: the project isn't in git (or has no commit yet), so a release has no branch")
            }
            Some(g) => match g.branch.as_deref() {
                Some(on) if on != given => bail!(
                    "`--branch {given}`: HEAD is on branch `{on}`, and a release records the branch it was made on (`--branch` is for a detached HEAD)"
                ),
                _ if given.is_empty() => bail!("`--branch` needs a name"),
                _ => g.branch = Some(given.to_string()),
            },
        }
    }
    if !picked.refused.is_empty() {
        bail!("a release can't carry these:\n{}", list(&picked.refused));
    }
    if !args.uncommitted && !picked.uncommitted.is_empty() {
        let committed = if picked.git.is_some() { "HEAD's commit" } else { "a commit (this repository has none yet)" };
        bail!(
            "{} file(s) in the project aren't committed:\n{}\na release is {committed}: commit them (or add them to .gitignore), or make it with `--uncommitted` to carry them as they are (the release then says so)",
            picked.uncommitted.len(),
            list(&picked.uncommitted)
        );
    }
    let keys: Vec<String> = picked.files.iter().filter(|f| release::private_key(f)).map(|f| f.path.clone()).collect();
    if !keys.is_empty() {
        bail!(
            "the release would carry a private key in the clear (and anyone given the release could read it):\n{}\nkeep keys out of the project's git (`git rm --cached <file>`; `*.key` is in the .gitignore `ductus init` writes) and name them in resources.yaml by a path outside the project",
            list(&keys)
        );
    }

    // The manifest is made from what the release carries, unpacked: never from the folder.
    let unpacked = tempfile::Builder::new().prefix("ductus-release-").tempdir().context("making a temporary folder")?;
    release::unpack(&picked.files, unpacked.path())?;
    let copy = std::fs::canonicalize(unpacked.path())?;
    let versions = {
        let (copy, env, root) = (copy.clone(), args.env.clone(), root.clone());
        tokio::task::spawn_blocking(move || {
            versions(&copy, &env).map_err(|e| {
                anyhow::anyhow!(
                    "{}\nthe code is loaded from what the release carries: a file it imports must be committed, and inside the project",
                    inside(&format!("{e:#}"), &copy, &root)
                )
            })
        })
        .await??
    };
    let outside: Vec<String> = versions
        .iter()
        .flat_map(|v| {
            v.files.iter().filter(|f| !relative(&f.path)).map(move |f| format!("{} imports {}", v.interface, f.path))
        })
        .collect();
    if !outside.is_empty() {
        bail!(
            "code imports files from outside the project, which a release can't carry:\n{}\nmove them into the project (`lib/` is the place for shared code) and import them from there",
            list(&outside)
        );
    }

    let tests = if args.no_tests {
        release::Tests::default()
    } else {
        if !args.json {
            eprintln!("running the tests on what the release carries…");
        }
        let (passed, failed, ms, output) = run_tests(&copy, &args.env, &[])?;
        if failed > 0 {
            eprint!("{}", inside(&output, &copy, &root));
            bail!(
                "{failed} test(s) failed: no release made (fix them, or `--no-tests` to make one anyway: the release then says its tests weren't run)"
            );
        }
        release::Tests { ran: true, environment: Some(args.env.clone()), passed, failed, ms }
    };

    let id = code::hex(&release::id(&picked.files));
    let (project_version, project_files) = release::project_version(&picked.files);
    let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "project".into());
    let manifest = Manifest {
        format: release::FORMAT,
        id: id.clone(),
        project: name.clone(),
        made: release::Made {
            at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            by: ductus_runtime::deployment::account(),
            host: ductus_runtime::audit::host_name(),
            reason: args.reason.clone(),
            binary: VERSION.to_string(),
            platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        },
        git: picked.git.clone(),
        uncommitted: if args.uncommitted { picked.uncommitted.clone() } else { Vec::new() },
        over,
        project_version: release::ProjectVersion { version: code::hex(&project_version), files: project_files },
        interfaces: versions
            .iter()
            .map(|v| {
                let interface = release::Interface {
                    version: code::hex(&v.hash),
                    files: v.files.iter().map(|f| f.path.clone()).collect(),
                    parts: v.parts.clone(),
                };
                (v.interface.clone(), interface)
            })
            .collect(),
        tests,
        files: picked.files.len(),
        bytes: picked.files.iter().map(|f| f.bytes.len() as u64).sum(),
        binary: picked.files.iter().filter(|f| f.binary).map(|f| f.path.clone()).collect(),
        deployed: Vec::new(),
    };
    let file_name = format!("{name}-{}.tar.gz", release::short(&id));
    let path = match &args.out {
        Some(out) if out.is_dir() => out.join(&file_name),
        // `--out dist/`: a folder, made when it's missing.
        Some(out) if out.to_string_lossy().ends_with(['/', '\\']) => {
            std::fs::create_dir_all(out).with_context(|| format!("making {}", out.display()))?;
            out.join(&file_name)
        }
        Some(out) => out.clone(),
        None => {
            let dir = root.join(".ductus").join("releases");
            std::fs::create_dir_all(&dir).with_context(|| format!("making {}", dir.display()))?;
            dir.join(&file_name)
        }
    };
    let ancestry = picked.git.as_ref().map(|g| g.ancestry.clone()).unwrap_or_default();
    let size = release::write(&path, &manifest, &ancestry, &picked.files)?;
    let took = started.elapsed().as_secs_f64();

    if args.json {
        let mut document = serde_json::to_value(&manifest)?;
        document["file"] = shown(&path).into();
        document["size"] = size.into();
        document["seconds"] = ((took * 100.0).round() / 100.0).into();
        println!("{}", serde_json::to_string_pretty(&document)?);
        return Ok(());
    }
    println!("release {} of {name}", release::short(&id));
    println!(
        "  {} interface(s), {} file(s), {} unpacked, {} as a release",
        manifest.interfaces.len(),
        manifest.files,
        crate::admin::size(manifest.bytes as i64),
        crate::admin::size(size as i64)
    );
    match &manifest.git {
        Some(g) => {
            let on = match &g.branch {
                Some(b) => format!("on branch {b}"),
                None => "HEAD detached, no branch recorded (`--branch NAME` names it)".to_string(),
            };
            println!("  from commit {} {on} ({} commit(s) of history carried)", release::short(&g.commit), g.commits)
        }
        None => println!("  not in git: a deploy holds it against the release it names with --over"),
    }
    if !manifest.uncommitted.is_empty() {
        println!("  ⚠ carries {} uncommitted file(s), as they were in the folder:", manifest.uncommitted.len());
        println!("{}", list(&manifest.uncommitted));
    }
    if let Some(base) = &manifest.over {
        println!("  made over release {}", release::short(base));
    }
    match &manifest.tests {
        t if t.ran => {
            println!("  tests: {} passed (environment {}) in {:.1} s", t.passed, args.env, t.ms as f64 / 1000.0)
        }
        _ => println!("  ⚠ tests not run (--no-tests): `ductus release check` runs every interface's"),
    }
    if !manifest.binary.is_empty() {
        println!("  {} file(s) that aren't text, carried byte for byte", manifest.binary.len());
    }
    println!("  project version {}", release::short(&manifest.project_version.version));
    println!("wrote {} ({took:.1} s)", shown(&path));
    println!("check it against an environment: ductus release check {} --env prod", shown(&path));
    Ok(())
}

/// A path as a person would type it: on Windows without the `\\?\` prefix `canonicalize` adds
/// (found by hand on Windows: `ductus deploy` then echoed it back).
fn shown(path: &Path) -> String {
    let text = path.display().to_string();
    if let Some(share) = text.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{share}");
    }
    text.strip_prefix(r"\\?\").map(str::to_string).unwrap_or(text)
}

#[cfg(test)]
mod shown_tests {
    use super::*;

    #[test]
    fn windows_verbatim_paths_are_shown_as_typed() {
        assert_eq!(shown(Path::new(r"\\?\C:\work\a.tar.gz")), r"C:\work\a.tar.gz");
        assert_eq!(shown(Path::new(r"\\?\UNC\server\share\a.tar.gz")), r"\\server\share\a.tar.gz");
        assert_eq!(shown(Path::new("/home/ann/a.tar.gz")), "/home/ann/a.tar.gz");
    }
}

/// `--over`: a release id (64 hex characters) or a release file, whose id is read from it.
fn base_release(given: &str) -> Result<String> {
    if given.len() == 64 && given.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(given.to_ascii_lowercase());
    }
    let path = Path::new(given);
    if path.is_file() {
        return Ok(release::read(path)?.manifest.id);
    }
    bail!(
        "`--over {given}`: give the release's whole id (64 hex characters, as `ductus release` prints with --json) or its file"
    )
}

/// Every interface's code version in the project at `copy`, loaded as a start loads code: an
/// isolate per thread, the interfaces spread over them, each one's files from the loader.
fn versions(copy: &Path, env: &str) -> Result<Vec<Version>> {
    let environment = Environment::load(copy, env)?;
    let configs = ductus_runtime::load_interfaces(copy, &environment)?;
    let heap_mb = ductus_core::settings::Settings::load(copy)?.isolates.heap_mb;
    let loaded_root = ductus_script::loaded_path(copy)?;
    let mut versions = Vec::with_capacity(configs.len());
    let job = |runtime: &tokio::runtime::Runtime,
               isolate: &mut Option<ductus_script::Isolate>,
               config: &ductus_core::InterfaceConfig| {
        let files = if config.functions().iter().all(Option::is_none) {
            Ok(Vec::new())
        } else {
            runtime.block_on(async {
                let isolate = isolate.get_or_insert_with(|| {
                    let mut i = ductus_script::Isolate::new(None, heap_mb);
                    i.capture_console();
                    i
                });
                isolate
                    .add_functions(&ductus_script::Function::all(&config.functions()), config.timeout)
                    .await
                    .with_context(|| format!("loading `{}`'s code", config.name))?;
                isolate.code_files(&config.modules())
            })
        };
        files.and_then(|files| Version::of(&loaded_root, config, &files))
    };
    crate::spread::in_order(&configs, crate::spread::threads(), job, |_, version| {
        versions.push(version?);
        Ok(())
    })?;
    Ok(versions)
}

/// A path a release can carry: relative (not `/x`, `C:/x` or `//server`).
fn relative(path: &str) -> bool {
    !path.starts_with('/') && !path.as_bytes().get(1).is_some_and(|&b| b == b':')
}

/// `text` with the temporary copy's paths named as the project's: what the person knows.
fn inside(text: &str, copy: &Path, root: &Path) -> String {
    let mut out = text.to_string();
    let copies = [copy.to_path_buf(), ductus_script::loaded_path(copy).unwrap_or_else(|_| copy.to_path_buf())];
    for c in copies {
        // As a module URL names it (`file:///tmp/…/`, `file:///C:/…/`).
        let path = c.display().to_string().replace('\\', "/");
        let url = if path.starts_with('/') { format!("file://{path}/") } else { format!("file:///{path}/") };
        for (from, to) in [
            (url, String::new()),
            (format!("{}/", c.display()), String::new()),
            (c.display().to_string(), root.display().to_string()),
        ] {
            out = out.replace(&from, &to);
        }
        let backslashed = format!("{}\\", c.display());
        out = out.replace(&backslashed, "");
    }
    out
}

/// Up to `NAMED` lines, indented, then how many more.
fn list(items: &[String]) -> String {
    let mut lines: Vec<String> = items.iter().take(NAMED).map(|i| format!("  {i}")).collect();
    if items.len() > NAMED {
        lines.push(format!("  … and {} more", items.len() - NAMED));
    }
    lines.join("\n")
}

/// `ductus test` as this binary on `root`, writing nothing (`--read-only`): passed, failed, the
/// milliseconds and its whole output. `only`: these interfaces (none: every one).
fn run_tests(root: &Path, env: &str, only: &[String]) -> Result<(u64, u64, u64, String)> {
    let mut command = Command::new(std::env::current_exe()?);
    command.args(["test", "--read-only", "--json", "--root"]).arg(root).args(["--env", env]);
    for name in only {
        command.args(["--interface", name]);
    }
    let out = command.output().context("running `ductus test`")?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let summary = stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|v| v["type"] == "summary")
        .with_context(|| format!("`ductus test` said: {}", String::from_utf8_lossy(&out.stderr).trim()))?;
    let count = |key: &str| summary[key].as_u64().unwrap_or(0);
    let (passed, failed, ms) = (count("passed"), count("failed"), count("ms"));
    let failed = if out.status.success() { failed } else { failed.max(1) };
    let mut text = String::new();
    for line in stdout.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()) {
        if line["ok"] == false || line["type"] == "load" {
            let place = line["file"].as_str().map(|f| format!(" ({f}:{})", line["line"])).unwrap_or_default();
            text.push_str(&format!(
                "✗ {}: {}{place}\n{}\n",
                line["interface"].as_str().unwrap_or(""),
                line["name"].as_str().unwrap_or("failed to load its tests"),
                indent(line["error"].as_str().unwrap_or(""))
            ));
        }
    }
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    Ok((passed, failed, ms, text))
}

fn indent(text: &str) -> String {
    text.lines().map(|l| format!("    {l}")).collect::<Vec<_>>().join("\n")
}

pub struct Check {
    pub bundle: PathBuf,
    pub env: String,
    pub all_tests: bool,
    pub database_url: Option<String>,
    pub json: bool,
}

/// `ductus release check`: the gate. Exits 1 on any problem.
pub async fn check(args: Check) -> Result<bool> {
    let started = Instant::now();
    let path = args.bundle.clone();
    let Bundle { manifest, files, .. } = tokio::task::spawn_blocking(move || release::read(&path)).await??;
    let mut report = Report::new(args.json);
    let short = release::short(&manifest.id).to_string();
    if !report.json {
        println!(
            "ductus release check: release {short} of {} against environment {} (nothing is changed)",
            manifest.project, args.env
        );
    }

    report.section("release", "The release");
    say_release(&manifest, files.len(), &mut report);

    let unpacked = tempfile::Builder::new().prefix("ductus-release-").tempdir().context("making a temporary folder")?;
    release::unpack(&files, unpacked.path())?;
    let copy = std::fs::canonicalize(unpacked.path())?;

    report.section("environment", &format!("What it changes in {}", args.env));
    let database_url = args.database_url.as_deref().map(str::trim).filter(|u| !u.is_empty());
    let changes = match database_url {
        None => {
            report.add(
                Level::Warning,
                "no database (DUCTUS_DATABASE_URL): what runs there isn't known, so every interface's tests run, and validate checks without it",
                Vec::new(),
            );
            None
        }
        Some(url) => changes(url, &manifest, &mut report).await,
    };

    report.section("validate", "validate");
    validate(&copy, &args.env, database_url, &mut report);

    report.section("tests", "Tests");
    let every: Vec<String> = manifest.interfaces.keys().cloned().collect();
    let (only, why): (Vec<String>, String) = match (&changes, args.all_tests, manifest.tests.ran) {
        (_, true, _) => (Vec::new(), "every interface's (--all-tests)".into()),
        (_, _, false) => (Vec::new(), "every interface's (the release was made without running them)".into()),
        (None, ..) => (Vec::new(), "every interface's (what runs isn't known)".into()),
        (Some(changed), ..) if changed.len() == every.len() => {
            (Vec::new(), "every interface's (each one changes there)".into())
        }
        (Some(changed), ..) => (changed.clone(), format!("those of the {} interface(s) it changes", changed.len())),
    };
    // A command line has room for a few thousand names on Linux, ~32,000 characters on Windows.
    let (only, why) = if only.iter().map(|n| n.len() + 14).sum::<usize>() > 24_000 {
        (Vec::new(), format!("every interface's ({why}: too many to name on a command line)"))
    } else {
        (only, why)
    };
    let tested: Vec<String> = if changes.as_ref().is_some_and(Vec::is_empty) && !args.all_tests && manifest.tests.ran {
        report.add(
            Level::Ok,
            "no interface changes what runs: no tests to run (--all-tests runs them all)",
            Vec::new(),
        );
        Vec::new()
    } else {
        match run_tests(&copy, &args.env, &only) {
            Err(e) => report.add(Level::Problem, format!("tests: {e:#}"), Vec::new()),
            Ok((passed, 0, ms, _)) => {
                report.add(Level::Ok, format!("{why}: {passed} passed in {:.1} s", ms as f64 / 1000.0), Vec::new())
            }
            Ok((passed, failed, _, output)) => report.add(
                Level::Problem,
                format!("{why}: {failed} failed, {passed} passed"),
                inside(&output, &copy, Path::new("the release")).lines().map(str::to_string).collect(),
            ),
        }
        if only.is_empty() { every } else { only }
    };
    report.detail("tested", json!(tested));

    let took = started.elapsed().as_secs_f64();
    let ok = report.problems == 0;
    if report.json {
        let mut document = json!({
            "release": manifest.id,
            "environment": args.env,
            "ok": ok,
            "problems": report.problems,
            "seconds": (took * 100.0).round() / 100.0,
            "checks": report.checks,
        });
        document.as_object_mut().unwrap().extend(report.details);
        println!("{}", serde_json::to_string_pretty(&document)?);
    } else if ok {
        println!("\nrelease {short} passes the gate for {} (checked in {took:.1} s)", args.env);
    } else {
        println!(
            "\n{} problem(s): release {short} doesn't pass the gate for {} (checked in {took:.1} s)",
            report.problems, args.env
        );
    }
    Ok(ok)
}

/// What the manifest says about where the release came from.
fn say_release(m: &Manifest, files: usize, report: &mut Report) {
    report.add(Level::Ok, format!("{files} file(s) match its id {}", m.id), Vec::new());
    let why = m.made.reason.as_deref().map(|r| format!(": {r}")).unwrap_or_default();
    report.add(
        Level::Ok,
        format!("made by {} on {} at {} with {}{why}", m.made.by, m.made.host, m.made.at, m.made.binary),
        Vec::new(),
    );
    match &m.git {
        Some(g) => report.add(
            Level::Ok,
            format!("from commit {} ({} commit(s) of history)", release::short(&g.commit), g.commits),
            Vec::new(),
        ),
        None => report.add(Level::Note, "not made from git", Vec::new()),
    }
    if !m.uncommitted.is_empty() {
        report.add(
            Level::Warning,
            format!("carries {} file(s) that weren't committed (made with --uncommitted)", m.uncommitted.len()),
            m.uncommitted.clone(),
        );
    }
    if m.made.binary != VERSION {
        report.add(Level::Note, format!("made with Ductus {}, checked with {VERSION}", m.made.binary), Vec::new());
    }
    if m.tests.ran {
        report.add(
            Level::Ok,
            format!(
                "its tests passed when it was made ({} in environment {})",
                m.tests.passed,
                m.tests.environment.as_deref().unwrap_or("?")
            ),
            Vec::new(),
        );
    } else {
        report.add(Level::Warning, "made without running its tests (--no-tests)", Vec::new());
    }
    report.detail("manifest", serde_json::to_value(m).unwrap_or_default());
}

/// The interfaces whose code version differs from what runs on that database (new ones too),
/// said in the report. None when what runs can't be read.
async fn changes(url: &str, m: &Manifest, report: &mut Report) -> Option<Vec<String>> {
    let names: Vec<&str> = m.interfaces.keys().map(String::as_str).collect();
    let current = match ductus_store::Store::connect_read_only(url).await {
        Err(e) => {
            report.add(Level::Problem, format!("database: {e:#}"), Vec::new());
            return None;
        }
        Ok(store) => store.current_versions(&names).await,
    };
    let current = match current {
        Ok(c) => c,
        Err(e) => {
            report.add(
                Level::Warning,
                format!("couldn't read what runs there ({e:#}): every interface's tests run"),
                Vec::new(),
            );
            return None;
        }
    };
    let running: BTreeMap<&str, String> =
        current.iter().map(|c| (c.interface.as_str(), code::hex(&c.version))).collect();
    let (mut changed, mut new, mut same) = (Vec::new(), Vec::new(), 0);
    for (name, interface) in &m.interfaces {
        match running.get(name.as_str()) {
            None => new.push(name.clone()),
            Some(v) if *v != interface.version => changed.push(name.clone()),
            Some(_) => same += 1,
        }
    }
    let line = |names: &[String]| -> Vec<String> {
        let mut lines: Vec<String> = names
            .iter()
            .take(10)
            .map(|n| {
                let was = running
                    .get(n.as_str())
                    .map(|v| format!(": {} → {}", release::short(v), release::short(&m.interfaces[n].version)))
                    .unwrap_or_default();
                format!("{n}{was}")
            })
            .collect();
        if names.len() > 10 {
            lines.push(format!("… and {} more", names.len() - 10));
        }
        lines
    };
    report.add(
        Level::Ok,
        format!("{} interface(s) change, {} new there, {same} as they run now", changed.len(), new.len()),
        Vec::new(),
    );
    if !changed.is_empty() {
        report.add(Level::Note, "changed:", line(&changed));
    }
    if !new.is_empty() {
        report.add(Level::Note, "new there:", line(&new));
    }
    report.detail("changes", json!({ "changed": changed, "new": new, "unchanged": same }));
    let mut all = changed;
    all.extend(new);
    all.sort();
    Some(all)
}

/// `ductus validate --read-only` as this binary on the unpacked release.
fn validate(copy: &Path, env: &str, database_url: Option<&str>, report: &mut Report) {
    let mut command = match std::env::current_exe() {
        Ok(exe) => Command::new(exe),
        Err(e) => return report.add(Level::Problem, format!("validate: {e}"), Vec::new()),
    };
    command.args(["validate", "--read-only", "--root"]).arg(copy).args(["--env", env]);
    // The URL goes in the environment: on a command line, other users could read its password.
    match database_url {
        Some(url) => command.env("DUCTUS_DATABASE_URL", url),
        None => command.env_remove("DUCTUS_DATABASE_URL"),
    };
    if std::env::var_os("RUST_LOG").is_none() {
        command.env("RUST_LOG", "warn,rustls_platform_verifier=off,tiberius=error");
    }
    let out = match command.output() {
        Ok(out) => out,
        Err(e) => return report.add(Level::Problem, format!("couldn't run `ductus validate`: {e}"), Vec::new()),
    };
    let output = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    // Its files named as the release has them, not by the temporary copy's path.
    let output = inside(&output, copy, Path::new("the release"));
    let last = output.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
    if out.status.success() {
        let worth: Vec<String> =
            output.lines().filter(|l| l.starts_with('⚠') || l.starts_with("note:")).map(str::to_string).collect();
        report.add(Level::Ok, last, worth);
    } else {
        report.add(Level::Problem, last, output.lines().map(str::to_string).collect());
    }
    report.detail("validate", json!({ "ok": out.status.success(), "output": output }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_from_outside_the_project_are_told() {
        assert!(relative("lib/x.ts"));
        for outside in ["/home/ann/shared/x.ts", "C:/shared/x.ts", "//server/share/x.ts"] {
            assert!(!relative(outside), "{outside}");
        }
    }

    #[test]
    fn the_copy_s_paths_read_as_the_project_s() {
        let copy = Path::new("/tmp/ductus-release-abc");
        let root = Path::new("/home/ann/p");
        let text = "loading `a`'s code: /tmp/ductus-release-abc/interfaces/a/t.ts:3:1\nin /tmp/ductus-release-abc\nnot found: file:///tmp/ductus-release-abc/lib/x.ts";
        assert_eq!(
            inside(text, copy, root),
            "loading `a`'s code: interfaces/a/t.ts:3:1\nin /home/ann/p\nnot found: lib/x.ts"
        );
    }

    #[test]
    fn long_lists_are_cut() {
        let items: Vec<String> = (0..25).map(|i| format!("f{i}")).collect();
        let text = list(&items);
        assert!(text.ends_with("… and 5 more") && text.lines().count() == 21, "{text}");
    }

    #[test]
    fn over_takes_an_id_or_says_what_it_wants() {
        let id = "a".repeat(64);
        assert_eq!(base_release(&id).unwrap(), id);
        assert!(base_release("abc").unwrap_err().to_string().contains("64 hex"));
    }
}
