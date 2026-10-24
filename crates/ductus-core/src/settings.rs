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
//!   tls: { certificate: ductus-api }  # HTTPS (a `certificates:` entry in resources.yaml)
//! retention: 7w       # how long messages are kept when an interface doesn't say (1w, 3w, 7w)
//! quarantined: keep   # quarantined messages past it: keep until resolved, or purge
//! auth:               # sign-in for people (all optional; see `AuthSettings`)
//!   session: { idle: 12h, max: 7d }
//!   password: { min_length: 12 }
//!   lockout: { attempts: 5, window: 15m, for: 15m }
//!   token_sign_in: true
//!   user_tokens: true
//!   ldap: ...         # sign-in with a directory (see `crate::ldap::LdapAuth`)
//! audit:              # the audit log
//!   keep: 6y          # at least 1y
//!   payload_reads: true
//!   payload_read_window: 10m
//! alerts: ...         # where alerts go and when (see `crate::alerts`); may use ${secret:...}
//! owners:             # owners that aren't users
//!   "Lab Systems": { email: lab-systems@hospital.org }
//! doctor:             # connection checks the engine runs by itself
//!   schedule: 6h      # or `off`; the first a minute after start
//!   mode: certificates  # certificates (DNS, TCP, TLS handshake: nothing logs in) | full
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
    /// How long messages are kept, for interfaces whose interface.yaml doesn't say.
    #[serde(default)]
    pub retention: crate::Retention,
    /// Quarantined messages past the retention: `keep` (until resolved) or `purge`, for
    /// interfaces whose interface.yaml doesn't say.
    #[serde(default)]
    pub quarantined: crate::Quarantined,
    /// How people sign in to the API and the web UI.
    #[serde(default)]
    pub auth: AuthSettings,
    /// The audit log: how long it is kept, and whether reading a payload is logged.
    #[serde(default)]
    pub audit: AuditSettings,
    /// Alerts (`crate::alerts`), as written: they may reference the environment's variables and
    /// secrets, so they are read per environment (`Settings::alerts`).
    #[serde(default)]
    alerts: Option<serde_yaml_ng::Value>,
    /// Owners that aren't users, and where alerts for them go (a team's mailing list).
    #[serde(default)]
    pub owners: std::collections::BTreeMap<String, crate::alerts::OwnerContact>,
    /// Connection checks the engine runs by itself (certificate expiry, by default).
    #[serde(default)]
    pub doctor: DoctorSettings,
}

/// `doctor:` in ductus.yaml: the engine's own connection checks.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DoctorSettings {
    /// How often: a duration (at least 1m), or `off`. The first run is a minute after the engine
    /// starts (plus up to a minute at random, so engines started together don't check together).
    #[serde(default = "default_doctor_schedule", deserialize_with = "off_or_duration")]
    pub schedule: Option<std::time::Duration>,
    #[serde(default)]
    pub mode: DoctorMode,
}

impl Default for DoctorSettings {
    fn default() -> Self {
        Self { schedule: default_doctor_schedule(), mode: DoctorMode::default() }
    }
}

fn default_doctor_schedule() -> Option<std::time::Duration> {
    Some(std::time::Duration::from_secs(6 * 3600))
}

/// What a check covers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DoctorMode {
    /// DNS, TCP and the TLS handshake of every connection with a certificate: nothing logs in,
    /// so no service account's log fills up and none is locked out.
    #[default]
    Certificates,
    /// Everything `ductus doctor` checks, logins included.
    Full,
}

impl DoctorMode {
    pub fn label(self) -> &'static str {
        match self {
            DoctorMode::Certificates => "certificates",
            DoctorMode::Full => "full",
        }
    }
}

/// `off`, or a duration.
fn off_or_duration<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<std::time::Duration>, D::Error> {
    let text = String::deserialize(d)?;
    if text.trim().eq_ignore_ascii_case("off") {
        return Ok(None);
    }
    crate::config::parse_duration(&text).map(Some).map_err(serde::de::Error::custom)
}

