//! What the engine has done since it started: counts and timings per interface, destination
//! and resource, for the REST API and Prometheus (`GET /api/v1/metrics`).
//!
//! Kept in memory as atomics (an increment per event, no locks, nothing written anywhere), so
//! they start at zero with the engine: Prometheus expects counters to reset on a restart, and
//! its `rate()` handles it. What monitoring must remember across a restart or a failover (last
//! activity, expectations) lives in the database instead.

use std::fmt::Write;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::Duration;

/// Upper bounds of the histogram buckets, in seconds: 1, 2.5 and 5 per decade from half a
/// millisecond (a database lookup) to 10 s, then a minute, 5 minutes and an hour (a message that
/// waited in a backlog). Each bucket is a Prometheus series per destination, and big
/// installations have thousands of destinations, so no finer: exact averages come from the sums.
pub const BUCKETS: [f64; 17] =
    [0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 60.0, 300.0, 3600.0];

/// How long things took: counts per bucket (the last one is "longer than all"), plus the total.
#[derive(Default)]
pub struct Histogram {
    counts: [AtomicU64; BUCKETS.len() + 1],
    sum_micros: AtomicU64,
}

impl Histogram {
    pub fn observe(&self, took: Duration) {
        let seconds = took.as_secs_f64();
        let bucket = BUCKETS.iter().position(|&b| seconds <= b).unwrap_or(BUCKETS.len());
        self.counts[bucket].fetch_add(1, Relaxed);
        self.sum_micros.fetch_add(took.as_micros() as u64, Relaxed);
    }

    pub fn count(&self) -> u64 {
        self.counts.iter().map(|c| c.load(Relaxed)).sum()
    }

    pub fn mean(&self) -> Option<Duration> {
        let count = self.count();
        (count > 0).then(|| Duration::from_micros(self.sum_micros.load(Relaxed) / count))
    }

    /// An estimate of the `q` quantile (0.5 = median), interpolated inside its bucket the way
    /// Prometheus's `histogram_quantile` does. Past the last bound it says the last bound.
    pub fn quantile(&self, q: f64) -> Option<Duration> {
        let counts: Vec<u64> = self.counts.iter().map(|c| c.load(Relaxed)).collect();
        let total: u64 = counts.iter().sum();
        if total == 0 {
            return None;
        }
        let rank = q.clamp(0.0, 1.0) * total as f64;
        let mut below = 0;
        for (i, &count) in counts.iter().enumerate() {
            if count > 0 && (below + count) as f64 >= rank {
                let Some(&upper) = BUCKETS.get(i) else { break };
                let lower = if i == 0 { 0.0 } else { BUCKETS[i - 1] };
                let within = (rank - below as f64) / count as f64;
                return Some(Duration::from_secs_f64(lower + (upper - lower) * within));
            }
            below += count;
        }
        Some(Duration::from_secs_f64(BUCKETS[BUCKETS.len() - 1]))
    }

    /// Prometheus text format: cumulative `_bucket` lines, `_sum` (seconds) and `_count`.
    fn write(&self, out: &mut String, name: &str, labels: &str) {
        let mut cumulative = 0;
        for (i, count) in self.counts.iter().enumerate() {
            cumulative += count.load(Relaxed);
            let le = BUCKETS.get(i).map_or("+Inf".to_string(), |b| b.to_string());
            let _ = writeln!(out, "{name}_bucket{{{labels},le=\"{le}\"}} {cumulative}");
        }
        let sum = self.sum_micros.load(Relaxed) as f64 / 1e6;
        let _ = writeln!(out, "{name}_sum{{{labels}}} {sum}");
        let _ = writeln!(out, "{name}_count{{{labels}}} {cumulative}");
    }
}

