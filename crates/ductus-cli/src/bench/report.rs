//! What `ductus bench` measured, as text or JSON (`--json`: the same fields, to keep and compare),
//! and what each number means for sizing, said as what was measured here, never as a limit.
//!
//! Each scenario's numbers are named with their unit (`stored_per_second`, `ack_p50_ms`,
//! `memory_1000_mb`) and kept per pass, so a run reports each as the range its passes gave and
//! the advice reads the median.

use serde::ser::SerializeMap;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// A durable commit slower than this is worth acting on (as `ductus doctor` warns).
pub const SLOW_COMMIT: Duration = Duration::from_millis(10);
/// A round trip to the database longer than this is said (each strict ACK waits for it).
pub const FAR_DATABASE: Duration = Duration::from_millis(1);
/// Delivery under this share of the receiving rate is said: the destinations were the slower side.
const DELIVERY_KEPT_UP: f64 = 0.9;
/// Relaxed under this many times strict's rate (16 connections) "buys little" here.
const RELAXED_GAINS: f64 = 1.2;
/// Fan-out ingest under this share of one destination's is said: delivery rows weigh on the ACK.
const FANOUT_HOLDS: f64 = 0.8;
/// A busy interface's peak, for "what share of a thread" (a million a day averages ~12/s;
/// peaks are often ten times that).
const PEAK: f64 = 150.0;

