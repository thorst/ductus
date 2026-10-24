//! `ductus doctor`: does every connection the project needs work from here? Each is checked
//! step by step (DNS, a TCP connection to every address, TLS, then the protocol's own login),
//! and a failure is explained in plain words: what it most likely means, and what to ask for
//! (a firewall request names this server's address, the target's and the port).
//!
//! The checks go through the engine's own connection code (the same Postgres options, SQL
//! Server pool, SFTP client and HTTP client), so what passes here connects the same way in
//! the engine. Nothing is ever sent to a destination: MLLP and HTTP destinations are only
//! connected to.
//!
//! Every TLS handshake also reports the server's certificate (`Check::certificate`), read even
//! when this server doesn't trust it: the engine keeps these (`certificate_status`) and alerts
//! before they expire. `Mode::Certificates` checks only what has a certificate, and logs in to
//! nothing (what the engine runs on its own every few hours).

use crate::services::PgTls;
use crate::tls::{Certificates, Trust};
use crate::{mssql, services, sftp};
use chrono::{DateTime, Utc};
use ductus_core::InterfaceConfig;
use ductus_core::alerts::{AlertSettings, SmtpTls};
use ductus_core::config::{Destination, Source};
use ductus_core::pgtls::{Security, Setting, SslMode, UrlTls};
use ductus_core::resources::{
    Api, Database, Kind, LdapSecurity, LdapServer, Resources, SftpServer, SqlServerConnection,
};
use ductus_core::tls::ConnectTls;
use ductus_script::TRANSIENT;
use serde::Serialize;
use sqlx::Connection;
use sqlx::postgres::{PgConnectOptions, PgConnection};
use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

pub use ductus_core::settings::DoctorMode as Mode;

/// Commits timed against the engine's database: enough for a stable median, quick to run.
const COMMITS: usize = 20;
/// Warn when a certificate expires within this many days.
pub const EXPIRY_WARNING_DAYS: i64 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Ok,
    Warn,
    Fail,
}

impl Outcome {
    pub fn mark(self) -> &'static str {
        match self {
            Outcome::Ok => "✓",
            Outcome::Warn => "⚠",
            Outcome::Fail => "✗",
        }
    }
}

/// One step of a check: `DNS`, `connect`, `TLS`, `login`, `HTTP`, `commits`, `folder`, `port`.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Step {
    pub name: &'static str,
    pub outcome: Outcome,
    pub detail: String,
}

/// The result for one thing the project connects to.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Check {
    /// `database patient_db`, `api empi`, `destination adt → lab`, `listener`.
    pub what: String,
    /// `epic.local:5000 MLLP`, a folder, `0.0.0.0:6661 MLLP`.
    pub endpoint: String,
    pub used_by: Vec<String>,
    /// The worst step.
    pub outcome: Outcome,
    pub steps: Vec<Step>,
    /// What a failure or warning most likely means, and what to do about it.
    pub advice: Vec<String>,
    /// For a server that speaks TLS: the certificate it presented (or, if none could be read, why).
    pub certificate: Option<SeenCertificate>,
}

/// A server's certificate as a check saw it.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct SeenCertificate {
    /// What it belongs to: `empi` (resources.yaml), `alerts.smtp`, `alerts.<channel>`,
    /// `alerts.heartbeat`, `<interface>/<destination>`.
    pub resource: String,
    pub host: String,
    pub port: u16,
    /// The server certificate's common name (or organisation); none if none was read.
    pub subject: Option<String>,
    pub issuer: Option<String>,
    /// SHA-256 of the server certificate (DER), hex.
    pub fingerprint: Option<String>,
    pub not_before: Option<DateTime<Utc>>,
    pub not_after: Option<DateTime<Utc>>,
    /// The earliest expiry in the chain the server presented.
    pub chain_not_after: Option<DateTime<Utc>>,
    /// Whose that is, when it isn't the server certificate's (an intermediate).
    pub chain_earliest: Option<String>,
    /// This server trusts it (the system's trusted certificates).
    pub trusted: bool,
    /// Why the check failed: couldn't connect, not trusted, expired.
    pub error: Option<String>,
}

/// Something to check.
#[derive(Debug, Clone)]
pub enum Target {
    /// The engine's own database (`DUCTUS_DATABASE_URL`); also times commits.
    Store(String),
    Database(String, Database),
    Sftp(String, SftpServer),
    Api(String, Api),
    /// A directory people sign in with: TLS (ldaps:// or StartTLS) and its certificate; in full mode the service account's bind.
    Ldap(String, LdapServer),
    /// An MLLP receiver: connected to, and with `tls` the handshake the engine would make.
    Mllp {
        host: String,
        port: u16,
        tls: Option<ConnectTls>,
    },
    Http {
        url: String,
        tls: ConnectTls,
    },
    /// The alerts' mail server: connected to (and, with TLS, its certificate read); never logged in to.
    Smtp {
        host: String,
        port: u16,
        tls: SmtpTls,
        trust: ConnectTls,
    },
    /// A local folder a file source reads or a file destination writes. Destinations create
    /// missing folders; sources need them to exist.
    Folder {
        path: PathBuf,
        created_if_missing: bool,
    },
    /// A port we listen on: free to listen on, unless `held` (the engine's own check: it holds
    /// it). With `tls` (the `certificates:` pair it presents), a port in use or held is
    /// connected to, and the chain it serves checked as a sender would see it.
    Listener {
        host: String,
        port: u16,
        protocol: &'static str,
        tls: Option<String>,
        held: bool,
        /// Who may connect: each host name in it is looked up, and whether this check can be
        /// let in is said.
        allow: Option<ductus_core::allow::Allow>,
    },
}

impl Target {
    /// Whether the connection has a certificate to read and watch (TLS). A Postgres database's
    /// only when the engine verifies it (`verify-full` or `verify-ca`): only then does its expiry
    /// stop the engine connecting. Others with TLS are read and described, not watched.
    pub fn has_certificate(&self) -> bool {
        let https = |url: &str| url.trim().to_ascii_lowercase().starts_with("https://");
        match self {
            Target::Api(_, api) => https(&api.url),
            Target::Http { url, .. } => https(url),
            Target::Smtp { tls, .. } => *tls != SmtpTls::None,
            Target::Ldap(_, l) => l.security() != LdapSecurity::Plain,
            Target::Mllp { tls, .. } => tls.is_some(),
            Target::Store(url) => UrlTls::read(url).is_some_and(|u| u.mode.verifies() && !u.socket()),
            Target::Database(_, Database::Postgres(c)) => c.mode().verifies(),
            _ => false,
        }
    }

    /// Where its certificate is: the host and port.
    pub fn address(&self) -> Option<(String, u16)> {
        match self {
            Target::Api(_, Api { url, .. }) | Target::Http { url, .. } => {
                ductus_core::resources::Endpoint::from_url(url).map(|e| (e.host, e.port))
            }
            Target::Smtp { host, port, .. } | Target::Mllp { host, port, .. } => Some((host.clone(), *port)),
            Target::Ldap(_, l) => l.address(),
            Target::Store(url) => UrlTls::read(url).filter(|u| !u.socket()).map(|u| (u.host, u.port)),
            Target::Database(_, Database::Postgres(c)) => Some((c.host.clone(), c.port.unwrap_or(5432))),
            _ => None,
        }
    }

    /// What its TLS trusts and presents, as the engine's connection does: the named CA and
    /// client certificate; for the engine's database, the URL's `sslrootcert`, `sslcert` and
    /// `sslkey` files.
    fn trust(&self, certificates: &Certificates) -> Result<Trust, String> {
        match self {
            Target::Store(url) => {
                let Some(u) = UrlTls::read(url) else { return Ok(Trust::default()) };
                let client = u.cert.as_deref().zip(u.key.as_deref());
                Trust::from_files(u.root_cert.as_deref(), client).map_err(|e| format!("{e:#}"))
            }
            Target::Database(_, Database::Postgres(c)) => {
                certificates.trust(&c.connect_tls()).map_err(|e| format!("{e:#}"))
            }
            t if t.has_certificate() => certificates.trust(&t.connect_tls()).map_err(|e| format!("{e:#}")),
            _ => Ok(Trust::default()),
        }
    }

    /// The CA and client certificate its TLS names (the same the engine's connection uses).
    fn connect_tls(&self) -> ConnectTls {
        match self {
            Target::Api(_, api) => api.tls(),
            Target::Ldap(_, l) => ConnectTls::new(&l.ca, &None),
            Target::Http { tls, .. } | Target::Smtp { trust: tls, .. } | Target::Mllp { tls: Some(tls), .. } => {
                tls.clone()
            }
            _ => ConnectTls::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Probe {
    pub what: String,
    pub target: Target,
    pub used_by: Vec<String>,
    /// The name its certificate is kept under (see `SeenCertificate::resource`).
    pub resource: Option<String>,
}

/// The whole report: what `ductus doctor` prints and `--report` saves.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Report {
    pub generated_at: String,
    pub environment: String,
    pub host: String,
    pub version: String,
    pub seconds: f64,
    pub checks: Vec<Check>,
}

impl Report {
    pub fn failed(&self) -> usize {
        self.checks.iter().filter(|c| c.outcome == Outcome::Fail).count()
    }

    pub fn to_text(&self) -> String {
        use std::fmt::Write;
        let count = |o: Outcome| self.checks.iter().filter(|c| c.outcome == o).count();
        let mut out = String::new();
        let _ = writeln!(out, "ductus doctor · environment {} · {}", self.environment, self.generated_at);
        let _ = writeln!(out, "on {} · ductus {}", self.host, self.version);
        let _ = writeln!(
            out,
            "checked {} connection(s) in {:.1} s: {} ok, {} warning(s), {} failed",
            self.checks.len(),
            self.seconds,
            count(Outcome::Ok),
            count(Outcome::Warn),
            count(Outcome::Fail)
        );
        for c in &self.checks {
            let _ = writeln!(out, "\n{} {}  {}", c.outcome.mark(), c.what, c.endpoint);
            if !c.used_by.is_empty() {
                let _ = writeln!(out, "    {:<8} {}", "used by", c.used_by.join(", "));
            }
            for s in &c.steps {
                let mark = if s.outcome == Outcome::Ok { " " } else { s.outcome.mark() };
                let _ = writeln!(out, "  {mark} {:<8} {}", s.name, s.detail);
            }
            for a in &c.advice {
                let _ = writeln!(out, "  → {a}");
            }
        }
        out
    }
}

/// Check everything at once (so firewalls that drop traffic don't add up their timeouts) and
/// return the results in the order given. `timeout` bounds each step. `certificates`: what TLS
/// connections trust and present, as the engine loads them.
pub async fn check(probes: Vec<Probe>, timeout: Duration, certificates: &Arc<Certificates>) -> Vec<Check> {
    check_with(probes, timeout, Mode::Full, certificates).await
}

/// `check`, in a mode: `Certificates` checks only what has a certificate (see `certificates_only`)
/// and stops after the TLS handshake (an API's GET is left out too).
pub async fn check_with(
    probes: Vec<Probe>,
    timeout: Duration,
    mode: Mode,
    certificates: &Arc<Certificates>,
) -> Vec<Check> {
    let mut tasks = tokio::task::JoinSet::new();
    for (n, probe) in probes.into_iter().enumerate() {
        let certificates = certificates.clone();
        tasks.spawn(async move { (n, run(probe, timeout, mode, &certificates).await) });
    }
    let mut checks: Vec<(usize, Check)> = tasks.join_all().await;
    checks.sort_by_key(|(n, _)| *n);
    checks.into_iter().map(|(_, c)| c).collect()
}

/// The probes a certificates-only run checks: the connections with a certificate.
pub fn certificates_only(probes: Vec<Probe>) -> Vec<Probe> {
    probes.into_iter().filter(|p| p.target.has_certificate()).collect()
}

async fn run(probe: Probe, timeout: Duration, mode: Mode, certificates: &Certificates) -> Check {
    let trust = probe.target.trust(certificates);
    let mut r = Run {
        steps: Vec::new(),
        advice: Vec::new(),
        timeout,
        from: None,
        seen: None,
        trust,
        certificates,
        name_unchecked: false,
    };
    let endpoint = match &probe.target {
        Target::Store(url) => r.store(url, mode).await,
        Target::Database(_, Database::Postgres(c)) => {
            let port = c.port.unwrap_or(5432);
            let endpoint = format!("{}:{port} Postgres", c.host);
            let Some(address) = r.connectable(&c.host, port).await else { return r.check(probe, endpoint) };
            let tls_ok = !c.mode().tries_tls() || r.postgres_tls(&c.host, address, c.mode(), Setting::Resource).await;
            if tls_ok && mode == Mode::Full {
                match PgTls::load(c, certificates) {
                    Ok(tls) => {
                        r.postgres_login(&services::postgres_options(c, &tls), c.mode(), Setting::Resource).await;
                    }
                    Err(e) => r.step("login", Outcome::Fail, format!("{e:#}")),
                }
            }
            endpoint
        }
        Target::Database(_, Database::Sqlserver(c)) => r.sql_server(c).await,
        Target::Sftp(name, s) => {
            if r.reach(&s.host, s.port).await {
                r.sftp_login(name, s).await;
            }
            format!("{}:{} SFTP", s.host, s.port)
        }
        Target::Api(_, api) => r.http(&api.url, (mode == Mode::Full).then_some(api)).await,
        Target::Http { url, .. } => r.http(url, None).await,
        Target::Smtp { host, port, tls, .. } => {
            if let Some(address) = r.connectable(host, *port).await {
                match tls {
                    SmtpTls::Tls => {
                        r.tls(host, address).await;
                    }
                    SmtpTls::Starttls => {
                        if let Some(stream) = r.starttls(address).await {
                            r.tls_on(host, stream).await;
                        }
                    }
                    SmtpTls::None => r.smtp_greeting(address).await,
                }
            }
            format!("{host}:{port} SMTP")
        }
        Target::Ldap(name, l) => r.ldap(name, l, mode).await,
        Target::Mllp { host, port, tls } => {
            if let Some(address) = r.connectable(host, *port).await
                && tls.is_some()
            {
                r.tls(host, address).await;
            }
            format!("{host}:{port} MLLP{}", if tls.is_some() { " over TLS" } else { "" })
        }
        Target::Folder { path, created_if_missing } => {
            r.folder(path, *created_if_missing);
            path.display().to_string()
        }
        Target::Listener { host, port, protocol, tls, held, allow } => {
            let in_use = *held || r.listen(host, *port).await;
            let let_in = match allow {
                Some(allow) => r.allow(allow, *port, in_use).await,
                None => true,
            };
            if let Some(name) = tls.as_deref().filter(|_| in_use && let_in) {
                r.served(host, *port, name).await;
            }
            format!("{host}:{port} {protocol}")
        }
    };
    r.check(probe, endpoint)
}

/// What a TLS handshake showed.
#[derive(Default)]
struct Seen {
    leaf: Option<Certificate>,
    chain_not_after: Option<DateTime<Utc>>,
    chain_earliest: Option<String>,
    trusted: bool,
    error: Option<String>,
}

/// The steps of one check as they happen.
struct Run<'a> {
    steps: Vec<Step>,
    advice: Vec<String>,
    timeout: Duration,
    /// This server's address towards the target (for firewall requests and pg_hba advice).
    from: Option<IpAddr>,
    /// The certificate a TLS handshake showed.
    seen: Option<Seen>,
    /// What the target's TLS trusts and presents (its `ca` and `certificate`), or why that
    /// can't be had (a certificate that didn't load).
    trust: Result<Trust, String>,
    certificates: &'a Certificates,
    /// The handshake checks the CA, not the name (Postgres's `verify-ca`).
    name_unchecked: bool,
}

impl Run<'_> {
    /// The check, from the steps so far: the worst step, and the certificate a handshake read.
    fn check(mut self, probe: Probe, endpoint: String) -> Check {
        let outcome = self.steps.iter().map(|s| s.outcome).max().unwrap_or(Outcome::Ok);
        let certificate = probe.target.address().filter(|_| probe.target.has_certificate()).map(|(host, port)| {
            let failed = self.steps.iter().find(|s| s.outcome == Outcome::Fail).map(|s| s.detail.clone());
            let seen = self.seen.take().unwrap_or_default();
            SeenCertificate {
                resource: probe.resource.clone().unwrap_or_else(|| probe.what.clone()),
                host,
                port,
                subject: seen.leaf.as_ref().map(|c| c.subject.clone()),
                issuer: seen.leaf.as_ref().map(|c| c.issuer.clone()),
                fingerprint: seen.leaf.as_ref().map(|c| c.fingerprint.clone()),
                not_before: seen.leaf.as_ref().map(|c| c.not_before),
                not_after: seen.leaf.as_ref().map(|c| c.not_after),
                chain_not_after: seen.chain_not_after,
                chain_earliest: seen.chain_earliest,
                trusted: seen.trusted,
                error: seen.error.or(failed),
            }
        });
        Check {
            what: probe.what,
            endpoint,
            used_by: probe.used_by,
            outcome,
            steps: self.steps,
            advice: self.advice,
            certificate,
        }
    }

