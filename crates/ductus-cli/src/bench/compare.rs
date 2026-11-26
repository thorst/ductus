//! `ductus bench compare a.json b.json`: two or more saved runs of `ductus bench --json` side by
//! side, to choose between machines (instance types, disks, a database near or far) or to see
//! what changed between two versions on one machine.
//!
//! Scenarios are matched by their `id` and numbers by their key, so a run from an older or newer
//! `ductus` compares as far as both measured (`-` where one didn't). Each cell is the median of
//! its run's passes, as the advice reads it. With two runs a last column says whether they
//! differ: the second's median over the first's (`×1.95`) where their ranges don't overlap, `≈`
//! where they do, since then the difference is within what the passes of one run already gave.

use super::report::{Commits, Database, Machine, Range, heading, label, number, took};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde::de::{MapAccess, Visitor};
use std::path::{Path, PathBuf};

/// A saved `ductus bench --json`, as far as comparing reads it.
#[derive(Debug, Deserialize)]
struct Saved {
    ductus: String,
    machine: Machine,
    database: Database,
    passes: usize,
    seconds_per_scenario: u64,
    commits: Commits,
    scenarios: Vec<SavedScenario>,
}

#[derive(Debug, Deserialize)]
struct SavedScenario {
    id: String,
    name: String,
    numbers: Ordered,
}

/// A scenario's numbers in the file's order (the report's column order).
#[derive(Debug)]
struct Ordered(Vec<(String, Range)>);

impl<'de> Deserialize<'de> for Ordered {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct InOrder;
        impl<'de> Visitor<'de> for InOrder {
            type Value = Ordered;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a scenario's numbers, each with its low, high and passes")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Ordered, M::Error> {
                let mut numbers = Vec::new();
                while let Some(entry) = map.next_entry::<String, Range>()? {
                    numbers.push(entry);
                }
                Ok(Ordered(numbers))
            }
        }
        d.deserialize_map(InOrder)
    }
}

/// One file to compare: its name in the table's heading, and what it holds.
struct Run {
    label: String,
    saved: Saved,
}

/// Print the runs in `files` side by side.
pub fn compare(files: &[PathBuf]) -> Result<()> {
    if files.len() < 2 {
        bail!("give two or more files of `ductus bench --json` to compare");
    }
    let mut runs = Vec::new();
    for file in files {
        runs.push(Run { label: name(file, files), saved: load(file)? });
    }
    print!("{}", text(&runs));
    Ok(())
}

fn load(file: &Path) -> Result<Saved> {
    let json = std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
    serde_json::from_str(&json).with_context(|| {
        format!(
            "{} isn't a report of `ductus bench --json` (save one with `ductus bench --json > run.json`)",
            file.display()
        )
    })
}

/// A file's column heading: its name without `.json`, or its whole path when two files share a
/// name (`old/laptop.json`, `new/laptop.json`).
fn name(file: &Path, all: &[PathBuf]) -> String {
    let stem = |f: &Path| f.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let mine = stem(file);
    if all.iter().filter(|f| stem(f) == mine).count() > 1 { file.display().to_string() } else { mine }
}

enum Row {
    Heading(String),
    Cells { name: String, cells: Vec<String>, change: Option<String> },
}

fn cells(name: impl Into<String>, cells: Vec<String>) -> Row {
    Row::Cells { name: name.into(), cells, change: None }
}