/// One interface's counts: what it received, and each destination's.
pub struct InterfaceStats {
    pub name: String,
    /// Messages stored and queued.
    pub received: AtomicU64,
    /// Stored as duplicates (seen within the dedupe window), not queued.
    pub duplicates: AtomicU64,
    /// Couldn't be stored (the sender was told to retry: AE, 503, the file stays).
    pub store_failures: AtomicU64,
    /// In `config.destinations()` order.
    pub destinations: Vec<std::sync::Arc<DestinationStats>>,
    /// Respond-mode HTTP: each request's time in the engine, from the transform's start to its
    /// answer (the HTTP server adds microseconds).
    pub requests: Histogram,
    pub request_errors: AtomicU64,
}

impl InterfaceStats {
    pub fn new(name: &str, destinations: impl IntoIterator<Item = String>) -> Self {
        Self {
            name: name.to_string(),
            received: AtomicU64::new(0),
            duplicates: AtomicU64::new(0),
            store_failures: AtomicU64::new(0),
            destinations: destinations.into_iter().map(|d| std::sync::Arc::new(DestinationStats::new(d))).collect(),
            requests: Histogram::default(),
            request_errors: AtomicU64::new(0),
        }
    }
}

/// One destination's counts and timings.
pub struct DestinationStats {
    pub name: String,
    pub sent: AtomicU64,
    pub filtered: AtomicU64,
    pub quarantined: AtomicU64,
    /// Temporary failures (each retry of a message counts).
    pub retries: AtomicU64,
    /// Its function, per message (without retries' waits).
    pub transform: Histogram,
    /// Each successful send: what the receiver takes (a slow receiver shows here first).
    pub send: Histogram,
    /// From receipt to sent: what a message's journey through the engine took, backlog included.
    pub latency: Histogram,
}

impl DestinationStats {
    fn new(name: String) -> Self {
        Self {
            name,
            sent: AtomicU64::new(0),
            filtered: AtomicU64::new(0),
            quarantined: AtomicU64::new(0),
            retries: AtomicU64::new(0),
            transform: Histogram::default(),
            send: Histogram::default(),
            latency: Histogram::default(),
        }
    }
}

/// Calls to one resource (a database or API in resources.yaml; `fetch` for URLs called
/// directly), from every interface.
#[derive(Default)]
pub struct CallStats {
    pub calls: Histogram,
    /// Calls that failed (any error: network, SQL, timeout). HTTP answers with an error status
    /// count as calls, not errors: the code decides what they mean.
    pub errors: AtomicU64,
}

impl CallStats {
    /// Time a call and count it.
    pub async fn time<T>(&self, call: impl Future<Output = anyhow::Result<T>>) -> anyhow::Result<T> {
        let started = std::time::Instant::now();
        let result = call.await;
        self.calls.observe(started.elapsed());
        if result.is_err() {
            add(&self.errors);
        }
        result
    }
}

/// Calls per resource, fixed at startup (resources.yaml doesn't change while the engine runs),
/// so counting a call takes no lock.
#[derive(Default)]
pub struct ResourceCalls {
    /// By (kind, name): kind is `db` or `api`.
    named: std::collections::BTreeMap<(&'static str, String), CallStats>,
    /// `fetch()` of a URL, not through a named API.
    fetch: CallStats,
}

impl ResourceCalls {
    pub fn new(databases: impl IntoIterator<Item = String>, apis: impl IntoIterator<Item = String>) -> Self {
        let named = databases.into_iter().map(|d| (("db", d), CallStats::default()));
        let named = named.chain(apis.into_iter().map(|a| (("api", a), CallStats::default()))).collect();
        Self { named, fetch: CallStats::default() }
    }

    /// An HTTP call's stats: the named API's for `api(name)`, else plain `fetch`'s. `None` for a
    /// name resources.yaml doesn't have (that call fails before it's made).
    pub fn http(&self, api: Option<&str>) -> Option<&CallStats> {
        match api {
            Some(name) => self.named.get(&("api", name.to_string())),
            None => Some(&self.fetch),
        }
    }

    /// A database's stats; `None` for a name resources.yaml doesn't have (that call fails anyway).
    pub fn db(&self, name: &str) -> Option<&CallStats> {
        self.named.get(&("db", name.to_string()))
    }