    fn step(&mut self, name: &'static str, outcome: Outcome, detail: impl Into<String>) {
        self.steps.push(Step { name, outcome, detail: detail.into() });
    }

    fn advise(&mut self, text: impl Into<String>) {
        self.advice.push(text.into());
    }

    fn waited(&self) -> String {
        took(self.timeout)
    }

    /// DNS, then a TCP connection to every address. True if any address accepted.
    async fn reach(&mut self, host: &str, port: u16) -> bool {
        self.connectable(host, port).await.is_some()
    }

    /// Like `reach`, returning the first address that accepted.
    async fn connectable(&mut self, host: &str, port: u16) -> Option<SocketAddr> {
        let started = Instant::now();
        let addresses: Vec<SocketAddr> = match tokio::time::timeout(self.timeout, tokio::net::lookup_host((host, port)))
            .await
        {
            Ok(Ok(found)) => {
                let mut all: Vec<SocketAddr> = found.collect();
                all.dedup();
                all
            }
            Ok(Err(e)) => {
                self.step("DNS", Outcome::Fail, format!("{host} doesn't resolve: {e}"));
                self.advise(format!(
                    "This server can't look up `{host}`: check the spelling, or ask for a DNS entry (names on the far side of a VPN often need the VPN's DNS)."
                ));
                return None;
            }
            Err(_) => {
                self.step("DNS", Outcome::Fail, format!("no answer for {host} in {}", self.waited()));
                self.advise("The DNS server didn't answer: check this server's DNS settings.");
                return None;
            }
        };
        if host.parse::<IpAddr>().is_ok() {
            self.step("address", Outcome::Ok, format!("{host} (an address, nothing to look up)"));
        } else {
            let list: Vec<String> = addresses.iter().map(|a| a.ip().to_string()).collect();
            self.step("DNS", Outcome::Ok, format!("{host} → {} ({})", list.join(", "), took(started.elapsed())));
        }

        let mut first_ok = None;
        let mut failures = Vec::new();
        let mut results = Vec::new();
        for address in &addresses {
            let from = source_address(*address);
            let started = Instant::now();
            // Registered as our own: one of our listeners lets it in and doesn't log it.
            let attempt = tokio::time::timeout(self.timeout, crate::tls::connect_own(*address)).await;
            let at = match from {
                Some(ip) => format!("{} from {ip}", show(address)),
                None => show(address),
            };
            match attempt {
                Ok(Ok(stream)) => {
                    let local = stream.local_addr().ok().map(|a| a.ip()).or(from);
                    self.from = self.from.or(local);
                    let at = match local {
                        Some(ip) => format!("{} from {ip}", show(address)),
                        None => show(address),
                    };
                    results.push((Outcome::Ok, format!("{at} in {}", took(started.elapsed()))));
                    first_ok = first_ok.or(Some(*address));
                }
                Ok(Err(e)) => {
                    let failure = Failure::of(&e);
                    results.push((Outcome::Fail, format!("{at}: {}", failure.describe(&e))));
                    failures.push((failure, *address, from));
                }
                Err(_) => {
                    results.push((Outcome::Fail, format!("{at}: no answer in {}", self.waited())));
                    failures.push((Failure::Timeout, *address, from));
                }
            }
            self.from = self.from.or(from);
        }
        // An address that fails while another works is a warning: the engine may pick it.
        for (outcome, detail) in results {
            let outcome = if outcome == Outcome::Fail && first_ok.is_some() { Outcome::Warn } else { outcome };
            self.step("connect", outcome, detail);
        }
        match (first_ok, failures.first()) {
            (Some(_), Some(_)) => self.advise(format!(
                "Some of {host}'s addresses don't accept connections: connecting may be slow or fail now and then. Ask for the DNS entry to list only working addresses, or for the others to be opened."
            )),
            (None, Some((failure, address, from))) => {
                let advice = failure.advice(host, *address, *from);
                self.advise(advice);
            }
            _ => {}
        }
        first_ok
    }

    /// The engine's database: TLS and its certificate, login, then how long a durable commit
    /// takes. In `Certificates` mode only TLS (nothing logs in).
    async fn store(&mut self, url: &str, mode: Mode) -> String {
        let options: PgConnectOptions = match ductus_store::tls::connect_url(url).parse() {
            Ok(o) => o,
            Err(e) => {
                self.step("URL", Outcome::Fail, format!("not a Postgres URL: {e}"));
                return "(DUCTUS_DATABASE_URL)".into();
            }
        };
        let ssl = UrlTls::read(url).map(|u| u.mode).unwrap_or_default();
        let (host, port) = (options.get_host().to_string(), options.get_port());
        let endpoint = format!("{host}:{port} Postgres");
        if host.starts_with('/') {
            self.step(
                "socket",
                Outcome::Ok,
                format!("local socket in {host} (no TLS there: nothing crosses a network)"),
            );
        } else {
            let Some(address) = self.connectable(&host, port).await else { return endpoint };
            if ssl.tries_tls() && !self.postgres_tls(&host, address, ssl, Setting::Url).await {
                return endpoint;
            }
        }
        if mode == Mode::Certificates {
            return endpoint;
        }
        let Some(mut connection) = self.postgres_login(&options, ssl, Setting::Url).await else { return endpoint };
        let timed = tokio::time::timeout(self.timeout * 4, commit_times(&mut connection)).await;
        match timed {
            Ok(Ok((median, synchronous))) => {
                let per_second = 1.0 / median.as_secs_f64().max(1e-6);
                let mut detail = format!(
                    "{} median of {COMMITS} (each waits for the disk): strict ingest ≈ {} msg/s per sender connection, one commit per ACK; more connections share commits. A 1M/day ADT feed averages ~12 msg/s.",
                    took(median),
                    round(per_second)
                );
                if synchronous != "on" {
                    detail = format!(
                        "{} median of {COMMITS} (synchronous_commit = {synchronous}: commits don't wait for the disk)",
                        took(median)
                    );
                }
                let slow = median > Duration::from_millis(10) && synchronous == "on";
                self.step("commits", if slow { Outcome::Warn } else { Outcome::Ok }, detail);
                if slow {
                    self.advise("Commits are slow (over 10 ms): the database's disk flush is the limit for strict ingest. A faster disk (SSD/NVMe, write cache with battery), or `durability: relaxed` for interfaces that can afford it, raise it.");
                }
            }
            Ok(Err(e)) => self.step("commits", Outcome::Fail, format!("couldn't time commits: {e}")),
            Err(_) => {
                self.step("commits", Outcome::Fail, format!("{COMMITS} commits took over {}", took(self.timeout * 4)))
            }
        }
        endpoint
    }

    /// Postgres's TLS by hand, as its clients start it (the 8-byte SSLRequest; the server answers
    /// `S` and the handshake follows, or `N`), so its certificate is read and judged however the
    /// connection is set. A connection that doesn't verify (`prefer`, `require`) is said to be
    /// unverified, with what `verify-full` would make of the certificate. False when the
    /// connection can't work (a verified handshake refused, TLS required but not offered).
    async fn postgres_tls(&mut self, host: &str, address: SocketAddr, mode: SslMode, setting: Setting) -> bool {
        use tokio::io::AsyncReadExt;
        let said = setting.written(mode);
        let full = setting.written(SslMode::VerifyFull);
        let local = ductus_core::pgtls::local(host);
        let unverified = if local { Outcome::Ok } else { Outcome::Warn };
        let asked = async {
            let mut tcp = TcpStream::connect(address).await?;
            tcp.write_all(&SSL_REQUEST).await?;
            let mut answer = [0u8; 1];
            tcp.read_exact(&mut answer).await?;
            Ok::<_, std::io::Error>((tcp, answer[0]))
        };
        let tcp = match tokio::time::timeout(self.timeout, asked).await {
            Ok(Ok((tcp, b'S'))) => tcp,
            Ok(Ok((_, b'N'))) if mode.requires_tls() => {
                self.step("TLS", Outcome::Fail, format!("the server doesn't offer TLS, and {said} needs it"));
                self.advise(
                    "Ask the database's DBA to turn on ssl (postgresql.conf), with a certificate for this host name.",
                );
                return false;
            }
            Ok(Ok((_, b'N'))) => {
                self.step(
                    "TLS",
                    unverified,
                    format!("none: the server doesn't offer TLS ({said}), the connection is plain"),
                );
                if !local {
                    self.advise(format!("Queries, their results and the login cross the network readable: ask the DBA to turn on ssl, then set {full}."));
                }
                return true;
            }
            Ok(Ok((_, other))) => {
                self.step(
                    "TLS",
                    Outcome::Fail,
                    format!("the answer to Postgres's SSLRequest is 0x{other:02x}, not S or N: is this Postgres?"),
                );
                return false;
            }
            Ok(Err(e)) => {
                self.step("TLS", Outcome::Fail, format!("asking for TLS: {e}"));
                return false;
            }
            Err(_) => {
                self.step("TLS", Outcome::Fail, format!("no answer to Postgres's SSLRequest in {}", self.waited()));
                return false;
            }
        };
        let advice = self.advice.len();
        self.name_unchecked = mode == SslMode::VerifyCa;
        let trusted = self.tls_on(host, tcp).await;
        if mode.verifies() {
            if setting == Setting::Url {
                // The engine's database names its CA in its URL, not in resources.yaml.
                for a in &mut self.advice[advice..] {
                    *a = a.replace(
                        "Name the issuing CA in `ca` (a `certificates:` entry in resources.yaml)",
                        "Give the issuing CA as sslrootcert=/path/ca.pem in DUCTUS_DATABASE_URL",
                    );
                }
            }
            return trusted;
        }
        // The connection checks nothing: what the handshake showed is what verify-full would do.
        let shown = self.steps.pop().map(|s| s.detail).unwrap_or_default();
        self.advice.truncate(advice);
        if trusted {
            self.step("TLS", unverified, format!("{shown}; not verified by the engine ({said}): {full} would pass"));
            if !local {
                self.advise(format!("This server already trusts the database's certificate: set {full} to have it checked at every connection."));
            }
        } else {
            self.step("TLS", unverified, format!("not verified ({said}), and {full} would refuse it: {shown}"));
            if !local {
                let ca = match setting {
                    Setting::Url => "sslrootcert=/path/ca.pem in DUCTUS_DATABASE_URL",
                    Setting::Resource => "`ca:` (a `certificates:` entry)",
                };
                self.advise(format!("To have it checked, trust the CA that issued it ({ca}, or install it on this server) and set {full}."));
            }
        }
        true
    }