/// The table: what each run ran on, its commits, then every scenario's numbers by group.
fn text(runs: &[Run]) -> String {
    let each = |f: &dyn Fn(&Saved) -> String| runs.iter().map(|r| f(&r.saved)).collect::<Vec<_>>();
    let mut rows = vec![
        cells("ductus", each(&|s| s.ductus.clone())),
        cells("host", each(&|s| s.machine.host.clone())),
        cells("Machine", each(&|s| machine(&s.machine))),
        // `Ubuntu 24.04.1 LTS (Linux 6.6.87…)`: the kernel's long name would widen every column.
        cells("OS", each(&|s| s.machine.os.split(" (").next().unwrap_or_default().to_string())),
        cells("Postgres", each(&|s| postgres(&s.database))),
        cells("round trip", each(&|s| took(s.database.round_trip_ms))),
        cells("synchronous_commit", each(&|s| s.database.synchronous_commit.clone())),
        cells("passes", each(&|s| format!("{} × {} s", s.passes, s.seconds_per_scenario))),
        cells("commit p50", each(&|s| took(s.commits.median_ms))),
        cells("commit p99", each(&|s| took(s.commits.p99_ms))),
    ];
    let ids = in_order(runs.iter().flat_map(|r| r.saved.scenarios.iter().map(|s| s.id.as_str())));
    let groups = in_order(ids.iter().map(|id| group(id)));
    for g in groups {
        rows.push(Row::Heading(heading(g).to_string()));
        for id in ids.iter().filter(|id| group(id) == g) {
            let found: Vec<Option<&SavedScenario>> =
                runs.iter().map(|r| r.saved.scenarios.iter().find(|s| s.id == *id)).collect();
            let scenario_name = found.iter().flatten().next().map_or(id.to_string(), |s| s.name.clone());
            rows.push(cells(format!("  {scenario_name}"), vec![String::new(); runs.len()]));
            let keys = in_order(found.iter().flatten().flat_map(|s| s.numbers.0.iter().map(|(k, _)| k.as_str())));
            for key in keys {
                let ranges: Vec<Option<&Range>> = found
                    .iter()
                    .map(|s| s.and_then(|s| s.numbers.0.iter().find(|(k, _)| k == key).map(|(_, r)| r)))
                    .collect();
                let cells = ranges.iter().map(|r| r.map_or("-".into(), |r| number(key, r.mid()))).collect();
                let change = match ranges[..] {
                    [Some(a), Some(b)] => Some(change(a, b)),
                    [_, _] => Some(String::new()),
                    _ => None,
                };
                rows.push(Row::Cells { name: format!("    {}", label(key)), cells, change });
            }
        }
    }
    let mut out = format!("ductus bench compare: {} runs\n\n", runs.len());
    out += &render(runs, &rows);
    out += "\nEach number is the median of its run's passes";
    if runs.len() == 2 {
        out += &format!(
            "; the last column is {}'s over {}'s where their ranges don't overlap, ≈ where they do (the difference is within the spread of one run's own passes)",
            runs[1].label, runs[0].label
        );
    }
    out += ". `-`: not measured in that run (another ductus version).\n";
    if runs.iter().any(|r| r.saved.passes == 1) {
        out += "A run of one pass (`--quick`) has no range, so any difference shows as one: bench without `--quick` to tell a difference from noise.\n";
    }
    out
}

/// The second run's median over the first's, or `≈` when their ranges overlap.
fn change(a: &Range, b: &Range) -> String {
    if a.low <= b.high && b.low <= a.high {
        return "≈".into();
    }
    let (first, second) = (a.mid(), b.mid());
    if first == 0.0 {
        return String::new();
    }
    let times = second / first;
    if times < 10.0 { format!("×{times:.2}") } else { format!("×{times:.0}") }
}

/// Each item once, in the order first seen.
fn in_order<'a>(items: impl Iterator<Item = &'a str>) -> Vec<&'a str> {
    let mut seen: Vec<&str> = Vec::new();
    for item in items {
        if !seen.contains(&item) {
            seen.push(item);
        }
    }
    seen
}

fn group(id: &str) -> &str {
    id.split('-').next().unwrap_or_default()
}

/// `8 cores, 15.5 GB` (the CPU's model is often too long for a column).
fn machine(m: &Machine) -> String {
    match m.memory_gb {
        Some(gb) => format!("{} cores, {gb:.1} GB", m.cores),
        None => format!("{} cores", m.cores),
    }
}

/// `18.1, this machine`, `18.1, db1 (network)`.
fn postgres(d: &Database) -> String {
    match d.location.as_str() {
        "socket" | "local" => format!("{}, this machine", d.version),
        _ => format!("{}, {} (network)", d.version, d.host),
    }
}

