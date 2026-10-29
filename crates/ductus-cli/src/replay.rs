//! `ductus replay`: a period's messages through this version's code again, sending nothing,
//! compared with another version run beside it (the installed one, or another version of the
//! project), with what each destination recorded, and with kept outputs. The comparing and the
//! report are `ductus_runtime::replay`; this runs both sides and lines their rows up.
//!
//! The other version runs as `ductus replay --emit` (its own binary, its own project), writing its
//! decisions as JSON lines in the order this one makes its own (interface, message, destination),
//! so the two streams merge as they come: nothing is held in memory but the report.

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use ductus_runtime::replay::{self, Event, Other, OtherSide, Report};
use std::collections::{BTreeMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufReadExt, BufReader};

pub struct Options {
    pub root: PathBuf,
    pub env: String,
    pub database_url: String,
    pub since: String,
    pub until: Option<String>,
    pub interfaces: Vec<String>,
    pub limit: Option<u64>,
    pub against: Option<String>,
    pub baseline: Option<String>,
    pub report: Option<PathBuf>,
    pub json: bool,
    pub hide_values: bool,
}

/// The period from `--since` (a duration back from `--until`, or a time) and `--until` (default now).
pub fn period(since: &str, until: Option<&str>) -> Result<(DateTime<Utc>, DateTime<Utc>)> {
    let time = |text: &str| {
        DateTime::parse_from_rfc3339(text.trim())
            .map(|t| t.with_timezone(&Utc))
            .with_context(|| format!("`{text}`: a time like 2026-10-29T08:00:00Z, or a duration like 24h"))
    };
    let to = match until {
        Some(text) => time(text)?,
        None => Utc::now(),
    };
    let from = match ductus_core::config::parse_duration(since) {
        Ok(back) => to - chrono::Duration::from_std(back)?,
        Err(_) => time(since)?,
    };
    if from >= to {
        bail!("the period is empty: {from} is not before {to}");
    }
    Ok((from, to))
}

/// `--emit`: this version's events as JSON lines on stdout.
pub async fn emit(options: replay::Options) -> Result<()> {
    let (_, mut events) = replay::start(options).await?;
    let mut out = std::io::BufWriter::new(std::io::stdout().lock());
    while let Some(event) = events.recv().await {
        serde_json::to_writer(&mut out, &event?)?;
        out.write_all(b"\n")?;
    }
    out.flush()?;
    Ok(())
}