    async fn postgres_login(
        &mut self,
        options: &PgConnectOptions,
        mode: SslMode,
        setting: Setting,
    ) -> Option<PgConnection> {
        let user = options.get_username().to_string();
        let database = options.get_database().unwrap_or(&user).to_string();
        let attempt = tokio::time::timeout(self.timeout, PgConnection::connect_with(options)).await;
        if let Ok(Err(e)) = &attempt
            && let Some((words, _)) = ductus_store::tls::refusal(e, options.get_host())
        {
            self.step("login", Outcome::Fail, format!("TLS: {words}"));
            return None;
        }
        let mut connection = match attempt {
            Ok(Ok(c)) => c,
            Ok(Err(sqlx::Error::Database(e))) => {
                let code = e.code().map(|c| c.to_string()).unwrap_or_default();
                self.step("login", Outcome::Fail, e.message().to_string());
                let from = self.from.map(|ip| ip.to_string()).unwrap_or_else(|| "this server".into());
                match code.as_str() {
                    "28P01" => self.advise(format!(
                        "The password for `{user}` was refused: check the secret this environment uses for it."
                    )),
                    "28000" => self.advise(format!(
                        "The server doesn't allow `{user}` to connect from {from} (pg_hba.conf): ask its DBA to allow it."
                    )),
                    "3D000" => self.advise(format!("There's no database `{database}` on that server: check its name.")),
                    _ => {}
                }
                return None;
            }
            Ok(Err(e)) => {
                self.step("login", Outcome::Fail, e.to_string());
                return None;
            }
            Err(_) => {
                self.step(
                    "login",
                    Outcome::Fail,
                    format!("the server accepted the connection but didn't answer in {}", self.waited()),
                );
                return None;
            }
        };
        let version: Result<String, _> = sqlx::query_scalar("SHOW server_version").fetch_one(&mut connection).await;
        let version = version.map(|v| format!(": PostgreSQL {v}")).unwrap_or_default();
        let tls = ductus_store::tls::version(&mut connection).await.ok().flatten();
        let security = Security::new(mode, tls.as_deref(), options.get_host(), setting);
        self.step("login", Outcome::Ok, format!("as `{user}`, database `{database}`{version}; {security}"));
        Some(connection)
    }

    async fn sql_server(&mut self, c: &SqlServerConnection) -> String {
        let browse = c.instance.is_some() && c.port.is_none();
        let endpoint = if browse {
            format!(
                "{}\\{} SQL Server (port from the SQL Server Browser, UDP 1434)",
                c.host,
                c.instance.as_deref().unwrap_or("")
            )
        } else {
            format!("{}:{} SQL Server", c.host, c.port.unwrap_or(1433))
        };
        if browse {
            // The Browser answers over UDP, which can't be "connected" to: the login below
            // asks it for the instance's port and connects there.
            let found = tokio::time::timeout(self.timeout, tokio::net::lookup_host((c.host.as_str(), 1434))).await;
            match found {
                Ok(Ok(a)) => {
                    let list: Vec<String> = a.map(|a| a.ip().to_string()).collect();
                    self.step("DNS", Outcome::Ok, format!("{} → {}", c.host, list.join(", ")));
                }
                _ => {
                    self.step("DNS", Outcome::Fail, format!("{} doesn't resolve", c.host));
                    self.advise(format!(
                        "This server can't look up `{}`: check the name, or ask for a DNS entry.",
                        c.host
                    ));
                    return endpoint;
                }
            }
        } else if !self.reach(&c.host, c.port.unwrap_or(1433)).await {
            return endpoint;
        }
        let pool = mssql::Pool::new(&SqlServerConnection { max_connections: 1, ..c.clone() });
        match tokio::time::timeout(self.timeout, pool.query("SELECT @@VERSION AS version", Vec::new())).await {
            Ok(Ok(rows)) => {
                let version =
                    rows.first().and_then(|r| r["version"].as_str()).and_then(|v| v.lines().next()).unwrap_or("");
                self.step(
                    "login",
                    Outcome::Ok,
                    format!("as `{}`, database `{}`: {}", c.user, c.database, version.trim()),
                );
            }
            Ok(Err(e)) => {
                let message =
                    e.to_string().replace(TRANSIENT, "").replace("database unavailable: ", "").trim().to_string();
                if message.contains("Login failed") {
                    self.advise(format!("The login `{}` was refused: check its password (secret) for this environment, and that SQL Server logins are enabled on the server.", c.user));
                } else if message.contains("Cannot open database") {
                    self.advise(format!("The login works but can't open `{}`: check the name, and that the login is mapped to that database.", c.database));
                } else if browse {
                    self.advise("Finding a named instance needs the SQL Server Browser (UDP 1434) and then the instance's own TCP port: ask the DBA for that port and set `port:` to skip the Browser.");
                }
                self.step("login", Outcome::Fail, message);
            }
            Err(_) => self.step("login", Outcome::Fail, format!("no answer in {}", self.waited())),
        }
        endpoint
    }

    async fn sftp_login(&mut self, name: &str, server: &SftpServer) {
        let mut connection = sftp::Connection::new(name, server.clone());
        match tokio::time::timeout(self.timeout, connection.sftp()).await {
            Ok(Ok(_)) => {
                let key = if server.host_key.as_deref().map(str::trim) == Some("any") {
                    self.advise("`host_key: any` accepts any server, including an impostor: set the real fingerprint outside test setups.");
                    (Outcome::Warn, "host key NOT checked (host_key: any)")
                } else {
                    (Outcome::Ok, "host key matches")
                };
                self.step("login", key.0, format!("{}; logged in as `{}`; SFTP ready", key.1, server.user));
            }
            Ok(Err(e)) => {
                let message = format!("{e:#}");
                if message.contains("has host key") {
                    self.advise("Confirm that fingerprint with the server's owner (not from this output alone) before adding it to resources.yaml.");
                } else if message.contains("refused the login") {
                    self.advise(format!("Check the user `{}` and its key or password (secret) for this environment with the server's owner.", server.user));
                }
                self.step("login", Outcome::Fail, message);
            }
            Err(_) => self.step("login", Outcome::Fail, format!("no SSH answer in {}", self.waited())),
        }
    }

    /// An HTTP(S) endpoint: connection and TLS; for an API also a GET of its URL with its
    /// headers (the status only). Destinations are never sent anything.
    async fn http(&mut self, url: &str, api: Option<&Api>) -> String {
        let Some(e) = ductus_core::resources::Endpoint::from_url(url) else {
            self.step("URL", Outcome::Fail, format!("not an http(s) URL: {url}"));
            return url.to_string();
        };
        let endpoint = e.to_string();
        let Some(address) = self.connectable(&e.host, e.port).await else { return endpoint };
        if e.protocol == "HTTPS" && !self.tls(&e.host, address).await {
            return endpoint;
        }
        let Some(api) = api else { return endpoint };
        let client = match services::api_client(api, self.certificates) {
            Ok(c) => c,
            Err(e) => {
                self.step("HTTP", Outcome::Fail, format!("{e:#}"));
                return endpoint;
            }
        };
        let mut request = client.get(url).timeout(self.timeout);
        for (name, value) in &api.headers {
            request = request.header(name, value);
        }
        let started = Instant::now();
        match request.send().await {
            Ok(response) => {
                let status = response.status();
                let detail = format!("GET {} → {status} in {}", strip_query(url), took(started.elapsed()));
                match status.as_u16() {
                    401 | 403 => {
                        self.step("HTTP", Outcome::Fail, detail);
                        self.advise("The API refused the request's credentials: check its headers in resources.yaml (the token secret for this environment).");
                    }
                    500.. => {
                        self.step("HTTP", Outcome::Warn, detail);
                        self.advise("The API answered with a server error: it's reachable, but ask its owner whether it's healthy.");
                    }
                    _ => self.step("HTTP", Outcome::Ok, detail),
                }
            }
            Err(e) => self.step("HTTP", Outcome::Fail, format!("GET {}: {}", strip_query(url), e.without_url())),
        }
        endpoint
    }

    /// A TLS handshake, verified the way the engine's connection verifies (`Trust`: the system's
    /// trusted certificates plus the named CA, presenting the named client certificate),
    /// reporting the server's certificate either way.
    async fn tls(&mut self, host: &str, address: SocketAddr) -> bool {
        match tokio::time::timeout(self.timeout, TcpStream::connect(address)).await {
            Ok(Ok(tcp)) => self.tls_on(host, tcp).await,
            Ok(Err(e)) => {
                self.step("TLS", Outcome::Fail, format!("handshake failed: {e}"));
                false
            }
            Err(_) => {
                self.step("TLS", Outcome::Fail, format!("no TLS answer in {}", self.waited()));
                false
            }
        }
    }

    /// The TLS handshake on a connection that is ready for it (fresh, or after STARTTLS).
    async fn tls_on<S: AsyncRead + AsyncWrite + Unpin>(&mut self, host: &str, stream: S) -> bool {
        use rustls::pki_types::ServerName;
        let trust = match &self.trust {
            Ok(trust) => trust.clone(),
            Err(why) => {
                let why = why.clone();
                self.step("TLS", Outcome::Fail, why);
                self.advise("Fix the certificate named in `ca` or `certificate` (`ductus validate` checks every one).");
                return false;
            }
        };
        let presented = Arc::new(Mutex::new(Vec::new()));
        let verifier = match trust.verifier() {
            Ok(v) => Recording { inner: v, seen: presented.clone(), name_unchecked: self.name_unchecked },
            Err(e) => {
                self.step("TLS", Outcome::Fail, format!("{e:#}"));
                return false;
            }
        };
        let config = trust.config_with(Arc::new(verifier));
        let Ok(name) = ServerName::try_from(host.to_string()) else {
            self.step("TLS", Outcome::Fail, format!("`{host}` isn't a valid name for TLS"));
            return false;
        };
        let handshake = tokio_rustls::TlsConnector::from(Arc::new(config)).connect(name, stream);
        let result = tokio::time::timeout(self.timeout, handshake).await;
        // The verifier records the chain before it judges it: read even when it isn't trusted.
        let chain = Chain::read(presented.lock().expect("certificate lock").as_slice());
        let mut seen = Seen { chain_not_after: chain.not_after, chain_earliest: chain.earliest, ..Default::default() };
        let leaf = chain.leaf;
        let about = leaf.as_ref().map(Certificate::describe).unwrap_or_default();
        let trusted = match result {
            Ok(Ok(mut stream)) => {
                let version = negotiated(stream.get_ref().1);
                let client = trust.client_pair().map(|p| format!("; presented client certificate `{}`", p.name));
                let client = client.unwrap_or_default();
                let days = seen.chain_not_after.map(|t| (t - Utc::now()).num_days()).unwrap_or(i64::MAX);
                // With TLS 1.3 the server judges our client certificate (or its absence) after
                // the handshake has finished on our side: its refusal arrives with the first read.
                if let Some(refused) = refused_after(&mut stream, self.timeout.min(REFUSAL_WAIT)).await {
                    seen.error = Some(refused.clone());
                    self.step("TLS", Outcome::Fail, format!("{version}, server trusted; {refused}{client}"));
                    self.advise(match trust.client_pair() {
                        Some(p) => format!("The server refused the client certificate `{}`: ask its owner which CA it trusts for clients, and whether this certificate's subject is allowed.", p.name),
                        None => "The server asks for a client certificate: name one in `certificate` (`tls: { certificate: ... }` for MLLP).".into(),
                    });
                    seen.trusted = true;
                    seen.leaf = leaf;
                    self.seen = Some(seen);
                    return false;
                }
                if days < EXPIRY_WARNING_DAYS {
                    self.step("TLS", Outcome::Warn, format!("{version}, trusted; {about}{client}"));
                    let whose = seen.chain_earliest.as_ref().map(|s| format!(" (its chain's {s})")).unwrap_or_default();
                    self.advise(format!(
                        "The certificate{whose} expires in {days} day(s): ask the server's owner to renew it before then."
                    ));
                } else {
                    self.step("TLS", Outcome::Ok, format!("{version}, trusted; {about}{client}"));
                }
                true
            }
            Ok(Err(e)) => {
                let tls = e.get_ref().and_then(|i| i.downcast_ref::<rustls::Error>()).cloned();
                let (detail, advice) = tls_failure(host, tls.as_ref(), &e, leaf.as_ref());
                seen.error = Some(detail.clone());
                self.step("TLS", Outcome::Fail, if about.is_empty() { detail } else { format!("{detail}; {about}") });
                self.advise(advice);
                false
            }
            Err(_) => {
                self.step("TLS", Outcome::Fail, format!("no TLS answer in {}", self.waited()));
                self.advise("The port accepts connections but doesn't speak TLS (in time): is it plain http, or the wrong port?");
                false
            }
        };
        seen.trusted = trusted;
        seen.leaf = leaf;
        self.seen = Some(seen);
        trusted
    }

    /// SMTP up to STARTTLS (greeting, EHLO, STARTTLS): the connection, ready for the handshake.
    /// Never logs in and never sends mail.
    async fn starttls(&mut self, address: SocketAddr) -> Option<TcpStream> {
        let dialogue = async {
            let mut smtp = tokio::io::BufReader::new(TcpStream::connect(address).await?);
            let greeting = smtp_reply(&mut smtp).await?;
            if !greeting.starts_with("220") {
                return Ok(Err(format!("the server greeted with `{}`", first(&greeting))));
            }
            smtp.get_mut().write_all(b"EHLO ductus-doctor\r\n").await?;
            let ehlo = smtp_reply(&mut smtp).await?;
            if !ehlo.starts_with("250") {
                return Ok(Err(format!("EHLO was answered `{}`", first(&ehlo))));
            }
            if !ehlo.to_ascii_uppercase().contains("STARTTLS") {
                return Ok(Err("the server doesn't offer STARTTLS".to_string()));
            }
            smtp.get_mut().write_all(b"STARTTLS\r\n").await?;
            let ready = smtp_reply(&mut smtp).await?;
            if !ready.starts_with("220") {
                return Ok(Err(format!("STARTTLS was answered `{}`", first(&ready))));
            }
            Ok::<_, std::io::Error>(Ok((smtp.into_inner(), first(&greeting))))
        };
        match tokio::time::timeout(self.timeout, dialogue).await {
            Ok(Ok(Ok((stream, greeting)))) => {
                self.step("SMTP", Outcome::Ok, format!("{greeting}; STARTTLS offered and accepted"));
                Some(stream)
            }
            Ok(Ok(Err(why))) => {
                self.step("SMTP", Outcome::Fail, why);
                self.advise("Alert mail is sent with STARTTLS (`tls: starttls`): use the port that offers it (usually 587), or `tls: tls` for a server that speaks TLS from the start (465).");
                None
            }
            Ok(Err(e)) => {
                self.step("SMTP", Outcome::Fail, e.to_string());
                None
            }
            Err(_) => {
                self.step("SMTP", Outcome::Fail, format!("no SMTP answer in {}", self.waited()));
                None
            }
        }
    }

