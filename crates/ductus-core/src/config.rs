//! `interface.yaml`: everything about an interface except its logic.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InterfaceConfig {
    pub name: String,
    #[serde(with = "serde_yaml_ng::with::singleton_map")]
    pub source: Source,
    /// TypeScript file exporting `transform(msg)`, relative to the interface folder.
    #[serde(default)]
    pub script: Option<PathBuf>,
    #[serde(default)]
    pub ordering: Ordering,
    #[serde(default = "defaults::workers")]
    pub workers: u32,
    #[serde(with = "serde_yaml_ng::with::singleton_map")]
    pub destination: Destination,
    #[serde(default)]
    pub retry: Retry,

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
    },
    File {
        dir: PathBuf,
    },
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
    pub fn workers() -> u32 { 4 }
    pub fn host() -> String { "0.0.0.0".into() }
    pub fn timeout_ms() -> u64 { 10_000 }
    pub fn partitions() -> u32 { 64 }
    pub fn backoff_ms() -> u64 { 1_000 }
    pub fn max_backoff_ms() -> u64 { 60_000 }
}

impl InterfaceConfig {
    /// Load `<dir>/interface.yaml`.
    pub fn load(dir: &Path) -> Result<Self> {
        let file = dir.join("interface.yaml");
        let text = std::fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?;
        let mut config: Self =
            serde_yaml_ng::from_str(&text).with_context(|| format!("parsing {}", file.display()))?;
        config.dir = dir.to_path_buf();
        config.validate().with_context(|| format!("validating {}", file.display()))?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        if self.workers == 0 {
            bail!("workers must be at least 1");
        }
        if self.ordering.partitions == 0 {
            bail!("ordering.partitions must be at least 1");
        }
        if let Some(script) = self.script_path() {
            if !script.is_file() {
                bail!("script not found: {}", script.display());
            }
        }
        Ok(())
    }

    pub fn script_path(&self) -> Option<PathBuf> {
        self.script.as_ref().map(|s| self.dir.join(s))
    }

    /// Partitions are spread across workers round-robin. Each partition has exactly one owner,
    /// which is what keeps per-key ordering.
    pub fn partitions_for_worker(&self, worker: u32) -> Vec<i32> {
        (0..self.partition_count())
            .filter(|p| p % self.workers == worker)
            .map(|p| p as i32)
            .collect()
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
    fn rejects_unknown_fields() {
        let yaml = "name: x\nsource: { mllp: { port: 1 } }\ndestination: { file: { dir: out } }\nbogus: 1\n";
        assert!(serde_yaml_ng::from_str::<InterfaceConfig>(yaml).is_err());
    }
}