/// The audit log (`audit:` in ductus.yaml).
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditSettings {
    /// How long audit rows (and the record of message actions) are kept, at least a year: `6y`
    /// (the usual HIPAA documentation period; ask your compliance office for yours), or `2190d`.
    #[serde(default = "default_keep", deserialize_with = "keep_duration")]
    pub keep: std::time::Duration,
    /// Log who reads a message payload. Reading is allowed only to those with the `payloads`
    /// permission whatever this says.
    #[serde(default = "yes")]
    pub payload_reads: bool,
    /// One person reading the same message again within this long is one row (a screen that
    /// reloads itself would write a row each time). `0s`: every read is a row.
    #[serde(default = "default_read_window", deserialize_with = "crate::config::duration")]
    pub payload_read_window: std::time::Duration,
}

impl Default for AuditSettings {
    fn default() -> Self {
        Self { keep: default_keep(), payload_reads: true, payload_read_window: default_read_window() }
    }
}

/// Six years.
fn default_keep() -> std::time::Duration {
    std::time::Duration::from_secs(6 * 365 * 86_400)
}

fn default_read_window() -> std::time::Duration {
    std::time::Duration::from_secs(10 * 60)
}

/// `6y` (365 days each), or any duration (`2190d`).
pub(crate) fn keep_duration<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<std::time::Duration, D::Error> {
    let text = String::deserialize(d)?;
    let text = text.trim();
    if let Some(years) = text.strip_suffix('y') {
        let years: u64 =
            years.trim().parse().map_err(|_| serde::de::Error::custom(format!("invalid duration `{text}`")))?;
        return Ok(std::time::Duration::from_secs(years * 365 * 86_400));
    }
    crate::config::parse_duration(text).map_err(serde::de::Error::custom)
}

/// Sign-in for people (`auth:` in ductus.yaml).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthSettings {
    #[serde(default)]
    pub session: SessionSettings,
    #[serde(default)]
    pub password: PasswordSettings,
    #[serde(default)]
    pub lockout: LockoutSettings,
    /// The sign-in page also accepts an API token. Turn it off once everyone has an account
    /// (`ductus user` on the server always works).
    #[serde(default = "yes")]
    pub token_sign_in: bool,
    /// Users may make API tokens of their own (never above their own role).
    #[serde(default = "yes")]
    pub user_tokens: bool,
    /// Sign-in with a directory (Active Directory, OpenLDAP), besides local accounts.
    #[serde(default)]
    pub ldap: Option<crate::ldap::LdapAuth>,
}

impl Default for AuthSettings {
    fn default() -> Self {
        Self {
            session: SessionSettings::default(),
            password: PasswordSettings::default(),
            lockout: LockoutSettings::default(),
            token_sign_in: true,
            user_tokens: true,
            ldap: None,
        }
    }
}

fn yes() -> bool {
    true
}

/// How long a browser session lasts.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionSettings {
    /// Ends after this long unused (a shift, with room to spare).
    #[serde(default = "default_idle", deserialize_with = "crate::config::duration")]
    pub idle: std::time::Duration,
    /// Ends this long after sign-in, however it is used.
    #[serde(default = "default_max", deserialize_with = "crate::config::duration")]
    pub max: std::time::Duration,
}

impl Default for SessionSettings {
    fn default() -> Self {
        Self { idle: default_idle(), max: default_max() }
    }
}

fn default_idle() -> std::time::Duration {
    std::time::Duration::from_secs(12 * 3600)
}

fn default_max() -> std::time::Duration {
    std::time::Duration::from_secs(7 * 86_400)
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PasswordSettings {
    /// The shortest password accepted (never below 8).
    #[serde(default = "default_min_length")]
    pub min_length: usize,
}

impl Default for PasswordSettings {
    fn default() -> Self {
        Self { min_length: default_min_length() }
    }
}

fn default_min_length() -> usize {
    12
}

/// Wrong passwords for one username: after `attempts` within `window`, sign-in as that user is
/// refused for `for`.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockoutSettings {
    #[serde(default = "default_attempts")]
    pub attempts: u32,
    #[serde(default = "default_window", deserialize_with = "crate::config::duration")]
    pub window: std::time::Duration,
    #[serde(rename = "for", default = "default_window", deserialize_with = "crate::config::duration")]
    pub lock_for: std::time::Duration,
}

