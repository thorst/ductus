//! `interface.yaml`: everything about an interface except its logic.

use crate::format::Format;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InterfaceConfig {
    pub name: String,
    #[serde(with = "serde_yaml_ng::with::singleton_map")]
    pub source: Source,
    /// TypeScript file exporting `transform(msg)`, relative to the interface folder.
    #[serde(default)]
    pub script: Option<PathBuf>,
    /// How incoming messages are parsed (default hl7). See `format.rs`.
    #[serde(default, deserialize_with = "crate::format::deserialize")]
    pub format: Format,
    /// How results are serialized, if different from `format`.
    #[serde(default, deserialize_with = "crate::format::deserialize_opt")]
    pub output: Option<Format>,
    #[serde(default)]
    pub ordering: Ordering,
    /// `shared` (default): runs in the engine's shared worker pool. `dedicated`: gets its own
    /// threads and V8 isolate, with `workers` parallel lanes.
    #[serde(default)]
    pub isolation: Isolation,
    /// Parallel workers, for `isolation: dedicated` only (default 4).
    #[serde(default)]
    pub workers: Option<u32>,
    /// Database connections of its own, for `isolation: dedicated` only: `{ patient_db: 4 }`
    /// gives this interface 4 connections to `patient_db` that no other interface uses, so a
    /// busy interface can't take every connection from the others (or the other way round).
    #[serde(default)]
    pub dedicated_pools: BTreeMap<String, u32>,
    /// `strict` (default): a message is flushed to disk before it's ACKed. `relaxed`: the ACK
    /// doesn't wait for the disk flush, for much higher throughput per connection; if the
    /// database server crashes or loses power, the last fraction of a second of ACKed
    /// messages can be lost. An engine crash alone never loses anything in either mode.
    #[serde(default)]
    pub durability: Durability,
    /// Where messages go. Not used by HTTP sources in `respond` mode (the response is the result).
    #[serde(default, with = "serde_yaml_ng::with::singleton_map_recursive")]
    pub destination: Option<Destination>,
    #[serde(default)]
    pub retry: Retry,
    /// Drop repeats of a message seen recently (e.g. a sender resending after a lost ACK).
    #[serde(default)]
    pub dedupe: Option<Dedupe>,
    /// Throttle delivery, e.g. for devices or vendor APIs that can't take bursts.
    #[serde(default)]
    pub rate_limit: Option<RateLimit>,
    /// Longest the interface's code may run without pausing, e.g. "5s" (the default) or
    /// "500ms". Waiting on `db()` or `fetch()` doesn't count: this catches endless loops,
    /// which would otherwise stall every interface sharing the thread. Past it, the code is
    /// stopped and the message quarantined; three in a row stop the interface.
    #[serde(default = "defaults::timeout", deserialize_with = "duration")]
    pub timeout: Duration,

    /// Folder the config was loaded from.
    #[serde(skip)]
    pub dir: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum Source {
    Mllp {
        #[serde(default = "defaults::host")]
        host: String,
        port: u16,
    },
    /// An HTTP endpoint. Several interfaces can share a port with different paths.
    Http {
        #[serde(default = "defaults::host")]
        host: String,
        port: u16,
        /// Route, with `{name}` parameters: `/patients/{mrn}`.
        path: String,
        #[serde(default = "defaults::post")]
        method: String,
        /// `queue` (default): store the body as a message and answer 202. `respond`: run the
        /// transform on the request and answer with its result (REST APIs, web pages).
        #[serde(default)]
        mode: HttpMode,
    },
    /// Files dropped in a local folder.
    File(FileSource),
    /// Files on an SFTP server (`server` names it in resources.yaml). Same options as `file`.
    Sftp(FileSource),
    /// No input: the transform runs on a timer and its result is the message.
    Schedule(Schedule),
}

/// Watch a folder for files. Each file (or each record in it, with `split: records`) becomes a
/// message. A file is moved or deleted only after everything in it is stored.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileSource {
    /// SFTP only: the server's name under `sftp:` in resources.yaml.
    #[serde(default)]
    pub server: Option<String>,
    /// Local folders are relative to the interface folder; SFTP folders to the login folder.
    pub dir: String,
    /// File names to pick up: `*` and `?` wildcards (default `*`). Names starting with `.` are
    /// never picked up (writers use them for files in progress).
    #[serde(default = "defaults::pattern")]
    pub pattern: String,
    /// How often to look (default 10s). A file is picked up once its size and time are unchanged
    /// between two looks, so a file still being written is never read half-done.
    #[serde(default = "defaults::poll", deserialize_with = "duration")]
    pub poll: Duration,
    /// Or: pick a file up only once a marker exists next to it (`marker: .done` waits for
    /// `a.csv.done` before reading `a.csv`). The marker is removed with the file.
    #[serde(default)]
    pub marker: Option<String>,
    /// After the file is stored: `move` it to `archive_dir` (default) or `delete` it.
    #[serde(default)]
    pub after: After,
    /// Where `after: move` puts files. Default: `archive` inside `dir`.
    #[serde(default)]
    pub archive_dir: Option<String>,
    /// `none` (default): one message per file. `records`: one message per record, read as a
    /// stream so files of any size work: HL7 batch files split per MSH, CSV per row (the header
    /// is kept with each row), fixed-length, text and JSON Lines per line.
    #[serde(default)]
    pub split: Split,
}

