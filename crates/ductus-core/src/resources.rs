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
//! ```
//!
//! Interface code refers to them by name (`db("patient_db")`), so no connection details or
//! credentials ever appear in interface code, and each environment supplies its own values.

use crate::InterfaceConfig;
use crate::env::Environment;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resources {
    #[serde(default, with = "serde_yaml_ng::with::singleton_map_recursive")]
    pub databases: BTreeMap<String, Database>,
    #[serde(default)]
    pub sftp: BTreeMap<String, SftpServer>,
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
        Ok(resources)
    }

    /// Interfaces that refer to SFTP servers or databases resources.yaml doesn't define.
    pub fn missing_references<'a>(&self, configs: impl IntoIterator<Item = &'a InterfaceConfig>) -> Vec<String> {
        let mut problems = Vec::new();
        for c in configs {
            for db in c.dedicated_pools.keys() {
                if !self.databases.contains_key(db) {
                    let known: Vec<_> = self.databases.keys().map(String::as_str).collect();
                    let known = if known.is_empty() { "none".to_string() } else { known.join(", ") };
                    problems.push(format!(
                        "interface `{}`: dedicated_pools names `{db}`, which isn't a database in resources.yaml (defined: {known})",
                        c.name
                    ));
                }
            }
            for server in c.sftp_servers() {
                if !self.sftp.contains_key(server) {
                    let known: Vec<_> = self.sftp.keys().map(String::as_str).collect();
                    let known = if known.is_empty() { "none".to_string() } else { known.join(", ") };
                    problems.push(format!(
                        "interface `{}`: no sftp server named `{server}` in resources.yaml (defined: {known})",
                        c.name
                    ));
                }
            }
        }
        problems
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
            ["interface `x`: no sftp server named `emr` in resources.yaml (defined: lab)"]
        );

        let config: InterfaceConfig = serde_yaml_ng::from_str(
            "name: y\nsource: { mllp: { port: 1 } }\ndestination: { mllp: { host: h, port: 2 } }\nisolation: dedicated\ndedicated_pools: { billing: 2 }\n",
        )
        .unwrap();
        assert_eq!(
            r.missing_references([&config]),
            [
                "interface `y`: dedicated_pools names `billing`, which isn't a database in resources.yaml (defined: none)"
            ]
        );

        std::fs::write(&file, "sftp:\n  lab: { host: h, user: u }\n").unwrap();
        let err = format!("{:#}", Resources::load(dir.path(), &env).unwrap_err());
        assert!(err.contains("needs `password` or `private_key`"), "{err}");
    }
}
