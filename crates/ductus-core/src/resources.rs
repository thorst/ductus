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
//! ```
//!
//! Interface code refers to them by name (`db("patient_db")`), so no connection details or
//! credentials ever appear in interface code, and each environment supplies its own values.

use crate::env::Environment;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resources {
    #[serde(default, with = "serde_yaml_ng::with::singleton_map_recursive")]
    pub databases: BTreeMap<String, Database>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum Database {
    Postgres(DbConnection),
    Sqlserver(DbConnection),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DbConnection {
    pub host: String,
    /// Default: 5432 for Postgres, 1433 for SQL Server.
    pub port: Option<u16>,
    pub database: String,
    pub user: String,
    #[serde(default)]
    pub password: String,
    /// Size of the shared connection pool.
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,
}

fn default_max_connections() -> u32 {
    5
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
        serde_yaml_ng::from_value(value).with_context(|| format!("in {}", file.display()))
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
}