    /// A directory: DNS, TCP, TLS (ldaps://, or StartTLS on ldap://) and its certificate; in
    /// full mode also the service account's bind (never a person's).
    async fn ldap(&mut self, name: &str, l: &LdapServer, mode: Mode) -> String {
        let Some((host, port)) = l.address() else {
            self.step("URL", Outcome::Fail, format!("not an ldap(s):// URL: {}", l.url));
            return l.url.clone();
        };
        let security = l.security();
        let endpoint = format!("{host}:{port} {}", if security == LdapSecurity::Tls { "LDAPS" } else { "LDAP" });
        let Some(address) = self.connectable(&host, port).await else { return endpoint };
        let secure = match security {
            LdapSecurity::Tls => self.tls(&host, address).await,
            LdapSecurity::StartTls => match self.ldap_starttls(address).await {
                Some(stream) => self.tls_on(&host, stream).await,
                None => false,
            },
            LdapSecurity::Plain => {
                self.step("TLS", Outcome::Warn, "none: plain ldap:// (`insecure: true`)");
                self.advise("Passwords cross the network readable: use ldaps:// (636) or `starttls: true` outside a test directory.");
                true
            }
        };
        if secure && mode == Mode::Full {
            let trust = self.trust.clone().unwrap_or_default();
            match crate::ldap::check_service_account(l, &trust).await {
                Ok(()) => {
                    let who = l.bind_dn.as_deref().map(|d| format!("as `{d}`")).unwrap_or_else(|| "anonymously".into());
                    self.step("login", Outcome::Ok, format!("bound {who}"));
                }
                Err(e) => {
                    self.step("login", Outcome::Fail, format!("{e:#}"));
                    self.advise(format!("Check `bind_dn` and `bind_password` (a secret) of ldap `{name}` for this environment with the directory's owners."));
                }
            }
        }
        endpoint
    }

    /// LDAP up to StartTLS (the extended operation, written by hand like SMTP's): the
    /// connection, ready for the handshake. Nothing is bound or searched.
    async fn ldap_starttls(&mut self, address: SocketAddr) -> Option<TcpStream> {
        let dialogue = async {
            let mut tcp = TcpStream::connect(address).await?;
            tcp.write_all(&STARTTLS_REQUEST).await?;
            let reply = read_ldap_message(&mut tcp).await?;
            Ok::<_, std::io::Error>((tcp, starttls_result(&reply)))
        };
        match tokio::time::timeout(self.timeout, dialogue).await {
            Ok(Ok((stream, Some((0, _))))) => {
                self.step("LDAP", Outcome::Ok, "StartTLS accepted");
                Some(stream)
            }
            Ok(Ok((_, Some((code, text))))) => {
                let text = if text.is_empty() { String::new() } else { format!(": {text}") };
                self.step("LDAP", Outcome::Fail, format!("StartTLS refused (result {code}){text}"));
                self.advise("The directory doesn't offer StartTLS on this port: use ldaps:// (usually 636), or ask its owners to enable StartTLS.");
                None
            }
            Ok(Ok((_, None))) => {
                self.step("LDAP", Outcome::Fail, "the answer to StartTLS isn't LDAP: is this the right port?");
                None
            }
            Ok(Err(e)) => {
                self.step("LDAP", Outcome::Fail, e.to_string());
                None
            }
            Err(_) => {
                self.step("LDAP", Outcome::Fail, format!("no LDAP answer in {}", self.waited()));
                None
            }
        }
    }

    /// A plain SMTP server's greeting (`tls: none`).
    async fn smtp_greeting(&mut self, address: SocketAddr) {
        let greeting = async {
            let mut smtp = tokio::io::BufReader::new(TcpStream::connect(address).await?);
            let reply = smtp_reply(&mut smtp).await?;
            let _ = smtp.get_mut().write_all(b"QUIT\r\n").await;
            Ok::<_, std::io::Error>(reply)
        };
        match tokio::time::timeout(self.timeout, greeting).await {
            Ok(Ok(reply)) if reply.starts_with("220") => self.step("SMTP", Outcome::Ok, first(&reply)),
            Ok(Ok(reply)) => self.step("SMTP", Outcome::Fail, format!("the server greeted with `{}`", first(&reply))),
            Ok(Err(e)) => self.step("SMTP", Outcome::Fail, e.to_string()),
            Err(_) => self.step("SMTP", Outcome::Fail, format!("no SMTP answer in {}", self.waited())),
        }
    }

    fn folder(&mut self, path: &std::path::Path, created_if_missing: bool) {
        match std::fs::metadata(path) {
            Ok(m) if m.is_dir() => {}
            Ok(_) => {
                self.step("folder", Outcome::Fail, "exists but is a file, not a folder");
                return;
            }
            Err(_) if created_if_missing => {
                self.step("folder", Outcome::Ok, "doesn't exist yet; the engine creates it on the first message");
                return;
            }
            Err(e) => {
                self.step("folder", Outcome::Fail, format!("can't be read: {e}"));
                self.advise("Create the folder, or mount the share it lives on (a share mounted by a user may not be visible to the service).");
                return;
            }
        }
        // Sources move or delete what they read and destinations write: both need to write.
        // A dot-name is never picked up by file sources.
        let probe = path.join(format!(".ductus-doctor-{}", std::process::id()));
        match std::fs::write(&probe, b"") {
            Ok(()) => {
                let _ = std::fs::remove_file(&probe);
                self.step("folder", Outcome::Ok, "exists; can write");
            }
            Err(e) => {
                self.step("folder", Outcome::Fail, format!("exists, but can't write in it: {e}"));
                self.advise("Give the account the engine runs as write access to it.");
            }
        }
    }

    /// Whether the port is free to listen on. True if it's in use (by a running engine, or not).
    async fn listen(&mut self, host: &str, port: u16) -> bool {
        match tokio::net::TcpListener::bind((host, port)).await {
            Ok(_) => self.step("port", Outcome::Ok, "free: the engine can listen here"),
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                self.step("port", Outcome::Warn, "in use: by a running engine, or another program");
                self.advise(format!(
                    "If the engine isn't running on this server, another program holds port {port}: find it with `ss -ltnp` (Linux) or `netstat -ano` (Windows)."
                ));
                return true;
            }
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                self.step("port", Outcome::Fail, format!("not allowed: {e}"));
                self.advise(
                    "Ports below 1024 need extra rights: use a higher port, or grant the engine CAP_NET_BIND_SERVICE.",
                );
            }
            Err(e) => {
                self.step("port", Outcome::Fail, e.to_string());
                if e.kind() == std::io::ErrorKind::AddrNotAvailable {
                    self.advise(format!(
                        "This server has no address {host}: listen on 0.0.0.0, or one of its own addresses."
                    ));
                }
            }
        }
        false
    }

    /// A listener's `allow:` list: each host name looked up from here (a name that doesn't
    /// resolve lets no one in for it), and whether this check itself can be let in (false: the
    /// listener is another process's, and this machine isn't on its list, so connecting to it
    /// would only be closed and logged there).
    async fn allow(&mut self, allow: &ductus_core::allow::Allow, port: u16, in_use: bool) -> bool {
        let rules = match allow.rules() {
            Ok(r) => r,
            Err(e) => {
                self.step("allow", Outcome::Fail, e.to_string());
                return false;
            }
        };
        let mut found: std::collections::HashMap<String, Vec<IpAddr>> = Default::default();
        let mut shown = Vec::new();
        for entry in &allow.0 {
            let entry = entry.trim();
            let name = entry.to_ascii_lowercase();
            if !rules.names.contains(&name) {
                shown.push(entry.to_string());
                continue;
            }
            let lookup = tokio::time::timeout(self.timeout, tokio::net::lookup_host((name.as_str(), 0)))
                .await
                .map(|found| found.map(|a| a.map(|a| ductus_core::allow::canonical(a.ip())).collect::<Vec<IpAddr>>()));
            match lookup {
                Ok(Ok(mut ips)) => {
                    ips.sort();
                    ips.dedup();
                    let list: Vec<String> = ips.iter().map(|ip| ip.to_string()).collect();
                    shown.push(format!("{entry} → {}", list.join(", ")));
                    found.insert(name, ips);
                }
                _ => {
                    shown.push(format!("{entry} (doesn't resolve)"));
                    self.step(
                        "allow",
                        Outcome::Warn,
                        format!("{entry} doesn't resolve from this server: nothing is let in for it until it does"),
                    );
                    self.advise(format!(
                        "Check the spelling of `{entry}` in `allow:`, or ask for a DNS entry; or list its address instead."
                    ));
                }
            }
        }
        self.step("allow", Outcome::Ok, format!("only {} may connect", shown.join(", ")));
        // In the engine its own checks are let in; `ductus doctor` is another process.
        if !in_use || crate::allow::on_port(port).is_some() {
            return true;
        }
        let here = |ip: IpAddr| rules.permits(ip, |n, ip| found.get(n).is_some_and(|a| a.contains(&ip)));
        let local = [IpAddr::from([127, 0, 0, 1]), IpAddr::from(std::net::Ipv6Addr::LOCALHOST)];
        if local.into_iter().any(here) {
            return true;
        }
        self.step(
            "allow",
            Outcome::Ok,
            "this machine (127.0.0.1) isn't on the list, so the running engine closes this program's connection: \
             not checked from here (the engine's own check, Connections → Check now, is let in)",
        );
        false
    }

    /// One of our TLS listeners, as a sender meets it: connected to (over loopback when it
    /// listens on every address), its chain taken from the handshake whatever it is, and judged
    /// the way senders build a path. Java and .NET senders don't fetch a missing intermediate
    /// (browsers often do), so a chain that stops short of a root is a warning. What this can't
    /// know: the name senders connect with, and which roots they trust.
    async fn served(&mut self, host: &str, port: u16, name: &str) {
        let target = match host.parse::<IpAddr>() {
            Ok(IpAddr::V4(ip)) if ip.is_unspecified() => "127.0.0.1".to_string(),
            Ok(IpAddr::V6(ip)) if ip.is_unspecified() => "::1".to_string(),
            _ => host.to_string(),
        };
        let Some(address) = self.connectable(&target, port).await else { return };
        let pair = self.certificates.pair(name).cloned();
        let loaded = pair.as_ref().map(|p| p.about());
        // The name we ask for (SNI) is the certificate's own: our listeners serve one pair
        // whatever is asked, so any name gets the same chain.
        let server_name = loaded.as_ref().and_then(|a| a.names.first().cloned()).unwrap_or_else(|| "localhost".into());
        let Ok(server_name) = rustls::pki_types::ServerName::try_from(server_name) else {
            self.step("TLS", Outcome::Fail, "the certificate's first name isn't one TLS can ask for");
            return;
        };
        let presented = Arc::new(Mutex::new(Vec::new()));
        let config = served_config(presented.clone());
        let handshake = async {
            // In the engine (the API's checks), the listener is in this process: not a refusal.
            let tcp = crate::tls::connect_own(address).await?;
            tokio_rustls::TlsConnector::from(Arc::new(config)).connect(server_name, tcp).await
        };
        let result = tokio::time::timeout(self.timeout, handshake).await;
        let served: Vec<Vec<u8>> = presented.lock().expect("certificate lock").clone();
        let version = match &result {
            Ok(Ok(stream)) => negotiated(stream.get_ref().1),
            _ => String::new(),
        };
        if served.is_empty() {
            let why = match result {
                Ok(Err(e)) => format!("handshake failed: {e}"),
                Err(_) => format!("no TLS answer in {}", self.waited()),
                Ok(Ok(_)) => "no certificate served".into(),
            };
            self.step("TLS", Outcome::Fail, why);
            self.advise(format!(
                "Port {port} should speak TLS with certificate `{name}`: is another program listening on it?"
            ));
            return;
        }
        let chain = Chain::read(served.as_slice());
        let Some(leaf) = chain.leaf.as_ref() else {
            self.step("TLS", Outcome::Fail, "the certificate served can't be read");
            return;
        };
        // Its handshake may then end for want of a client certificate (we send none): the chain
        // is all this check is about.
        let version = if version.is_empty() { String::new() } else { format!("{version}, ") };
        let names = if leaf.names.is_empty() { "no names".to_string() } else { leaf.names.join(", ") };
        self.step("TLS", Outcome::Ok, format!("{version}serves `{name}`: {}; covers {names}", leaf.describe()));
        let loaded_leaf = pair.as_ref().and_then(|p| Chain::read(p.chain().as_slice()).leaf).map(|c| c.fingerprint);
        if loaded_leaf.as_ref().is_some_and(|f| *f != leaf.fingerprint) {
            self.step("served", Outcome::Warn, format!("not the certificate `{name}` the engine loaded"));
            self.advise(format!("Port {port} answers with another certificate than `{name}`: another program may hold it, or an older engine still runs."));
        }
        let (outcome, detail, advice) = judge_chain(&served, self.certificates);
        self.step("chain", outcome, detail);
        self.advice.extend(advice);
        if let Some(ends) = chain.not_after {
            let days = (ends - Utc::now()).num_days();
            if days < EXPIRY_WARNING_DAYS {
                let whose = chain.earliest.as_ref().map(|s| format!(" (its chain's {s})")).unwrap_or_default();
                self.step("expiry", Outcome::Warn, format!("expires in {days} day(s){whose}"));
                self.advise(format!(
                    "Renew `{name}` before then: replace its files (the engine picks them up within a minute)."
                ));
            }
        }
        self.advise(format!(
            "Doctor can't know the name senders connect with: it must be one of {names} (ask senders which name or address they use)."
        ));
    }
}

