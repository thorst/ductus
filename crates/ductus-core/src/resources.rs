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
//!       tls: verify-full                       # libpq's sslmode; default prefer (not verified), or
//!       ca: hospital-ca                        #   verify-full when a CA is named (`certificates:`)
//!       certificate: ductus-db                 # optional: log in with a client certificate
//!   billing_db:
//!     sqlserver:
//!       host: billing.hospital.local
//!       instance: PROD                           # optional named instance
//!       database: Billing
//!       user: ductus                             # a SQL login (`auth: sql`, the default), or
//!       password: ${secret:BILLING_DB_PASSWORD}  #   `auth: windows`: as the service's own account
//!                                                #   (Windows), or as `user: 'HOSPITAL\svc'` + password
//!       ca: hospital-ca                          # optional: the CA that issued its certificate
//!       encrypt: strict                          # optional: TDS 8 (TLS first), for servers that force it
//!       name_in_certificate: sql1.hospital.org   # optional: when `host` isn't a name on its certificate
//!                                                # (trust_server_certificate: true checks nothing: labs only)
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
//!     ca: hospital-ca                          # optional: also trust this CA (a `certificates:` entry)
//!     certificate: ductus-client               # optional: present this client certificate (mTLS)
//!     application: EMPI
//!     vendor: Rhapsody
//! ldap:
//!   corp-ad:                                   # the directory people sign in with (`auth.ldap`)
//!     url: ldaps://dc1.hospital.org            # ldaps://, or ldap:// with starttls: true
//!     bind_dn: CN=svc-ductus,OU=Service,DC=hospital,DC=org
//!     bind_password: ${secret:LDAP_BIND_PASSWORD}
//!     ca: hospital-ca                          # optional: a private CA this server doesn't trust yet
//!     owner: Directory Services
//! certificates:                                # what we present (`tls:`, `certificate:`), and CAs
//!   ductus-mllp:
//!     cert_file: /etc/ductus/tls/mllp.pem       # leaf first, then intermediates; re-read when it changes
//!     key_file: /etc/ductus/tls/mllp.key
//!     owner: Infrastructure
//!   ductus-api:
//!     cert: ${secret:API_CERT}                  # or PEM text from secrets (read at start)
//!     key: ${secret:API_KEY}
//!   hospital-ca:
//!     ca: ${secret:HOSPITAL_CA}                 # a CA only (or ca_file): clients or servers it issued are trusted
//! ```
//!
//! Interface code refers to them by name (`db("patient_db")`, `api("empi")`), so no connection
//! details or credentials ever appear in interface code, and each environment supplies its own
//! values. Every entry may say which `application` and `vendor` it belongs to; interfaces that
//! use it are then found by `ductus impact`. `owner` is who to call about it (certificate alerts
//! with `to: owner` reach them).

use crate::InterfaceConfig;
use crate::about::System;
use crate::env::Environment;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// `resources.yaml`: what many interfaces share, each defined once by a name: databases, SFTP
/// servers, HTTP APIs, directories for signing in, and certificates.
///
/// Interfaces and their code refer to them by name (`db("patient_db")`, `api("empi")`, `server:
/// lab_server`), so no address or credential is ever written in an interface or its code, and
/// each environment fills in its own with `${NAME}` and `${secret:NAME}`. `ductus doctor` checks
/// every entry from the server, and `ductus impact` finds the interfaces behind each one.
#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Resources {
    /// Databases the interfaces' code queries with `db("<name>")`: each a `postgres` or a
    /// `sqlserver` entry, by a name you choose. The engine keeps a pool of connections to each
    /// and logs in with the entry's credentials, so code never sees them. Default: none.
    /// Example: `databases: { patient_db: { postgres: { host: db1, database: patients, user:
    /// ductus, password: "${secret:PATIENT_DB_PASSWORD}" } } }`.
    #[serde(default, deserialize_with = "serde_yaml_ng::with::singleton_map_recursive::deserialize")]
    #[cfg_attr(test, schemars(with = "BTreeMap<String, Database>"))]
    pub databases: BTreeMap<String, Database>,
    /// SFTP servers that `sftp` sources read from and `sftp` destinations write to (`server:
    /// <name>`), with their login and host key. Default: none. Example: `sftp: { lab_server: {
    /// host: sftp.lab.example.org, user: hospital, private_key: "${secret:LAB_SFTP_KEY}",
    /// host_key: "SHA256:…" } }`.
    #[serde(default)]
    pub sftp: BTreeMap<String, SftpServer>,
    /// HTTP APIs the code calls with `api("<name>")` and `http` destinations send through (`api:
    /// <name>`): a base URL, headers such as the token, a timeout and TLS, set once per
    /// environment. Default: none. Example: `apis: { empi: { url: "https://empi.hospital.local/api/v2",
    /// headers: { Authorization: "Bearer ${secret:EMPI_TOKEN}" } } }`.
    #[serde(default)]
    pub apis: BTreeMap<String, Api>,
    /// Directories (Active Directory, OpenLDAP) people sign in to the web UI with: `auth.ldap` in
    /// ductus.yaml names the one to use and how its groups map to roles. Default: none. Example:
    /// `ldap: { corp-ad: { url: "ldaps://dc1.hospital.org", bind_dn: "CN=svc-ductus,OU=Service,DC=hospital,DC=org",
    /// bind_password: "${secret:LDAP_BIND_PASSWORD}" } }`.
    #[serde(default)]
    pub ldap: BTreeMap<String, LdapServer>,
    /// Certificates the engine presents (`tls: { certificate: <name> }` on listeners and the API,
    /// `certificate: <name>` on connections it makes), and CAs it trusts or checks clients against
    /// (`ca: <name>`). Each entry is a certificate with its key (from files or secrets) or a CA
    /// alone. Their expiry is watched and alerts at 30, 14 and 7 days. Default: none. Example:
    /// `certificates: { ductus-mllp: { cert_file: /etc/ductus/tls/mllp.pem, key_file:
    /// /etc/ductus/tls/mllp.key } }`.
    #[serde(default)]
    pub certificates: BTreeMap<String, CertificateEntry>,
    /// Certificate files written with a root but no drive (Windows only): (entry, as written, read
    /// from), for validate's warning.
    #[serde(skip)]
    pub without_drive: Vec<(String, PathBuf, PathBuf)>,
}

/// The kinds of things resources.yaml defines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Database,
    Api,
    Sftp,
    Ldap,
    Certificate,
}

impl Kind {
    /// How it's named in messages and reports: `database`, `api`, `sftp server`, `directory`,
    /// `certificate`.
    pub fn label(self) -> &'static str {
        match self {
            Kind::Database => "database",
            Kind::Api => "api",
            Kind::Sftp => "sftp server",
            Kind::Ldap => "directory",
            Kind::Certificate => "certificate",
        }
    }
}