impl FileSource {
    /// Where processed files go (`after: move`).
    pub fn archive(&self) -> String {
        self.archive_dir.clone().unwrap_or_else(|| format!("{}/archive", self.dir.trim_end_matches('/')))
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum After {
    #[default]
    Move,
    Delete,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Split {
    #[default]
    None,
    Records,
}

/// When a schedule source runs: `every: 15m`, or `cron: "0 2 * * *"` (server local time).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Schedule {
    #[serde(default, deserialize_with = "duration_opt")]
    pub every: Option<Duration>,
    #[serde(default)]
    pub cron: Option<String>,
}

/// Problems with how a set of interfaces share network ports: two MLLP listeners on one port,
/// MLLP and HTTP on one port, or two interfaces handling the same HTTP method and path.
/// (Several HTTP interfaces may share a port with different routes.)
pub fn check_endpoints<'a>(configs: impl IntoIterator<Item = &'a InterfaceConfig>) -> Vec<String> {
    use std::collections::HashMap;
    let mut problems = Vec::new();
    let mut mllp: HashMap<u16, &str> = HashMap::new();
    let mut http_ports: HashMap<u16, &str> = HashMap::new();
    let mut routes: HashMap<(u16, String, String), &str> = HashMap::new();
    for c in configs {
        match &c.source {
            Source::Mllp { port, .. } => {
                let first = *mllp.entry(*port).or_insert(&c.name);
                if first != c.name {
                    problems.push(format!("port {port}: MLLP listeners for both `{first}` and `{}`", c.name));
                }
            }
            Source::Http { port, method, path, .. } => {
                http_ports.entry(*port).or_insert(&c.name);
                let key = (*port, method.to_ascii_uppercase(), path.clone());
                if let Some(other) = routes.insert(key, &c.name) {
                    problems.push(format!(
                        "port {port}: `{other}` and `{}` both handle {} {path}",
                        c.name,
                        method.to_ascii_uppercase()
                    ));
                }
            }
            Source::File(_) | Source::Sftp(_) | Source::Schedule(_) => {}
        }
    }
    for (port, m) in &mllp {
        if let Some(h) = http_ports.get(port) {
            problems.push(format!("port {port}: MLLP (`{m}`) and HTTP (`{h}`) can't share a port"));
        }
    }
    problems.sort();
    problems
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HttpMode {
    #[default]
    Queue,
    Respond,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum Destination {
    Mllp {
        host: String,
        port: u16,
        #[serde(default = "defaults::timeout_ms")]
        timeout_ms: u64,
        /// Wait for the receiver's ACK before moving on (default), or send and move on.
        #[serde(default)]
        ack: AckMode,
        /// What to do when the receiver answers AR (reject). AE (error) is always retried.
        #[serde(default)]
        on_reject: OnReject,
    },
    File {
        dir: PathBuf,
    },
    /// Write each message as a file on an SFTP server (`server` names it in resources.yaml).
    /// Files are written under a temporary name and renamed, so readers never see half a file.
    Sftp {
        server: String,
        dir: String,
    },
    /// POST (or PUT...) each message to a URL. 2xx = delivered; 408, 429, 5xx and network
    /// errors retry; other 4xx quarantine.
    Http {
        url: String,
        #[serde(default = "defaults::post")]
        method: String,
        #[serde(default)]
        headers: std::collections::BTreeMap<String, String>,
        #[serde(default = "defaults::timeout_ms")]
        timeout_ms: u64,
    },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Durability {
    #[default]
    Strict,
    Relaxed,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Isolation {
    #[default]
    Shared,
    Dedicated,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AckMode {
    #[default]
    Wait,
    None,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OnReject {
    #[default]
    Quarantine,
    Retry,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dedupe {
    /// HL7 paths whose values together identify a message. Default: MSH-10 (control ID).
    #[serde(default = "defaults::dedupe_keys")]
    pub keys: Vec<String>,
    /// How long a key is remembered, e.g. "30m", "24h", "7d".
    #[serde(default = "defaults::dedupe_window", deserialize_with = "duration")]
    pub window: Duration,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RateLimit {
    pub per_minute: u32,
    /// `average`: token bucket, allows bursts up to `burst` (default). `hard`: at most
    /// `per_minute` in any calendar minute.
    #[serde(default)]
    pub mode: RateMode,
    /// Bucket size for `average` mode. Default: same as per_minute.
    pub burst: Option<u32>,
    /// `wait` (default): hold messages until allowed. `drop`: discard what's over the limit.
    #[serde(default)]
    pub over_limit: OverLimit,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RateMode {
    #[default]
    Average,
    Hard,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OverLimit {
    #[default]
    Wait,
    Drop,
}

/// Parse durations like "500ms", "90s", "15m", "24h", "7d".
pub fn parse_duration(text: &str) -> Result<Duration> {
    let text = text.trim();
    let split = text.find(|c: char| !c.is_ascii_digit()).unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let n: u64 = number.parse().with_context(|| format!("invalid duration `{text}`"))?;
    let seconds = match unit.trim() {
        "ms" => return Ok(Duration::from_millis(n)),
        "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        "d" => n * 86_400,
        _ => bail!("invalid duration `{text}` (use e.g. 90s, 15m, 24h, 7d)"),
    };
    Ok(Duration::from_secs(seconds))
}

fn duration<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Duration, D::Error> {
    let text = String::deserialize(d)?;
    parse_duration(&text).map_err(serde::de::Error::custom)
}

fn duration_opt<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Option<Duration>, D::Error> {
    duration(d).map(Some)
}

/// Messages with the same key value are delivered in the order received.
/// With no key, the whole interface is one ordered lane.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ordering {
    /// HL7 path such as `PID-3.1`.
    pub key: Option<String>,
    #[serde(default = "defaults::partitions")]
    pub partitions: u32,
}

impl Default for Ordering {
    fn default() -> Self {
        Self { key: None, partitions: defaults::partitions() }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Retry {
    /// Give up and quarantine after this many attempts. Unset = retry until it succeeds.
    pub max_attempts: Option<u32>,
    #[serde(default = "defaults::backoff_ms")]
    pub initial_backoff_ms: u64,
    #[serde(default = "defaults::max_backoff_ms")]
    pub max_backoff_ms: u64,
}

impl Default for Retry {
    fn default() -> Self {
        Self {
            max_attempts: None,
            initial_backoff_ms: defaults::backoff_ms(),
            max_backoff_ms: defaults::max_backoff_ms(),
        }
    }
}

mod defaults {
    pub fn workers() -> u32 {
        4
    }
    pub fn post() -> String {
        "POST".into()
    }
    pub fn host() -> String {
        "0.0.0.0".into()
    }
    pub fn timeout_ms() -> u64 {
        10_000
    }
    pub fn partitions() -> u32 {
        64
    }
    pub fn backoff_ms() -> u64 {
        1_000
    }
    pub fn max_backoff_ms() -> u64 {
        60_000
    }
    pub fn dedupe_keys() -> Vec<String> {
        vec!["MSH-10".into()]
    }
    pub fn dedupe_window() -> std::time::Duration {
        std::time::Duration::from_secs(24 * 3600)
    }
    pub fn pattern() -> String {
        "*".into()
    }
    pub fn poll() -> std::time::Duration {
        std::time::Duration::from_secs(10)
    }
    pub fn timeout() -> std::time::Duration {
        std::time::Duration::from_secs(5)
    }
}

impl InterfaceConfig {
    /// Load `<dir>/interface.yaml` with no environment (any `${...}` reference is an error).
    pub fn load(dir: &Path) -> Result<Self> {
        Self::load_in(dir, &crate::Environment::default())
    }

    /// Load `<dir>/interface.yaml` with the environment's variables and secrets substituted.
    pub fn load_in(dir: &Path, env: &crate::Environment) -> Result<Self> {
        let file = dir.join("interface.yaml");
        let text = std::fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?;
        let mut value: serde_yaml_ng::Value =
            crate::env::parse_yaml(&text).with_context(|| format!("parsing {}", file.display()))?;
        env.substitute(&mut value).with_context(|| format!("in {}", file.display()))?;
        let mut config: Self = serde_yaml_ng::from_value(value).with_context(|| format!("in {}", file.display()))?;
        config.dir = dir.to_path_buf();
        config.validate().with_context(|| format!("validating {}", file.display()))?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        match (self.isolation, self.workers) {
            (Isolation::Shared, Some(_)) => {
                bail!("workers only applies with `isolation: dedicated` (shared interfaces use the engine's pool)")
            }
            (Isolation::Dedicated, Some(0)) => bail!("workers must be at least 1"),
            _ => {}
        }
        if !self.dedicated_pools.is_empty() && self.isolation == Isolation::Shared {
            bail!(
                "dedicated_pools needs `isolation: dedicated` (interfaces in the shared pool use the shared \
                 connection pools; set max_connections in resources.yaml)"
            );
        }
        if let Some((name, _)) = self.dedicated_pools.iter().find(|(_, n)| **n == 0) {
            bail!("dedicated_pools.{name} must be at least 1 connection");
        }
        if self.ordering.partitions == 0 {
            bail!("ordering.partitions must be at least 1");
        }
        if !(Duration::from_millis(10)..=Duration::from_secs(600)).contains(&self.timeout) {
            bail!("timeout must be between 10ms and 10m (other interfaces on the thread wait while code runs)");
        }
        if let Some(script) = self.script_path()
            && !script.is_file()
        {
            bail!("script not found: {}", script.display());
        }
        if let Some(d) = &self.dedupe
            && d.keys.is_empty()
        {
            bail!("dedupe.keys must list at least one key path");
        }
        let keys = self.ordering.key.iter().chain(self.dedupe.iter().flat_map(|d| d.keys.iter()));
        for key in keys {
            self.format.check_key_path(key).with_context(|| format!("key `{key}` (format {})", self.format.name()))?;
        }
        if matches!(self.source, Source::Mllp { .. }) && self.format != Format::Hl7 {
            bail!("an MLLP source carries HL7: `format: {}` needs another source type", self.format.name());
        }
        match (self.responds(), &self.destination) {
            (true, Some(_)) => {
                bail!("an HTTP source in `respond` mode answers with the transform's result; remove `destination`")
            }
            (true, None) if self.script.is_none() => bail!("an HTTP source in `respond` mode needs a `script`"),
            (false, None) => bail!("`destination` is required"),
            _ => {}
        }
        if let Source::Http { path, method, .. } = &self.source {
            if !path.starts_with('/') {
                bail!("HTTP path must start with /");
            }
            if !matches!(method.to_ascii_uppercase().as_str(), "GET" | "POST" | "PUT" | "PATCH" | "DELETE") {
                bail!("HTTP method must be GET, POST, PUT, PATCH or DELETE");
            }
        }
        if self.responds() && (self.ordering.key.is_some() || self.dedupe.is_some() || self.rate_limit.is_some()) {
            bail!("ordering, dedupe and rate_limit apply to queued messages, not `respond` mode");
        }
        match &self.source {
            Source::File(f) | Source::Sftp(f) => {
                let sftp = matches!(self.source, Source::Sftp(_));
                match (&f.server, sftp) {
                    (None, true) => bail!("an SFTP source needs `server` (a name under `sftp:` in resources.yaml)"),
                    (Some(_), false) => bail!("`server` is for SFTP sources; use `source: {{ sftp: ... }}`"),
                    _ => {}
                }
                if f.pattern.trim().is_empty() {
                    bail!("pattern can't be empty (use * for every file)");
                }
                if f.poll < Duration::from_millis(100) {
                    bail!("poll must be at least 100ms");
                }
                if f.marker.as_deref().is_some_and(|m| m.trim().is_empty()) {
                    bail!("marker can't be empty (e.g. `.done`)");
                }
                if f.split == Split::Records && self.format == Format::Xml {
                    bail!("`split: records` doesn't apply to XML; each XML file is one message");
                }
            }
            Source::Schedule(s) => {
                crate::schedule::Timing::new(s)?;
                if self.script.is_none() {
                    bail!("a schedule source needs a `script`: its transform produces the messages");
                }
                if self.ordering.key.is_some() || self.dedupe.is_some() {
                    bail!("ordering and dedupe don't apply to a schedule source (runs are one at a time)");
                }
            }
            Source::Mllp { .. } | Source::Http { .. } => {}
        }
        if let Some(Destination::Sftp { server, .. }) = &self.destination
            && server.trim().is_empty()
        {
            bail!("SFTP destination needs `server` (a name under `sftp:` in resources.yaml)");
        }
        if let Some(r) = &self.rate_limit {
            if r.per_minute == 0 {
                bail!("rate_limit.per_minute must be at least 1");
            }
            if r.burst == Some(0) {
                bail!("rate_limit.burst must be at least 1");
            }
        }
        Ok(())
    }

    /// Input and output formats as JSON, for the SDK: `{"input": {...}, "output": {...}}`,
    /// plus `"http": true` when `transform` receives HTTP requests.
    ///
    /// Two sources change what `transform` receives: a schedule passes its run details (JSON;
    /// `format` is then what the transform produces), and a CSV or fixed-length file split into
    /// records passes one record (`"record": true`) rather than a list of one.
    pub fn format_spec(&self) -> String {
        let output = self.output.as_ref().unwrap_or(&self.format);
        let mut input = serde_json::to_value(&self.format).expect("formats serialize");
        match &self.source {
            Source::Schedule(_) => input = serde_json::json!({ "kind": "json" }),
            Source::File(f) | Source::Sftp(f) if f.split == Split::Records => {
                if matches!(self.format, Format::Csv(_) | Format::Fixed(_)) {
                    input["record"] = true.into();
                }
            }
            _ => {}
        }
        serde_json::json!({ "input": input, "output": output, "http": self.responds() }).to_string()
    }

    /// The SFTP servers this interface uses (names in resources.yaml).
    pub fn sftp_servers(&self) -> Vec<&str> {
        let source = match &self.source {
            Source::Sftp(f) => f.server.as_deref(),
            _ => None,
        };
        let destination = match &self.destination {
            Some(Destination::Sftp { server, .. }) => Some(server.as_str()),
            _ => None,
        };
        source.into_iter().chain(destination).collect()
    }

    /// An HTTP source in `respond` mode: requests are answered directly, nothing is queued.
    pub fn responds(&self) -> bool {
        matches!(self.source, Source::Http { mode: HttpMode::Respond, .. })
    }

    pub fn script_path(&self) -> Option<PathBuf> {
        self.script.as_ref().map(|s| self.dir.join(s))
    }

    /// Number of independent workers (slots) processing this interface's lanes:
    /// one in the shared pool, `workers` when dedicated.
    pub fn slots(&self) -> u32 {
        match self.isolation {
            Isolation::Shared => 1,
            Isolation::Dedicated => self.workers.unwrap_or(defaults::workers()),
        }
    }

    /// Partitions are spread across slots round-robin. Each partition has exactly one owner,
    /// which is what keeps per-key ordering.
    pub fn partitions_for_worker(&self, worker: u32) -> Vec<i32> {
        let slots = self.slots();
        (0..self.partition_count()).filter(|p| p % slots == worker).map(|p| p as i32).collect()
    }

    /// Effective partition count: one lane when there's no ordering key.
    pub fn partition_count(&self) -> u32 {
        if self.ordering.key.is_some() { self.ordering.partitions } else { 1 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_config() {
        let yaml = r#"
name: adt
source: { mllp: { port: 6661 } }
script: transform.ts
ordering: { key: PID-3.1, partitions: 8 }
isolation: dedicated
workers: 2
destination: { mllp: { host: 127.0.0.1, port: 6662 } }
retry: { max_attempts: 3 }
"#;
        let c: InterfaceConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(c.name, "adt");
        assert!(matches!(c.source, Source::Mllp { port: 6661, .. }));
        assert_eq!(c.ordering.key.as_deref(), Some("PID-3.1"));
        assert_eq!(c.retry.max_attempts, Some(3));
        assert_eq!(c.partitions_for_worker(0), vec![0, 2, 4, 6]);
        assert_eq!(c.partitions_for_worker(1), vec![1, 3, 5, 7]);
    }

    #[test]
    fn script_timeout_has_a_safe_default_and_range() {
        let load = |extra: &str| -> InterfaceConfig {
            let dest = "destination: { file: { dir: o } }";
            serde_yaml_ng::from_str(&format!("name: a\nsource: {{ mllp: {{ port: 6661 }} }}\n{dest}\n{extra}")).unwrap()
        };
        assert_eq!(load("").timeout, Duration::from_secs(5));
        let c = load("timeout: 250ms");
        assert_eq!(c.timeout, Duration::from_millis(250));
        assert!(c.validate().is_ok());
        for bad in ["timeout: 5ms", "timeout: 1h"] {
            assert!(load(bad).validate().unwrap_err().to_string().contains("between 10ms and 10m"), "{bad}");
        }
    }

    #[test]
    fn http_sources_and_destinations() {
        let respond: InterfaceConfig = serde_yaml_ng::from_str(
            "name: api\nsource: { http: { port: 8080, path: '/patients/{mrn}', method: GET, mode: respond } }\nscript: t.ts\nformat: json\n",
        )
        .unwrap();
        assert!(respond.responds() && respond.destination.is_none());
        assert!(respond.format_spec().contains(r#""http":true"#));

        let queue: InterfaceConfig = serde_yaml_ng::from_str(
            "name: orders\nsource: { http: { port: 8080, path: /orders } }\nformat: json\ndestination: { http: { url: 'https://x/api', headers: { Authorization: Bearer t } } }\n",
        )
        .unwrap();
        assert!(!queue.responds());
        assert!(matches!(&queue.source, Source::Http { method, mode: HttpMode::Queue, .. } if method == "POST"));
        assert!(
            matches!(&queue.destination, Some(Destination::Http { headers, .. }) if headers["Authorization"] == "Bearer t")
        );
        assert!(queue.validate().is_ok());

        let mut bad = respond.clone();
        bad.script = None; // no script file in this test; the destination check comes first
        bad.destination = queue.destination.clone();
        assert!(bad.validate().unwrap_err().to_string().contains("remove `destination`"));
        let mut bad = queue.clone();
        bad.destination = None;
        assert!(bad.validate().unwrap_err().to_string().contains("required"));
    }

    #[test]
    fn finds_port_conflicts() {
        let load = |yaml: &str| -> InterfaceConfig { serde_yaml_ng::from_str(yaml).unwrap() };
        let dest = "destination: { file: { dir: o } }";
        let a = load(&format!("name: a\nsource: {{ mllp: {{ port: 6661 }} }}\n{dest}"));
        let b = load(&format!("name: b\nsource: {{ mllp: {{ port: 6661 }} }}\n{dest}"));
        let c = load(&format!("name: c\nsource: {{ http: {{ port: 8080, path: /x }} }}\nformat: json\n{dest}"));
        let d = load(&format!(
            "name: d\nsource: {{ http: {{ port: 8080, path: /x, method: post }} }}\nformat: json\n{dest}"
        ));
        let e = load(&format!("name: e\nsource: {{ http: {{ port: 8080, path: /y }} }}\nformat: json\n{dest}"));
        let f = load(&format!("name: f\nsource: {{ http: {{ port: 6661, path: /z }} }}\nformat: json\n{dest}"));
        assert!(check_endpoints([&c, &e]).is_empty(), "different paths may share a port");
        assert_eq!(
            check_endpoints([&a, &b, &c, &d, &f]),
            [
                "port 6661: MLLP (`a`) and HTTP (`f`) can't share a port",
                "port 6661: MLLP listeners for both `a` and `b`",
                "port 8080: `c` and `d` both handle POST /x",
            ]
        );
    }

    #[test]
    fn parses_reliability_options() {
        let yaml = r#"
name: device
source: { mllp: { port: 1 } }
destination: { mllp: { host: h, port: 2, ack: none, on_reject: retry } }
dedupe: { keys: [PID-3.1, EVN-2], window: 30m }
rate_limit: { per_minute: 100, burst: 500, over_limit: drop }
"#;
        let c: InterfaceConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert!(matches!(
            c.destination,
            Some(Destination::Mllp { ack: AckMode::None, on_reject: OnReject::Retry, .. })
        ));
        let d = c.dedupe.unwrap();
        assert_eq!(d.keys, ["PID-3.1", "EVN-2"]);
        assert_eq!(d.window, Duration::from_secs(1800));
        let r = c.rate_limit.unwrap();
        assert_eq!((r.per_minute, r.burst, r.mode, r.over_limit), (100, Some(500), RateMode::Average, OverLimit::Drop));

        let defaults: InterfaceConfig = serde_yaml_ng::from_str(
            "name: x\nsource: { mllp: { port: 1 } }\ndestination: { mllp: { host: h, port: 2 } }\ndedupe: {}\n",
        )
        .unwrap();
        assert!(matches!(
            defaults.destination,
            Some(Destination::Mllp { ack: AckMode::Wait, on_reject: OnReject::Quarantine, .. })
        ));
        assert_eq!(defaults.dedupe.unwrap().keys, ["MSH-10"]);
    }

    #[test]
    fn shared_interfaces_have_one_slot_with_every_lane() {
        let shared: InterfaceConfig = serde_yaml_ng::from_str(
            "name: x\nsource: { mllp: { port: 1 } }\nordering: { key: PID-3.1, partitions: 4 }\ndestination: { file: { dir: o } }\n",
        )
        .unwrap();
        assert_eq!(shared.isolation, Isolation::Shared);
        assert_eq!(shared.slots(), 1);
        assert_eq!(shared.partitions_for_worker(0), vec![0, 1, 2, 3]);

        let mut bad = shared.clone();
        bad.workers = Some(4);
        assert!(bad.validate().unwrap_err().to_string().contains("isolation: dedicated"));
    }

    #[test]
    fn dedicated_pools_need_a_dedicated_interface() {
        let mut c: InterfaceConfig = serde_yaml_ng::from_str(
            "name: x\nsource: { mllp: { port: 1 } }\ndestination: { file: { dir: o } }\ndedicated_pools: { patient_db: 4 }\n",
        )
        .unwrap();
        assert!(c.validate().unwrap_err().to_string().contains("dedicated_pools needs `isolation: dedicated`"));
        c.isolation = Isolation::Dedicated;
        assert!(c.validate().is_ok());
        c.dedicated_pools.insert("patient_db".into(), 0);
        assert!(c.validate().unwrap_err().to_string().contains("at least 1"));
    }

    #[test]
    fn parses_durations() {
        assert_eq!(parse_duration("90s").unwrap(), Duration::from_secs(90));
        assert_eq!(parse_duration("24h").unwrap(), Duration::from_secs(86_400));
        assert_eq!(parse_duration("7d").unwrap(), Duration::from_secs(604_800));
        assert!(parse_duration("5 weeks").is_err());
        assert!(parse_duration("h").is_err());
    }

    #[test]
    fn file_sftp_and_schedule_sources() {
        let load = |yaml: &str| -> Result<InterfaceConfig> {
            let c: InterfaceConfig = serde_yaml_ng::from_str(yaml)?;
            c.validate()?;
            Ok(c)
        };
        let dest = "destination: { file: { dir: out } }";
        let csv = load(&format!(
            "name: a\nsource: {{ file: {{ dir: in, pattern: '*.csv', poll: 2s, split: records }} }}\nformat: csv\n{dest}"
        ))
        .unwrap();
        let Source::File(f) = &csv.source else { panic!() };
        assert_eq!((f.poll, f.after, f.archive().as_str()), (Duration::from_secs(2), After::Move, "in/archive"));
        assert!(csv.format_spec().contains(r#""record":true"#), "a split CSV passes one record");
        assert!(csv.sftp_servers().is_empty());

        let whole = load(&format!("name: b\nsource: {{ file: {{ dir: in }} }}\nformat: csv\n{dest}")).unwrap();
        assert!(!whole.format_spec().contains("record"));

        let sftp = load(
            "name: c\nsource: { sftp: { server: lab, dir: /out, after: delete } }\ndestination: { sftp: { server: emr, dir: inbound } }\n",
        )
        .unwrap();
        assert_eq!(sftp.sftp_servers(), ["lab", "emr"]);

        let err = |yaml: &str| format!("{:#}", load(yaml).unwrap_err());
        assert!(err(&format!("name: d\nsource: {{ sftp: {{ dir: x }} }}\n{dest}")).contains("needs `server`"));
        assert!(err(&format!("name: d\nsource: {{ file: {{ dir: x, server: s }} }}\n{dest}")).contains("for SFTP"));
        assert!(
            err(&format!("name: d\nsource: {{ file: {{ dir: x, split: records }} }}\nformat: xml\n{dest}"))
                .contains("XML")
        );
        assert!(err(&format!("name: d\nsource: {{ file: {{ dir: x, poll: 10ms }} }}\n{dest}")).contains("100ms"));

        let mut nightly: InterfaceConfig = serde_yaml_ng::from_str(&format!(
            "name: e\nsource: {{ schedule: {{ cron: '0 2 * * *' }} }}\nscript: t.ts\nformat: csv\n{dest}"
        ))
        .unwrap();
        assert!(nightly.format_spec().contains(r#""input":{"kind":"json"}"#), "the transform gets the run details");
        nightly.script = None;
        assert!(nightly.validate().unwrap_err().to_string().contains("needs a `script`"));
        assert!(
            err(&format!("name: f\nsource: {{ schedule: {{ every: 1m, cron: '* * * * *' }} }}\n{dest}"))
                .contains("one of `every`")
        );
    }

    #[test]
    fn rejects_unknown_fields() {
        let yaml = "name: x\nsource: { mllp: { port: 1 } }\ndestination: { file: { dir: out } }\nbogus: 1\n";
        assert!(serde_yaml_ng::from_str::<InterfaceConfig>(yaml).is_err());
    }
}
