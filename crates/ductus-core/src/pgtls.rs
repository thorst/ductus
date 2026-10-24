//! TLS to Postgres, as libpq means it: `sslmode` in the engine's database URL
//! (`DUCTUS_DATABASE_URL`, with `sslrootcert`, `sslcert` and `sslkey`, or their `PGSSL...`
//! variables), and `tls:` on a resources.yaml `postgres` entry (with `ca` and `certificate`, which
//! name `certificates:` entries).
//!
//! ```yaml
//! # DUCTUS_DATABASE_URL=postgres://ductus@db1/ductus?sslmode=verify-full&sslrootcert=/etc/ductus/tls/db-ca.pem
//! databases:
//!   patient_db: { postgres: { host: db1, database: patients, user: ductus, tls: verify-full, ca: hospital-ca } }
//! ```
//!
//! The default is libpq's `prefer`: TLS when the server offers it, its certificate not checked (any
//! server answering at that address is believed). A URL means what it means in psql and every other
//! tool; the engine says plainly how each connection is protected instead (`Security`). Only
//! `verify-full` checks both the CA and that the certificate is for the host connected to.

use serde::Deserialize;
use std::path::PathBuf;

/// libpq's `sslmode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SslMode {
    /// No TLS.
    Disable,
    /// libpq tries plain first; sqlx doesn't try TLS at all. Only read from URLs.
    #[serde(skip_deserializing)]
    Allow,
    /// TLS when the server offers it, its certificate not checked (libpq's default).
    #[default]
    Prefer,
    /// TLS or no connection, the certificate not checked.
    Require,
    /// TLS, the certificate issued by a trusted CA; the name in it not checked.
    VerifyCa,
    /// TLS, a trusted CA's certificate for the host connected to.
    VerifyFull,
}

impl SslMode {
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text.trim().to_ascii_lowercase().as_str() {
            "disable" => SslMode::Disable,
            "allow" => SslMode::Allow,
            "prefer" => SslMode::Prefer,
            "require" => SslMode::Require,
            "verify-ca" => SslMode::VerifyCa,
            "verify-full" => SslMode::VerifyFull,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            SslMode::Disable => "disable",
            SslMode::Allow => "allow",
            SslMode::Prefer => "prefer",
            SslMode::Require => "require",
            SslMode::VerifyCa => "verify-ca",
            SslMode::VerifyFull => "verify-full",
        }
    }

    /// The server's certificate is checked against trusted CAs.
    pub fn verifies(self) -> bool {
        matches!(self, SslMode::VerifyCa | SslMode::VerifyFull)
    }

    /// Without TLS there is no connection.
    pub fn requires_tls(self) -> bool {
        matches!(self, SslMode::Require | SslMode::VerifyCa | SslMode::VerifyFull)
    }

    /// TLS is tried at all (sqlx, like libpq for `prefer`, sends the SSLRequest first).
    pub fn tries_tls(self) -> bool {
        !matches!(self, SslMode::Disable | SslMode::Allow)
    }

    /// What a connection with this mode will be, before connecting (for tables like `ductus
    /// ports`): `TLS, verified`, `TLS if offered, not verified (prefer)`, `none (disable)`.
    pub fn expected(self) -> &'static str {
        match self {
            SslMode::VerifyFull => "TLS, verified",
            SslMode::VerifyCa => "TLS, verified by its CA (name not checked: verify-ca)",
            SslMode::Require => "TLS, not verified (require)",
            SslMode::Prefer => "TLS if offered, not verified (prefer)",
            SslMode::Disable => "none (disable)",
            SslMode::Allow => "none (allow)",
        }
    }
}

impl std::fmt::Display for SslMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Whether `host` is this machine (a loopback address, `localhost`, or a Unix socket folder): a
/// connection that never crosses a network, so an unverified one is said but not warned about.
pub fn local(host: &str) -> bool {
    host.is_empty() || host.starts_with('/') || crate::settings::loopback(host.trim_matches(['[', ']']))
}

