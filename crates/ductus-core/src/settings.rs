//! `ductus.yaml`: project settings (optional; everything has a default).
//!
//! ```yaml
//! ports:              # ranges `ductus new interface` picks free ports from
//!   mllp: 6661-6699
//!   http: 8080-8099
//! isolates:
//!   heap_mb: 512      # memory limit of each V8 isolate (one per pool thread and per dedicated worker)
//! require_about: true # `ductus validate` fails when an interface doesn't name the systems it connects
//! api:
//!   listen: 127.0.0.1:7700  # the REST API (`/api/v1`); `off` to run without it
//! ```

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::Path;

pub const FILE: &str = "ductus.yaml";

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    #[serde(default)]
    pub ports: PortRanges,
    #[serde(default)]
    pub isolates: Isolates,
    /// Interfaces must name the application or vendor on each side (`about:`), so `ductus
    /// impact` is never missing one: `ductus validate` reports gaps as problems, not warnings.
    #[serde(default)]
    pub require_about: bool,
    #[serde(default)]
    pub api: ApiSettings,
}

/// The REST API the engine serves (`/api/v1`).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiSettings {
    /// `host:port`, or `off`. Defaults to this machine only: opening it to the network is a
    /// choice (every call still needs a token).
    #[serde(default = "default_api_listen")]
    pub listen: String,
}

impl Default for ApiSettings {
    fn default() -> Self {
        Self { listen: default_api_listen() }
    }
}

pub const DEFAULT_API_LISTEN: &str = "127.0.0.1:7700";

fn default_api_listen() -> String {
    DEFAULT_API_LISTEN.into()
}

impl ApiSettings {
    /// Where to listen, or `None` when it's `off`. `override_` (a command-line option) wins.
    pub fn address(&self, override_: Option<&str>) -> Result<Option<(String, u16)>> {
        let listen = override_.unwrap_or(&self.listen).trim();
        if listen.eq_ignore_ascii_case("off") {
            return Ok(None);
        }
        match listen.rsplit_once(':').map(|(h, p)| (h.trim_matches(['[', ']']), p.parse::<u16>())) {
            Some((host, Ok(port))) if !host.is_empty() && port > 0 => Ok(Some((host.to_string(), port))),
            _ => bail!("api.listen `{listen}`: write it like 127.0.0.1:7700 (or `off`)"),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Isolates {
    /// Heap limit of each V8 isolate, in MB. Interface code that fills it is stopped (the
    /// process keeps running). Every pool thread and every dedicated worker has one isolate.
    #[serde(default = "default_heap_mb")]
    pub heap_mb: usize,
}

impl Default for Isolates {
    fn default() -> Self {
        Self { heap_mb: default_heap_mb() }
    }
}

fn default_heap_mb() -> usize {
    512
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortRanges {
    #[serde(default = "default_mllp")]
    pub mllp: PortRange,
    #[serde(default = "default_http")]
    pub http: PortRange,
}

impl Default for PortRanges {
    fn default() -> Self {
        Self { mllp: default_mllp(), http: default_http() }
    }
}

fn default_mllp() -> PortRange {
    PortRange { first: 6661, last: 6699 }
}

fn default_http() -> PortRange {
    PortRange { first: 8080, last: 8099 }
}

/// Inclusive, written `6661-6699`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortRange {
    pub first: u16,
    pub last: u16,
}

impl std::fmt::Display for PortRange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}-{}", self.first, self.last)
    }
}

impl<'de> Deserialize<'de> for PortRange {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        let parsed = text.split_once('-').and_then(|(a, b)| Some((a.trim().parse().ok()?, b.trim().parse().ok()?)));
        match parsed {
            Some((first, last)) if first <= last && first > 0 => Ok(Self { first, last }),
            _ => Err(serde::de::Error::custom(format!("port range `{text}`: write it like 6661-6699"))),
        }
    }
}