impl Default for LockoutSettings {
    fn default() -> Self {
        Self { attempts: default_attempts(), window: default_window(), lock_for: default_window() }
    }
}

fn default_attempts() -> u32 {
    5
}

fn default_window() -> std::time::Duration {
    std::time::Duration::from_secs(15 * 60)
}

/// The REST API the engine serves (`/api/v1`).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiSettings {
    /// `host:port`, or `off`. Defaults to this machine only: opening it to the network is a
    /// choice (every call still needs a token).
    #[serde(default = "default_api_listen")]
    pub listen: String,
    /// HTTPS only, with this `certificates:` entry (needed for the web UI anywhere but this
    /// machine: its session cookie is `Secure`).
    #[serde(default)]
    pub tls: Option<crate::tls::ListenTls>,
    /// Only these addresses, subnets and host names may connect (default: anyone who can reach
    /// `listen`). List 127.0.0.1 too, or commands on this machine (`ductus status`) are refused.
    #[serde(default)]
    pub allow: Option<crate::allow::Allow>,
}

impl Default for ApiSettings {
    fn default() -> Self {
        Self { listen: default_api_listen(), tls: None, allow: None }
    }
}

/// Whether `host` is only reachable from this machine (`127.0.0.1`, `::1`, `localhost`).
pub fn loopback(host: &str) -> bool {
    match host.parse::<std::net::IpAddr>() {
        Ok(ip) => ip.is_loopback(),
        Err(_) => host.eq_ignore_ascii_case("localhost"),
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
        // `${...}` references (alerts only) must not break the YAML: parsed protected.
        let value = crate::env::parse_yaml(&text).with_context(|| format!("parsing {}", file.display()))?;
        if value.is_null() {
            return Ok(Self::default());
        }
        let settings: Self = serde_yaml_ng::from_value(value).with_context(|| format!("in {}", file.display()))?;
        settings.api.address(None).with_context(|| format!("in {}", file.display()))?;
        if let Some(allow) = &settings.api.allow {
            allow.rules().with_context(|| format!("in {}: api.allow", file.display()))?;
        }
        if settings.isolates.heap_mb < 64 {
            bail!("in {}: isolates.heap_mb must be at least 64 (the SDK alone needs a few MB)", file.display());
        }
        settings.auth.check().with_context(|| format!("in {}", file.display()))?;
        if settings.audit.keep < std::time::Duration::from_secs(365 * 86_400) {
            bail!(
                "in {}: audit.keep must be at least a year (`1y`): the log is the record of who did what",
                file.display()
            );
        }
        if settings.doctor.schedule.is_some_and(|s| s < std::time::Duration::from_secs(60)) {
            bail!("in {}: doctor.schedule must be at least 1m (or `off`)", file.display());
        }
        Ok(settings)
    }

    /// The alerts, with the environment's variables and secrets filled in and checked; `None`
    /// when ductus.yaml has no `alerts:`.
    pub fn alerts(&self, env: &crate::Environment) -> Result<Option<crate::alerts::AlertSettings>> {
        let Some(raw) = &self.alerts else { return Ok(None) };
        let mut value = raw.clone();
        env.substitute(&mut value).with_context(|| format!("in {FILE}: alerts"))?;
        let alerts: crate::alerts::AlertSettings =
            serde_yaml_ng::from_value(value).with_context(|| format!("in {FILE}: alerts"))?;
        alerts.check().with_context(|| format!("in {FILE}"))?;
        Ok(Some(alerts))
    }
}