/// The TLS settings of a Postgres URL, as libpq (and sqlx) read them: its parameters, else the
/// `PG...` environment variables, else the defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlTls {
    pub mode: SslMode,
    /// The host connected to (a folder for a Unix socket).
    pub host: String,
    pub port: u16,
    /// `sslrootcert`: a CA's certificates (PEM file) trusted besides the system's.
    pub root_cert: Option<PathBuf>,
    /// `sslcert` and `sslkey`: the client certificate presented, with its key.
    pub cert: Option<PathBuf>,
    pub key: Option<PathBuf>,
}

impl UrlTls {
    /// None when it isn't a `postgres://` URL.
    pub fn read(url: &str) -> Option<Self> {
        let u = url::Url::parse(url.trim()).ok()?;
        if !matches!(u.scheme(), "postgres" | "postgresql") {
            return None;
        }
        let param =
            |names: &[&str]| u.query_pairs().find(|(k, _)| names.contains(&k.as_ref())).map(|(_, v)| v.into_owned());
        let or_env =
            |value: Option<String>, var: &str| value.or_else(|| std::env::var(var).ok()).filter(|v| !v.is_empty());
        let host = param(&["host"])
            .or_else(|| u.host_str().map(|h| h.trim_matches(['[', ']']).to_string()))
            .filter(|h| !h.is_empty());
        let host = or_env(host, "PGHOST").unwrap_or_else(|| "localhost".into());
        let port = param(&["port"]).and_then(|p| p.parse().ok()).or(u.port()).unwrap_or(5432);
        let root_cert = or_env(param(&["sslrootcert", "ssl-root-cert", "ssl-ca"]), "PGSSLROOTCERT");
        // libpq 16's `system`: the system's certificates (trusted anyway), verify-full by default
        // (`ductus_store::tls::connect_url` connects that way).
        let system = root_cert.as_deref() == Some("system");
        let mode = or_env(param(&["sslmode", "ssl-mode"]), "PGSSLMODE").and_then(|m| SslMode::parse(&m));
        let mode = mode.unwrap_or(if system { SslMode::VerifyFull } else { SslMode::Prefer });
        let path = |names: &[&str], var: &str| or_env(param(names), var).map(PathBuf::from);
        Some(Self {
            mode,
            host,
            port,
            root_cert: root_cert.filter(|_| !system).map(PathBuf::from),
            cert: path(&["sslcert", "ssl-cert"], "PGSSLCERT"),
            key: path(&["sslkey", "ssl-key"], "PGSSLKEY"),
        })
    }

    /// The connection goes through a Unix socket (no TLS there, nothing to verify).
    pub fn socket(&self) -> bool {
        self.host.starts_with('/')
    }
}

/// How a Postgres connection is protected, as its server reports it (`pg_stat_ssl`) and its mode
/// says, in words: `TLS 1.3, verified (sslmode=verify-full)`, `TLS 1.3, not verified
/// (sslmode=prefer): ...`, `plain, no TLS: ...`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Security {
    pub mode: SslMode,
    /// The TLS version the server reports (`TLS 1.3`); None for a plain connection.
    pub tls: Option<String>,
    /// The database is on this machine.
    pub local: bool,
    /// How the mode is written where it's set: `sslmode` (a URL) or `tls:` (resources.yaml).
    pub setting: Setting,
}

/// Where a connection's mode is set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    /// `sslmode=` in a URL.
    Url,
    /// `tls:` on a resources.yaml `postgres` entry.
    Resource,
}

impl Setting {
    /// `sslmode=prefer` or `tls: prefer`.
    pub fn written(self, mode: SslMode) -> String {
        match self {
            Setting::Url => format!("sslmode={mode}"),
            Setting::Resource => format!("tls: {mode}"),
        }
    }
}

impl Security {
    /// From the version `pg_stat_ssl` reports (`TLSv1.3`; None when the connection is plain).
    pub fn new(mode: SslMode, reported: Option<&str>, host: &str, setting: Setting) -> Self {
        let tls = reported.map(|v| v.replace("TLSv", "TLS ").replace("SSLv", "SSL "));
        Self { mode, tls, local: local(host), setting }
    }