/// A `certificates:` entry: a certificate with its private key (from files, which are re-read
/// when they change, or PEM text from secrets), or a CA alone.
#[derive(Clone, Default, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CertificateEntry {
    /// A PEM file with the certificate to present, followed by the intermediate certificates that
    /// issued it (not the root): the "full chain" a CA sends. Goes with `key_file`.
    ///
    /// The engine reads both files at start (a certificate it can't use stops the start, saying
    /// why) and again whenever they change, checked every minute: to renew, replace both files,
    /// with no restart. Senders that don't have the intermediates can't check a certificate
    /// without them, so leave none out. A relative path is read from the project folder; on
    /// Windows, write the drive. To keep the PEM in a secret instead, use `cert` and `key`.
    /// Example: `cert_file: /etc/ductus/tls/mllp.pem`.
    #[serde(default)]
    pub cert_file: Option<std::path::PathBuf>,
    /// The PEM file with the certificate's private key, unencrypted, readable by the account the
    /// engine runs as and nobody else (`chmod 600`, or the service's account only on Windows).
    /// Goes with `cert_file`, renewed with it. `ductus cert request` makes one with its CSR.
    /// Example: `key_file: /etc/ductus/tls/mllp.key`.
    #[serde(default)]
    pub key_file: Option<std::path::PathBuf>,
    /// The certificate (then its intermediates) as PEM text, usually from a secret
    /// (`${secret:MLLP_CERT}`), for servers where certificates live in a vault rather than in
    /// files. Goes with `key`. Read at start only: a renewal needs a restart, unlike files.
    /// Example: `cert: "${secret:MLLP_CERT}"`.
    #[serde(default)]
    pub cert: Option<String>,
    /// The private key as PEM text, from a secret (`${secret:MLLP_KEY}`). Goes with `cert`.
    /// Example: `key: "${secret:MLLP_KEY}"`.
    #[serde(default)]
    pub key: Option<String>,
    /// A CA's certificate (or several) as PEM text, often from a secret. An entry with only this
    /// is a CA: what `ca: <name>` trusts for a server the engine connects to, and what `clients:
    /// { ca: <name> }` checks senders' certificates against. Example: `ca: "${secret:HOSPITAL_CA}"`.
    #[serde(default)]
    pub ca: Option<String>,
    /// A CA's certificate (or several) in a PEM file; the same as `ca`, read from a file
    /// (relative to the project folder unless written whole). Example: `ca_file:
    /// /etc/ductus/tls/hospital-ca.pem`.
    #[serde(default)]
    pub ca_file: Option<std::path::PathBuf>,
    /// The application the certificate belongs to, for `ductus impact`. Default: none. Example:
    /// `application: Ductus MLLP listener`.
    #[serde(default)]
    pub application: Option<String>,
    /// The vendor the certificate belongs to (the CA's, for a CA entry), for `ductus impact`.
    /// Default: none. Example: `vendor: DigiCert`.
    #[serde(default)]
    pub vendor: Option<String>,
    /// Who renews this certificate, best as an email address: alert rules with `to: owner` mail
    /// them as it nears expiry, and the web UI shows them on its row. Default: the owner of the
    /// one interface that uses it, if just one does. Example: `owner: infrastructure@hospital.org`.
    #[serde(default)]
    pub owner: Option<String>,
    /// Your company's documentation fields for this entry (those whose `applies_to` in
    /// `docs-schema.yaml` includes `resource`). Default: none. Example: `custom: { ticket:
    /// CHG-1234 }`.
    #[serde(default)]
    pub custom: crate::docs::Custom,
}

/// What a `certificates:` entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CertificateKind {
    /// A certificate with its private key: what a listener presents.
    Pair,
    /// Only a CA's certificates: what client certificates are checked against.
    Ca,
}

/// Where a `certificates:` entry's PEM comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pem<'a> {
    Files { cert: &'a Path, key: &'a Path },
    Text { cert: &'a str, key: &'a str },
    CaText(&'a str),
    CaFile(&'a Path),
}

impl CertificateEntry {
    pub fn kind(&self) -> CertificateKind {
        match self.pem() {
            Some(Pem::Files { .. } | Pem::Text { .. }) => CertificateKind::Pair,
            _ => CertificateKind::Ca,
        }
    }

    /// Where its PEM is; None if the entry mixes or misses fields (`problem` says which).
    pub fn pem(&self) -> Option<Pem<'_>> {
        match (&self.cert_file, &self.key_file, &self.cert, &self.key, &self.ca, &self.ca_file) {
            (Some(cert), Some(key), None, None, None, None) => Some(Pem::Files { cert, key }),
            (None, None, Some(cert), Some(key), None, None) => Some(Pem::Text { cert, key }),
            (None, None, None, None, Some(ca), None) => Some(Pem::CaText(ca)),
            (None, None, None, None, None, Some(ca)) => Some(Pem::CaFile(ca)),
            _ => None,
        }
    }

    fn problem(&self) -> Option<&'static str> {
        self.pem().is_none().then_some(
            "give `cert_file` and `key_file`, or `cert` and `key` (PEM text, from secrets), or only a CA (`ca` or `ca_file`)",
        )
    }

    /// Relative file names are read from the project folder. On Windows a name with a root but no
    /// drive (`/etc/ductus/x.key`, written for Linux) is read from the project's drive: those are
    /// returned as (written, read from), for a warning.
    fn resolve(&mut self, root: &Path) -> Vec<(PathBuf, PathBuf)> {
        let mut without_drive = Vec::new();
        for file in [&mut self.cert_file, &mut self.key_file, &mut self.ca_file].into_iter().flatten() {
            if file.is_relative() {
                let read = root.join(&*file);
                if file.has_root() {
                    without_drive.push((file.clone(), read.clone()));
                }
                *file = read;
            }
        }
        without_drive
    }
}

// Hand-written so private keys never end up in logs.
impl std::fmt::Debug for CertificateEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CertificateEntry")
            .field("cert_file", &self.cert_file)
            .field("key_file", &self.key_file)
            .field("ca_file", &self.ca_file)
            .field("kind", &self.kind())
            .finish_non_exhaustive()
    }
}

/// An LDAP directory (Active Directory, OpenLDAP) people sign in with.
#[derive(Clone, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct LdapServer {
    /// The directory server: `ldaps://host` (TLS from the start, port 636 unless given) or
    /// `ldap://host` (port 389) with `starttls: true`. For Active Directory, a domain
    /// controller's name or the domain's (`ldaps://hospital.org` reaches any of them, through
    /// DNS). Its certificate must be for this name. Required. Example: `url:
    /// "ldaps://dc1.hospital.org"`.
    pub url: String,
    /// With an `ldap://` URL: turn the connection into TLS (StartTLS) before anything else is
    /// sent, for directories that don't offer ldaps://. Refused if the server can't. Default:
    /// `false`. Example: `starttls: true`.
    #[serde(default)]
    pub starttls: bool,
    /// Allow plain `ldap://` with no TLS at all: people's passwords then cross the network
    /// readable to anyone in between. Only for a test directory on this machine; `ductus
    /// validate` and the engine's start warn about it. Default: `false`. Example: `insecure:
    /// true`.
    #[serde(default)]
    pub insecure: bool,
    /// The service account the engine binds as to find people and read their groups, as a DN.
    /// Give it read access to people and groups and nothing else. Without it the directory is
    /// searched anonymously, which Active Directory refuses. Default: none. Example: `bind_dn:
    /// "CN=svc-ductus,OU=Service Accounts,DC=hospital,DC=org"`.
    #[serde(default)]
    pub bind_dn: Option<String>,
    /// The service account's password: always from a secret (`${secret:LDAP_BIND_PASSWORD}`).
    /// When it expires, sign-ins through the directory fail (local accounts still work), so give
    /// the account a password that doesn't expire, or note when it does. Example: `bind_password:
    /// "${secret:LDAP_BIND_PASSWORD}"`.
    #[serde(default)]
    pub bind_password: Option<String>,
    /// A CA to trust for this directory besides the ones this server trusts (a `certificates:`
    /// entry), for a domain CA this server doesn't have installed (common on Linux; Windows
    /// servers in the domain usually have it). Default: the system's trusted CAs only. Example:
    /// `ca: hospital-ca`.
    #[serde(default)]
    pub ca: Option<String>,
    /// How long connecting, binding or one search may take before a sign-in gives up (and says
    /// the directory can't be reached). Default: `10s`. Example: `timeout: 5s`.
    #[serde(default, deserialize_with = "crate::config::duration_opt")]
    #[cfg_attr(test, schemars(with = "Option<crate::schema::shapes::Duration>"))]
    pub timeout: Option<Duration>,
    /// The application the directory is, for `ductus impact`. Default: none. Example:
    /// `application: Active Directory`.
    #[serde(default)]
    pub application: Option<String>,
    /// The directory's vendor, for `ductus impact`. Default: none. Example: `vendor: Microsoft`.
    #[serde(default)]
    pub vendor: Option<String>,
    /// Who looks after the directory, best as an email address (its certificate's expiry alerts
    /// reach them with `to: owner`). Default: none. Example: `owner: directory@hospital.org`.
    #[serde(default)]
    pub owner: Option<String>,
    /// Your company's documentation fields for this entry (`applies_to: [resource]` in
    /// `docs-schema.yaml`). Default: none. Example: `custom: { ticket: CHG-1234 }`.
    #[serde(default)]
    pub custom: crate::docs::Custom,
}