/// The whole run. Field names carry their units; `--json` is this.
#[derive(Debug, Serialize)]
pub struct Report {
    /// The ductus version that measured.
    pub ductus: String,
    pub machine: Machine,
    pub database: Database,
    /// How many times each scenario ran (alternated: every scenario once, then again).
    pub passes: usize,
    /// Seconds each time-boxed scenario sent for (after a second's warm-up, not counted).
    pub seconds_per_scenario: u64,
    pub commits: Commits,
    pub scenarios: Vec<Scenario>,
    /// What the numbers mean, one sentence each (the text report's "What it means").
    pub advice: Vec<String>,
    /// How long the whole bench took.
    pub took_s: f64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Machine {
    pub host: String,
    /// `Ubuntu 24.04.1 LTS (Linux 6.6.87)`, `Windows`.
    pub os: String,
    /// CPU threads this process may use.
    pub cores: usize,
    pub cpu: Option<String>,
    pub memory_gb: Option<f64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Database {
    /// The server's version, `18.1`.
    pub version: String,
    /// `socket` (a local socket), `local` (this machine, over TCP) or `network`.
    pub location: String,
    /// The host as the URL gives it (no login, no password).
    pub host: String,
    pub round_trip_ms: f64,
    pub synchronous_commit: String,
    /// The schema the bench ran in (dropped at its end).
    pub schema: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Commits {
    pub count: usize,
    pub median_ms: f64,
    pub p99_ms: f64,
}

/// One scenario's numbers, over every pass.
#[derive(Debug, Serialize)]
pub struct Scenario {
    /// Stable, for comparing runs: `mllp-strict-1`. Its first part is its group (the report's
    /// table): `mllp`, `fanout`, `file`, `http`, `transforms`, `memory`.
    pub id: String,
    /// As the report says it: `strict, 1 connection`.
    pub name: String,
    /// Each number by its name (which ends in its unit), in the report's column order.
    pub numbers: Numbers,
}

/// A scenario's numbers in their order (a JSON object, keys in that order).
#[derive(Debug, Default)]
pub struct Numbers(pub Vec<(&'static str, Range)>);

impl Serialize for Numbers {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(Some(self.0.len()))?;
        for (key, range) in &self.0 {
            map.serialize_entry(key, range)?;
        }
        map.end()
    }
}

/// One number over the passes: the lowest, the highest, and each pass's in the order run.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct Range {
    pub low: f64,
    pub high: f64,
    pub passes: Vec<f64>,
}

impl Range {
    /// The median pass: what the advice reads.
    pub fn mid(&self) -> f64 {
        let mut sorted = self.passes.clone();
        sorted.sort_by(f64::total_cmp);
        sorted.get(sorted.len() / 2).copied().unwrap_or_default()
    }
}

impl Scenario {
    /// A number's median pass, by name.
    pub fn get(&self, key: &str) -> Option<f64> {
        self.numbers.0.iter().find(|(k, _)| *k == key).map(|(_, r)| r.mid())
    }

    fn group(&self) -> &str {
        self.id.split('-').next().unwrap_or_default()
    }
}

/// What one pass of one scenario measured.
pub struct Measured {
    pub id: String,
    pub name: String,
    pub numbers: Vec<(&'static str, f64)>,
}

/// The passes' measurements as scenarios, in the order first run, each number a range.
pub fn gather(runs: Vec<Measured>) -> Vec<Scenario> {
    let mut scenarios: Vec<Scenario> = Vec::new();
    for run in runs {
        let i = match scenarios.iter().position(|s| s.id == run.id) {
            Some(i) => i,
            None => {
                scenarios.push(Scenario { id: run.id, name: run.name, numbers: Numbers::default() });
                scenarios.len() - 1
            }
        };
        let numbers = &mut scenarios[i].numbers.0;
        for (key, value) in run.numbers {
            match numbers.iter_mut().find(|(k, _)| *k == key) {
                Some((_, r)) => {
                    r.low = r.low.min(value);
                    r.high = r.high.max(value);
                    r.passes.push(value);
                }
                None => numbers.push((key, Range { low: value, high: value, passes: vec![value] })),
            }
        }
    }
    scenarios
}

pub fn ms(d: Duration) -> f64 {
    (d.as_secs_f64() * 1000.0 * 100.0).round() / 100.0
}

/// Seconds, to two places.
pub fn secs(d: Duration) -> f64 {
    (d.as_secs_f64() * 100.0).round() / 100.0
}

/// The `q` quantile (0..1) of sorted `times`.
pub fn quantile(sorted: &[Duration], q: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    sorted[((sorted.len() as f64 * q) as usize).min(sorted.len() - 1)]
}

/// `12,345` (thousands grouped; rounded to a whole number).
pub fn whole(x: f64) -> String {
    let digits = format!("{:.0}", x.max(0.0));
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// To two significant figures, for a number whose precision would mislead: `24,000`, `850`.
pub(super) fn about(x: f64) -> String {
    if x < 100.0 {
        return whole(x);
    }
    let step = 10f64.powi(x.log10().floor() as i32 - 1);
    whole((x / step).round() * step)
}

/// `2.41 ms`, `0.08 ms`, `1.2 s`.
pub(super) fn took(ms: f64) -> String {
    if ms >= 1000.0 { format!("{:.1} s", ms / 1000.0) } else { format!("{ms:.2} ms") }
}

/// `112 MB`, `0.11 MB`.
pub(super) fn megabytes(mb: f64) -> String {
    if mb >= 10.0 { format!("{mb:.0} MB") } else { format!("{mb:.2} MB") }
}

/// A number as the report prints it, by its name's unit.
pub(super) fn number(key: &str, x: f64) -> String {
    if key.ends_with("_ms") {
        took(x)
    } else if key.ends_with("_s") {
        format!("{x:.2} s")
    } else if key.ends_with("_mb") {
        megabytes(x)
    } else {
        whole(x)
    }
}

/// A range as the report prints it: `6,272`, `6,100–6,400`, `1.40–1.52 ms`.
pub(super) fn range(key: &str, r: &Range) -> String {
    let (low, high) = (number(key, r.low), number(key, r.high));
    if low == high {
        return low;
    }
    match high.rsplit_once(' ') {
        Some((_, unit)) if low.ends_with(&format!(" {unit}")) => {
            format!("{}–{high}", &low[..low.len() - unit.len() - 1])
        }
        _ => format!("{low}–{high}"),
    }
}

/// A number's column heading.
pub(super) fn label(key: &str) -> &str {
    match key {
        "stored_per_second" => "stored/s",
        "ack_p50_ms" => "ACK p50",
        "ack_p99_ms" => "ACK p99",
        "delivered_per_second" => "delivered/s",
        "all_delivered_s" => "all delivered",
        "deliveries_per_second" => "deliveries/s",
        "rows_per_second" => "rows/s",
        "requests_per_second" => "requests/s",
        "p50_ms" => "p50",
        "p99_ms" => "p99",
        "transforms_per_second" => "transforms/s",
        "per_thread_per_second" => "per thread/s",
        "memory_100_mb" => "100 idle",
        "memory_1000_mb" => "1,000 idle",
        "per_interface_mb" => "each",
        "interfaces_per_gb" => "per GB",
        "ready_1000_s" => "1,000 ready in",
        other => other,
    }
}

/// A group's table heading: what was run.
pub(super) fn heading(group: &str) -> &str {
    match group {
        "mllp" => "MLLP ingest (synthetic ADT, each message delivered to a file)",
        "fanout" => "Fan-out (strict, 16 connections, each message to every destination's folder)",
        "file" => "File ingest (one CSV, `split: records`, a row a message)",
        "http" => "HTTP respond mode (a GET answered by a transform; nothing stored)",
        "transforms" => "Transforms (the guides' ORU to FHIR transform in V8, no database)",
        "memory" => "Memory (an engine of its own, idle, MLLP interfaces with a transform each)",
        other => other,
    }
}

/// What the numbers mean, each sentence tied to the number it reads (the advice in both the text
/// and the JSON). Workloads for scale: a million messages a day averages ~12/s, with peaks
/// often ten times that.
pub fn advise(commits: &Commits, database: &Database, scenarios: &[Scenario]) -> Vec<String> {
    let mut advice = Vec::new();
    let find = |id: &str| scenarios.iter().find(|s| s.id == id);
    let get = |id: &str, key: &str| find(id).and_then(|s| s.get(key));
    let synchronous = database.synchronous_commit == "on";
    if synchronous {
        advice.push(format!(
            "A durable commit took {} (median): a strict interface's sender waits for one per message, so one connection was answered about {} times a second here. For scale, a million messages a day averages 12 a second.",
            took(commits.median_ms),
            whole(1000.0 / commits.median_ms.max(0.001))
        ));
        if commits.median_ms > ms(SLOW_COMMIT) {
            advice.push("Commits are slow (over 10 ms): the database's disk flush is the limit for strict ingest. A faster disk (SSD/NVMe, a write cache with a battery) for the database, or `durability: relaxed` for interfaces that can afford it, raise it.".into());
        }
    } else {
        advice.push(format!(
            "synchronous_commit is {} on this database, so commits don't wait for the disk ({} each): strict interfaces can lose the last moments' messages if the database's server crashes, as relaxed ones can. Turn it on for what must not be lost.",
            database.synchronous_commit,
            took(commits.median_ms)
        ));
    }
    if database.round_trip_ms > ms(FAR_DATABASE) {
        advice.push(format!(
            "The database is {} away (a round trip): every strict ACK waits for at least that besides the commit. On the same network segment or host it's usually under 0.5 ms.",
            took(database.round_trip_ms)
        ));
    }
    let stored = |id| get(id, "stored_per_second");
    if let (Some(one), Some(many)) = (stored("mllp-strict-1"), stored("mllp-strict-16")) {
        advice.push(format!(
            "16 connections stored {} messages a second, {:.1}× one connection's {}: Postgres flushes several connections' commits together, so senders on connections of their own add up.",
            whole(many),
            many / one.max(0.001),
            whole(one)
        ));
    }
    if let (Some(strict), Some(relaxed)) = (stored("mllp-strict-16"), stored("mllp-relaxed-16")) {
        let times = relaxed / strict.max(0.001);
        let gain = if times < RELAXED_GAINS {
            "about the same: 16 strict connections already share their flushes, so the disk isn't what limits them here and relaxed buys little on this machine"
        } else {
            "its ACKs don't wait for the disk flush"
        };
        advice.push(format!(
            "`durability: relaxed` stored {} a second on 16 connections, {times:.1}× strict, {gain}. A crash of the database's server can lose a relaxed interface's last moments' messages (an engine crash never does).",
            whole(relaxed),
        ));
    }
    let behind: Vec<String> = scenarios
        .iter()
        .filter(|s| s.group() == "mllp")
        .filter_map(|s| Some((s, s.get("stored_per_second")?, s.get("delivered_per_second")?)))
        .filter(|(_, stored, delivered)| *delivered < stored * DELIVERY_KEPT_UP)
        .map(|(s, stored, delivered)| {
            format!("{} ({} delivered a second of {} received)", s.name, whole(delivered), whole(stored))
        })
        .collect();
    if !behind.is_empty() {
        advice.push(format!(
            "Delivery fell behind receiving over MLLP, {}: one worker delivers each destination of a shared interface, so a backlog grew. `isolation: dedicated` with `workers: N` shares its lanes (from `ordering.key`) among N; in production the destination's own speed decides it too.",
            behind.join(" and ")
        ));
    }
    if let (Some(one), Some(many), Some(deliveries)) =
        (stored("fanout-1"), stored("fanout-25"), get("fanout-25", "deliveries_per_second"))
    {
        let holds = if many < one * FANOUT_HOLDS {
            format!(
                "ingest fell to {:.0}% of one destination's {}: each message's ACK waits for its delivery rows too (one per destination)",
                many / one.max(0.001) * 100.0,
                whole(one)
            )
        } else {
            format!("ingest held near one destination's {}", whole(one))
        };
        advice.push(format!(
            "With 25 destinations {} messages a second were stored and {} deliveries a second made: {holds}. A feed to many receivers can be split with routes (`interface:` destinations) when its ACKs must stay fast.",
            whole(many),
            whole(deliveries)
        ));
    }
    if let Some(rows) = get("file-csv", "rows_per_second") {
        advice.push(format!(
            "A CSV's rows were stored at {} a second: a file of a million rows takes about {} to read in, then its rows go out as their destinations take them.",
            whole(rows),
            took(1_000_000.0 / rows.max(0.001) * 1000.0)
        ));
    }
    if let (Some(one), Some(many), Some(p50)) = (
        get("http-respond-1", "p50_ms"),
        get("http-respond-16", "requests_per_second"),
        get("http-respond-16", "p50_ms"),
    ) {
        advice.push(format!(
            "Respond mode answered {} requests a second on 16 kept-alive connections ({} p50; {} with one caller): what the engine adds to a call. Your code's own lookups add theirs, and a caller opening a new connection per call pays the connect too.",
            whole(many),
            took(p50),
            took(one)
        ));
    }
    if let Some(one) = get("transforms-1", "transforms_per_second") {
        let all = find("transforms-all").and_then(|s| Some((s.get("transforms_per_second")?, s.name.clone())));
        let together = match all {
            Some((all, name)) => format!(", {} on {name} ({:.1}× one)", whole(all), all / one.max(0.001)),
            None => String::new(),
        };
        advice.push(format!(
            "One thread ran the guides' ORU to FHIR transform {} times a second{together}: an interface at a peak of {PEAK:.0} messages a second keeps {:.1}% of one thread busy with code like it, so code is rarely the limit; lookups (`db()`, `fetch`) usually are, and run while other messages are transformed.",
            whole(one),
            PEAK / one.max(0.001) * 100.0
        ));
    }
    if let (Some(each), Some(per_gb), Some(base)) = (
        get("memory-idle", "per_interface_mb"),
        get("memory-idle", "interfaces_per_gb"),
        get("memory-idle", "memory_100_mb"),
    ) {
        advice.push(format!(
            "An idle interface with its code took about {} of memory, so a GB holds about {} of them beside the engine's own (~{} with 100). Traffic adds its own: messages in hand, connections, the code's data.",
            megabytes(each),
            about(per_gb),
            megabytes(base)
        ));
    }
    advice
}

impl Report {
    pub fn text(&self) -> String {
        let m = &self.machine;
        let d = &self.database;
        let passes = match self.passes {
            1 => "one pass".to_string(),
            n => format!("{n} passes, alternated, each number the range they gave"),
        };
        let mut out = format!(
            "ductus bench {}: {} and its database, {passes}; timed scenarios {} s after a second's warm-up\n\n",
            self.ductus, m.host, self.seconds_per_scenario
        );
        let mut machine = format!("{} cores", m.cores);
        if let Some(cpu) = &m.cpu {
            machine += &format!(" ({cpu})");
        }
        if let Some(gb) = m.memory_gb {
            machine += &format!(", {gb:.1} GB");
        }
        out += &format!("Machine    {machine}, {}\n", m.os);
        let location = match d.location.as_str() {
            "socket" => format!("on this machine (a local socket in {})", d.host),
            "local" => format!("on this machine ({})", d.host),
            _ => format!("over the network ({})", d.host),
        };
        out += &format!(
            "Database   Postgres {} {location}, round trip {}, synchronous_commit {}\n           in a schema of its own, {} (dropped at the end)\n\n",
            d.version,
            took(d.round_trip_ms),
            d.synchronous_commit,
            d.schema
        );
        out += &format!(
            "Commits    {} median, {} p99 ({} durable commits, each waiting for the disk)\n",
            took(self.commits.median_ms),
            took(self.commits.p99_ms),
            self.commits.count
        );
        let mut groups: Vec<&str> = Vec::new();
        for s in &self.scenarios {
            if !groups.contains(&s.group()) {
                groups.push(s.group());
            }
        }
        for group in groups {
            out += &format!("\n{}\n", heading(group));
            out += &table(self.scenarios.iter().filter(|s| s.group() == group).collect());
        }
        out += "\nWhat it means\n";
        for a in &self.advice {
            out += &format!("- {a}\n");
        }
        out += &format!(
            "\nMeasured in {:.0} s with synthetic messages delivered to files: your interfaces' code and destinations add their own time (`ductus bench interface NAME` measures an interface's code).\n",
            self.took_s
        );
        out
    }
}

/// One group's scenarios as a table: a column per number (the first scenario's), right-aligned.
fn table(scenarios: Vec<&Scenario>) -> String {
    let Some(first) = scenarios.first() else { return String::new() };
    let keys: Vec<&str> = first.numbers.0.iter().map(|(k, _)| *k).collect();
    let rows: Vec<(String, Vec<String>)> = scenarios
        .iter()
        .map(|s| {
            let cells = keys
                .iter()
                .map(|k| s.numbers.0.iter().find(|(key, _)| key == k).map_or("-".into(), |(_, r)| range(k, r)))
                .collect();
            (s.name.clone(), cells)
        })
        .collect();
    let name_width = rows.iter().map(|(n, _)| n.chars().count()).max().unwrap_or(0).max(2);
    let widths: Vec<usize> = keys
        .iter()
        .enumerate()
        .map(|(i, k)| rows.iter().map(|(_, c)| c[i].chars().count()).max().unwrap_or(0).max(label(k).chars().count()))
        .collect();
    let pad = |text: &str, width: usize| format!("{}{text}", " ".repeat(width.saturating_sub(text.chars().count())));
    let mut out = format!("  {}", " ".repeat(name_width));
    for (k, w) in keys.iter().zip(&widths) {
        out += &format!("  {}", pad(label(k), *w));
    }
    out += "\n";
    for (name, cells) in &rows {
        out += &format!("  {name}{}", " ".repeat(name_width - name.chars().count()));
        for (cell, w) in cells.iter().zip(&widths) {
            out += &format!("  {}", pad(cell, *w));
        }
        out += "\n";
    }
    out
}

/// This machine, as far as it can be read without privileges.
pub fn machine() -> Machine {
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    Machine { host: ductus_runtime::doctor::hostname(), os: os(), cores, cpu: cpu(), memory_gb: memory_gb() }
}

#[cfg(target_os = "linux")]
fn os() -> String {
    let release = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    let name = release
        .lines()
        .find_map(|l| l.strip_prefix("PRETTY_NAME="))
        .map(|n| n.trim_matches('"').to_string())
        .unwrap_or_else(|| "Linux".into());
    match std::fs::read_to_string("/proc/sys/kernel/osrelease") {
        Ok(kernel) => format!("{name} (Linux {})", kernel.trim()),
        Err(_) => name,
    }
}

#[cfg(not(target_os = "linux"))]
fn os() -> String {
    match std::env::consts::OS {
        "windows" => "Windows".into(),
        other => other.into(),
    }
}

#[cfg(target_os = "linux")]
fn cpu() -> Option<String> {
    let info = std::fs::read_to_string("/proc/cpuinfo").ok()?;
    let model = info.lines().find(|l| l.starts_with("model name"))?.split_once(':')?.1.trim();
    Some(model.split_whitespace().collect::<Vec<_>>().join(" "))
}

#[cfg(not(target_os = "linux"))]
fn cpu() -> Option<String> {
    std::env::var("PROCESSOR_IDENTIFIER").ok()
}

#[cfg(target_os = "linux")]
fn memory_gb() -> Option<f64> {
    let info = std::fs::read_to_string("/proc/meminfo").ok()?;
    let kb: f64 =
        info.lines().find_map(|l| l.strip_prefix("MemTotal:"))?.trim().trim_end_matches("kB").trim().parse().ok()?;
    Some(kb / 1024.0 / 1024.0)
}

#[cfg(windows)]
fn memory_gb() -> Option<f64> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut status = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    // SAFETY: `status` is a MEMORYSTATUSEX with its length set, as the call requires.
    let ok = unsafe { GlobalMemoryStatusEx(&mut status) };
    (ok != 0).then(|| status.ullTotalPhys as f64 / 1024.0 / 1024.0 / 1024.0)
}

#[cfg(not(any(target_os = "linux", windows)))]
fn memory_gb() -> Option<f64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(id: &str, numbers: &[(&'static str, f64)]) -> Measured {
        Measured { id: id.into(), name: id.into(), numbers: numbers.to_vec() }
    }

    fn database(synchronous: &str, round_trip_ms: f64) -> Database {
        Database {
            version: "18.1".into(),
            location: "local".into(),
            host: "localhost".into(),
            round_trip_ms,
            synchronous_commit: synchronous.into(),
            schema: "ductus_bench_1_abcdef".into(),
        }
    }

    fn mllp(id: &str, stored: f64, delivered: f64) -> Measured {
        run(id, &[("stored_per_second", stored), ("delivered_per_second", delivered)])
    }

    #[test]
    fn passes_gather_into_ranges_in_the_order_run_and_the_advice_reads_the_median() {
        let scenarios = gather(vec![
            run("mllp-strict-1", &[("stored_per_second", 600.0), ("ack_p50_ms", 1.5)]),
            run("file-csv", &[("rows_per_second", 90_000.0)]),
            run("mllp-strict-1", &[("stored_per_second", 640.0), ("ack_p50_ms", 1.4)]),
            run("file-csv", &[("rows_per_second", 95_000.0)]),
            run("mllp-strict-1", &[("stored_per_second", 610.0), ("ack_p50_ms", 1.6)]),
        ]);
        assert_eq!(scenarios.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["mllp-strict-1", "file-csv"]);
        let stored = &scenarios[0].numbers.0[0];
        assert_eq!(stored.0, "stored_per_second");
        assert_eq!(stored.1, Range { low: 600.0, high: 640.0, passes: vec![600.0, 640.0, 610.0] });
        assert_eq!(scenarios[0].get("stored_per_second"), Some(610.0));
        assert_eq!(scenarios[1].get("rows_per_second"), Some(95_000.0), "two passes: the higher of the middle two");
        let json = serde_json::to_string(&scenarios[0].numbers).unwrap();
        assert!(
            json.starts_with(
                r#"{"stored_per_second":{"low":600.0,"high":640.0,"passes":[600.0,640.0,610.0]},"ack_p50_ms""#
            ),
            "{json}"
        );
    }

    #[test]
    fn the_advice_reads_each_number_and_says_what_it_means() {
        let commits = Commits { count: 100, median_ms: 2.5, p99_ms: 4.0 };
        let scenarios = gather(vec![
            mllp("mllp-strict-1", 400.0, 400.0),
            mllp("mllp-strict-16", 5_200.0, 1_500.0),
            mllp("mllp-relaxed-16", 13_000.0, 12_500.0),
            run("fanout-1", &[("stored_per_second", 3_000.0)]),
            run("fanout-25", &[("stored_per_second", 1_500.0), ("deliveries_per_second", 18_000.0)]),
            run("file-csv", &[("rows_per_second", 100_000.0)]),
            run("http-respond-1", &[("p50_ms", 0.3)]),
            run("http-respond-16", &[("requests_per_second", 20_000.0), ("p50_ms", 0.7)]),
            run("transforms-1", &[("transforms_per_second", 15_000.0)]),
            Measured {
                id: "transforms-all".into(),
                name: "8 threads".into(),
                numbers: vec![("transforms_per_second", 90_000.0)],
            },
            run("memory-idle", &[("memory_100_mb", 40.0), ("per_interface_mb", 0.08), ("interfaces_per_gb", 12_800.0)]),
        ]);
        let advice = advise(&commits, &database("on", 0.1), &scenarios).join("\n");
        assert!(advice.contains("took 2.50 ms (median)") && advice.contains("about 400 times a second"), "{advice}");
        assert!(
            advice.contains("16 connections stored 5,200 messages a second, 13.0× one connection's 400"),
            "{advice}"
        );
        assert!(
            advice.contains("relaxed` stored 13,000 a second on 16 connections, 2.5× strict, its ACKs"),
            "{advice}"
        );
        assert!(
            advice.contains(
                "Delivery fell behind receiving over MLLP, mllp-strict-16 (1,500 delivered a second of 5,200 received): one worker"
            ),
            "{advice}"
        );
        assert!(
            !advice.contains("mllp-strict-1 (") && !advice.contains("relaxed-16 ("),
            "only where it fell behind: {advice}"
        );
        assert!(
            advice.contains("With 25 destinations 1,500 messages a second were stored and 18,000 deliveries a second made: ingest fell to 50% of one destination's 3,000"),
            "{advice}"
        );
        assert!(advice.contains("stored at 100,000 a second: a file of a million rows takes about 10.0 s"), "{advice}");
        assert!(
            advice.contains(
                "answered 20,000 requests a second on 16 kept-alive connections (0.70 ms p50; 0.30 ms with one caller)"
            ),
            "{advice}"
        );
        assert!(advice.contains("transform 15,000 times a second, 90,000 on 8 threads (6.0× one): an interface at a peak of 150 messages a second keeps 1.0% of one thread"), "{advice}");
        assert!(
            advice.contains(
                "about 0.08 MB of memory, so a GB holds about 13,000 of them beside the engine's own (~40 MB with 100)"
            ),
            "{advice}"
        );
        assert!(!advice.contains("slow") && !advice.contains("away"), "nothing said that wasn't measured: {advice}");

        let even = gather(vec![mllp("mllp-strict-16", 5_600.0, 5_600.0), mllp("mllp-relaxed-16", 5_500.0, 5_400.0)]);
        let advice = advise(&commits, &database("on", 0.1), &even).join("\n");
        assert!(advice.contains("1.0× strict, about the same") && advice.contains("buys little"), "{advice}");
        assert!(!advice.contains("fell behind"), "{advice}");

        let slow = Commits { count: 100, median_ms: 12.0, p99_ms: 30.0 };
        let advice = advise(&slow, &database("on", 2.0), &[]).join("\n");
        assert!(advice.contains("Commits are slow (over 10 ms)"), "{advice}");
        assert!(advice.contains("The database is 2.00 ms away"), "{advice}");

        let advice = advise(&commits, &database("off", 0.1), &[]).join("\n");
        assert!(advice.contains("synchronous_commit is off") && !advice.contains("times a second"), "{advice}");
    }

    #[test]
    fn numbers_read_as_people_write_them() {
        assert_eq!(whole(0.4), "0");
        assert_eq!(whole(999.6), "1,000");
        assert_eq!(whole(1_234_567.0), "1,234,567");
        assert_eq!(took(0.084), "0.08 ms");
        assert_eq!(took(1234.0), "1.2 s");
        assert_eq!(about(24_328.0), "24,000");
        assert_eq!(about(851.0), "850");
        assert_eq!(about(42.4), "42");
        let r = |low, high| Range { low, high, passes: vec![low, high] };
        assert_eq!(range("stored_per_second", &r(6_100.0, 6_400.0)), "6,100–6,400");
        assert_eq!(range("stored_per_second", &r(6_100.2, 6_100.4)), "6,100");
        assert_eq!(range("ack_p50_ms", &r(1.4, 1.52)), "1.40–1.52 ms");
        assert_eq!(range("ack_p99_ms", &r(900.0, 1_200.0)), "900.00 ms–1.2 s");
        assert_eq!(range("all_delivered_s", &r(1.2, 1.5)), "1.20–1.50 s");
        assert_eq!(range("memory_1000_mb", &r(110.2, 114.0)), "110–114 MB");
        assert_eq!(range("per_interface_mb", &r(0.081, 0.081)), "0.08 MB");
        let times: Vec<Duration> = (1..=100).map(Duration::from_millis).collect();
        assert_eq!(quantile(&times, 0.5), Duration::from_millis(51));
        assert_eq!(quantile(&times, 0.99), Duration::from_millis(100));
        assert_eq!(quantile(&[], 0.5), Duration::ZERO);
    }

    #[test]
    fn each_group_is_a_table_with_a_column_per_number() {
        let scenarios = gather(vec![
            mllp("mllp-strict-1", 616.0, 616.0),
            Measured {
                id: "mllp-strict-16".into(),
                name: "strict, 16 connections".into(),
                numbers: vec![("stored_per_second", 6_272.0), ("delivered_per_second", 1_700.0)],
            },
        ]);
        let text = table(scenarios.iter().collect());
        assert_eq!(
            text,
            "                          stored/s  delivered/s\n  mllp-strict-1                616          616\n  strict, 16 connections     6,272        1,700\n"
        );
    }
}