pub async fn run(o: Options) -> Result<bool> {
    let started = std::time::Instant::now();
    let (from, to) = period(&o.since, o.until.as_deref())?;
    let mut notes = Vec::new();
    let (spec, _baseline) = other_side(&o, &mut notes)?;
    let (names, mut mine) = replay::start(replay::Options {
        root: o.root.clone(),
        env: o.env.clone(),
        database_url: o.database_url.clone(),
        from,
        to,
        interfaces: o.interfaces.clone(),
        limit: o.limit,
    })
    .await?;
    let mut theirs = match spec {
        None => None,
        Some(spec) => match Theirs::start(&spec, &o, from, to).await {
            Ok(theirs) => Some(theirs),
            Err(why) => {
                notes.push(format!("{why}: outputs compared with kept outputs only"));
                None
            }
        },
    };
    let other = theirs.as_ref().map(|t| t.other.clone());
    let mut report = Report::new(&o.env, from, to, other, o.hide_values);
    let names: HashSet<String> = names.into_iter().collect();
    // Interfaces only the other version has: how many rows each.
    let mut only_theirs: BTreeMap<String, u64> = BTreeMap::new();
    let mut not_in_theirs: Vec<String> = Vec::new();

    while let Some(event) = mine.recv().await {
        let event = event?;
        let Event::Row(row) = &event else {
            if let Event::Done { interface, messages, seconds } = &event {
                eprintln!("{interface}: {messages} message(s) replayed in {seconds:.1} s");
            }
            report.event(&event);
            continue;
        };
        let Some(t) = theirs.as_mut() else {
            report.add(row, OtherSide::NotRun);
            continue;
        };
        // Their events before this row: interfaces they announce, rows this version has no match for.
        loop {
            let before = match t.peek().await? {
                Some(Event::Row(r)) => r.key() < row.key(),
                Some(Event::Interface { interface, .. }) => interface.as_str() <= row.interface.as_str(),
                Some(Event::Start { .. } | Event::Done { .. }) => true,
                None => false,
            };
            if !before {
                break;
            }
            match t.next() {
                Some(Event::Row(r)) if names.contains(&r.interface) => report.only_other(&r),
                Some(Event::Row(r)) => *only_theirs.entry(r.interface).or_default() += 1,
                Some(Event::Interface { interface, skipped, outputs }) => {
                    if skipped.is_some() {
                        t.skipped.insert(interface.clone());
                    }
                    t.announced.insert(interface.clone());
                    report.other_event(&Event::Interface { interface, skipped, outputs });
                }
                _ => {}
            }
        }
        let side = if t.failed.is_some() || t.skipped.contains(&row.interface) {
            OtherSide::NotRun
        } else if !t.announced.contains(&row.interface) {
            if !not_in_theirs.contains(&row.interface) {
                not_in_theirs.push(row.interface.clone());
            }
            OtherSide::NotRun
        } else if matches!(t.peek().await?, Some(Event::Row(r)) if r.key() == row.key()) {
            let Some(Event::Row(r)) = t.next() else { unreachable!("just peeked") };
            report.add(row, OtherSide::Row(&r));
            continue;
        } else {
            OtherSide::Missing
        };
        report.add(row, side);
    }
    if let Some(mut t) = theirs {
        while let Some(event) = {
            t.peek().await?;
            t.next()
        } {
            match event {
                Event::Row(r) if names.contains(&r.interface) => report.only_other(&r),
                Event::Row(r) => *only_theirs.entry(r.interface).or_default() += 1,
                _ => {}
            }
        }
        if let Some(why) = t.finish().await {
            notes.push(why);
        }
    }
    let label = report.other.as_ref().map(|o| o.label.clone()).unwrap_or_default();
    for interface in not_in_theirs {
        notes.push(format!("interface `{interface}` isn't in {label}: compared with the record and kept outputs only"));
    }
    for (interface, rows) in only_theirs {
        notes.push(format!(
            "interface `{interface}` is only in {label} ({rows} comparison(s) there): not replayed by this version"
        ));
    }
    report.notes = notes;
    report.seconds = started.elapsed().as_secs_f64();

    let document = report.document();
    if o.json {
        println!("{}", serde_json::to_string_pretty(&document)?);
    } else {
        print!("{}", report.text());
    }
    if let Some(path) = &o.report {
        let html = !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("json"));
        let content = if html { report.html() } else { serde_json::to_string_pretty(&document)? };
        std::fs::write(path, content).with_context(|| format!("writing {}", path.display()))?;
        let line = format!("report: {} ({})", path.display(), document["checksum"].as_str().unwrap_or_default());
        if o.json { eprintln!("{line}") } else { println!("{line}") }
    }
    Ok(report.ok())
}

/// Who the other side is: a binary and a project, and how the report names it.
struct Spec {
    binary: PathBuf,
    project: PathBuf,
    /// "the project at main~3"; None: named by its version once it says it.
    baseline: Option<String>,
    installed: bool,
}

/// From `--against` and `--baseline`. A baseline given as a Git revision is written into a temporary
/// folder, removed when the returned guard goes.
fn other_side(o: &Options, notes: &mut Vec<String>) -> Result<(Option<Spec>, Option<Temporary>)> {
    let this = std::env::current_exe().context("finding this binary")?;
    let against = o.against.as_deref();
    if against == Some("stored") {
        if o.baseline.is_some() {
            bail!("--baseline runs another version of the project: it can't go with --against stored");
        }
        notes.push("outputs compared with kept outputs only (--against stored)".into());
        return Ok((None, None));
    }
    let named = |path: &str| -> Result<PathBuf> {
        let path = PathBuf::from(path);
        if !path.is_file() {
            bail!("--against {}: no such file (installed, stored, or the path of a ductus binary)", path.display());
        }
        Ok(path)
    };
    if let Some(given) = &o.baseline {
        let (project, temporary) = baseline(&o.root, given)?;
        let binary = match against {
            None => this,
            Some("installed") => {
                installed().ok_or_else(|| anyhow!("--against installed: no installed version found"))?
            }
            Some(path) => named(path)?,
        };
        let spec = Spec { binary, project, baseline: Some(format!("the project at {given}")), installed: false };
        return Ok((Some(spec), temporary));
    }
    let (binary, installed) = match against {
        None | Some("installed") => match installed() {
            None => {
                notes.push(format!(
                    "no installed version found ({}): outputs compared with kept outputs only; name a binary with --against",
                    installed_path().display()
                ));
                return Ok((None, None));
            }
            Some(path) if same_file(&path, &this) => {
                notes.push(format!(
                    "the installed version ({}) is this binary: outputs compared with kept outputs only; for a code change use --baseline",
                    path.display()
                ));
                return Ok((None, None));
            }
            Some(path) => (path, true),
        },
        Some(path) => (named(path)?, false),
    };
    Ok((Some(Spec { binary, project: o.root.clone(), baseline: None, installed }), None))
}

