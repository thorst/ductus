//! `resources.yaml`: things many interfaces share, defined once.
//!
//! ```yaml
//! databases:
//!   patient_db:
//!     postgres:
//!       host: ${PATIENT_DB_HOST}
//!       database: patients
//!       user: ductus
//!       password: ${secret:PATIENT_DB_PASSWORD}
//!       max_connections: 10
//!   billing_db:
//!     sqlserver:
//!       host: billing.hospital.local
//!       instance: PROD                           # optional named instance
//!       database: Billing
//!       user: ductus
//!       password: ${secret:BILLING_DB_PASSWORD}
//!       trust_server_certificate: true           # self-signed certificate (trusted network only)
//! sftp:
//!   lab_server:
//!     host: sftp.lab.example.org
//!     user: hospital
//!     private_key: ${secret:LAB_SFTP_KEY}     # or password: ${secret:...}
//!     host_key: SHA256:Xm3H1...                # the server's key fingerprint (required)
//!     vendor: Quest                            # application / vendor: for `ductus impact`
//! apis:
//!   empi:
//!     url: https://empi.hospital.local/api/v2
//!     headers: { Authorization: "Bearer ${secret:EMPI_TOKEN}" }
//!     timeout: 10s                             # default 30s
//!     application: EMPI
//!     vendor: Rhapsody
//! ```
//!
//! Interface code refers to them by name (`db("patient_db")`, `api("empi")`), so no connection
//! details or credentials ever appear in interface code, and each environment supplies its own
//! values. Every entry may say which `application` and `vendor` it belongs to; interfaces that
//! use it are then found by `ductus impact`.

use crate::InterfaceConfig;
use crate::about::System;
use crate::env::Environment;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resources {
    #[serde(default, with = "serde_yaml_ng::with::singleton_map_recursive")]
    pub databases: BTreeMap<String, Database>,
    #[serde(default)]
    pub sftp: BTreeMap<String, SftpServer>,
    #[serde(default)]
    pub apis: BTreeMap<String, Api>,
}

/// The kinds of things resources.yaml defines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Database,
    Api,
    Sftp,
}

impl Kind {
    /// How it's named in messages and reports: `database`, `api`, `sftp server`.
    pub fn label(self) -> &'static str {
        match self {
            Kind::Database => "database",
            Kind::Api => "api",
            Kind::Sftp => "sftp server",
        }
    }
}

/// An HTTP API used through `api(name)` in code and `http: { api: name }` destinations. The
/// engine adds the URL and headers, so tokens never pass through interface code.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Api {
    /// Base URL; paths in code and destinations go under it.
    pub url: String,
    /// Sent with every request (e.g. `Authorization: "Bearer ${secret:TOKEN}"`). Headers the
    /// code or destination sets itself win.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// Default for calls that don't set their own (default 30s from code, 10s for destinations).
    #[serde(default, deserialize_with = "crate::config::duration_opt")]
    pub timeout: Option<Duration>,
    #[serde(default)]
    pub application: Option<String>,
    #[serde(default)]
    pub vendor: Option<String>,
}

impl Api {
    /// The URL of `path` under this API: `/patients/1` under `https://h/api/v2` is
    /// `https://h/api/v2/patients/1`.
    pub fn url_for(&self, path: &str) -> String {
        let base = self.url.trim_end_matches('/');
        let path = path.trim_start_matches('/');
        if path.is_empty() {
            base.to_string()
        } else if path.starts_with(['?', '#']) {
            format!("{base}{path}")
        } else {
            format!("{base}/{path}")
        }
    }
}

// Hand-written so header values (tokens) never end up in logs.
impl std::fmt::Debug for Api {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Api")
            .field("url", &self.url)
            .field("headers", &self.headers.keys().collect::<Vec<_>>())
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

/// An SFTP server used by `sftp` sources and destinations.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SftpServer {
    pub host: String,
    #[serde(default = "default_sftp_port")]
    pub port: u16,
    pub user: String,
    #[serde(default)]
    pub password: Option<String>,
    /// OpenSSH private key text (keep it in a secret: `${secret:NAME}`).
    #[serde(default)]
    pub private_key: Option<String>,
    #[serde(default)]
    pub passphrase: Option<String>,
    /// The server's host key fingerprint, `SHA256:...`, so a spoofed server is refused. If it's
    /// missing, connecting fails with the fingerprint to check and paste in. `any` turns the
    /// check off (test servers only).
    #[serde(default)]
    pub host_key: Option<String>,
    #[serde(default)]
    pub application: Option<String>,
    #[serde(default)]
    pub vendor: Option<String>,
}