impl AuthSettings {
    fn check(&self) -> Result<()> {
        if self.session.idle.is_zero() || self.session.max < self.session.idle {
            bail!("auth.session: `idle` must be above zero and no longer than `max`");
        }
        if self.password.min_length < crate::password::MIN_LENGTH_FLOOR {
            bail!("auth.password.min_length must be at least {}", crate::password::MIN_LENGTH_FLOOR);
        }
        if self.lockout.attempts == 0 {
            bail!("auth.lockout.attempts must be at least 1");
        }
        if let Some(ldap) = &self.ldap {
            ldap.check().context("auth.ldap")?;
        }
        Ok(())
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
        assert_eq!(s.retention.days(), 49, "the default");
        assert_eq!(s.quarantined, crate::Quarantined::Keep, "the default");
        std::fs::write(dir.path().join(FILE), "retention: 3w\nquarantined: purge\n").unwrap();
        let s = Settings::load(dir.path()).unwrap();
        assert_eq!((s.retention.days(), s.quarantined), (21, crate::Quarantined::Purge));
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

        std::fs::write(dir.path().join(FILE), "api: { listen: '0.0.0.0:7700', tls: { certificate: ductus-api } }\n")
            .unwrap();
        let api = Settings::load(dir.path()).unwrap().api;
        assert_eq!(api.tls.map(|t| t.certificate).as_deref(), Some("ductus-api"));
        std::fs::write(dir.path().join(FILE), "api: { listen: '0.0.0.0:7700', allow: [127.0.0.1, 10.1.0.0/16] }\n")
            .unwrap();
        assert_eq!(Settings::load(dir.path()).unwrap().api.allow.unwrap().summary(), "127.0.0.1, 10.1.0.0/16");
        std::fs::write(dir.path().join(FILE), "api: { allow: [10.1.0.0/40] }\n").unwrap();
        let err = format!("{:#}", Settings::load(dir.path()).unwrap_err());
        assert!(err.contains("api.allow") && err.contains("prefix"), "{err}");
        assert!(loopback("127.0.0.1") && loopback("::1") && loopback("LOCALHOST"));
        assert!(!loopback("0.0.0.0") && !loopback("10.1.2.3") && !loopback("engine.hospital.org"));
    }

    #[test]
    fn auth_has_safe_defaults_and_checks_itself() {
        let dir = tempfile::tempdir().unwrap();
        let a = Settings::load(dir.path()).unwrap().auth;
        assert_eq!((a.session.idle.as_secs(), a.session.max.as_secs()), (12 * 3600, 7 * 86_400));
        assert_eq!((a.password.min_length, a.lockout.attempts, a.lockout.lock_for.as_secs()), (12, 5, 900));
        assert!(a.token_sign_in && a.user_tokens);
        std::fs::write(
            dir.path().join(FILE),
            "auth: { session: { idle: 1h, max: 2d }, lockout: { attempts: 3, for: 1m }, token_sign_in: false }
",
        )
        .unwrap();
        let a = Settings::load(dir.path()).unwrap().auth;
        assert_eq!((a.session.idle.as_secs(), a.lockout.attempts, a.lockout.lock_for.as_secs()), (3600, 3, 60));
        assert!(!a.token_sign_in);
        for bad in ["auth: { session: { idle: 2d, max: 1d } }", "auth: { password: { min_length: 4 } }"] {
            std::fs::write(dir.path().join(FILE), bad).unwrap();
            assert!(Settings::load(dir.path()).is_err(), "{bad}");
        }
        assert!(a.ldap.is_none());
    }

