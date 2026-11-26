//! What `ductus bench` measured, as text or JSON (`--json`: the same fields, to keep and compare),
//! and what each number means for sizing, said as what was measured here, never as a limit.

use serde::Serialize;
use std::time::Duration;

/// A durable commit slower than this is worth acting on (as `ductus doctor` warns).
pub const SLOW_COMMIT: Duration = Duration::from_millis(10);
/// A round trip to the database longer than this is said (each strict ACK waits for it).
pub const FAR_DATABASE: Duration = Duration::from_millis(1);
/// Delivery this far behind the last ACK is said: the destinations were the slower side.
pub const DELIVERY_BEHIND: Duration = Duration::from_secs(2);
/// Relaxed under this many times strict's rate (16 connections) "buys little" here.
const RELAXED_GAINS: f64 = 1.2;

/// The whole run. Field names carry their units; `--json` is this.
#[derive(Debug, Serialize)]
pub struct Report {
    /// The ductus version that measured.
    pub ductus: String,
    pub machine: Machine,
    pub database: Database,
    /// Seconds each scenario sent for (after a second's warm-up, not counted).
    pub seconds_per_scenario: u64,
    pub commits: Commits,
    pub scenarios: Vec<Scenario>,
    /// What the numbers mean, one sentence each (the text report's "What it means").
    pub advice: Vec<String>,
    /// How long the whole bench took.
    pub took_s: f64,
}

#[derive(Debug, Serialize)]
pub struct Machine {
    pub host: String,
    /// `Ubuntu 24.04.1 LTS (Linux 6.6.87)`, `Windows`.
    pub os: String,
    /// CPU threads this process may use.
    pub cores: usize,
    pub cpu: Option<String>,
    pub memory_gb: Option<f64>,
}

#[derive(Debug, Serialize)]
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

#[derive(Debug, Serialize)]
pub struct Commits {
    pub count: usize,
    pub median_ms: f64,
    pub p99_ms: f64,
}

/// One scenario's numbers.
#[derive(Debug, Serialize)]
pub struct Scenario {
    /// Stable, for comparing runs: `mllp-strict-1`.
    pub id: String,
    /// As the report says it: `strict MLLP, 1 connection`.
    pub name: String,
    pub connections: u64,
    pub durability: String,
    /// Messages ACKed in the measured time (stored before each ACK).
    pub messages: u64,
    pub seconds: f64,
    /// Messages stored and ACKed per second, all connections together.
    pub per_second: f64,
    /// A message's wait for its ACK, as the sender saw it.
    pub ack_p50_ms: f64,
    pub ack_p99_ms: f64,
    /// From the last ACK until every message was at its destination (files, in the bench).
    pub delivered_after_ms: f64,
    /// Messages delivered per second over the whole scenario (warm-up to the last delivery):
    /// the receiving rate when delivery kept up, the delivering side's own rate when it didn't.
    pub delivered_per_second: f64,
}