// Hand-written so passwords and keys never end up in logs.
impl std::fmt::Debug for SftpServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SftpServer")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("user", &self.user)
            .field("host_key", &self.host_key)
            .finish_non_exhaustive()
    }
}

fn default_sftp_port() -> u16 {
    22
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum Database {
    Postgres(DbConnection),
    Sqlserver(SqlServerConnection),
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DbConnection {
    pub host: String,
    /// Default: 5432.
    pub port: Option<u16>,
    pub database: String,
    pub user: String,
    #[serde(default)]
    pub password: String,
    /// Size of the shared connection pool.
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,
    #[serde(default)]
    pub application: Option<String>,
    #[serde(default)]
    pub vendor: Option<String>,
}

/// SQL Server, with SQL Server authentication (user and password).
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SqlServerConnection {
    pub host: String,
    /// Default: 1433, or looked up through the SQL Server Browser when `instance` is set.
    pub port: Option<u16>,
    /// Named instance (`host\INSTANCE` in SQL Server tools), e.g. `SQLEXPRESS`.
    #[serde(default)]
    pub instance: Option<String>,
    pub database: String,
    pub user: String,
    #[serde(default)]
    pub password: String,
    /// Size of the shared connection pool.
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,
    /// Accept the server's TLS certificate without checking it. Many SQL Servers use the
    /// self-signed certificate they generate at install; set this only on a network you trust.
    #[serde(default)]
    pub trust_server_certificate: bool,
    #[serde(default)]
    pub application: Option<String>,
    #[serde(default)]
    pub vendor: Option<String>,
}

fn default_max_connections() -> u32 {
    5
}

// Hand-written so passwords never end up in logs.
impl std::fmt::Debug for DbConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DbConnection")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("database", &self.database)
            .field("user", &self.user)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for SqlServerConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqlServerConnection")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("instance", &self.instance)
            .field("database", &self.database)
            .field("user", &self.user)
            .finish_non_exhaustive()
    }
}

impl Resources {
    /// Load `<root>/resources.yaml` (optional) with the environment's values substituted.
    pub fn load(root: &Path, env: &Environment) -> Result<Self> {
        let file = root.join("resources.yaml");
        if !file.is_file() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?;
        let mut value: serde_yaml_ng::Value =
            crate::env::parse_yaml(&text).with_context(|| format!("parsing {}", file.display()))?;
        env.substitute(&mut value).with_context(|| format!("in {}", file.display()))?;
        let resources: Self = serde_yaml_ng::from_value(value).with_context(|| format!("in {}", file.display()))?;
        for (name, s) in &resources.sftp {
            if s.password.is_none() && s.private_key.is_none() {
                bail!("in {}: sftp server `{name}` needs `password` or `private_key`", file.display());
            }
        }
        for (name, api) in &resources.apis {
            match url::Url::parse(&api.url) {
                Ok(u) if matches!(u.scheme(), "http" | "https") && u.host_str().is_some() => {}
                _ => bail!("in {}: api `{name}`: `url` must be an http(s) URL, not `{}`", file.display(), api.url),
            }
        }
        Ok(resources)
    }

    /// The names defined for a kind, for "defined: ..." hints.
    fn names(&self, kind: Kind) -> Vec<&str> {
        match kind {
            Kind::Database => self.databases.keys().map(String::as_str).collect(),
            Kind::Api => self.apis.keys().map(String::as_str).collect(),
            Kind::Sftp => self.sftp.keys().map(String::as_str).collect(),
        }
    }

    pub fn contains(&self, kind: Kind, name: &str) -> bool {
        self.names(kind).contains(&name)
    }

    /// `no api named `x` in resources.yaml (defined: a, b)`, or None if it exists.
    pub fn missing(&self, kind: Kind, name: &str) -> Option<String> {
        if self.contains(kind, name) {
            return None;
        }
        let known = self.names(kind);
        let known = if known.is_empty() { "none".to_string() } else { known.join(", ") };
        Some(format!("no {} named `{name}` in resources.yaml (defined: {known})", kind.label()))
    }

    /// The application and vendor an entry belongs to (unknown if it isn't defined).
    pub fn system(&self, kind: Kind, name: &str) -> System {
        match kind {
            Kind::Database => match self.databases.get(name) {
                Some(Database::Postgres(c)) => System::new(&c.application, &c.vendor),
                Some(Database::Sqlserver(c)) => System::new(&c.application, &c.vendor),
                None => System::default(),
            },
            Kind::Api => self.apis.get(name).map(|a| System::new(&a.application, &a.vendor)).unwrap_or_default(),
            Kind::Sftp => self.sftp.get(name).map(|s| System::new(&s.application, &s.vendor)).unwrap_or_default(),
        }
    }