    #[test]
    fn directory_sign_in_is_read_from_auth() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(FILE),
            "auth:\n  ldap:\n    resource: corp\n    user_base: ou=people,dc=example,dc=org\n    user_filter: (uid={username})\n    groups:\n      admin: cn=admins,ou=groups,dc=example,dc=org\n    nested: true\n    refresh: 5m\n",
        )
        .unwrap();
        let l = Settings::load(dir.path()).unwrap().auth.ldap.unwrap();
        assert_eq!((l.resource.as_str(), l.filter_for("anna").as_str()), ("corp", "(uid=anna)"));
        assert_eq!((l.nested, l.refresh.as_secs(), l.groups.admin.len()), (true, 300, 1));
        std::fs::write(
            dir.path().join(FILE),
            "auth:\n  ldap:\n    resource: corp\n    user_base: dc=x\n    groups: {}\n",
        )
        .unwrap();
        let err = format!("{:#}", Settings::load(dir.path()).unwrap_err());
        assert!(err.contains("auth.ldap") && err.contains("at least one group"), "{err}");
    }

    #[test]
    fn the_audit_log_is_kept_six_years_unless_told_otherwise() {
        let dir = tempfile::tempdir().unwrap();
        let a = Settings::load(dir.path()).unwrap().audit;
        assert_eq!((a.keep.as_secs() / 86_400, a.payload_reads, a.payload_read_window.as_secs()), (2190, true, 600));
        std::fs::write(dir.path().join(FILE), "audit: { keep: 7y, payload_reads: false, payload_read_window: 0s }")
            .unwrap();
        let a = Settings::load(dir.path()).unwrap().audit;
        assert_eq!((a.keep.as_secs() / 86_400, a.payload_reads, a.payload_read_window.as_secs()), (2555, false, 0));
        std::fs::write(dir.path().join(FILE), "audit: { keep: 400d }").unwrap();
        assert_eq!(Settings::load(dir.path()).unwrap().audit.keep.as_secs() / 86_400, 400);
        for bad in ["audit: { keep: 90d }", "audit: { keep: 6months }", "audit: { keep: y }"] {
            std::fs::write(dir.path().join(FILE), bad).unwrap();
            assert!(Settings::load(dir.path()).is_err(), "{bad}");
        }
    }

    #[test]
    fn alerts_are_read_per_environment() {
        let dir = tempfile::tempdir().unwrap();
        let env = crate::Environment::with_variables("prod", &[("HOOK", "https://hooks.example.org/x"), ("HB", "300")]);
        assert!(Settings::load(dir.path()).unwrap().alerts(&env).unwrap().is_none());
        std::fs::write(
            dir.path().join(FILE),
            "alerts:\n  channels: { chat: { webhook: { url: ${HOOK}, format: teams } } }\n  rules: [{ when: [error], severity: critical, to: [chat] }]\nowners:\n  Lab: { email: lab@example.org }\n",
        )
        .unwrap();
        let s = Settings::load(dir.path()).unwrap();
        assert_eq!(s.owners["Lab"].email, ["lab@example.org"]);
        let a = s.alerts(&env).unwrap().unwrap();
        assert_eq!(a.channels["chat"].webhook.as_ref().unwrap().url, "https://hooks.example.org/x");
        let err = format!("{:#}", s.alerts(&crate::Environment::with_variables("dev", &[])).unwrap_err());
        assert!(err.contains("missing in environment `dev`: HOOK"), "{err}");
        std::fs::write(
            dir.path().join(FILE),
            "alerts:\n  rules: [{ when: [error], severity: critical, to: [nope] }]\n",
        )
        .unwrap();
        let err = format!("{:#}", Settings::load(dir.path()).unwrap().alerts(&env).unwrap_err());
        assert!(err.contains("no channel named `nope`"), "{err}");
    }

    #[test]
    fn the_doctor_checks_certificates_every_six_hours_unless_told_otherwise() {
        let dir = tempfile::tempdir().unwrap();
        let d = Settings::load(dir.path()).unwrap().doctor;
        assert_eq!((d.schedule.map(|s| s.as_secs()), d.mode), (Some(6 * 3600), DoctorMode::Certificates));
        std::fs::write(dir.path().join(FILE), "doctor: { schedule: 30m, mode: full }").unwrap();
        let d = Settings::load(dir.path()).unwrap().doctor;
        assert_eq!((d.schedule.map(|s| s.as_secs()), d.mode), (Some(1800), DoctorMode::Full));
        std::fs::write(dir.path().join(FILE), "doctor: { schedule: off }").unwrap();
        assert_eq!(Settings::load(dir.path()).unwrap().doctor.schedule, None);
        for bad in ["doctor: { schedule: 10s }", "doctor: { mode: logins }", "doctor: { schedule: sometimes }"] {
            std::fs::write(dir.path().join(FILE), bad).unwrap();
            assert!(Settings::load(dir.path()).is_err(), "{bad}");
        }
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
