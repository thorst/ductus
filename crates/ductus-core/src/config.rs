//! `interface.yaml`: everything about an interface except its logic.

use crate::format::Format;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
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
    /// `strict` (default): a message is flushed to disk before it's ACKed. `relaxed`: the ACK
    /// doesn't wait for the disk flush, for much higher throughput per connection; if the
    /// database server crashes or loses power, the last fraction of a second of ACKed
    /// messages can be lost. An engine crash alone never loses anything in either mode.
    #[serde(default)]
    pub durability: Durability,
    #[serde(with = "serde_yaml_ng::with::singleton_map")]
    pub destination: Destination,
    #[serde(default)]
    pub retry: Retry,
    /// Drop repeats of a message seen recently (e.g. a sender resending after a lost ACK).
    #[serde(default)]
    pub dedupe: Option<Dedupe>,
    /// Throttle delivery, e.g. for devices or vendor APIs that can't take bursts.
    #[serde(default)]
    pub rate_limit: Option<RateLimit>,

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

/// Parse durations like "90s", "15m", "24h", "7d".
pub fn parse_duration(text: &str) -> Result<Duration> {
    let text = text.trim();
    let split = text.find(|c: char| !c.is_ascii_digit()).unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let n: u64 = number.parse().with_context(|| format!("invalid duration `{text}`"))?;
    let seconds = match unit.trim() {
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
        if self.ordering.partitions == 0 {
            bail!("ordering.partitions must be at least 1");
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

    /// Input and output formats as JSON, for the SDK: `{"input": {...}, "output": {...}}`.
    pub fn format_spec(&self) -> String {
        let output = self.output.as_ref().unwrap_or(&self.format);
        serde_json::json!({ "input": self.format, "output": output }).to_string()
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
    fn parses_reliability_options() {
        let yaml = r#"
name: device
source: { mllp: { port: 1 } }
destination: { mllp: { host: h, port: 2, ack: none, on_reject: retry } }
dedupe: { keys: [PID-3.1, EVN-2], window: 30m }
rate_limit: { per_minute: 100, burst: 500, over_limit: drop }
"#;
        let c: InterfaceConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert!(matches!(c.destination, Destination::Mllp { ack: AckMode::None, on_reject: OnReject::Retry, .. }));
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
            Destination::Mllp { ack: AckMode::Wait, on_reject: OnReject::Quarantine, .. }
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
    fn parses_durations() {
        assert_eq!(parse_duration("90s").unwrap(), Duration::from_secs(90));
        assert_eq!(parse_duration("24h").unwrap(), Duration::from_secs(86_400));
        assert_eq!(parse_duration("7d").unwrap(), Duration::from_secs(604_800));
        assert!(parse_duration("5 weeks").is_err());
        assert!(parse_duration("h").is_err());
    }

    #[test]
    fn rejects_unknown_fields() {
        let yaml = "name: x\nsource: { mllp: { port: 1 } }\ndestination: { file: { dir: out } }\nbogus: 1\n";
        assert!(serde_yaml_ng::from_str::<InterfaceConfig>(yaml).is_err());
    }
}