/// How an LDAP connection is protected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LdapSecurity {
    /// `ldaps://`: TLS from the start.
    Tls,
    /// `ldap://` upgraded with StartTLS.
    StartTls,
    /// `ldap://` with `insecure: true`.
    Plain,
}

impl LdapServer {
    pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

    /// Host and port, from the URL (389 for ldap://, 636 for ldaps://).
    pub fn address(&self) -> Option<(String, u16)> {
        let u = url::Url::parse(&self.url).ok()?;
        let port = u.port().unwrap_or(if u.scheme() == "ldaps" { 636 } else { 389 });
        Some((u.host_str()?.trim_matches(['[', ']']).to_string(), port))
    }

    pub fn security(&self) -> LdapSecurity {
        if self.url.trim().to_ascii_lowercase().starts_with("ldaps://") {
            LdapSecurity::Tls
        } else if self.starttls {
            LdapSecurity::StartTls
        } else {
            LdapSecurity::Plain
        }
    }

    pub fn timeout(&self) -> Duration {
        self.timeout.unwrap_or(Self::DEFAULT_TIMEOUT)
    }

    /// What's wrong with it, if anything (checked when resources.yaml is loaded).
    fn problem(&self) -> Option<String> {
        let scheme = url::Url::parse(&self.url).ok().filter(|u| u.host_str().is_some()).map(|u| u.scheme().to_string());
        match scheme.as_deref() {
            Some("ldaps") if self.starttls => Some("`starttls` is for ldap:// URLs (ldaps:// is TLS already)".into()),
            Some("ldaps") => None,
            Some("ldap") if self.starttls || self.insecure => None,
            Some("ldap") => Some(
                "plain ldap:// sends passwords readable: use ldaps://, or `starttls: true` (`insecure: true` for a test directory)".into(),
            ),
            _ => Some(format!("`url` must be an ldap:// or ldaps:// URL, not `{}`", self.url)),
        }
        .or_else(|| {
            (self.bind_dn.is_some() != self.bind_password.is_some())
                .then(|| "`bind_dn` and `bind_password` go together".into())
        })
    }
}

// Hand-written so the service account's password never ends up in logs.
impl std::fmt::Debug for LdapServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LdapServer")
            .field("url", &self.url)
            .field("starttls", &self.starttls)
            .field("insecure", &self.insecure)
            .field("bind_dn", &self.bind_dn)
            .finish_non_exhaustive()
    }
}

/// An HTTP API used through `api(name)` in code and `http: { api: name }` destinations. The
/// engine adds the URL and headers, so tokens never pass through interface code.
#[derive(Clone, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Api {
    /// The API's base URL, `https://` (or `http://` on a trusted network): paths in code
    /// (`api("empi").get("/patients/1")`) and `path` on destinations go under it. Usually from
    /// the environment (`${EMPI_URL}`), since test and production differ. Required. Example:
    /// `url: "https://empi.hospital.local/api/v2"`.
    pub url: String,
    /// Headers sent with every request, typically the credential: `Authorization: "Bearer
    /// ${secret:EMPI_TOKEN}"`, or an API key header. A header the code or a destination sets
    /// itself replaces the one here. Tokens stay out of interface code and differ per
    /// environment. Default: none. Example: `headers: { Authorization: "Bearer
    /// ${secret:EMPI_TOKEN}" }`.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// How long one request may take, for calls and destinations that don't set their own; a
    /// call past it fails as a temporary error, so its message waits and is tried again.
    /// Default: `30s` for calls from code, `10s` for destinations. Example: `timeout: 10s`.
    #[serde(default, deserialize_with = "crate::config::duration_opt")]
    #[cfg_attr(test, schemars(with = "Option<crate::schema::shapes::Duration>"))]
    pub timeout: Option<Duration>,
    /// A CA to trust for this API's server besides the ones this server trusts (a
    /// `certificates:` entry), for an API whose certificate comes from a private CA. Default:
    /// the system's trusted CAs only. Example: `ca: hospital-ca`.
    #[serde(default)]
    pub ca: Option<String>,
    /// The client certificate to present to this API (a `certificates:` entry with its key), for
    /// APIs that ask for one (mutual TLS). Default: none. Example: `certificate: ductus-client`.
    #[serde(default)]
    pub certificate: Option<String>,
    /// The application behind the API, for `ductus impact`. Default: none. Example:
    /// `application: EMPI`.
    #[serde(default)]
    pub application: Option<String>,
    /// The API's vendor, for `ductus impact`. Default: none. Example: `vendor: Rhapsody`.
    #[serde(default)]
    pub vendor: Option<String>,
    /// Who to ask about this API, best as an email address (its certificate's expiry alerts
    /// reach them with `to: owner`). Default: none. Example: `owner: empi-team@hospital.org`.
    #[serde(default)]
    pub owner: Option<String>,
    /// Your company's documentation fields for this entry (`applies_to: [resource]` in
    /// `docs-schema.yaml`). Default: none. Example: `custom: { contract: MSA-2024-17 }`.
    #[serde(default)]
    pub custom: crate::docs::Custom,
}