    /// TLS, with the server's certificate checked.
    pub fn verified(&self) -> bool {
        self.tls.is_some() && self.mode.verifies()
    }

    /// Worth a warning: a database on another machine whose certificate isn't checked (anyone
    /// answering at its address would be believed, and could read every query).
    pub fn warns(&self) -> bool {
        !self.verified() && !self.local
    }
}

impl std::fmt::Display for Security {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mode = self.setting.written(self.mode);
        match (&self.tls, self.mode) {
            (Some(v), SslMode::VerifyFull) => write!(f, "{v}, verified ({mode})"),
            (Some(v), SslMode::VerifyCa) => write!(f, "{v}, verified by its CA, its name not checked ({mode})"),
            (Some(v), _) => write!(f, "{v}, not verified ({mode}): the server's certificate isn't checked"),
            (None, SslMode::Disable | SslMode::Allow) => write!(f, "plain, no TLS ({mode})"),
            (None, _) => write!(f, "plain, no TLS: the server doesn't offer it ({mode})"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_libpq_s_parameters_from_a_url() {
        let t = UrlTls::read(
            "postgres://ductus:pw@db1.hospital.org:6432/ductus?sslmode=verify-full&sslrootcert=/etc/ductus/tls/db-ca.pem&sslcert=c.pem&sslkey=c.key",
        )
        .unwrap();
        assert_eq!(
            (t.mode, t.host.as_str(), t.port, t.root_cert, t.cert, t.key),
            (
                SslMode::VerifyFull,
                "db1.hospital.org",
                6432,
                Some("/etc/ductus/tls/db-ca.pem".into()),
                Some("c.pem".into()),
                Some("c.key".into())
            )
        );
        let socket = UrlTls::read("postgresql:///ductus?host=/var/run/postgresql").unwrap();
        assert!(socket.socket() && local(&socket.host));
        assert_eq!(UrlTls::read("postgres://[::1]/d?sslmode=disable").unwrap().host, "::1");
        assert!(UrlTls::read("mysql://h/d").is_none());
        assert_eq!(SslMode::parse("Verify-CA"), Some(SslMode::VerifyCa));
        assert_eq!(SslMode::parse("strict"), None);
    }

    #[test]
    fn sslrootcert_system_means_the_system_s_certificates_and_verify_full() {
        let t = UrlTls::read("postgres://u@db1/d?sslrootcert=system").unwrap();
        assert_eq!((t.mode, t.root_cert), (SslMode::VerifyFull, None));
        let t = UrlTls::read("postgres://u@db1/d?sslrootcert=system&sslmode=verify-ca").unwrap();
        assert_eq!(t.mode, SslMode::VerifyCa);
    }

    #[test]
    fn says_how_a_connection_is_protected() {
        let said = |mode, tls: Option<&str>, host: &str| Security::new(mode, tls, host, Setting::Url);
        let full = said(SslMode::VerifyFull, Some("TLSv1.3"), "db1.hospital.org");
        assert_eq!(full.to_string(), "TLS 1.3, verified (sslmode=verify-full)");
        assert!(full.verified() && !full.warns());

        let prefer = said(SslMode::Prefer, Some("TLSv1.3"), "db1.hospital.org");
        assert_eq!(
            prefer.to_string(),
            "TLS 1.3, not verified (sslmode=prefer): the server's certificate isn't checked"
        );
        assert!(prefer.warns(), "a database elsewhere that anyone could stand in for");
        assert!(!said(SslMode::Prefer, Some("TLSv1.3"), "localhost").warns(), "this machine: said, not warned");

        assert_eq!(
            said(SslMode::Prefer, None, "10.1.2.3").to_string(),
            "plain, no TLS: the server doesn't offer it (sslmode=prefer)"
        );
        assert_eq!(
            Security::new(SslMode::Disable, None, "/run/postgresql", Setting::Resource).to_string(),
            "plain, no TLS (tls: disable)"
        );
        assert_eq!(
            said(SslMode::VerifyCa, Some("TLSv1.2"), "db1").to_string(),
            "TLS 1.2, verified by its CA, its name not checked (sslmode=verify-ca)"
        );
    }
}