/// Where the packages install the binary.
fn installed_path() -> PathBuf {
    #[cfg(windows)]
    {
        let programs = std::env::var_os("ProgramFiles").unwrap_or_else(|| "C:\\Program Files".into());
        PathBuf::from(programs).join("Ductus").join("ductus.exe")
    }
    #[cfg(not(windows))]
    PathBuf::from("/usr/bin/ductus")
}

fn installed() -> Option<PathBuf> {
    Some(installed_path()).filter(|p| p.is_file())
}

fn same_file(a: &Path, b: &Path) -> bool {
    matches!((std::fs::canonicalize(a), std::fs::canonicalize(b)), (Ok(a), Ok(b)) if a == b)
}

/// A folder removed when this goes.
struct Temporary(PathBuf);

impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `--baseline`: a project folder, or a Git revision of the project's repository, written into a
/// temporary folder with Git's own plumbing (reads only: the repository isn't changed). Secrets,
/// rarely committed, come from the project as it is now when the revision has none.
fn baseline(root: &Path, given: &str) -> Result<(PathBuf, Option<Temporary>)> {
    if Path::new(given).is_dir() {
        return Ok((PathBuf::from(given), None));
    }
    let git = |dir: &Path, args: &[&str]| -> Result<Vec<u8>> {
        let out = std::process::Command::new("git").arg("-C").arg(dir).args(args).output().with_context(|| {
            format!("--baseline {given}: not a folder, and Git isn't available to read it as a revision")
        })?;
        if !out.status.success() {
            bail!(
                "--baseline {given}: not a folder or a Git revision of {} ({})",
                root.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(out.stdout)
    };
    let text = |bytes: Vec<u8>| String::from_utf8_lossy(&bytes).trim().to_string();
    let top = PathBuf::from(text(git(root, &["rev-parse", "--show-toplevel"])?));
    let prefix = text(git(root, &["rev-parse", "--show-prefix"])?);
    let commit = text(git(&top, &["rev-parse", "--verify", "--quiet", &format!("{given}^{{commit}}")])?);
    let mut list = vec!["ls-tree", "-r", "-z", "--name-only", &commit];
    if !prefix.is_empty() {
        list.extend(["--", &prefix]);
    }
    let files = git(&top, &list)?;
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let dir = Temporary(std::env::temp_dir().join(format!("ductus-replay-{}-{nanos}", std::process::id())));
    for name in files.split(|b| *b == 0).filter(|n| !n.is_empty()) {
        let name = String::from_utf8_lossy(name);
        let relative = name.strip_prefix(&prefix).unwrap_or(&name);
        let path = dir.0.join(relative);
        std::fs::create_dir_all(path.parent().unwrap_or(&dir.0))?;
        std::fs::write(&path, git(&top, &["cat-file", "blob", &format!("{commit}:{name}")])?)?;
    }
    let secrets = root.join("secrets");
    if secrets.is_dir() && !dir.0.join("secrets").exists() {
        std::fs::create_dir_all(dir.0.join("secrets"))?;
        for entry in std::fs::read_dir(&secrets)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                std::fs::copy(entry.path(), dir.0.join("secrets").join(entry.file_name()))?;
            }
        }
    }
    Ok((dir.0.clone(), Some(dir)))
}

/// The other version, running as `ductus replay --emit`.
struct Theirs {
    other: Other,
    child: tokio::process::Child,
    lines: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    stderr: tokio::task::JoinHandle<String>,
    peeked: Option<Event>,
    ended: bool,
    /// Interfaces it said it replays, and those it couldn't.
    announced: HashSet<String>,
    skipped: HashSet<String>,
    /// It stopped before its end: why.
    failed: Option<String>,
}