    /// Where an entry connects to (empty if it isn't defined).
    pub fn endpoints(&self, kind: Kind, name: &str) -> Vec<Endpoint> {
        match kind {
            Kind::Database => match self.databases.get(name) {
                Some(Database::Postgres(c)) => vec![Endpoint::new(&c.host, c.port.unwrap_or(5432), "Postgres")],
                // A named instance without a port is found through the SQL Server Browser; the
                // instance's own TCP port is whatever the server assigned it.
                Some(Database::Sqlserver(c)) => match (&c.instance, c.port) {
                    (Some(i), None) => vec![Endpoint {
                        note: Some(format!("instance {i}; also its TCP port, set on the server")),
                        ..Endpoint::new(&c.host, 1434, "SQL Browser (UDP)")
                    }],
                    _ => vec![Endpoint::new(&c.host, c.port.unwrap_or(1433), "SQL Server")],
                },
                None => Vec::new(),
            },
            Kind::Api => self.apis.get(name).and_then(|a| Endpoint::from_url(&a.url)).into_iter().collect(),
            Kind::Sftp => self.sftp.get(name).map(|s| Endpoint::new(&s.host, s.port, "SFTP")).into_iter().collect(),
        }
    }

    /// Interfaces whose config refers to SFTP servers, databases or APIs resources.yaml doesn't
    /// define.
    pub fn missing_references<'a>(&self, configs: impl IntoIterator<Item = &'a InterfaceConfig>) -> Vec<String> {
        let mut problems = Vec::new();
        for c in configs {
            for (kind, name, place) in c.resource_references() {
                if let Some(missing) = self.missing(kind, name) {
                    problems.push(format!("interface `{}`: {place}: {missing}", c.name));
                }
            }
        }
        problems
    }
}

/// A host and port something connects to.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
    /// `MLLP`, `HTTP`, `HTTPS`, `SFTP`, `Postgres`, `SQL Server` or `SQL Browser (UDP)`.
    pub protocol: &'static str,
    pub note: Option<String>,
}

impl Endpoint {
    pub fn new(host: &str, port: u16, protocol: &'static str) -> Self {
        Self { host: host.to_string(), port, protocol, note: None }
    }

    /// An http(s) URL's host and port; None if it isn't one.
    pub fn from_url(url: &str) -> Option<Self> {
        let u = url::Url::parse(url).ok()?;
        let protocol = match u.scheme() {
            "https" => "HTTPS",
            "http" => "HTTP",
            _ => return None,
        };
        Some(Self::new(u.host_str()?, u.port_or_known_default()?, protocol))
    }
}