/// The rows as aligned columns: names left, each run's cells right-aligned under its label.
fn render(runs: &[Run], rows: &[Row]) -> String {
    let width = |s: &str| s.chars().count();
    let cell_rows = || {
        rows.iter().filter_map(|r| match r {
            Row::Cells { name, cells, change } => Some((name, cells, change)),
            Row::Heading(_) => None,
        })
    };
    let name_width = cell_rows().map(|(n, _, _)| width(n)).max().unwrap_or(0);
    let widths: Vec<usize> = runs
        .iter()
        .enumerate()
        .map(|(i, run)| cell_rows().map(|(_, c, _)| width(&c[i])).max().unwrap_or(0).max(width(&run.label)))
        .collect();
    let pad = |text: &str, w: usize| format!("{}{text}", " ".repeat(w.saturating_sub(width(text))));
    let line = |name: &str, cells: &[String], change: &str| {
        let mut out = format!("{name}{}", " ".repeat(name_width - width(name)));
        for (cell, w) in cells.iter().zip(&widths) {
            out += &format!("  {}", pad(cell, *w));
        }
        if !change.is_empty() {
            out += &format!("  {change}");
        }
        out.trim_end().to_string() + "\n"
    };
    let labels: Vec<String> = runs.iter().map(|r| r.label.clone()).collect();
    let mut out = line("", &labels, "");
    for row in rows {
        match row {
            Row::Heading(h) => out += &format!("\n{h}\n"),
            Row::Cells { name, cells, change } => out += &line(name, cells, change.as_deref().unwrap_or_default()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::report::{Measured, Report, gather};
    use super::*;

    fn report(passes: usize, commit_ms: f64, runs: Vec<Measured>) -> String {
        let report = Report {
            ductus: "0.8.0".into(),
            machine: Machine { host: "h".into(), os: "Linux".into(), cores: 8, cpu: None, memory_gb: Some(16.0) },
            database: Database {
                version: "18.1".into(),
                location: "local".into(),
                host: "localhost".into(),
                round_trip_ms: 0.08,
                synchronous_commit: "on".into(),
                schema: "ductus_bench_1_abcdef".into(),
            },
            passes,
            seconds_per_scenario: 2,
            commits: Commits { count: 100, median_ms: commit_ms, p99_ms: commit_ms * 2.0 },
            scenarios: gather(runs),
            advice: vec!["it means something".into()],
            took_s: 100.0,
        };
        serde_json::to_string_pretty(&report).unwrap()
    }

    fn run(id: &str, name: &str, numbers: &[(&'static str, f64)]) -> Measured {
        Measured { id: id.into(), name: name.into(), numbers: numbers.to_vec() }
    }

    fn saved(label: &str, json: &str) -> Run {
        Run { label: label.into(), saved: serde_json::from_str(json).expect("a saved report reads back") }
    }

    #[test]
    fn two_runs_side_by_side_with_what_changed_beyond_the_noise() {
        let mut a = Vec::new();
        let mut b = Vec::new();
        for (stored_a, stored_b, p50_a, p50_b) in
            [(600.0, 1_200.0, 1.4, 1.38), (620.0, 1_250.0, 1.5, 1.45), (610.0, 1_230.0, 1.45, 1.6)]
        {
            a.push(run(
                "mllp-strict-1",
                "strict, 1 connection",
                &[("stored_per_second", stored_a), ("ack_p50_ms", p50_a)],
            ));
            b.push(run(
                "mllp-strict-1",
                "strict, 1 connection",
                &[("stored_per_second", stored_b), ("ack_p50_ms", p50_b)],
            ));
            a.push(run("file-csv", "CSV", &[("rows_per_second", 90_000.0)]));
        }
        b.push(run("memory-idle", "100 and 1,000", &[("per_interface_mb", 0.08)]));
        let runs = [saved("laptop", &report(3, 2.4, a)), saved("m6i", &report(3, 1.2, b))];
        let text = text(&runs);
        assert!(text.starts_with("ductus bench compare: 2 runs\n"), "{text}");
        let words = |start: &str| {
            let line = text.lines().find(|l| l.trim_start().starts_with(start)).unwrap_or_default();
            line.split_whitespace().collect::<Vec<_>>().join(" ")
        };
        assert_eq!(
            words("stored/s"),
            "stored/s 610 1,230 ×2.02",
            "medians, and the ratio where ranges are apart: {text}"
        );
        assert_eq!(words("ACK p50"), "ACK p50 1.45 ms 1.45 ms ≈", "overlapping ranges: {text}");
        assert_eq!(words("rows/s"), "rows/s 90,000 -", "a scenario only one run has: {text}");
        assert_eq!(words("each"), "each - 0.08 MB", "{text}");
        assert_eq!(words("commit p50"), "commit p50 2.40 ms 1.20 ms", "{text}");
        assert_eq!(words("strict, 1"), "strict, 1 connection", "the scenario's name on a line of its own: {text}");
        let header = text.lines().nth(2).unwrap();
        let machine = text.lines().find(|l| l.starts_with("Machine")).unwrap();
        assert_eq!(header.len(), machine.len(), "run labels right-aligned over their columns:\n{text}");
        assert!(text.contains("\nMLLP ingest (") && text.contains("\nFile ingest (") && text.contains("\nMemory ("));
        assert!(text.find("MLLP ingest").unwrap() < text.find("File ingest").unwrap(), "groups in the order first run");
        assert!(text.contains("m6i's over laptop's"), "{text}");
        assert!(!text.contains("--quick"), "{text}");
    }

    #[test]
    fn one_pass_says_it_has_no_range_and_three_runs_have_no_ratio() {
        let one = || report(1, 2.0, vec![run("mllp-strict-1", "strict", &[("stored_per_second", 500.0)])]);
        let runs = [saved("a", &one()), saved("b", &one()), saved("c", &one())];
        let text = text(&runs);
        assert!(text.contains("3 runs") && text.contains("bench without `--quick`"), "{text}");
        assert!(!text.contains("≈") && !text.contains("×2") && !text.contains("over"), "{text}");
    }

    #[test]
    fn a_file_that_isnt_a_report_says_so_and_names_shared_by_two_files_show_their_paths() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("notes.json");
        std::fs::write(&file, r#"{"hello": 1}"#).unwrap();
        let err = format!("{:#}", load(&file).unwrap_err());
        assert!(err.contains("notes.json isn't a report of `ductus bench --json`"), "{err}");
        let files = [PathBuf::from("old/laptop.json"), PathBuf::from("new/laptop.json"), PathBuf::from("m6i.json")];
        assert_eq!(name(&files[2], &files), "m6i");
        assert!(name(&files[0], &files).ends_with("laptop.json"));
        assert!(compare(&files[..1]).is_err());
    }
}