impl Settings {
    /// `<root>/ductus.yaml`, or the defaults if there's none.
    pub fn load(root: &Path) -> Result<Self> {
        let file = root.join(FILE);
        if !file.is_file() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?;
        let settings: Self = serde_yaml_ng::from_str(&text).with_context(|| format!("in {}", file.display()))?;
        settings.api.address(None).with_context(|| format!("in {}", file.display()))?;
        if settings.isolates.heap_mb < 64 {
            bail!("in {}: isolates.heap_mb must be at least 64 (the SDK alone needs a few MB)", file.display());
        }
        Ok(settings)
    }
}

/// The project's environments: the files in `environments/`, or just `dev` if there are none.
pub fn environments(root: &Path) -> Result<Vec<String>> {
    let dir = root.join("environments");
    if !dir.is_dir() {
        return Ok(vec!["dev".into()]);
    }
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| {
            let path = e.ok()?.path();
            if path.extension()? != "yaml" {
                return None;
            }
            Some(path.file_stem()?.to_string_lossy().into_owned())
        })
        .collect();
    names.sort();
    if names.is_empty() {
        bail!("{} has no <env>.yaml files", dir.display());
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_port_ranges_with_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let s = Settings::load(dir.path()).unwrap();
        assert_eq!((s.ports.mllp.to_string(), s.ports.http.to_string()), ("6661-6699".into(), "8080-8099".into()));

        std::fs::write(dir.path().join(FILE), "ports:\n  mllp: 7000 - 7010\n").unwrap();
        let s = Settings::load(dir.path()).unwrap();
        assert_eq!((s.ports.mllp, s.ports.http), (PortRange { first: 7000, last: 7010 }, default_http()));

        std::fs::write(dir.path().join(FILE), "ports:\n  http: 9000-8000\n").unwrap();
        let err = format!("{:#}", Settings::load(dir.path()).unwrap_err());
        assert!(err.contains("write it like 6661-6699"), "{err}");
    }

    #[test]
    fn loads_the_isolate_heap_limit() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Settings::load(dir.path()).unwrap().isolates.heap_mb, 512);
        assert!(!Settings::load(dir.path()).unwrap().require_about);
        std::fs::write(dir.path().join(FILE), "isolates: { heap_mb: 2048 }\nrequire_about: true\n").unwrap();
        let s = Settings::load(dir.path()).unwrap();
        assert_eq!((s.isolates.heap_mb, s.require_about), (2048, true));
        std::fs::write(dir.path().join(FILE), "isolates: { heap_mb: 8 }\n").unwrap();
        let err = format!("{:#}", Settings::load(dir.path()).unwrap_err());
        assert!(err.contains("at least 64"), "{err}");
    }

    #[test]
    fn reads_where_the_api_listens() {
        let dir = tempfile::tempdir().unwrap();
        let api = Settings::load(dir.path()).unwrap().api;
        assert_eq!(api.address(None).unwrap(), Some(("127.0.0.1".into(), 7700)));
        assert_eq!(api.address(Some("0.0.0.0:9000")).unwrap(), Some(("0.0.0.0".into(), 9000)));
        assert_eq!(api.address(Some("[::1]:9000")).unwrap(), Some(("::1".into(), 9000)));
        assert_eq!(api.address(Some("OFF")).unwrap(), None);
        std::fs::write(
            dir.path().join(FILE),
            "api: { listen: off }
",
        )
        .unwrap();
        assert_eq!(Settings::load(dir.path()).unwrap().api.address(None).unwrap(), None);
        std::fs::write(
            dir.path().join(FILE),
            "api: { listen: localhost }
",
        )
        .unwrap();
        let err = format!("{:#}", Settings::load(dir.path()).unwrap_err());
        assert!(err.contains("write it like 127.0.0.1:7700"), "{err}");
    }

    #[test]
    fn lists_environments() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(environments(dir.path()).unwrap(), ["dev"]);
        std::fs::create_dir(dir.path().join("environments")).unwrap();
        for f in ["prod.yaml", "dev.yaml", "notes.txt"] {
            std::fs::write(dir.path().join("environments").join(f), "").unwrap();
        }
        assert_eq!(environments(dir.path()).unwrap(), ["dev", "prod"]);
    }
}