/// Client settings that take whatever chain a server presents, and record it (still checking
/// the handshake's signatures): for looking at what our own listeners serve.
fn served_config(seen: Arc<Mutex<Vec<Vec<u8>>>>) -> rustls::ClientConfig {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .expect("the default provider supports the default versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Accepting { seen, provider }))
        .with_no_client_auth()
}

/// Records the chain and accepts it (see `served_config`).
#[derive(Debug)]
struct Accepting {
    seen: Arc<Mutex<Vec<Vec<u8>>>>,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl rustls::client::danger::ServerCertVerifier for Accepting {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        let mut seen = self.seen.lock().expect("certificate lock");
        seen.push(end_entity.to_vec());
        seen.extend(intermediates.iter().map(|c| c.to_vec()));
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

/// Whether a sender can build a path from the chain we serve (leaf first): each certificate
/// issued by the next, and the last a root (self-signed: sending it is harmless) or issued by
/// one this server knows (the system's trusted certificates, or a `certificates:` CA). A last
/// certificate whose issuer is neither is a missing intermediate, or a private root senders must
/// have: said both ways, since this server can't tell. The step's outcome and words, and advice.
/// `ductus cert show` judges a file the same way.
pub fn judge_chain(served: &[Vec<u8>], certificates: &Certificates) -> (Outcome, String, Vec<String>) {
    use x509_parser::prelude::*;
    struct Names {
        subject: Vec<u8>,
        issuer: Vec<u8>,
        subject_name: String,
        issuer_name: String,
    }
    let read = |der: &[u8]| {
        let (_, c) = X509Certificate::from_der(der).ok()?;
        let name = |n: &X509Name| Certificate::name(n);
        Some(Names {
            subject: c.subject().as_raw().to_vec(),
            issuer: c.issuer().as_raw().to_vec(),
            subject_name: name(c.subject()),
            issuer_name: name(c.issuer()),
        })
    };
    let chain: Vec<Names> = served.iter().filter_map(|d| read(d)).collect();
    let (Some(leaf), Some(last)) = (chain.first(), chain.last()) else {
        return (Outcome::Fail, "no certificate could be read".into(), Vec::new());
    };
    let path: Vec<&str> = chain.iter().map(|c| c.subject_name.as_str()).collect();
    let path = path.join(" → ");
    if let Some(w) = chain.windows(2).find(|w| w[0].issuer != w[1].subject) {
        return (
            Outcome::Warn,
            format!("out of order: {path} ({} isn't issued by {})", w[0].subject_name, w[1].subject_name),
            vec![
                "Put the certificates in cert_file in order: ours first, then the one that issued it, and so on up."
                    .into(),
            ],
        );
    }
    if chain.len() == 1 && leaf.subject == leaf.issuer {
        return (
            Outcome::Ok,
            "self-signed: senders must trust this very certificate".into(),
            vec!["A self-signed certificate is for labs: senders must add it to what they trust; one from the hospital's CA spares them that.".into()],
        );
    }
    if last.subject == last.issuer {
        return (Outcome::Ok, format!("complete: {path} (a root; sending it is harmless)"), Vec::new());
    }
    if let Some((ca, _)) = certificates.authorities().find(|(_, c)| read(c).is_some_and(|r| r.subject == last.issuer)) {
        return (
            Outcome::Ok,
            format!("complete: {path}, issued by {} (the CA `{ca}` in resources.yaml)", last.issuer_name),
            vec![format!("Senders must trust {} (the hospital's CA): ask them to confirm.", last.issuer_name)],
        );
    }
    let roots = rustls_native_certs::load_native_certs().certs;
    if roots.iter().any(|r| read(r).is_some_and(|r| r.subject == last.issuer)) {
        return (
            Outcome::Ok,
            format!("complete: {path}, issued by {} (a root this server trusts)", last.issuer_name),
            Vec::new(),
        );
    }
    let what = if chain.len() == 1 {
        format!("only our certificate is served ({path}), not the one that issued it ({})", last.issuer_name)
    } else {
        format!("it stops at {} ({path}), issued by {}, which isn't served", last.subject_name, last.issuer_name)
    };
    (
        Outcome::Warn,
        format!("incomplete: {what}"),
        vec![
            format!(
                "If {} is an intermediate, append its certificate to cert_file after ours (leaf first): Java and .NET senders don't fetch a missing intermediate and refuse the connection (browsers often do, so a browser test passes).",
                last.issuer_name
            ),
            format!(
                "If {} is your private CA's root, senders need it in what they trust; name it as a `certificates:` CA (ca_file) and doctor will know it.",
                last.issuer_name
            ),
        ],
    )
}

/// How long a finished handshake is watched for the server refusing our client certificate (a
/// round trip is enough; a server that accepts sends nothing and waits for us).
const REFUSAL_WAIT: Duration = Duration::from_millis(500);

/// The TLS version and key exchange a handshake agreed: `TLS 1.3, post-quantum key exchange
/// (X25519MLKEM768)` or `TLS 1.3, key exchange X25519` (a peer that doesn't offer the hybrid yet:
/// what it sends today can be recorded and decrypted once quantum computers can).
fn negotiated(conn: &rustls::CommonState) -> String {
    let version = conn.protocol_version().map(|v| format!("{v:?}").replace("TLSv1_", "TLS 1.")).unwrap_or_default();
    match conn.negotiated_key_exchange_group().map(|g| g.name()) {
        Some(g @ (rustls::NamedGroup::X25519MLKEM768 | rustls::NamedGroup::secp256r1MLKEM768)) => {
            format!("{version}, post-quantum key exchange ({g:?})")
        }
        Some(g) => format!("{version}, key exchange {g:?}"),
        None => version,
    }
}

/// The server's refusal after the handshake (TLS 1.3: of our client certificate, or of none), in
/// plain words: a TLS alert. `None` when it said nothing within `wait`, sent data, or just closed.
async fn refused_after<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut tokio_rustls::client::TlsStream<S>,
    wait: Duration,
) -> Option<String> {
    use tokio::io::AsyncReadExt;
    let mut byte = [0u8; 1];
    match tokio::time::timeout(wait, stream.read(&mut byte)).await {
        Ok(Err(e)) if e.get_ref().is_some_and(|i| i.is::<rustls::Error>()) => crate::tls::outbound_failure(&e),
        _ => None,
    }
}

/// Median time of a durable commit, and whether commits wait for the disk at all.
///
/// Each commit writes a transactional WAL message (no table is touched), so it waits for the
/// flush like a message insert does. A transaction that only takes an id is not enough:
/// without WAL of its own Postgres doesn't wait for the flush (measured 0.28 ms vs 3.06 ms on
/// the same disk). Logical decoding consumers, if any, see the message under the prefix
/// `ductus doctor` and ignore it.
async fn commit_times(connection: &mut PgConnection) -> Result<(Duration, String), sqlx::Error> {
    let synchronous: String = sqlx::query_scalar("SHOW synchronous_commit").fetch_one(&mut *connection).await?;
    let mut times = Vec::with_capacity(COMMITS);
    for _ in 0..COMMITS {
        let started = Instant::now();
        sqlx::query("SELECT pg_logical_emit_message(true, 'ductus doctor', '')").execute(&mut *connection).await?;
        times.push(started.elapsed());
    }
    times.sort();
    Ok((times[COMMITS / 2], synchronous))
}

/// Why a TCP connection failed, for advice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Failure {
    Refused,
    Timeout,
    NoRoute,
    Blocked,
    Other,
}

impl Failure {
    fn of(e: &std::io::Error) -> Self {
        use std::io::ErrorKind::*;
        match e.kind() {
            ConnectionRefused => Failure::Refused,
            TimedOut => Failure::Timeout,
            HostUnreachable | NetworkUnreachable => Failure::NoRoute,
            PermissionDenied => Failure::Blocked,
            _ => Failure::Other,
        }
    }

    fn describe(self, e: &std::io::Error) -> String {
        match self {
            Failure::Refused => "refused".into(),
            Failure::Timeout => "no answer".into(),
            Failure::NoRoute => "no route to it".into(),
            Failure::Blocked => format!("blocked on this server ({e})"),
            Failure::Other => e.to_string(),
        }
    }

    fn advice(self, host: &str, address: SocketAddr, from: Option<IpAddr>) -> String {
        let from = from.map(|ip| ip.to_string()).unwrap_or_else(|| "this server".into());
        let (ip, port) = (address.ip(), address.port());
        match self {
            Failure::Refused => format!(
                "{host} is reachable but nothing accepts connections on port {port}: the service is down, listens on another port, or a firewall at the host rejects it. Ask its owner whether it's up on {port}."
            ),
            Failure::Timeout => format!(
                "Nothing answered: something on the way drops the traffic (a firewall or VPN), or the host is off. Firewall request: from {from} to {ip} port {port} (TCP)."
            ),
            Failure::NoRoute => format!(
                "This server has no route to {ip}: the VPN or the routing to that network is missing (from {from})."
            ),
            Failure::Blocked => "This server's own firewall or security policy blocks the connection.".into(),
            Failure::Other => format!("Connecting from {from} to {ip} port {port} failed."),
        }
    }
}

/// The address this server would send from to reach `target` (no packet is sent: a UDP
/// "connect" only asks the routing table).
fn source_address(target: SocketAddr) -> Option<IpAddr> {
    let any: SocketAddr = if target.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" }.parse().ok()?;
    let socket = std::net::UdpSocket::bind(any).ok()?;
    socket.connect(target).ok()?;
    Some(socket.local_addr().ok()?.ip())
}

fn show(address: &SocketAddr) -> String {
    address.to_string()
}

fn took(d: Duration) -> String {
    let ms = d.as_secs_f64() * 1000.0;
    if ms < 10.0 {
        format!("{ms:.1} ms")
    } else if ms < 1000.0 {
        format!("{ms:.0} ms")
    } else {
        format!("{:.1} s", ms / 1000.0)
    }
}

/// One SMTP reply, all its lines (`250-...` continues, `250 ...` ends it).
async fn smtp_reply<R: tokio::io::AsyncBufRead + Unpin>(r: &mut R) -> std::io::Result<String> {
    let mut reply = String::new();
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).await? == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "the server closed the connection"));
        }
        let last = line.as_bytes().get(3) != Some(&b'-');
        reply.push_str(&line);
        if last || reply.len() > 16_384 {
            return Ok(reply);
        }
    }
}

/// Postgres's SSLRequest: length 8, then the code 1234 5679 (protocol docs, "SSLSessionEncryption").
const SSL_REQUEST: [u8; 8] = [0x00, 0x00, 0x00, 0x08, 0x04, 0xd2, 0x16, 0x2f];

/// LDAP's StartTLS extended request (RFC 4511 4.14.1), message id 1: SEQUENCE { 1,
/// [APPLICATION 23] { [0] "1.3.6.1.4.1.1466.20037" } }.
const STARTTLS_REQUEST: [u8; 31] = [
    0x30, 0x1d, 0x02, 0x01, 0x01, 0x77, 0x18, 0x80, 0x16, b'1', b'.', b'3', b'.', b'6', b'.', b'1', b'.', b'4', b'.',
    b'1', b'.', b'1', b'4', b'6', b'6', b'.', b'2', b'0', b'0', b'3', b'7',
];

/// One LDAP message off the wire (a BER SEQUENCE: tag, length, contents), at most 64 KB.
async fn read_ldap_message<R: AsyncRead + Unpin>(r: &mut R) -> std::io::Result<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    let bad = |what: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, what.to_string());
    let mut head = [0u8; 2];
    r.read_exact(&mut head).await?;
    let mut message = head.to_vec();
    let length = if head[1] & 0x80 == 0 {
        usize::from(head[1])
    } else {
        let n = usize::from(head[1] & 0x7f);
        if n == 0 || n > 3 {
            return Err(bad("not an LDAP answer"));
        }
        let mut bytes = vec![0u8; n];
        r.read_exact(&mut bytes).await?;
        message.extend_from_slice(&bytes);
        bytes.iter().fold(0usize, |len, b| (len << 8) | usize::from(*b))
    };
    if length > 65_536 {
        return Err(bad("an LDAP answer too long for StartTLS"));
    }
    let mut body = vec![0u8; length];
    r.read_exact(&mut body).await?;
    message.extend_from_slice(&body);
    Ok(message)
}

/// One BER element: its tag, contents and what follows.
fn ber(buf: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = buf.split_first()?;
    let (&first, rest) = rest.split_first()?;
    let (length, rest) = if first & 0x80 == 0 {
        (usize::from(first), rest)
    } else {
        let n = usize::from(first & 0x7f);
        if n == 0 || n > 4 || rest.len() < n {
            return None;
        }
        (rest[..n].iter().fold(0usize, |len, b| (len << 8) | usize::from(*b)), &rest[n..])
    };
    (rest.len() >= length).then(|| (tag, &rest[..length], &rest[length..]))
}

/// The result code and diagnostic message of an ExtendedResponse ([APPLICATION 24]), or None if
/// the bytes aren't one.
fn starttls_result(message: &[u8]) -> Option<(u32, String)> {
    let (0x30, body, _) = ber(message)? else { return None };
    let (0x02, _, rest) = ber(body)? else { return None }; // message id
    let (0x78, response, _) = ber(rest)? else { return None };
    let (0x0a, code, rest) = ber(response)? else { return None };
    let code = code.iter().fold(0u32, |c, b| (c << 8) | u32::from(*b));
    let (_, _, rest) = ber(rest)?; // matched DN
    let (_, text, _) = ber(rest)?;
    Some((code, String::from_utf8_lossy(text).trim().to_string()))
}