impl Theirs {
    /// Start it and read its first line (its version). An error says why it can't be compared with.
    async fn start(spec: &Spec, o: &Options, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Self, String> {
        let mut command = tokio::process::Command::new(&spec.binary);
        command.args(["replay", "--emit", "--root"]).arg(&spec.project);
        command.args(["--env", &o.env, "--since", &from.to_rfc3339(), "--until", &to.to_rfc3339()]);
        for interface in &o.interfaces {
            command.args(["--interface", interface]);
        }
        if let Some(limit) = o.limit {
            command.args(["--limit", &limit.to_string()]);
        }
        // The URL goes in the environment: on a command line, other users could read its password.
        command.env("DUCTUS_DATABASE_URL", &o.database_url).env_remove("DUCTUS_PROJECT");
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        command.kill_on_drop(true);
        let shown = spec.binary.display().to_string();
        let mut child = command.spawn().map_err(|e| format!("couldn't run {shown}: {e}"))?;
        let stdout = child.stdout.take().expect("piped");
        let mut stderr = child.stderr.take().expect("piped");
        let stderr = tokio::spawn(async move {
            let mut text = String::new();
            let _ = tokio::io::AsyncReadExt::read_to_string(&mut stderr, &mut text).await;
            text
        });
        let mut lines = BufReader::new(stdout).lines();
        let first = lines.next_line().await.ok().flatten();
        let version = match first.as_deref().map(serde_json::from_str::<Event>) {
            Some(Ok(Event::Start { version })) => version,
            _ => {
                let _ = child.wait().await;
                let said = stderr.await.unwrap_or_default();
                let said = said.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
                let version = std::process::Command::new(&spec.binary)
                    .arg("--version")
                    .output()
                    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                    .unwrap_or_default();
                return Err(format!(
                    "{shown} ({version}) can't replay (it has no `replay --emit`: versions before 0.6.6.4 don't; {said})"
                ));
            }
        };
        let label = match &spec.baseline {
            Some(baseline) if spec.binary == std::env::current_exe().unwrap_or_default() => baseline.clone(),
            Some(baseline) => format!("{baseline} with version {version}"),
            None if spec.installed => format!("version {version} (installed)"),
            None => format!("version {version} ({shown})"),
        };
        let other = Other { label, version, binary: shown, project: spec.project.display().to_string() };
        Ok(Self {
            other,
            child,
            lines,
            stderr,
            peeked: None,
            ended: false,
            announced: HashSet::new(),
            skipped: HashSet::new(),
            failed: None,
        })
    }

    /// Its next event, without taking it (None at its end).
    async fn peek(&mut self) -> Result<Option<&Event>> {
        if self.peeked.is_none() && !self.ended {
            match self.lines.next_line().await? {
                Some(line) => {
                    let event = serde_json::from_str(&line)
                        .with_context(|| format!("{}: an unreadable line: {line:.200}", self.other.binary))?;
                    self.peeked = Some(event);
                }
                None => {
                    self.ended = true;
                    let status = self.child.wait().await?;
                    if !status.success() {
                        let said = std::mem::replace(&mut self.stderr, tokio::spawn(async { String::new() }));
                        let said = said.await.unwrap_or_default();
                        let said = said.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
                        self.failed = Some(format!(
                            "{} stopped before its end ({status}): {said}; the rest is compared with the record and kept outputs only",
                            self.other.label
                        ));
                    }
                }
            }
        }
        Ok(self.peeked.as_ref())
    }

    /// Take the event `peek` read.
    fn next(&mut self) -> Option<Event> {
        self.peeked.take()
    }

    /// After its end: why it failed, if it did.
    async fn finish(mut self) -> Option<String> {
        if !self.ended {
            let _ = self.child.kill().await;
        }
        self.failed.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_period_is_a_duration_back_or_two_times() {
        let (from, to) = period("24h", Some("2026-10-29T12:00:00Z")).unwrap();
        assert_eq!(
            (from.to_rfc3339(), to.to_rfc3339()),
            ("2026-10-28T12:00:00+00:00".into(), "2026-10-29T12:00:00+00:00".into())
        );
        let (from, _) = period("2026-10-29T08:00:00+02:00", Some("2026-10-29T12:00:00Z")).unwrap();
        assert_eq!(from.to_rfc3339(), "2026-10-29T06:00:00+00:00");
        let err = period("2026-10-30T00:00:00Z", Some("2026-10-29T12:00:00Z")).unwrap_err().to_string();
        assert!(err.contains("the period is empty"), "{err}");
        let err = period("yesterday", None).unwrap_err().to_string();
        assert!(err.contains("a duration like 24h"), "{err}");
    }
}