impl Api {
    /// Its `ca` and `certificate`.
    pub fn tls(&self) -> crate::tls::ConnectTls {
        crate::tls::ConnectTls::new(&self.ca, &self.certificate)
    }

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
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SftpServer {
    /// The SFTP server's host name or address. Required. Example: `host: sftp.lab.example.org`.
    pub host: String,
    /// The server's SSH port. Default: `22`. Example: `port: 2222`.
    #[serde(default = "default_sftp_port")]
    pub port: u16,
    /// The account to log in as, given by the server's owner. Required. Example: `user:
    /// hospital`.
    pub user: String,
    /// The account's password, from a secret (`${secret:LAB_SFTP_PASSWORD}`). Give this or
    /// `private_key`; a key is better where the server takes one (nothing reusable crosses the
    /// network). Example: `password: "${secret:LAB_SFTP_PASSWORD}"`.
    #[serde(default)]
    pub password: Option<String>,
    /// The private key to log in with, as OpenSSH key text from a secret
    /// (`${secret:LAB_SFTP_KEY}`); the server's owner adds its public half to the account.
    /// `ssh-keygen -t ed25519` makes a pair. Example: `private_key: "${secret:LAB_SFTP_KEY}"`.
    #[serde(default)]
    pub private_key: Option<String>,
    /// The passphrase of an encrypted `private_key`, from a secret. Default: none (an unencrypted
    /// key, protected by the secrets file). Example: `passphrase: "${secret:LAB_SFTP_PASSPHRASE}"`.
    #[serde(default)]
    pub passphrase: Option<String>,
    /// The server's host key fingerprint, as `SHA256:…` (what `ssh-keygen -lf` prints for the key
    /// file), so a server pretending to be this one is refused before any password or key is
    /// sent to it.
    ///
    /// Leave it out the first time: connecting then fails, and the error gives the fingerprint
    /// the server presented, to check with the server's administrator and paste in. When the
    /// server's key changes (a rebuilt server, another host behind the name), connections fail
    /// until it's updated: on purpose, since that's what an impostor looks like too. `any` turns
    /// the check off, for a test server on your own machine only. No default. Example:
    /// `host_key: "SHA256:nThbg6kXUpJWGl7E1IGOCspRomTxdCARLviKw6E5SY8"`.
    #[serde(default)]
    pub host_key: Option<String>,
    /// The application behind the server, for `ductus impact`. Default: none. Example:
    /// `application: Quest Lab Results`.
    #[serde(default)]
    pub application: Option<String>,
    /// The server's vendor (often the partner whose server it is), for `ductus impact`: "what
    /// does Quest's outage touch?". Default: none. Example: `vendor: Quest`.
    #[serde(default)]
    pub vendor: Option<String>,
    /// Who to ask about this server, best as an email address. Default: none. Example: `owner:
    /// lab-it@hospital.org`.
    #[serde(default)]
    pub owner: Option<String>,
    /// Your company's documentation fields for this entry (`applies_to: [resource]` in
    /// `docs-schema.yaml`). Default: none. Example: `custom: { contract: QUEST-2025 }`.
    #[serde(default)]
    pub custom: crate::docs::Custom,
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
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum Database {
    /// A PostgreSQL database. Example: `postgres: { host: db1.hospital.local, database:
    /// patients, user: ductus, password: "${secret:PATIENT_DB_PASSWORD}", tls: verify-full }`.
    Postgres(DbConnection),
    /// A Microsoft SQL Server database, with a SQL login or a Windows one. Always over TLS, its
    /// certificate checked. Example: `sqlserver: { host: billing.hospital.local, database:
    /// Billing, user: ductus, password: "${secret:BILLING_DB_PASSWORD}" }`.
    Sqlserver(SqlServerConnection),
}

#[derive(Clone, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct DbConnection {
    /// The database server's host name or address. With `tls: verify-full`, its certificate
    /// must be for this name. Usually from the environment (`${PATIENT_DB_HOST}`). Required.
    /// Example: `host: db1.hospital.local`.
    pub host: String,
    /// The server's port. Default: `5432`. Example: `port: 5433`.
    pub port: Option<u16>,
    /// The database to connect to on that server. Required. Example: `database: patients`.
    pub database: String,
    /// The login. Give it only what the interfaces' queries need (often read-only on a few
    /// tables or views). Required. Example: `user: ductus`.
    pub user: String,
    /// The login's password, from a secret (`${secret:PATIENT_DB_PASSWORD}`). A wrong one makes
    /// messages wait and retry rather than quarantine, since fixing it fixes them all. Default:
    /// none (for `certificate` or trust logins). Example: `password:
    /// "${secret:PATIENT_DB_PASSWORD}"`.
    #[serde(default)]
    pub password: String,
    /// The most connections the engine opens to this database, shared by every interface's
    /// `db("<name>")` calls, opened as they're needed.
    ///
    /// A call that finds them all busy waits up to 10 seconds for one, then fails as a temporary
    /// error: its message waits and is tried again, nothing is quarantined. Raise it when calls
    /// queue for a connection while the database itself is quick, up to what the database's own
    /// limit and its DBA allow for every engine that connects (each engine has its own pool).
    /// Don't raise it to cure slow queries: more of the same slow query at once makes the
    /// database slower. An interface with `isolation: dedicated` can have connections of its own
    /// instead (`dedicated_pools`), so one busy interface can't take them all. Default: `5`.
    /// Example: `max_connections: 10`.
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,
    /// How the connection is protected, with the names and meanings of libpq's `sslmode`, as
    /// DBAs write them:
    ///
    /// - `disable`: no TLS.
    /// - `prefer`: TLS when the server offers it, its certificate not checked (anyone answering
    ///   at that address is believed).
    /// - `require`: TLS or no connection, the certificate not checked.
    /// - `verify-ca`: TLS, the certificate issued by a trusted CA; the name in it not checked.
    /// - `verify-full`: TLS, a trusted CA's certificate for `host`. The only one that stops a
    ///   server pretending to be this one.
    ///
    /// Use `verify-full` for any database on another machine (cloud Postgres requires TLS),
    /// with `ca` for a private CA. The engine's start, `ductus validate` and `ductus doctor` say
    /// how each connection is protected. Default: `verify-full` when `ca` is given, else
    /// `prefer`. Example: `tls: verify-full`.
    #[serde(default)]
    pub tls: Option<crate::pgtls::SslMode>,
    /// A CA to trust for this server besides the ones this server trusts (a `certificates:`
    /// entry), for a database whose certificate comes from a private CA. Naming one means
    /// `tls: verify-full` unless `tls` says otherwise. Default: the system's trusted CAs only.
    /// Example: `ca: hospital-ca`.
    #[serde(default)]
    pub ca: Option<String>,
    /// A client certificate to present (a `certificates:` entry with its key): for a login by
    /// certificate (`cert` in the server's pg_hba.conf), or alongside the password where the DBA
    /// asks for both. A renewed certificate is used by new connections without a restart.
    /// Default: none. Example: `certificate: ductus-db`.
    #[serde(default)]
    pub certificate: Option<String>,
    /// The application whose database this is, for `ductus impact`. Default: none. Example:
    /// `application: Patient index`.
    #[serde(default)]
    pub application: Option<String>,
    /// The database's (or application's) vendor, for `ductus impact`. Default: none. Example:
    /// `vendor: In-house`.
    #[serde(default)]
    pub vendor: Option<String>,
    /// Who to ask about this database, best as an email address (its certificate's expiry
    /// alerts reach them with `to: owner`). Default: none. Example: `owner: dba@hospital.org`.
    #[serde(default)]
    pub owner: Option<String>,
    /// Your company's documentation fields for this entry (`applies_to: [resource]` in
    /// `docs-schema.yaml`). Default: none. Example: `custom: { ticket: CHG-1234 }`.
    #[serde(default)]
    pub custom: crate::docs::Custom,
}

impl DbConnection {
    /// Its `sslmode`: `tls:`, else `verify-full` when it names a CA (naming one only makes sense
    /// to check against it), else libpq's `prefer`.
    pub fn mode(&self) -> crate::pgtls::SslMode {
        use crate::pgtls::SslMode;
        self.tls.unwrap_or(if self.ca.is_some() { SslMode::VerifyFull } else { SslMode::Prefer })
    }

    /// Its `ca` and `certificate`.
    pub fn connect_tls(&self) -> crate::tls::ConnectTls {
        crate::tls::ConnectTls::new(&self.ca, &self.certificate)
    }

    /// What's wrong with its TLS settings, if anything (checked when resources.yaml is loaded).
    fn problem(&self) -> Option<String> {
        let mode = self.mode();
        if self.ca.is_some() && !mode.verifies() {
            return Some(format!(
                "`ca` is only checked with `tls: verify-full` (or verify-ca), and this says `tls: {mode}`: drop `tls` (naming a CA means verify-full) or set it"
            ));
        }
        (self.certificate.is_some() && !mode.tries_tls())
            .then(|| format!("`certificate` is presented over TLS, and `tls: {mode}` turns TLS off"))
    }