/// The first line of a reply, trimmed.
fn first(reply: &str) -> String {
    reply.lines().next().unwrap_or("").trim().to_string()
}

/// 2 significant figures: measured numbers aren't more precise than that.
fn round(x: f64) -> String {
    if x <= 0.0 || !x.is_finite() {
        return "0".into();
    }
    let magnitude = 10f64.powi(x.log10().floor() as i32 - 1);
    format!("{:.0}", (x / magnitude).round() * magnitude)
}

fn strip_query(url: &str) -> String {
    url.split(['?', '#']).next().unwrap_or(url).to_string()
}

/// Verifies like the platform verifier, keeping the certificates the server presented so a
/// failure can say which certificate it was.
#[derive(Debug)]
struct Recording {
    inner: rustls_platform_verifier::Verifier,
    seen: Arc<Mutex<Vec<Vec<u8>>>>,
    /// A certificate that isn't for the host still passes (Postgres's `verify-ca`).
    name_unchecked: bool,
}

impl rustls::client::danger::ServerCertVerifier for Recording {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        intermediates: &[rustls::pki_types::CertificateDer<'_>],
        server_name: &rustls::pki_types::ServerName<'_>,
        ocsp_response: &[u8],
        now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        let mut seen = self.seen.lock().expect("certificate lock");
        seen.push(end_entity.to_vec());
        seen.extend(intermediates.iter().map(|c| c.to_vec()));
        drop(seen);
        use rustls::CertificateError as C;
        match self.inner.verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now) {
            Err(rustls::Error::InvalidCertificate(C::NotValidForName | C::NotValidForNameContext { .. }))
                if self.name_unchecked =>
            {
                Ok(rustls::client::danger::ServerCertVerified::assertion())
            }
            verdict => verdict,
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

/// A chain of certificates, read: its first (the server's own), and the earliest expiry in it.
#[derive(Default)]
pub(crate) struct Chain {
    pub(crate) leaf: Option<Certificate>,
    pub(crate) not_after: Option<DateTime<Utc>>,
    /// Whose that is, when it isn't the first's (an intermediate ends sooner).
    pub(crate) earliest: Option<String>,
}

impl Chain {
    pub(crate) fn read<C: AsRef<[u8]>>(chain: &[C]) -> Self {
        let chain: Vec<Certificate> = chain.iter().filter_map(|c| Certificate::read(c.as_ref())).collect();
        let mut out = Chain::default();
        if let Some((n, earliest)) = chain.iter().enumerate().min_by_key(|(_, c)| c.not_after) {
            out.not_after = Some(earliest.not_after);
            out.earliest = (n > 0 && earliest.not_after < chain[0].not_after).then(|| earliest.subject.clone());
        }
        out.leaf = chain.into_iter().next();
        out
    }
}

/// What a report says about a certificate.
pub struct Certificate {
    /// Its common name (or organisation): the name people say.
    pub subject: String,
    pub issuer: String,
    pub names: Vec<String>,
    pub not_before: DateTime<Utc>,
    pub not_after: DateTime<Utc>,
    /// SHA-256 of the DER, hex.
    pub fingerprint: String,
}

impl Certificate {
    /// A name as people say it: its common name, else its organisation, else all of it.
    fn name(n: &x509_parser::x509::X509Name) -> String {
        n.iter_common_name()
            .next()
            .and_then(|cn| cn.as_str().ok().map(str::to_string))
            .or_else(|| n.iter_organization().next().and_then(|o| o.as_str().ok().map(str::to_string)))
            .unwrap_or_else(|| n.to_string())
    }

    pub fn read(der: &[u8]) -> Option<Self> {
        use x509_parser::prelude::*;
        let (_, cert) = X509Certificate::from_der(der).ok()?;
        let name = Self::name;
        let mut names = Vec::new();
        if let Ok(Some(san)) = cert.subject_alternative_name() {
            for n in &san.value.general_names {
                match n {
                    GeneralName::DNSName(d) => names.push(d.to_string()),
                    GeneralName::IPAddress(b) if b.len() == 4 => {
                        names.push(IpAddr::from(<[u8; 4]>::try_from(*b).ok()?).to_string());
                    }
                    GeneralName::IPAddress(b) if b.len() == 16 => {
                        names.push(IpAddr::from(<[u8; 16]>::try_from(*b).ok()?).to_string());
                    }
                    _ => {}
                }
            }
        }
        use sha2::Digest;
        let fingerprint = sha2::Sha256::digest(der).iter().map(|b| format!("{b:02x}")).collect();
        Some(Self {
            subject: name(cert.subject()),
            issuer: name(cert.issuer()),
            names,
            not_before: DateTime::from_timestamp(cert.validity().not_before.timestamp(), 0)?,
            not_after: DateTime::from_timestamp(cert.validity().not_after.timestamp(), 0)?,
            fingerprint,
        })
    }

    pub fn days_left(&self) -> i64 {
        (self.not_after - chrono::Utc::now()).num_days()
    }

    fn describe(&self) -> String {
        let issuer =
            if self.issuer == self.subject { "self-signed".to_string() } else { format!("issued by {}", self.issuer) };
        let days = self.days_left();
        let until = if days < 0 {
            format!("expired {}", self.not_after.format("%Y-%m-%d"))
        } else {
            format!("valid until {} ({days} days)", self.not_after.format("%Y-%m-%d"))
        };
        format!("certificate for {}, {issuer}, {until}", self.subject)
    }
}

/// A failed handshake in plain words, and what to do.
fn tls_failure(
    host: &str,
    error: Option<&rustls::Error>,
    io: &std::io::Error,
    leaf: Option<&Certificate>,
) -> (String, String) {
    use rustls::CertificateError as C;
    let Some(rustls::Error::InvalidCertificate(c)) = error else {
        if let Some(rustls::Error::AlertReceived(_)) = error
            && let Some(words) = crate::tls::outbound_failure(io)
        {
            return (
                words,
                "The server ended the handshake: if it asks for a client certificate, name one in `certificate` and check its CA is the one the server trusts for clients.".into(),
            );
        }
        let what = error.map(|e| e.to_string()).unwrap_or_else(|| io.to_string());
        return (
            format!("handshake failed: {what}"),
            "The TLS handshake failed: the server may only offer old TLS versions or ciphers, or isn't speaking TLS on this port.".into(),
        );
    };
    match c {
        C::Expired | C::ExpiredContext { .. } => (
            "the certificate has expired".into(),
            "The server's certificate has expired: its owner must renew it.".into(),
        ),
        C::NotValidForName | C::NotValidForNameContext { .. } => {
            let names = leaf.map(|l| l.names.join(", ")).filter(|n| !n.is_empty());
            (
                match names {
                    Some(n) => format!("the certificate is for {n}, not {host}"),
                    None => format!("the certificate isn't for {host}"),
                },
                format!("Connect with a name the certificate lists, or ask the server's owner for a certificate that includes {host}."),
            )
        }
        C::UnknownIssuer => (
            "not trusted: its issuer isn't one this server trusts (self-signed, or a private CA)".into(),
            "Name the issuing CA in `ca` (a `certificates:` entry in resources.yaml), or add it to this server's trusted certificates, or ask for a certificate from a public CA.".into(),
        ),
        C::NotValidYet | C::NotValidYetContext { .. } => (
            "the certificate isn't valid yet".into(),
            "The certificate's start date is in the future: check this server's clock, and the server's certificate.".into(),
        ),
        C::Revoked => ("the certificate was revoked".into(), "The certificate was revoked: its owner must replace it.".into()),
        other => (format!("certificate rejected: {other:?}"), "The server's certificate was rejected; show this to its owner.".into()),
    }
}

/// Each thing to check once, however many use it, in report order.
#[derive(Default)]
struct Plan {
    /// By (section, key): 1 databases, 2 APIs, 3 SFTP servers, 4 destinations, 5 folders,
    /// 6 listening ports, 7 directories.
    entries: BTreeMap<(u8, String), Entry>,
}

struct Entry {
    what: String,
    target: Target,
    used_by: Vec<String>,
    interfaces: BTreeSet<String>,
    /// What its certificate is kept under: the resource's name, or the first destination's.
    resource: String,
}

/// Users shown for a resource nothing uses.
const UNUSED: &str = "nothing yet (defined in resources.yaml)";

impl Plan {
    #[allow(clippy::too_many_arguments)]
    fn add(
        &mut self,
        section: u8,
        key: String,
        what: &str,
        target: Target,
        user: String,
        interface: Option<&str>,
        resource: &str,
    ) {
        let e = self.entries.entry((section, key)).or_insert_with(|| Entry {
            what: what.into(),
            target,
            used_by: Vec::new(),
            interfaces: BTreeSet::new(),
            resource: resource.into(),
        });
        if !e.used_by.contains(&user) {
            e.used_by.push(user);
        }
        e.interfaces.extend(interface.map(str::to_string));
    }

    /// A resources.yaml entry; names that aren't defined are left to `ductus validate`.
    fn resource(&mut self, resources: &Resources, kind: Kind, name: &str, user: String, interface: Option<&str>) {
        let (section, target) = match kind {
            Kind::Database => match resources.databases.get(name) {
                Some(db) => (1, Target::Database(name.into(), db.clone())),
                None => return,
            },
            Kind::Api => match resources.apis.get(name) {
                Some(api) => (2, Target::Api(name.into(), api.clone())),
                None => return,
            },
            Kind::Sftp => match resources.sftp.get(name) {
                Some(s) => (3, Target::Sftp(name.into(), s.clone())),
                None => return,
            },
            Kind::Ldap => match resources.ldap.get(name) {
                Some(l) => (7, Target::Ldap(name.into(), l.clone())),
                None => return,
            },
            // Ours, not something to connect to: checked when loaded (and served, 0.6.3.3).
            Kind::Certificate => return,
        };
        self.add(section, name.into(), &format!("{} {name}", kind.label()), target, user, interface, name);
    }
}

/// What to check: the engine's database first, then everything the interfaces (or only
/// `only`) connect to, read from and listen on, and the resources nothing uses yet.
pub fn plan(
    configs: &[&InterfaceConfig],
    resources: &Resources,
    code: &ductus_core::impact::CodeUses,
    database_url: &str,
    only: &[String],
) -> Vec<Probe> {
    let mut plan = Plan::default();
    for c in configs {
        let me = Some(c.name.as_str());
        match &c.source {
            Source::Mllp { host, port, tls, allow } | Source::Http { host, port, tls, allow, .. } => {
                let protocol = match (&c.source, tls) {
                    (Source::Mllp { .. }, None) => "MLLP",
                    (Source::Mllp { .. }, Some(_)) => "MLLP over TLS",
                    (_, None) => "HTTP",
                    (_, Some(_)) => "HTTPS",
                };
                let tls = tls.as_ref().map(|t| t.certificate.clone());
                let target = Target::Listener {
                    host: host.clone(),
                    port: *port,
                    protocol,
                    tls,
                    held: false,
                    allow: allow.clone(),
                };
                plan.add(6, format!("{port:05} {host}"), "listening port", target, c.name.clone(), me, &c.name);
            }
            Source::File(f) => {
                let path = c.dir.join(&f.dir);
                let target = Target::Folder { path: path.clone(), created_if_missing: false };
                plan.add(5, path.display().to_string(), "source folder", target, c.name.clone(), me, &c.name);
            }
            Source::Sftp(f) => {
                let server = f.server.as_deref().unwrap_or_default();
                plan.resource(resources, Kind::Sftp, server, format!("{} (source)", c.name), me);
            }
            Source::Schedule(_) | Source::Internal => {}
        }
        for (name, d) in c.destinations() {
            let label = if c.destination.is_some() { c.name.clone() } else { format!("{} → {name}", c.name) };
            let resource = format!("{}/{name}", c.name);
            match &d.connector {
                Destination::Mllp { host, port, tls, .. } => {
                    let target = Target::Mllp { host: host.clone(), port: *port, tls: tls.clone() };
                    // The same receiver over TLS with other settings is checked on its own.
                    let key =
                        format!("{host}:{port}{}", tls.as_ref().map(|t| format!(" tls{t:?}")).unwrap_or_default());
                    plan.add(4, key, "MLLP destination", target, label, me, &resource);
                }
                Destination::Http { url: Some(url), ca, certificate, .. } => {
                    let tls = ConnectTls::new(ca, certificate);
                    let key = format!("{url}{}", if tls.is_set() { format!(" {tls:?}") } else { String::new() });
                    let target = Target::Http { url: url.clone(), tls };
                    plan.add(4, key, "HTTP destination", target, label, me, &resource);
                }
                Destination::Http { api: Some(api), .. } => plan.resource(resources, Kind::Api, api, label, me),
                Destination::Http { .. } | Destination::Interface(_) => {}
                Destination::Sftp { server, .. } => plan.resource(resources, Kind::Sftp, server, label, me),
                Destination::File { dir } => {
                    let path = c.dir.join(dir);
                    let target = Target::Folder { path: path.clone(), created_if_missing: true };
                    plan.add(5, path.display().to_string(), "destination folder", target, label, me, &resource);
                }
            }
        }
        for db in c.dedicated_pools.keys() {
            plan.resource(resources, Kind::Database, db, format!("{} (dedicated pool)", c.name), me);
        }
        for (kind, name) in code.get(&c.name).into_iter().flatten() {
            plan.resource(resources, *kind, name, format!("{} (code)", c.name), me);
        }
    }
    let defined = [
        (Kind::Database, resources.databases.keys().collect::<Vec<_>>()),
        (Kind::Api, resources.apis.keys().collect()),
        (Kind::Sftp, resources.sftp.keys().collect()),
    ];
    for (kind, names) in defined {
        for name in names {
            plan.resource(resources, kind, name, UNUSED.into(), None);
        }
    }
    // Directories are what people sign in with, not something interfaces use.
    for name in resources.ldap.keys() {
        plan.resource(resources, Kind::Ldap, name, "sign-in for people (auth.ldap)".into(), None);
    }

    let store = Probe {
        what: "engine database".into(),
        target: Target::Store(database_url.into()),
        used_by: vec!["the engine (DUCTUS_DATABASE_URL)".into()],
        resource: None,
    };
    let wanted = |e: &Entry| only.is_empty() || only.iter().any(|i| e.interfaces.contains(i));
    let rest = plan.entries.into_values().filter(wanted).map(|mut e| {
        if e.used_by.len() > 1 {
            e.used_by.retain(|u| u != UNUSED);
        }
        Probe { what: e.what, target: e.target, used_by: e.used_by, resource: Some(e.resource) }
    });
    std::iter::once(store).chain(rest).collect()
}

/// The REST API's listener with `tls`, for the engine's own full check (it holds the port).
pub fn api_probe(
    host: &str,
    port: u16,
    tls: &ductus_core::tls::ListenTls,
    allow: Option<&ductus_core::allow::Allow>,
) -> Probe {
    Probe {
        what: "listening port".into(),
        target: Target::Listener {
            host: host.into(),
            port,
            protocol: "HTTPS",
            tls: Some(tls.certificate.clone()),
            held: true,
            allow: allow.cloned(),
        },
        used_by: vec!["the REST API and web UI (ductus.yaml)".into()],
        resource: None,
    }
}

/// What alerts connect to (`alerts:` in ductus.yaml): the mail server, each webhook, the
/// heartbeat. Checked like the rest; a webhook's URL is never shown (it is its secret), only its
/// server.
pub fn alert_probes(a: &AlertSettings) -> Vec<Probe> {
    let mut probes = Vec::new();
    if let Some(smtp) = &a.smtp {
        probes.push(Probe {
            what: "mail server (alerts)".into(),
            target: Target::Smtp {
                host: smtp.host.clone(),
                port: smtp.port(),
                tls: smtp.tls,
                trust: ConnectTls::new(&smtp.ca, &smtp.certificate),
            },
            used_by: vec!["alerts".into()],
            resource: Some("alerts.smtp".into()),
        });
    }
    for (name, channel) in &a.channels {
        if let Some(hook) = &channel.webhook {
            probes.push(Probe {
                what: format!("webhook {name} (alerts)"),
                target: Target::Http { url: hook.url.clone(), tls: ConnectTls::new(&hook.ca, &hook.certificate) },
                used_by: vec![format!("alerts → {name}")],
                resource: Some(format!("alerts.{name}")),
            });
        }
    }
    if let Some(heartbeat) = &a.heartbeat {
        probes.push(Probe {
            what: "heartbeat (alerts)".into(),
            target: Target::Http {
                url: heartbeat.url.clone(),
                tls: ConnectTls::new(&heartbeat.ca, &heartbeat.certificate),
            },
            used_by: vec!["alerts".into()],
            resource: Some("alerts.heartbeat".into()),
        });
    }
    probes
}

/// This server's name, for the report header.
pub fn hostname() -> String {
    ["HOSTNAME", "COMPUTERNAME"]
        .iter()
        .find_map(|v| std::env::var(v).ok())
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "this server".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(target: Target) -> Probe {
        Probe { what: "test".into(), target, used_by: vec!["x".into()], resource: Some("test".into()) }
    }

    async fn one(target: Target) -> Check {
        check(vec![probe(target)], Duration::from_secs(2), &Default::default()).await.remove(0)
    }

    fn https(url: &str) -> Target {
        Target::Http { url: url.into(), tls: Default::default() }
    }

    fn step<'a>(c: &'a Check, name: &str) -> &'a Step {
        c.steps.iter().find(|s| s.name == name).unwrap_or_else(|| panic!("no {name} step: {c:#?}"))
    }