impl std::fmt::Display for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // IPv6 addresses need brackets before a port.
        if self.host.contains(':') {
            write!(f, "[{}]:{} {}", self.host, self.port, self.protocol)
        } else {
            write!(f, "{}:{} {}", self.host, self.port, self.protocol)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_databases_with_environment_values() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("resources.yaml"),
            "databases:\n  patient_db:\n    postgres:\n      host: ${DB_HOST}\n      database: patients\n      user: ductus\n      max_connections: ${DB_POOL}\n",
        )
        .unwrap();
        let env = Environment::with_variables("dev", &[("DB_HOST", "localhost"), ("DB_POOL", "3")]);
        let r = Resources::load(dir.path(), &env).unwrap();
        let Database::Postgres(c) = &r.databases["patient_db"] else { panic!("expected postgres") };
        assert_eq!((c.host.as_str(), c.max_connections, c.port), ("localhost", 3, None));

        let missing = Environment::with_variables("prod", &[]);
        let err = format!("{:#}", Resources::load(dir.path(), &missing).unwrap_err());
        assert!(err.contains("DB_HOST, DB_POOL"), "{err}");
    }

    #[test]
    fn loads_sql_server_databases() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("resources.yaml");
        std::fs::write(
            &file,
            "databases:\n  billing:\n    sqlserver:\n      host: db1\n      instance: PROD\n      database: Billing\n      user: ductus\n      password: hunter2\n      trust_server_certificate: true\n",
        )
        .unwrap();
        let r = Resources::load(dir.path(), &Environment::with_variables("dev", &[])).unwrap();
        let Database::Sqlserver(c) = &r.databases["billing"] else { panic!("expected sqlserver") };
        assert_eq!(
            (c.instance.as_deref(), c.port, c.max_connections, c.trust_server_certificate),
            (Some("PROD"), None, 5, true)
        );
        assert!(!format!("{:?}", r.databases).contains("hunter2"), "passwords stay out of logs");

        // SQL Server-only options are refused on Postgres.
        std::fs::write(&file, "databases:\n  p:\n    postgres: { host: h, database: d, user: u, instance: X }\n")
            .unwrap();
        assert!(Resources::load(dir.path(), &Environment::with_variables("dev", &[])).is_err());
    }

    #[test]
    fn loads_sftp_servers_and_finds_missing_ones() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("resources.yaml");
        std::fs::write(&file, "sftp:\n  lab:\n    host: h\n    user: u\n    password: ${PW}\n").unwrap();
        let env = Environment::with_variables("dev", &[("PW", "hunter2")]);
        let r = Resources::load(dir.path(), &env).unwrap();
        assert_eq!((r.sftp["lab"].port, r.sftp["lab"].password.as_deref()), (22, Some("hunter2")));
        assert!(!format!("{:?}", r.sftp["lab"]).contains("hunter2"), "credentials stay out of logs");

        let config: InterfaceConfig = serde_yaml_ng::from_str(
            "name: x\nsource: { sftp: { server: lab, dir: out } }\ndestination: { sftp: { server: emr, dir: in } }\n",
        )
        .unwrap();
        assert_eq!(
            r.missing_references([&config]),
            ["interface `x`: destination `default`: no sftp server named `emr` in resources.yaml (defined: lab)"]
        );

        let config: InterfaceConfig = serde_yaml_ng::from_str(
            "name: y\nsource: { mllp: { port: 1 } }\ndestination: { mllp: { host: h, port: 2 } }\nisolation: dedicated\ndedicated_pools: { billing: 2 }\n",
        )
        .unwrap();
        assert_eq!(
            r.missing_references([&config]),
            ["interface `y`: dedicated_pools: no database named `billing` in resources.yaml (defined: none)"]
        );

        std::fs::write(&file, "sftp:\n  lab: { host: h, user: u }\n").unwrap();
        let err = format!("{:#}", Resources::load(dir.path(), &env).unwrap_err());
        assert!(err.contains("needs `password` or `private_key`"), "{err}");
    }

    #[test]
    fn loads_apis_and_who_they_belong_to() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("resources.yaml");
        std::fs::write(
            &file,
            "apis:\n  empi:\n    url: https://empi.local/api/v2/\n    headers: { Authorization: 'Bearer ${TOKEN}' }\n    timeout: 10s\n    application: EMPI\n    vendor: Rhapsody\nsftp:\n  lab: { host: sftp.lab, user: u, password: p, vendor: Quest }\ndatabases:\n  pdb: { postgres: { host: db1, database: p, user: u, application: Patient DB } }\n",
        )
        .unwrap();
        let env = Environment::with_variables("dev", &[("TOKEN", "t0k3n")]);
        let r = Resources::load(dir.path(), &env).unwrap();
        let empi = &r.apis["empi"];
        assert_eq!(
            (empi.timeout, empi.headers["Authorization"].as_str()),
            (Some(Duration::from_secs(10)), "Bearer t0k3n")
        );
        assert!(!format!("{empi:?}").contains("t0k3n"), "tokens stay out of logs");
        assert_eq!(empi.url_for("/patients/1"), "https://empi.local/api/v2/patients/1");
        assert_eq!(empi.url_for("patients?x=1"), "https://empi.local/api/v2/patients?x=1");
        assert_eq!(empi.url_for(""), "https://empi.local/api/v2");
        assert_eq!(empi.url_for("?q=1"), "https://empi.local/api/v2?q=1");

        assert_eq!(r.system(Kind::Api, "empi").label(), "EMPI (Rhapsody)");
        assert_eq!(r.system(Kind::Sftp, "lab").label(), "(Quest)");
        assert_eq!(r.system(Kind::Database, "pdb").label(), "Patient DB");
        assert!(!r.system(Kind::Api, "nope").is_known());
        assert_eq!(
            r.endpoints(Kind::Api, "empi").iter().map(ToString::to_string).collect::<Vec<_>>(),
            ["empi.local:443 HTTPS"]
        );
        assert_eq!(
            r.missing(Kind::Api, "billing").unwrap(),
            "no api named `billing` in resources.yaml (defined: empi)"
        );

        std::fs::write(&file, "apis:\n  x: { url: 'empi.local/api' }\n").unwrap();
        let err = format!("{:#}", Resources::load(dir.path(), &env).unwrap_err());
        assert!(err.contains("api `x`: `url` must be an http(s) URL"), "{err}");
    }
}