    /// Its TLS for a table (`ductus ports`, Connections): `TLS, verified: hospital-ca trusted`.
    pub fn tls_summary(&self) -> String {
        let summary = self.connect_tls().summary();
        let expected = self.mode().expected();
        if summary.is_empty() { expected.to_string() } else { format!("{expected}: {summary}") }
    }
}

/// SQL Server, with a SQL login (user and password) or a Windows login.
#[derive(Clone, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SqlServerConnection {
    /// The SQL Server's host name or address (without `\INSTANCE`: that goes in `instance`). Its
    /// certificate must be for this name, unless `name_in_certificate` says another. Required.
    /// Example: `host: billing.hospital.local`.
    pub host: String,
    /// The server's port. Default: `1433`, or, with `instance`, the port the SQL Server Browser
    /// gives for it (UDP 1434: the firewall must let that through too). Example: `port: 1435`.
    pub port: Option<u16>,
    /// A named instance, as SQL Server tools write `host\INSTANCE`: `SQLEXPRESS`, `PROD`. Its port
    /// is asked of the SQL Server Browser, unless `port` is given. Default: the default instance.
    /// Example: `instance: PROD`.
    #[serde(default)]
    pub instance: Option<String>,
    /// The database to use on that server. Required. Example: `database: Billing`.
    pub database: String,
    /// How the engine logs in:
    ///
    /// - `sql`: a SQL Server login, `user` and `password`.
    /// - `windows`: a Windows login. Without `user`, as the account the engine runs as (the
    ///   Windows service's account; Windows only, Kerberos where the domain allows it); with
    ///   `user` and `password`, as that domain account (from Linux this is NTLM, which some
    ///   servers refuse: Extended Protection set to Required).
    ///
    /// Use `windows` where the DBA allows only Windows logins; on Windows prefer the service's
    /// own account (a gMSA), which has no password to keep. Default: `sql`. Example: `auth:
    /// windows`.
    #[serde(default)]
    pub auth: SqlAuth,
    /// The login: a SQL login's name with `auth: sql`, or a domain account (`HOSPITAL\svc-lab`)
    /// with `auth: windows`, where leaving it out means the account the engine runs as. Give
    /// it only what the queries need. Example: `user: ductus`.
    #[serde(default)]
    pub user: String,
    /// The login's password, from a secret (`${secret:BILLING_DB_PASSWORD}`). Not with `auth:
    /// windows` and no `user`. Example: `password: "${secret:BILLING_DB_PASSWORD}"`.
    #[serde(default)]
    pub password: String,
    /// The most connections the engine opens to this database, shared by every interface's
    /// `db("<name>")` calls, opened as they're needed. A call that finds them all busy waits for
    /// one, then fails as a temporary error (the message waits and is tried again). Raise it
    /// when calls queue while the server is quick, within what its DBA allows for every engine.
    /// Default: `5`. Example: `max_connections: 10`.
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,
    /// A CA to trust for this server besides the ones this server trusts (a `certificates:`
    /// entry), for a SQL Server whose certificate comes from a private CA (common: the domain's
    /// CA, or a certificate the DBA made). Default: the system's trusted CAs only. Example: `ca:
    /// hospital-ca`.
    #[serde(default)]
    pub ca: Option<String>,
    /// How the connection is encrypted, always with the server's certificate checked:
    ///
    /// - `mandatory`: TLS for the whole session, begun after SQL Server's first exchange, as
    ///   every SQL Server client does it.
    /// - `strict`: TDS 8, TLS before anything else, for servers set to force it (SQL Server 2022
    ///   and later; 2022 on Linux refuses it).
    ///
    /// There's no `false`: SQL Server would encrypt the login only and send queries and results
    /// readable. Default: `mandatory`. Example: `encrypt: strict`.
    #[serde(default)]
    pub encrypt: Encrypt,
    /// The name the server's certificate is checked for, when it isn't `host`: connecting to an
    /// availability group listener, an alias or an address whose certificate names the server
    /// itself (ADO.NET's HostNameInCertificate). Default: `host`. Example: `name_in_certificate:
    /// sql1.hospital.org`.
    #[serde(default)]
    pub name_in_certificate: Option<String>,
    /// Accept the server's certificate without checking it: anyone answering at its address would
    /// be believed, and could read the credentials and data. For a lab server only; the start and
    /// `ductus validate` warn about it on another machine. For a server with a private CA, name the
    /// CA in `ca` instead. Not with `ca` or `name_in_certificate`. Default: `false`. Example:
    /// `trust_server_certificate: true`.
    #[serde(default)]
    pub trust_server_certificate: bool,
    /// The application whose database this is, for `ductus impact`. Default: none. Example:
    /// `application: Billing`.
    #[serde(default)]
    pub application: Option<String>,
    /// The application's vendor, for `ductus impact`. Default: none. Example: `vendor: Cerner`.
    #[serde(default)]
    pub vendor: Option<String>,
    /// Who to ask about this database, best as an email address (its certificate's expiry
    /// alerts reach them with `to: owner`). Default: none. Example: `owner: dba@hospital.org`.
    #[serde(default)]
    pub owner: Option<String>,
    /// Your company's documentation fields for this entry (`applies_to: [resource]` in
    /// `docs-schema.yaml`). Default: none. Example: `custom: { ticket: CHG-1234 }`.
    #[serde(default)]
    pub custom: crate::docs::Custom,
}

/// How a SQL Server connection is encrypted. There's no `false` (TLS for the login only, then
/// queries in the clear: it reads safe and isn't).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Encrypt {
    /// TLS for the whole session, started after the PRELOGIN exchange (TDS 7.x).
    #[default]
    Mandatory,
    /// TDS 8: TLS before anything (ALPN `tds/8.0`); always verified.
    Strict,
}

impl<'de> Deserialize<'de> for Encrypt {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        use serde::de::Error;
        match serde_yaml_ng::Value::deserialize(d)? {
            serde_yaml_ng::Value::String(s) if s == "mandatory" => Ok(Encrypt::Mandatory),
            serde_yaml_ng::Value::String(s) if s == "strict" => Ok(Encrypt::Strict),
            serde_yaml_ng::Value::Bool(true) => Ok(Encrypt::Mandatory),
            serde_yaml_ng::Value::Bool(false) => Err(D::Error::custom(
                "`encrypt: false` isn't offered: SQL Server would encrypt the login only and send queries and results readable. Leave `encrypt` out (TLS throughout) or say `strict`",
            )),
            other => Err(D::Error::custom(format!(
                "`encrypt` is `mandatory` (the default) or `strict` (TDS 8), not `{}`",
                serde_yaml_ng::to_string(&other).unwrap_or_default().trim()
            ))),
        }
    }
}

/// How the engine logs in to SQL Server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum SqlAuth {
    /// A SQL Server login: `user` and `password`.
    #[default]
    Sql,
    /// A Windows login: the account the engine runs as (integrated, Windows only), or the named
    /// `user` with its `password` (on Linux: NTLM).
    Windows,
}

/// Who a SQL Server connection logs in as, worked out from its settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Login<'a> {
    /// A SQL login.
    Sql(&'a str),
    /// The account the engine runs as (Windows's SSPI).
    Integrated,
    /// A named Windows account.
    Windows(&'a str),
}

impl std::fmt::Display for Login<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Login::Sql(user) => write!(f, "SQL login `{user}`"),
            Login::Integrated => f.write_str("Windows login as the account the engine runs as"),
            Login::Windows(user) => write!(f, "Windows login `{user}`"),
        }
    }
}