    /// Every resource's stats: (kind, name, stats), `fetch` last.
    pub fn all(&self) -> impl Iterator<Item = (&str, &str, &CallStats)> {
        let named = self.named.iter().map(|((kind, name), s)| (*kind, name.as_str(), s));
        named.chain(std::iter::once(("fetch", "fetch", &self.fetch)))
    }
}

pub fn add(counter: &AtomicU64) {
    counter.fetch_add(1, Relaxed);
}

pub fn get(counter: &AtomicU64) -> u64 {
    counter.load(Relaxed)
}

/// A label value for Prometheus: backslash, quote and newline escaped.
pub fn label(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
}

/// Builds a Prometheus text exposition: each metric's HELP and TYPE once, then its samples.
pub struct Exposition {
    out: String,
}

impl Exposition {
    pub fn new() -> Self {
        Self { out: String::with_capacity(64 * 1024) }
    }

    /// Start a metric family.
    pub fn family(&mut self, name: &str, kind: &str, help: &str) -> &mut Self {
        let _ = writeln!(self.out, "# HELP {name} {help}\n# TYPE {name} {kind}");
        self
    }

    pub fn sample(&mut self, name: &str, labels: &str, value: impl std::fmt::Display) -> &mut Self {
        let _ = if labels.is_empty() {
            writeln!(self.out, "{name} {value}")
        } else {
            writeln!(self.out, "{name}{{{labels}}} {value}")
        };
        self
    }

    pub fn histogram(&mut self, name: &str, labels: &str, histogram: &Histogram) -> &mut Self {
        histogram.write(&mut self.out, name, labels);
        self
    }

    pub fn finish(self) -> String {
        self.out
    }
}

impl Default for Exposition {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantiles_interpolate_inside_their_bucket() {
        let h = Histogram::default();
        assert_eq!(h.quantile(0.5), None);
        // 10 calls between 10 and 25 ms (one bucket), 10 between 50 and 100 ms.
        for _ in 0..10 {
            h.observe(Duration::from_millis(20));
            h.observe(Duration::from_millis(70));
        }
        let ms = |q| h.quantile(q).unwrap().as_secs_f64() * 1000.0;
        assert!((ms(0.5) - 25.0).abs() < 0.01, "median at the bucket boundary: {}", ms(0.5));
        assert!((ms(0.75) - 75.0).abs() < 0.01, "halfway into 50–100 ms: {}", ms(0.75));
        assert_eq!(h.mean(), Some(Duration::from_millis(45)));
        // Past the last bound: the last bound.
        h.observe(Duration::from_secs(7200));
        assert_eq!(h.quantile(1.0), Some(Duration::from_secs(3600)));
    }

    #[test]
    fn histograms_print_cumulative_buckets() {
        let h = Histogram::default();
        h.observe(Duration::from_micros(500));
        h.observe(Duration::from_millis(3));
        h.observe(Duration::from_secs(4000));
        let mut out = Exposition::new();
        out.histogram("x_seconds", "interface=\"a\"", &h);
        let text = out.finish();
        assert!(text.contains("x_seconds_bucket{interface=\"a\",le=\"0.001\"} 1\n"), "{text}");
        assert!(text.contains("x_seconds_bucket{interface=\"a\",le=\"0.005\"} 2\n"), "{text}");
        assert!(text.contains("x_seconds_bucket{interface=\"a\",le=\"3600\"} 2\n"), "{text}");
        assert!(text.contains("x_seconds_bucket{interface=\"a\",le=\"+Inf\"} 3\n"), "{text}");
        assert!(text.contains("x_seconds_count{interface=\"a\"} 3\n"), "{text}");
        assert!(text.contains("x_seconds_sum{interface=\"a\"} 4000.0035\n"), "{text}");
    }

    #[test]
    fn label_values_are_escaped() {
        assert_eq!(label("a\"b\\c\nd"), "a\\\"b\\\\c\\nd");
    }
}