    #[tokio::test]
    async fn explains_refused_connections_and_names_that_dont_resolve() {
        // A port nothing listens on.
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let c = one(Target::Mllp { host: "127.0.0.1".into(), port: free, tls: None }).await;
        assert_eq!(c.outcome, Outcome::Fail);
        assert_eq!(step(&c, "address").outcome, Outcome::Ok);
        assert!(step(&c, "connect").detail.ends_with("from 127.0.0.1: refused"), "{c:#?}");
        assert!(c.advice.iter().any(|a| a.contains("nothing accepts connections on port")), "{c:#?}");

        let c = one(Target::Mllp { host: "no-such-host.invalid".into(), port: 1, tls: None }).await;
        assert_eq!(step(&c, "DNS").outcome, Outcome::Fail);
        assert!(c.advice.iter().any(|a| a.contains("can't look up `no-such-host.invalid`")), "{c:#?}");

        // Something listening.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let c = one(Target::Mllp { host: "localhost".into(), port, tls: None }).await;
        assert!(
            c.steps.iter().any(|s| s.name == "connect" && s.outcome == Outcome::Ok),
            "connects to 127.0.0.1: {c:#?}"
        );
        assert_ne!(c.outcome, Outcome::Fail, "{c:#?}");
        assert!(step(&c, "DNS").detail.starts_with("localhost → "), "{c:#?}");
    }

    /// Our TLS listener, as senders meet it: over loopback when it listens on every address. A
    /// chain up to a root it knows passes; our certificate alone, without the intermediate that
    /// issued it, warns (Java and .NET senders refuse it); and it says what it can't know.
    #[tokio::test]
    async fn checks_the_chain_our_listener_serves() {
        use crate::tls::testing::*;
        let root = ca("Test Root");
        let middle = intermediate(&root, "Test Intermediate");
        let (leaf_pem, key) = leaf(&middle, &[(rcgen::DnType::CommonName, "engine")], &["engine.lab", "127.0.0.1"], 90);
        let full = format!("{leaf_pem}{}", middle.cert.pem());
        let other = leaf(&middle, &[(rcgen::DnType::CommonName, "other")], &["other.lab"], 90);
        let certificates = Arc::new(
            Certificates::load(&BTreeMap::from([
                ("full".to_string(), entry(&full, &key)),
                ("short".to_string(), entry(&leaf_pem, &key)),
                ("other".to_string(), entry(&other.0, &other.1)),
                ("root".to_string(), ca_entry(&root)),
            ]))
            .unwrap(),
        );
        // Our listener (as the engine's: it asks for client certificates too, which doctor
        // doesn't have), on every address.
        let serve = |name: &str| {
            let tls = ductus_core::tls::ListenTls {
                certificate: name.into(),
                clients: Some(ductus_core::tls::Clients { ca: "root".into(), subjects: vec![] }),
            };
            let acceptor = tokio_rustls::TlsAcceptor::from(certificates.server(&tls, &[]).unwrap());
            let listener = std::net::TcpListener::bind("0.0.0.0:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let port = listener.local_addr().unwrap().port();
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            tokio::spawn(async move {
                while let Ok((tcp, _)) = listener.accept().await {
                    let acceptor = acceptor.clone();
                    tokio::spawn(async move { drop(acceptor.accept(tcp).await) });
                }
            });
            port
        };
        let checked = |port: u16, name: &str| {
            let target = Target::Listener {
                host: "0.0.0.0".into(),
                port,
                protocol: "MLLP over TLS",
                tls: Some(name.into()),
                held: true,
                allow: None,
            };
            let certificates = certificates.clone();
            async move { check(vec![probe(target)], Duration::from_secs(2), &certificates).await.remove(0) }
        };

        let c = checked(serve("full"), "full").await;
        assert_eq!(c.outcome, Outcome::Ok, "{c:#?}");
        assert!(step(&c, "connect").detail.starts_with("127.0.0.1:"), "over loopback: {c:#?}");
        assert!(step(&c, "TLS").detail.contains("serves `full`: certificate for engine, issued by Test Intermediate"));
        assert!(step(&c, "TLS").detail.contains("covers engine.lab, 127.0.0.1"), "{c:#?}");
        assert!(step(&c, "TLS").detail.starts_with("TLS 1.3, post-quantum key exchange (X25519MLKEM768)"), "{c:#?}");
        assert_eq!(
            step(&c, "chain").detail,
            "complete: engine → Test Intermediate, issued by Test Root (the CA `root` in resources.yaml)"
        );
        assert!(c.advice.iter().any(|a| a.contains("can't know the name senders connect with")), "{c:#?}");
        assert!(c.certificate.is_none(), "ours aren't kept from a check: the engine reads them from memory");

        let c = checked(serve("short"), "short").await;
        assert_eq!((c.outcome, step(&c, "chain").outcome), (Outcome::Warn, Outcome::Warn), "{c:#?}");
        assert_eq!(
            step(&c, "chain").detail,
            "incomplete: only our certificate is served (engine), not the one that issued it (Test Intermediate)"
        );
        assert!(c.advice.iter().any(|a| a.contains("Java and .NET senders don't fetch a missing intermediate")));

        // A port serving another certificate than the one named (another program, an old engine).
        let c = checked(serve("other"), "full").await;
        assert_eq!(step(&c, "served").outcome, Outcome::Warn, "{c:#?}");
    }

    #[tokio::test]
    async fn checks_listening_ports_and_folders() {
        let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = held.local_addr().unwrap().port();
        let listener = || Target::Listener {
            host: "127.0.0.1".into(),
            port,
            protocol: "MLLP",
            tls: None,
            held: false,
            allow: None,
        };
        let c = one(listener()).await;
        assert_eq!((c.outcome, c.endpoint.as_str()), (Outcome::Warn, &*format!("127.0.0.1:{port} MLLP")));
        drop(held);
        assert_eq!(one(listener()).await.outcome, Outcome::Ok);

        let dir = tempfile::tempdir().unwrap();
        let c = one(Target::Folder { path: dir.path().into(), created_if_missing: false }).await;
        assert_eq!((c.outcome, step(&c, "folder").detail.as_str()), (Outcome::Ok, "exists; can write"));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0, "the probe file is removed");
        let missing = dir.path().join("in");
        let c = one(Target::Folder { path: missing.clone(), created_if_missing: false }).await;
        assert_eq!(c.outcome, Outcome::Fail);
        let c = one(Target::Folder { path: missing, created_if_missing: true }).await;
        assert_eq!(c.outcome, Outcome::Ok, "a destination creates its folder");
    }