impl SqlServerConnection {
    /// Who it logs in as.
    pub fn login(&self) -> Login<'_> {
        match (self.auth, self.user.is_empty()) {
            (SqlAuth::Sql, _) => Login::Sql(&self.user),
            (SqlAuth::Windows, true) => Login::Integrated,
            (SqlAuth::Windows, false) => Login::Windows(&self.user),
        }
    }

    /// Logs in with NTLM from this machine whatever the domain offers: a named Windows account
    /// from anything but Windows (no Kerberos there yet).
    pub fn ntlm_only(&self) -> bool {
        !cfg!(windows) && matches!(self.login(), Login::Windows(_))
    }

    /// What's wrong with its login settings, if anything (checked when resources.yaml is loaded).
    fn login_problem(&self) -> Option<String> {
        match (self.auth, self.user.is_empty(), self.password.is_empty()) {
            (SqlAuth::Sql, true, _) => Some(
                "a SQL login needs `user` (and `password`); for a Windows login say `auth: windows`".into(),
            ),
            (SqlAuth::Windows, true, false) => {
                Some("`password` goes with a Windows account in `user` (`HOSPITAL\\svc-lab`)".into())
            }
            (SqlAuth::Windows, false, true) => Some(format!(
                "the Windows account `{}` needs its `password` (from secrets); leave both out to log in as the account the engine runs as{}",
                self.user,
                if cfg!(windows) { "" } else { " (Windows only)" }
            )),
            (SqlAuth::Windows, true, true) if !cfg!(windows) => Some(
                "`auth: windows` without `user` logs in as the account the engine runs as, which needs Windows: on Linux name an account (`user: 'HOSPITAL\\svc-lab'` and its `password`: NTLM), or use a SQL login".into(),
            ),
            _ => None,
        }
    }

    /// The server's certificate is checked (no `trust_server_certificate`).
    pub fn verifies(&self) -> bool {
        !self.trust_server_certificate
    }

    /// The name its certificate must be for: `name_in_certificate`, else `host`.
    pub fn certificate_name(&self) -> &str {
        self.name_in_certificate.as_deref().unwrap_or(&self.host)
    }

    /// Its `ca` (SQL Server logins present no client certificate).
    pub fn connect_tls(&self) -> crate::tls::ConnectTls {
        crate::tls::ConnectTls::new(&self.ca, &None)
    }

    /// What its connections will be, before connecting: `TLS, verified`, `TDS 8 strict, TLS
    /// verified`, `TLS, not verified (trust_server_certificate)`.
    pub fn expected(&self) -> String {
        let how = if self.encrypt == Encrypt::Strict { "TDS 8 strict, TLS" } else { "TLS," };
        let name = match &self.name_in_certificate {
            Some(n) => format!(" as {n}"),
            None => String::new(),
        };
        if self.verifies() {
            format!("{how} verified{name}")
        } else {
            format!("{how} not verified (trust_server_certificate)")
        }
    }

    /// Its TLS for a table (`ductus ports`, Connections): `TLS, verified: hospital-ca trusted`.
    pub fn tls_summary(&self) -> String {
        let summary = self.connect_tls().summary();
        let expected = self.expected();
        if summary.is_empty() { expected } else { format!("{expected}: {summary}") }
    }

    /// What's wrong with its TLS settings, if anything (checked when resources.yaml is loaded).
    fn problem(&self) -> Option<String> {
        if let Some(problem) = self.login_problem() {
            return Some(problem);
        }
        let trusting = "`trust_server_certificate` accepts any certificate";
        match (&self.ca, &self.name_in_certificate) {
            _ if !self.trust_server_certificate => None,
            (Some(_), _) => Some(format!(
                "{trusting}, so `ca` would never be checked: remove `trust_server_certificate` (with `ca` the server's certificate is verified)"
            )),
            (None, Some(_)) => {
                Some(format!("{trusting}, so `name_in_certificate` would never be checked: remove one of them"))
            }
            (None, None) => None,
        }
    }
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
            .field("tls", &self.mode())
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
            .field("auth", &self.auth)
            .field("user", &self.user)
            .field("encrypt", &self.encrypt)
            .field("verifies", &self.verifies())
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
        if value.is_null() {
            return Ok(Self::default()); // only comments (as `ductus init` writes it)
        }
        env.substitute(&mut value).with_context(|| format!("in {}", file.display()))?;
        let mut resources: Self = serde_yaml_ng::from_value(value).with_context(|| format!("in {}", file.display()))?;
        for (name, c) in &mut resources.certificates {
            if let Some(problem) = c.problem() {
                bail!("in {}: certificate `{name}`: {problem}", file.display());
            }
            for (written, read) in c.resolve(root) {
                resources.without_drive.push((name.clone(), written, read));
            }
        }
        for (name, s) in &resources.sftp {
            if s.password.is_none() && s.private_key.is_none() {
                bail!("in {}: sftp server `{name}` needs `password` or `private_key`", file.display());
            }
        }
        for (name, api) in &resources.apis {
            match url::Url::parse(&api.url) {
                Ok(u) if u.scheme() == "http" && api.tls().is_set() => {
                    bail!("in {}: api `{name}`: `ca` and `certificate` are for an https:// `url`", file.display())
                }
                Ok(u) if matches!(u.scheme(), "http" | "https") && u.host_str().is_some() => {}
                _ => bail!("in {}: api `{name}`: `url` must be an http(s) URL, not `{}`", file.display(), api.url),
            }
        }
        for (name, l) in &resources.ldap {
            if let Some(problem) = l.problem() {
                bail!("in {}: ldap `{name}`: {problem}", file.display());
            }
        }
        for (name, db) in &resources.databases {
            let problem = match db {
                Database::Postgres(c) => c.problem(),
                Database::Sqlserver(c) => c.problem(),
            };
            if let Some(problem) = problem {
                bail!("in {}: database `{name}`: {problem}", file.display());
            }
        }
        Ok(resources)
    }

    /// Postgres databases on another machine whose certificate the engine doesn't check
    /// (`tls:` other than verify-full or verify-ca), with how they're set: for warnings.
    pub fn unverified_postgres(&self) -> Vec<(&str, crate::pgtls::SslMode)> {
        self.databases
            .iter()
            .filter_map(|(name, d)| match d {
                Database::Postgres(c) if !c.mode().verifies() && !crate::pgtls::local(&c.host) => {
                    Some((name.as_str(), c.mode()))
                }
                _ => None,
            })
            .collect()
    }

    /// SQL Servers on another machine whose certificate the engine doesn't check
    /// (`trust_server_certificate: true`): for warnings.
    pub fn unverified_sqlserver(&self) -> Vec<&str> {
        self.databases
            .iter()
            .filter_map(|(name, d)| match d {
                Database::Sqlserver(c) if !c.verifies() && !crate::pgtls::local(&c.host) => Some(name.as_str()),
                _ => None,
            })
            .collect()
    }

    /// SQL Servers this machine logs in to with NTLM only (a named Windows account from Linux):
    /// for warnings.
    pub fn ntlm_sqlserver(&self) -> Vec<&str> {
        self.databases
            .iter()
            .filter_map(|(name, d)| match d {
                Database::Sqlserver(c) if c.ntlm_only() => Some(name.as_str()),
                _ => None,
            })
            .collect()
    }

    /// SQL Servers logged in to as the account the engine runs as (`auth: windows` without `user`).
    pub fn integrated_sqlserver(&self) -> Vec<&str> {
        self.databases
            .iter()
            .filter_map(|(name, d)| match d {
                Database::Sqlserver(c) if c.login() == Login::Integrated => Some(name.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Directories reached without TLS (`insecure: true`), for warnings.
    pub fn insecure_ldap(&self) -> Vec<&str> {
        self.ldap.iter().filter(|(_, l)| l.security() == LdapSecurity::Plain).map(|(n, _)| n.as_str()).collect()
    }

    /// Who an entry's `owner` names (any kind), if anyone.
    pub fn owner(&self, name: &str) -> Option<&str> {
        let owner = match self.databases.get(name) {
            Some(Database::Postgres(c)) => &c.owner,
            Some(Database::Sqlserver(c)) => &c.owner,
            None => {
                if let Some(a) = self.apis.get(name) {
                    &a.owner
                } else if let Some(s) = self.sftp.get(name) {
                    &s.owner
                } else if let Some(l) = self.ldap.get(name) {
                    &l.owner
                } else {
                    &self.certificates.get(name)?.owner
                }
            }
        };
        crate::about::given(owner)
    }

    /// The names defined for a kind, for "defined: ..." hints.
    fn names(&self, kind: Kind) -> Vec<&str> {
        match kind {
            Kind::Database => self.databases.keys().map(String::as_str).collect(),
            Kind::Api => self.apis.keys().map(String::as_str).collect(),
            Kind::Sftp => self.sftp.keys().map(String::as_str).collect(),
            Kind::Ldap => self.ldap.keys().map(String::as_str).collect(),
            Kind::Certificate => self.certificates.keys().map(String::as_str).collect(),
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

    /// Every entry's documentation fields, in kind and name order.
    pub fn customs(&self) -> Vec<(Kind, &str, &crate::docs::Custom)> {
        let mut all = Vec::new();
        for (name, d) in &self.databases {
            let custom = match d {
                Database::Postgres(c) => &c.custom,
                Database::Sqlserver(c) => &c.custom,
            };
            all.push((Kind::Database, name.as_str(), custom));
        }
        all.extend(self.apis.iter().map(|(n, a)| (Kind::Api, n.as_str(), &a.custom)));
        all.extend(self.sftp.iter().map(|(n, s)| (Kind::Sftp, n.as_str(), &s.custom)));
        all.extend(self.ldap.iter().map(|(n, l)| (Kind::Ldap, n.as_str(), &l.custom)));
        all.extend(self.certificates.iter().map(|(n, c)| (Kind::Certificate, n.as_str(), &c.custom)));
        all
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
            Kind::Ldap => self.ldap.get(name).map(|l| System::new(&l.application, &l.vendor)).unwrap_or_default(),
            Kind::Certificate => {
                self.certificates.get(name).map(|c| System::new(&c.application, &c.vendor)).unwrap_or_default()
            }
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
            Kind::Ldap => self
                .ldap
                .get(name)
                .and_then(|l| {
                    let protocol = if l.security() == LdapSecurity::Tls { "LDAPS" } else { "LDAP" };
                    l.address().map(|(host, port)| Endpoint::new(&host, port, protocol))
                })
                .into_iter()
                .collect(),
            Kind::Certificate => Vec::new(), // presented by us, not connected to
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
    /// `MLLP`, `HTTP`, `HTTPS`, `SFTP`, `LDAP`, `LDAPS`, `Postgres`, `SQL Server` or `SQL Browser (UDP)`.
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
    fn a_file_of_comments_defines_nothing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("resources.yaml"), "# databases:\n#   patient_db: ...\n").unwrap();
        let r = Resources::load(dir.path(), &Environment::with_variables("dev", &[])).unwrap();
        assert!(r.databases.is_empty() && r.sftp.is_empty() && r.apis.is_empty());
    }

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
    fn postgres_tls_is_libpq_s_sslmode_and_a_named_ca_means_verify_full() {
        use crate::pgtls::SslMode;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("resources.yaml");
        let load = |text: &str| {
            std::fs::write(&file, format!("databases:\n{text}")).unwrap();
            Resources::load(dir.path(), &Environment::with_variables("dev", &[])).map_err(|e| format!("{e:#}"))
        };
        let r = load(
            "  a: { postgres: { host: db1, database: d, user: u } }\n  b: { postgres: { host: db1, database: d, user: u, ca: hospital-ca } }\n  c: { postgres: { host: db1, database: d, user: u, tls: require, certificate: me } }\n  d: { postgres: { host: localhost, database: d, user: u } }\n  e: { postgres: { host: db1, database: d, user: u, tls: verify-ca, ca: x } }\n",
        )
        .unwrap();
        let mode = |name: &str| match &r.databases[name] {
            Database::Postgres(c) => (c.mode(), c.tls_summary()),
            _ => unreachable!(),
        };
        assert_eq!(mode("a"), (SslMode::Prefer, "TLS if offered, not verified (prefer)".into()), "libpq's default");
        assert_eq!(mode("b"), (SslMode::VerifyFull, "TLS, verified: hospital-ca trusted".into()));
        assert_eq!(mode("c"), (SslMode::Require, "TLS, not verified (require): client certificate me".into()));
        assert_eq!(mode("e").0, SslMode::VerifyCa);
        assert_eq!(
            r.unverified_postgres(),
            [("a", SslMode::Prefer), ("c", SslMode::Require)],
            "elsewhere and not checked; d is this machine"
        );

        let err = load("  a: { postgres: { host: h, database: d, user: u, tls: prefer, ca: x } }\n").unwrap_err();
        assert!(err.contains("database `a`: `ca` is only checked with `tls: verify-full`"), "{err}");
        let err =
            load("  a: { postgres: { host: h, database: d, user: u, tls: disable, certificate: x } }\n").unwrap_err();
        assert!(err.contains("`tls: disable` turns TLS off"), "{err}");
        let err = load("  a: { postgres: { host: h, database: d, user: u, tls: strict } }\n").unwrap_err();
        assert!(err.contains("unknown variant `strict`"), "{err}");
    }

    #[test]
    fn sql_server_tls_is_verified_unless_trust_server_certificate_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("resources.yaml");
        let load = |text: &str| {
            std::fs::write(&file, format!("databases:\n{text}")).unwrap();
            Resources::load(dir.path(), &Environment::with_variables("dev", &[])).map_err(|e| format!("{e:#}"))
        };
        let r = load(concat!(
            "  a: { sqlserver: { host: db1, database: d, user: u } }\n",
            "  b: { sqlserver: { host: db1, database: d, user: u, ca: hospital-ca, encrypt: strict } }\n",
            "  c: { sqlserver: { host: 10.1.2.3, database: d, user: u, name_in_certificate: sql1.hospital.org } }\n",
            "  d: { sqlserver: { host: db1, database: d, user: u, trust_server_certificate: true } }\n",
            "  e: { sqlserver: { host: localhost, database: d, user: u, trust_server_certificate: true } }\n",
            "  f: { sqlserver: { host: db1, database: d, user: u, encrypt: mandatory } }\n",
        ))
        .unwrap();
        let said = |name: &str| match &r.databases[name] {
            Database::Sqlserver(c) => (c.encrypt, c.tls_summary(), c.certificate_name().to_string()),
            _ => unreachable!(),
        };
        assert_eq!(said("a"), (Encrypt::Mandatory, "TLS, verified".into(), "db1".into()));
        assert_eq!(
            said("b"),
            (Encrypt::Strict, "TDS 8 strict, TLS verified: hospital-ca trusted".into(), "db1".into())
        );
        assert_eq!(
            said("c"),
            (Encrypt::Mandatory, "TLS, verified as sql1.hospital.org".into(), "sql1.hospital.org".into())
        );
        assert_eq!(said("d").1, "TLS, not verified (trust_server_certificate)");
        assert_eq!(said("f").0, Encrypt::Mandatory);
        assert_eq!(r.unverified_sqlserver(), ["d"], "elsewhere and not checked; e is this machine");

        let err = load("  a: { sqlserver: { host: h, database: d, user: u, encrypt: false } }\n").unwrap_err();
        assert!(err.contains("`encrypt: false` isn't offered"), "{err}");
        let err = load("  a: { sqlserver: { host: h, database: d, user: u, encrypt: optional } }\n").unwrap_err();
        assert!(err.contains("`encrypt` is `mandatory` (the default) or `strict` (TDS 8), not `optional`"), "{err}");
        let err =
            load("  a: { sqlserver: { host: h, database: d, user: u, ca: x, trust_server_certificate: true } }\n")
                .unwrap_err();
        assert!(
            err.contains(
                "database `a`: `trust_server_certificate` accepts any certificate, so `ca` would never be checked"
            ),
            "{err}"
        );
        let err = load(
            "  a: { sqlserver: { host: h, database: d, user: u, name_in_certificate: n, trust_server_certificate: true } }\n",
        )
        .unwrap_err();
        assert!(err.contains("so `name_in_certificate` would never be checked"), "{err}");
    }

    #[test]
    fn sql_server_logins_are_sql_or_windows_and_integrated_needs_windows() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("resources.yaml");
        let load = |text: &str| {
            std::fs::write(&file, format!("databases:\n{text}")).unwrap();
            Resources::load(dir.path(), &Environment::with_variables("dev", &[])).map_err(|e| format!("{e:#}"))
        };
        let r = load(concat!(
            "  a: { sqlserver: { host: h, database: d, user: u, password: p } }\n",
            "  b: { sqlserver: { host: h, database: d, auth: sql, user: u } }\n",
            "  c: { sqlserver: { host: h, database: d, auth: windows, user: 'HOSPITAL\\svc-lab', password: p } }\n",
        ))
        .unwrap();
        let login = |name: &str| match &r.databases[name] {
            Database::Sqlserver(c) => (c.login().to_string(), c.ntlm_only()),
            _ => unreachable!(),
        };
        assert_eq!(login("a"), ("SQL login `u`".into(), false));
        assert_eq!(login("b").0, "SQL login `u`");
        assert_eq!(login("c"), ("Windows login `HOSPITAL\\svc-lab`".into(), !cfg!(windows)), "NTLM from Linux");
        assert_eq!(r.ntlm_sqlserver(), if cfg!(windows) { vec![] } else { vec!["c"] });

        // Integrated: the engine's own account, which only Windows has.
        let integrated = load("  i: { sqlserver: { host: h, database: d, auth: windows } }\n");
        if cfg!(windows) {
            let r = integrated.unwrap();
            let Database::Sqlserver(c) = &r.databases["i"] else { unreachable!() };
            assert_eq!(c.login(), Login::Integrated);
            assert_eq!(c.login().to_string(), "Windows login as the account the engine runs as");
        } else {
            let err = integrated.unwrap_err();
            assert!(
                err.contains("database `i`: `auth: windows` without `user` logs in as the account the engine runs as, which needs Windows"),
                "{err}"
            );
        }

        let err = load("  a: { sqlserver: { host: h, database: d } }\n").unwrap_err();
        assert!(err.contains("a SQL login needs `user`"), "{err}");
        let err = load("  a: { sqlserver: { host: h, database: d, auth: windows, password: p } }\n").unwrap_err();
        assert!(err.contains("`password` goes with a Windows account in `user`"), "{err}");
        let err = load("  a: { sqlserver: { host: h, database: d, auth: windows, user: 'H\\svc' } }\n").unwrap_err();
        assert!(err.contains("the Windows account `H\\svc` needs its `password`"), "{err}");
        let err = load("  a: { sqlserver: { host: h, database: d, user: u, auth: kerberos } }\n").unwrap_err();
        assert!(err.contains("unknown variant `kerberos`"), "{err}");
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
        std::fs::write(&file, "apis:\n  x: { url: 'http://empi.local/api', certificate: ours }\n").unwrap();
        let err = format!("{:#}", Resources::load(dir.path(), &env).unwrap_err());
        assert!(err.contains("api `x`: `ca` and `certificate` are for an https:// `url`"), "{err}");
    }

    #[test]
    fn loads_directories_and_refuses_plain_ldap_unless_insecure() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("resources.yaml");
        let env = Environment::with_variables("dev", &[("PW", "s3cret")]);
        let load = |text: &str| {
            std::fs::write(&file, text).unwrap();
            Resources::load(dir.path(), &env).map_err(|e| format!("{e:#}"))
        };
        let r = load(
            "ldap:\n  corp:\n    url: ldaps://dc1.hospital.org\n    bind_dn: cn=svc\n    bind_password: ${PW}\n    owner: Directory Services\n    application: Active Directory\n",
        )
        .unwrap();
        let corp = &r.ldap["corp"];
        assert_eq!((corp.security(), corp.address()), (LdapSecurity::Tls, Some(("dc1.hospital.org".into(), 636))));
        assert_eq!(corp.timeout(), Duration::from_secs(10));
        assert!(!format!("{corp:?}").contains("s3cret"), "the bind password stays out of logs");
        assert_eq!(r.owner("corp"), Some("Directory Services"));
        assert_eq!(r.system(Kind::Ldap, "corp").label(), "Active Directory");
        assert_eq!(
            r.endpoints(Kind::Ldap, "corp").iter().map(ToString::to_string).collect::<Vec<_>>(),
            ["dc1.hospital.org:636 LDAPS"]
        );
        assert!(r.insecure_ldap().is_empty());
        assert_eq!(r.missing(Kind::Ldap, "ad").unwrap(), "no directory named `ad` in resources.yaml (defined: corp)");

        let r = load("ldap:\n  dc: { url: 'ldap://dc1:3389', starttls: true }\n").unwrap();
        assert_eq!(
            (r.ldap["dc"].security(), r.ldap["dc"].address()),
            (LdapSecurity::StartTls, Some(("dc1".into(), 3389)))
        );
        let r = load("ldap:\n  test: { url: 'ldap://127.0.0.1', insecure: true }\n").unwrap();
        assert_eq!(r.insecure_ldap(), ["test"]);

        let err = load("ldap:\n  dc: { url: 'ldap://dc1' }\n").unwrap_err();
        assert!(err.contains("ldap `dc`: plain ldap:// sends passwords readable"), "{err}");
        let err = load("ldap:\n  dc: { url: 'ldaps://dc1', starttls: true }\n").unwrap_err();
        assert!(err.contains("`starttls` is for ldap:// URLs"), "{err}");
        let err = load("ldap:\n  dc: { url: 'https://dc1' }\n").unwrap_err();
        assert!(err.contains("must be an ldap:// or ldaps:// URL"), "{err}");
        let err = load("ldap:\n  dc: { url: 'ldaps://dc1', bind_dn: cn=svc }\n").unwrap_err();
        assert!(err.contains("`bind_dn` and `bind_password` go together"), "{err}");

        // Every kind takes an owner.
        let r = load("apis:\n  a: { url: 'https://a', owner: ' Web Team ' }\nsftp:\n  s: { host: h, user: u, password: p, owner: Lab }\n").unwrap();
        assert_eq!((r.owner("a"), r.owner("s"), r.owner("none")), (Some("Web Team"), Some("Lab"), None));
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths_with_a_drive_or_a_share_are_read_as_written() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("resources.yaml"),
            "certificates:\n  mllp: { cert_file: 'C:\\certs\\mllp.pem', key_file: '\\\\vault\\certs\\mllp.key' }\n  ca: { ca_file: 'D:/pki/ca.pem' }\n",
        )
        .unwrap();
        let r = Resources::load(dir.path(), &Environment::with_variables("dev", &[])).unwrap();
        assert_eq!(
            r.certificates["mllp"].pem(),
            Some(Pem::Files { cert: Path::new(r"C:\certs\mllp.pem"), key: Path::new(r"\\vault\certs\mllp.key") })
        );
        assert_eq!(r.certificates["ca"].pem(), Some(Pem::CaFile(Path::new("D:/pki/ca.pem"))));
        assert!(r.without_drive.is_empty());
    }

    #[test]
    fn loads_certificates_as_pairs_or_cas() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("resources.yaml");
        let env = Environment::with_variables("dev", &[("KEY", "-----BEGIN PRIVATE KEY-----secret")]);
        let load = |text: &str| {
            std::fs::write(&file, text).unwrap();
            Resources::load(dir.path(), &env).map_err(|e| format!("{e:#}"))
        };
        let r = load(
            "certificates:\n  mllp: { cert_file: tls/mllp.pem, key_file: /etc/ductus/mllp.key, owner: Infrastructure }\n  api: { cert: CERT, key: '${KEY}' }\n  ca: { ca: CA }\n  ca2: { ca_file: ca.pem }\n",
        )
        .unwrap();
        let mllp = &r.certificates["mllp"];
        // Windows: a root without a drive is the project's drive, and validate says so.
        let drive = &dir.path().to_str().unwrap()[..2];
        let key = PathBuf::from(if cfg!(windows) {
            format!("{drive}/etc/ductus/mllp.key")
        } else {
            "/etc/ductus/mllp.key".into()
        });
        assert_eq!(
            mllp.pem(),
            Some(Pem::Files { cert: &dir.path().join("tls/mllp.pem"), key: &key }),
            "relative files are read from the project folder"
        );
        let without_drive: Vec<_> = r.without_drive.iter().map(|(name, written, _)| (name.as_str(), written)).collect();
        if cfg!(windows) {
            assert_eq!(without_drive, [("mllp", &PathBuf::from("/etc/ductus/mllp.key"))]);
        } else {
            assert!(without_drive.is_empty());
        }
        assert_eq!(
            [&r.certificates["api"], &r.certificates["ca"], &r.certificates["ca2"]].map(CertificateEntry::kind),
            [CertificateKind::Pair, CertificateKind::Ca, CertificateKind::Ca]
        );
        assert_eq!(r.owner("mllp"), Some("Infrastructure"));
        assert!(!format!("{:?}", r.certificates).contains("secret"), "keys stay out of logs");
        assert_eq!(
            r.missing(Kind::Certificate, "x").unwrap(),
            "no certificate named `x` in resources.yaml (defined: api, ca, ca2, mllp)"
        );

        for wrong in ["{ cert_file: a.pem }", "{ cert: C, key_file: k }", "{ cert: C, key: K, ca: A }", "{ owner: me }"]
        {
            let err = load(&format!("certificates:\n  x: {wrong}\n")).unwrap_err();
            assert!(
                err.contains("certificate `x`: give `cert_file` and `key_file`, or `cert` and `key`"),
                "{wrong}: {err}"
            );
        }
    }
}