pub fn ms(d: Duration) -> f64 {
    (d.as_secs_f64() * 1000.0 * 100.0).round() / 100.0
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

/// `2.41 ms`, `0.08 ms`, `1.2 s`.
fn took(ms: f64) -> String {
    if ms >= 1000.0 { format!("{:.1} s", ms / 1000.0) } else { format!("{ms:.2} ms") }
}

/// What the numbers mean, each sentence tied to the number it reads (the advice in both the text
/// and the JSON). Workloads for scale: a million messages a day averages ~12/s, with peaks
/// often ten times that.
pub fn advise(commits: &Commits, database: &Database, scenarios: &[Scenario]) -> Vec<String> {
    let mut advice = Vec::new();
    let find = |id: &str| scenarios.iter().find(|s| s.id == id);
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
    if let (Some(one), Some(many)) = (find("mllp-strict-1"), find("mllp-strict-16")) {
        advice.push(format!(
            "16 connections stored {} messages a second, {:.1}× one connection's {}: Postgres flushes several connections' commits together, so senders on connections of their own add up.",
            whole(many.per_second),
            many.per_second / one.per_second.max(0.001),
            whole(one.per_second)
        ));
    }
    if let (Some(strict), Some(relaxed)) = (find("mllp-strict-16"), find("mllp-relaxed-16")) {
        let times = relaxed.per_second / strict.per_second.max(0.001);
        let gain = if times < RELAXED_GAINS {
            "about the same: 16 strict connections already share their flushes, so the disk isn't what limits them here and relaxed buys little on this machine"
        } else {
            "its ACKs don't wait for the disk flush"
        };
        advice.push(format!(
            "`durability: relaxed` stored {} a second on 16 connections, {times:.1}× strict, {gain}. A crash of the database's server can lose a relaxed interface's last moments' messages (an engine crash never does).",
            whole(relaxed.per_second),
        ));
    }
    let behind: Vec<String> = scenarios
        .iter()
        .filter(|s| s.delivered_after_ms > ms(DELIVERY_BEHIND))
        .map(|s| {
            format!(
                "{} ({} delivered a second of {} received, done {} after the last ACK)",
                s.name,
                whole(s.delivered_per_second),
                whole(s.per_second),
                took(s.delivered_after_ms)
            )
        })
        .collect();
    if !behind.is_empty() {
        advice.push(format!(
            "Delivery fell behind receiving in {}: one worker delivers each destination of a shared interface. `isolation: dedicated` with `workers: N` shares its lanes (from `ordering.key`) among N; in production the destination's own speed decides it too.",
            behind.join(" and ")
        ));
    }
    advice
}

impl Report {
    pub fn text(&self) -> String {
        let m = &self.machine;
        let d = &self.database;
        let mut out = format!(
            "ductus bench {}: {} and its database, each scenario {} s after a second's warm-up\n\n",
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
            "Commits    {} median, {} p99 ({} durable commits, each waiting for the disk)\n\n",
            took(self.commits.median_ms),
            took(self.commits.p99_ms),
            self.commits.count
        );
        let width = self.scenarios.iter().map(|s| s.name.len()).max().unwrap_or(8).max(8);
        out += &format!(
            "{:width$}  {:>8}  {:>9}  {:>9}  {:>11}  all delivered\n",
            "Scenario", "stored/s", "ACK p50", "ACK p99", "delivered/s"
        );
        for s in &self.scenarios {
            out += &format!(
                "{:width$}  {:>8}  {:>9}  {:>9}  {:>11}  {} after the last ACK\n",
                s.name,
                whole(s.per_second),
                took(s.ack_p50_ms),
                took(s.ack_p99_ms),
                whole(s.delivered_per_second),
                took(s.delivered_after_ms)
            );
        }
        out += "\nWhat it means\n";
        for a in &self.advice {
            out += &format!("- {a}\n");
        }
        out += &format!(
            "\nMeasured in {:.0} s with synthetic ADT (about 200 bytes each) delivered to files: your interfaces' code and destinations add their own time (`ductus test` times a message through the code).\n",
            self.took_s
        );
        out
    }
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

    fn scenario(id: &str, per_second: f64, delivered_after_ms: f64) -> Scenario {
        Scenario {
            id: id.into(),
            name: id.into(),
            connections: 1,
            durability: "strict".into(),
            messages: 0,
            seconds: 5.0,
            per_second,
            ack_p50_ms: 1.0,
            ack_p99_ms: 2.0,
            delivered_after_ms,
            delivered_per_second: 1_500.0,
        }
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

    #[test]
    fn the_advice_reads_each_number_and_says_what_it_means() {
        let commits = Commits { count: 100, median_ms: 2.5, p99_ms: 4.0 };
        let scenarios = [
            scenario("mllp-strict-1", 400.0, 50.0),
            scenario("mllp-strict-16", 5_200.0, 3_500.0),
            scenario("mllp-relaxed-16", 13_000.0, 100.0),
        ];
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
            advice.contains("Delivery fell behind receiving in mllp-strict-16 (1,500 delivered a second of 5,200 received, done 3.5 s after the last ACK): one worker"),
            "{advice}"
        );
        assert!(!advice.contains("mllp-strict-1,"), "only where delivery fell behind: {advice}");
        assert!(!advice.contains("slow") && !advice.contains("away"), "nothing said that wasn't measured: {advice}");

        let even = [scenario("mllp-strict-16", 5_600.0, 0.0), scenario("mllp-relaxed-16", 5_500.0, 0.0)];
        let advice = advise(&commits, &database("on", 0.1), &even).join("\n");
        assert!(advice.contains("1.0× strict, about the same") && advice.contains("buys little"), "{advice}");

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
        let times: Vec<Duration> = (1..=100).map(Duration::from_millis).collect();
        assert_eq!(quantile(&times, 0.5), Duration::from_millis(51));
        assert_eq!(quantile(&times, 0.99), Duration::from_millis(100));
        assert_eq!(quantile(&[], 0.5), Duration::ZERO);
    }
}