    #[tokio::test]
    async fn reports_an_untrusted_certificate_and_an_apis_status() {
        // HTTPS with a self-signed certificate.
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
        let key = rustls::pki_types::PrivateKeyDer::try_from(cert.signing_key.serialize_der()).unwrap();
        let config =
            rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(vec![cert.cert.der().clone()], key)
                .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                tokio::spawn(async move { drop(acceptor.accept(tcp).await) });
            }
        });
        let c = one(https(&format!("https://localhost:{port}/orders"))).await;
        let tls = step(&c, "TLS");
        assert_eq!(tls.outcome, Outcome::Fail);
        assert!(tls.detail.starts_with("not trusted"), "{c:#?}");
        assert!(tls.detail.contains("certificate for rcgen self signed cert, self-signed, valid until"), "{c:#?}");
        assert!(c.advice.iter().any(|a| a.contains("trusted certificates")), "{c:#?}");

        // Plain HTTP API: its headers are sent, and a refused token says so.
        use axum::http::StatusCode;
        let app = axum::Router::new().route(
            "/v2",
            axum::routing::get(|h: axum::http::HeaderMap| async move {
                let good = h.get("authorization").is_some_and(|v| v == "Bearer good");
                if good { StatusCode::OK } else { StatusCode::UNAUTHORIZED }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v2", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let api = |token: &str| {
            serde_yaml_ng::from_str::<Api>(&format!(
                "{{ url: '{base}?key=s3cret', headers: {{ Authorization: 'Bearer {token}' }} }}"
            ))
            .unwrap()
        };
        let c = one(Target::Api("orders".into(), api("good"))).await;
        assert_eq!(c.outcome, Outcome::Ok, "{c:#?}");
        assert!(step(&c, "HTTP").detail.starts_with(&format!("GET {base} → 200 OK")), "no query string: {c:#?}");
        let c = one(Target::Api("orders".into(), api("bad"))).await;
        assert_eq!(step(&c, "HTTP").outcome, Outcome::Fail);
        assert!(c.advice.iter().any(|a| a.contains("refused the request's credentials")), "{c:#?}");
    }

    #[tokio::test]
    async fn logs_in_to_the_store_and_times_commits() {
        let Ok(url) = std::env::var("DUCTUS_TEST_DATABASE_URL") else { return };
        let c = one(Target::Store(url.clone())).await;
        assert_eq!(step(&c, "login").outcome, Outcome::Ok, "{c:#?}");
        assert!(step(&c, "login").detail.contains("PostgreSQL"), "{c:#?}");
        assert!(step(&c, "commits").detail.contains(&format!("median of {COMMITS}")), "{c:#?}");

        // A wrong password is named as such.
        let mut wrong: url::Url = url.parse().unwrap();
        wrong.set_password(Some("wrong-password")).unwrap();
        let c = one(Target::Store(wrong.to_string())).await;
        assert_eq!(step(&c, "login").outcome, Outcome::Fail, "{c:#?}");
        assert!(c.advice.iter().any(|a| a.contains("was refused")), "{c:#?}");
    }

    #[tokio::test]
    async fn logs_in_to_sql_server_and_names_a_refused_login() {
        let Some(c) = crate::mssql::tests::test_connection() else { return };
        let check = one(Target::Database("billing".into(), Database::Sqlserver(c.clone()))).await;
        assert_eq!(check.outcome, Outcome::Ok, "{check:#?}");
        assert!(step(&check, "login").detail.contains("Microsoft SQL Server"), "{check:#?}");

        let wrong = SqlServerConnection { password: "wrong-password".into(), ..c };
        let check = one(Target::Database("billing".into(), Database::Sqlserver(wrong))).await;
        assert_eq!(step(&check, "login").outcome, Outcome::Fail, "{check:#?}");
        assert!(!step(&check, "login").detail.contains(TRANSIENT), "{check:#?}");
        assert!(check.advice.iter().any(|a| a.contains("was refused")), "{check:#?}");
    }

    /// A certificate for `localhost` ending `days` whole days from today (UTC midnight; negative:
    /// already ended), signed by `issuer` (or by itself).
    fn certificate(
        name: &str,
        days: i64,
        ca: bool,
        issuer: Option<&rcgen::Issuer<'_, rcgen::KeyPair>>,
    ) -> (rcgen::Certificate, rcgen::CertificateParams, rcgen::KeyPair) {
        use chrono::Datelike;
        let date = |d: i64| {
            let day = (Utc::now() + chrono::Duration::days(d)).date_naive();
            rcgen::date_time_ymd(day.year(), day.month() as u8, day.day() as u8)
        };
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(vec!["localhost".to_string()]).unwrap();
        params.distinguished_name.push(rcgen::DnType::CommonName, name);
        params.not_before = date(-400);
        params.not_after = date(days);
        if ca {
            params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        }
        let cert = match issuer {
            Some(issuer) => params.signed_by(&key, issuer).unwrap(),
            None => params.self_signed(&key).unwrap(),
        };
        (cert, params, key)
    }

    fn tls_config(chain: &[&rcgen::Certificate], key: &rcgen::KeyPair) -> Arc<rustls::ServerConfig> {
        let key = rustls::pki_types::PrivateKeyDer::try_from(key.serialize_der()).unwrap();
        let config =
            rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(chain.iter().map(|c| c.der().clone()).collect(), key)
                .unwrap();
        Arc::new(config)
    }

    /// A TLS server on localhost presenting `config`'s chain; its port.
    async fn tls_server(config: Arc<rustls::ServerConfig>) -> u16 {
        let acceptor = tokio_rustls::TlsAcceptor::from(config);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                tokio::spawn(async move { drop(acceptor.accept(tcp).await) });
            }
        });
        port
    }

    async fn certificate_of(target: Target) -> (Check, SeenCertificate) {
        let c = check_with(vec![probe(target)], Duration::from_secs(2), Mode::Certificates, &Default::default())
            .await
            .remove(0);
        let seen = c.certificate.clone().unwrap_or_else(|| panic!("no certificate: {c:#?}"));
        (c, seen)
    }

    #[tokio::test]
    async fn reads_certificates_with_40_20_10_3_days_left_and_an_expired_one() {
        let warn = [30, 14, 7];
        for (days, stage) in [(40, None), (20, Some(30)), (10, Some(14)), (3, Some(7)), (-2, Some(0))] {
            let (cert, _, key) = certificate("Test Server", days, false, None);
            let port = tls_server(tls_config(&[&cert], &key)).await;
            let (c, seen) = certificate_of(https(&format!("https://localhost:{port}/orders"))).await;
            let ends = seen.chain_not_after.expect("read though not trusted");
            assert_eq!(ductus_core::alerts::days_left(ends, Utc::now()), days, "{seen:#?}");
            assert_eq!(ductus_core::alerts::certificate_stage(ends, Utc::now(), &warn), stage, "{days} days");
            assert_eq!((seen.host.as_str(), seen.port, seen.resource.as_str()), ("localhost", port, "test"));
            assert_eq!((seen.subject.as_deref(), seen.chain_earliest.as_deref()), (Some("Test Server"), None));
            assert_eq!(seen.not_after, seen.chain_not_after, "a chain of one");
            assert_eq!(seen.fingerprint.as_ref().map(String::len), Some(64), "SHA-256, hex");
            assert!(!seen.trusted, "self-signed: this server doesn't trust it");
            assert!(
                seen.error.as_deref().is_some_and(|e| e.contains("not trusted") || e.contains("expired")),
                "{c:#?}"
            );
            assert_eq!(step(&c, "TLS").outcome, Outcome::Fail);
        }
    }

    #[tokio::test]
    async fn the_earliest_expiry_in_the_chain_counts() {
        let (_, ca_params, ca_key) = certificate("Test Root", 3650, true, None);
        let root = rcgen::Issuer::new(ca_params, ca_key);
        let (intermediate, int_params, int_key) = certificate("Test Intermediate", 5, true, Some(&root));
        let intermediate_issuer = rcgen::Issuer::new(int_params, int_key);
        let (leaf, _, leaf_key) = certificate("Test Server", 60, false, Some(&intermediate_issuer));
        let port = tls_server(tls_config(&[&leaf, &intermediate], &leaf_key)).await;
        let (_, seen) = certificate_of(https(&format!("https://localhost:{port}/"))).await;
        let days = |t: Option<DateTime<Utc>>| ductus_core::alerts::days_left(t.unwrap(), Utc::now());
        assert_eq!((days(seen.not_after), days(seen.chain_not_after)), (60, 5), "{seen:#?}");
        assert_eq!(seen.chain_earliest.as_deref(), Some("Test Intermediate"));
        assert_eq!(seen.issuer.as_deref(), Some("Test Intermediate"));
    }

    /// A pretend mail server: greets, answers EHLO (offering STARTTLS if `offer`), then TLS.
    #[test]
    fn the_starttls_request_and_its_answers_are_ldap() {
        // The request parses as SEQUENCE { 1, [APPLICATION 23] { [0] the StartTLS OID } }.
        let (0x30, body, rest) = ber(&STARTTLS_REQUEST).unwrap() else { panic!() };
        assert!(rest.is_empty());
        let (0x02, [1], op) = ber(body).unwrap() else { panic!() };
        let (0x77, request, _) = ber(op).unwrap() else { panic!() };
        let (0x80, oid, _) = ber(request).unwrap() else { panic!() };
        assert_eq!(oid, b"1.3.6.1.4.1.1466.20037");

        // slapd's success, and a refusal with its message (protocolError, 2).
        let ok = [0x30, 0x0c, 0x02, 0x01, 0x01, 0x78, 0x07, 0x0a, 0x01, 0x00, 0x04, 0x00, 0x04, 0x00];
        assert_eq!(starttls_result(&ok), Some((0, String::new())));
        let mut refused = vec![0x30, 0x1d, 0x02, 0x01, 0x01, 0x78, 0x18, 0x0a, 0x01, 0x02, 0x04, 0x00, 0x04, 0x11];
        refused.extend_from_slice(b"unsupported op  ");
        refused.push(b'x');
        assert_eq!(starttls_result(&refused), Some((2, "unsupported op  x".into())));
        // Long-form lengths, and what isn't LDAP.
        let long = [0x30, 0x81, 0x0c, 0x02, 0x01, 0x01, 0x78, 0x07, 0x0a, 0x01, 0x00, 0x04, 0x00, 0x04, 0x00];
        assert_eq!(starttls_result(&long), Some((0, String::new())));
        assert_eq!(starttls_result(b"220 smtp.example.org ESMTP\r\n"), None);
        assert_eq!(starttls_result(&ok[..8]), None, "cut short");
    }

    async fn smtp_server(config: Arc<rustls::ServerConfig>, offer: bool) -> u16 {
        let acceptor = tokio_rustls::TlsAcceptor::from(config);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let mut smtp = tokio::io::BufReader::new(tcp);
                    let mut line = String::new();
                    smtp.get_mut().write_all(b"220 mail.test ESMTP\r\n").await?;
                    smtp.read_line(&mut line).await?;
                    let caps: &[u8] = if offer { b"250-mail.test\r\n250 STARTTLS\r\n" } else { b"250 mail.test\r\n" };
                    smtp.get_mut().write_all(caps).await?;
                    line.clear();
                    smtp.read_line(&mut line).await?;
                    if line.starts_with("STARTTLS") {
                        smtp.get_mut().write_all(b"220 go ahead\r\n").await?;
                        drop(acceptor.accept(smtp.into_inner()).await);
                    }
                    Ok::<_, std::io::Error>(())
                });
            }
        });
        port
    }

    #[tokio::test]
    async fn reads_the_mail_server_s_certificate_after_starttls() {
        let (cert, _, key) = certificate("Test Mail", 10, false, None);
        let config = tls_config(&[&cert], &key);
        let port = smtp_server(config.clone(), true).await;
        let target = Target::Smtp { host: "localhost".into(), port, tls: SmtpTls::Starttls, trust: Default::default() };
        let (c, seen) = certificate_of(target).await;
        assert_eq!(step(&c, "SMTP").outcome, Outcome::Ok, "{c:#?}");
        assert!(step(&c, "SMTP").detail.contains("STARTTLS offered and accepted"), "{c:#?}");
        assert_eq!(seen.subject.as_deref(), Some("Test Mail"));
        assert_eq!(c.endpoint, format!("localhost:{port} SMTP"));

        let port = smtp_server(config.clone(), false).await;
        let target = Target::Smtp { host: "localhost".into(), port, tls: SmtpTls::Starttls, trust: Default::default() };
        let (c, seen) = certificate_of(target).await;
        assert_eq!(step(&c, "SMTP").detail, "the server doesn't offer STARTTLS");
        assert!(c.advice.iter().any(|a| a.contains("usually 587")), "{c:#?}");
        assert_eq!((seen.fingerprint, seen.error.as_deref()), (None, Some("the server doesn't offer STARTTLS")));

        // TLS from the first byte (port 465).
        let port = tls_server(config).await;
        let target = Target::Smtp { host: "localhost".into(), port, tls: SmtpTls::Tls, trust: Default::default() };
        let (_, seen) = certificate_of(target).await;
        assert_eq!(seen.subject.as_deref(), Some("Test Mail"));
    }

    #[tokio::test]
    async fn checks_an_mllp_receiver_over_tls_as_the_engine_connects() {
        use crate::tls::testing::{ca, connecting, receiver_and_client, receiving};
        let (server_ca, client_ca) = (ca("receiver's CA"), ca("clients' CA"));
        let certificates = Arc::new(receiver_and_client(&server_ca, &client_ca));
        // A receiver that asks for a client certificate and keeps the connection open, as MLLP does.
        let acceptor = tokio_rustls::TlsAcceptor::from(certificates.server(&receiving(true), &[]).unwrap());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    use tokio::io::AsyncReadExt;
                    if let Ok(mut tls) = acceptor.accept(tcp).await {
                        let _ = tls.read(&mut [0u8; 16]).await;
                    }
                });
            }
        });
        let mllp = |tls| Target::Mllp { host: "localhost".into(), port, tls: Some(tls) };
        let check = |target| {
            let certificates = certificates.clone();
            async move { check(vec![probe(target)], Duration::from_secs(2), &certificates).await.remove(0) }
        };

        let c = check(mllp(connecting(Some("server-ca"), Some("client")))).await;
        // Not the whole check: where localhost is also ::1 (CI), that address warns (nothing listens there).
        assert_eq!(step(&c, "TLS").outcome, Outcome::Ok, "{c:#?}");
        assert!(step(&c, "TLS").detail.contains("presented client certificate `client`"), "{c:#?}");
        assert!(step(&c, "TLS").detail.contains("post-quantum key exchange (X25519MLKEM768), trusted"), "{c:#?}");
        assert_eq!(c.endpoint, format!("localhost:{port} MLLP over TLS"));
        assert!(c.certificate.as_ref().is_some_and(|s| s.trusted && s.subject.as_deref() == Some("receiver")));

        // What the engine would see: no client certificate, or its CA not named.
        let c = check(mllp(connecting(Some("server-ca"), None))).await;
        assert!(step(&c, "TLS").detail.contains("the receiver wants a client certificate"), "{c:#?}");
        assert!(c.advice.iter().any(|a| a.contains("name one in `certificate`")), "{c:#?}");
        let c = check(mllp(connecting(None, Some("client")))).await;
        assert!(step(&c, "TLS").detail.starts_with("not trusted"), "{c:#?}");
        assert!(c.advice.iter().any(|a| a.contains("Name the issuing CA in `ca`")), "{c:#?}");
        // A name that doesn't load fails the check with why.
        let c = check(mllp(connecting(Some("missing"), None))).await;
        assert!(step(&c, "TLS").detail.contains("no CA named `missing`"), "{c:#?}");
    }

    #[tokio::test]
    async fn a_server_that_can_t_be_reached_has_an_error_and_no_certificate() {
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let (_, seen) = certificate_of(https(&format!("https://127.0.0.1:{free}/"))).await;
        assert_eq!((seen.fingerprint.as_deref(), seen.chain_not_after), (None, None));
        assert!(seen.error.as_deref().is_some_and(|e| e.ends_with("refused")), "{seen:#?}");
    }

    #[test]
    fn only_what_has_a_certificate_is_checked_in_certificates_mode() {
        let alerts: AlertSettings = serde_yaml_ng::from_str(
            "smtp: { host: mail.example.org, from: d@example.org }
channels:
  chat: { webhook: { url: 'https://hooks.example.org/T0/secret', format: slack } }
  plain: { webhook: { url: 'http://10.1.1.1/hook' } }
heartbeat: { url: 'https://hb.example.org/ping/abc' }",
        )
        .unwrap();
        let mut probes = alert_probes(&alerts);
        probes.push(probe(Target::Mllp { host: "lab".into(), port: 6661, tls: None }));
        probes.push(probe(Target::Mllp { host: "hie".into(), port: 7443, tls: Some(Default::default()) }));
        let kept: Vec<(String, Option<(String, u16)>)> =
            certificates_only(probes).into_iter().map(|p| (p.resource.unwrap(), p.target.address())).collect();
        assert_eq!(
            kept,
            [
                ("alerts.smtp".into(), Some(("mail.example.org".into(), 587))),
                ("alerts.chat".into(), Some(("hooks.example.org".into(), 443))),
                ("alerts.heartbeat".into(), Some(("hb.example.org".into(), 443))),
                ("test".into(), Some(("hie".into(), 7443))),
            ]
        );
    }

    #[test]
    fn rounds_and_formats() {
        assert_eq!((round(412.7), round(3125.0), round(8.0)), ("410".into(), "3100".into(), "8".into()));
        assert_eq!(took(Duration::from_micros(2_440)), "2.4 ms");
        assert_eq!(took(Duration::from_millis(5_000)), "5.0 s");
    }
}
