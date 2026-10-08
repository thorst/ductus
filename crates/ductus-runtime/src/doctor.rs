//! `ductus doctor`: does every connection the project needs work from here? Each is checked
//! step by step (DNS, a TCP connection to every address, TLS, then the protocol's own login),
//! and a failure is explained in plain words: what it most likely means, and what to ask for
//! (a firewall request names this server's address, the target's and the port).
//!
//! The checks go through the engine's own connection code (the same Postgres options, SQL
//! Server pool, SFTP client and HTTP client), so what passes here connects the same way in
//! the engine. Nothing is ever sent to a destination: MLLP and HTTP destinations are only
//! connected to.

use crate::{mssql, services, sftp};
use ductus_core::resources::{Api, Database, SftpServer, SqlServerConnection};
use ductus_script::TRANSIENT;
use serde::Serialize;
use sqlx::Connection;
use sqlx::postgres::{PgConnectOptions, PgConnection};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::TcpStream;

/// Commits timed against the engine's database: enough for a stable median, quick to run.
const COMMITS: usize = 20;
/// Warn when a certificate expires within this many days.
const EXPIRY_WARNING_DAYS: i64 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
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
#[derive(Debug, Clone, Serialize)]
pub struct Step {
    pub name: &'static str,
    pub outcome: Outcome,
    pub detail: String,
}

/// The result for one thing the project connects to.
#[derive(Debug, Clone, Serialize)]
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
}

/// Something to check.
#[derive(Debug, Clone)]
pub enum Target {
    /// The engine's own database (`DUCTUS_DATABASE_URL`); also times commits.
    Store(String),
    Database(String, Database),
    Sftp(String, SftpServer),
    Api(String, Api),
    Mllp {
        host: String,
        port: u16,
    },
    Http {
        url: String,
    },
    /// A local folder a file source reads or a file destination writes. Destinations create
    /// missing folders; sources need them to exist.
    Folder {
        path: PathBuf,
        created_if_missing: bool,
    },
    Listener {
        host: String,
        port: u16,
        protocol: &'static str,
    },
}

#[derive(Debug, Clone)]
pub struct Probe {
    pub what: String,
    pub target: Target,
    pub used_by: Vec<String>,
}

/// The whole report: what `ductus doctor` prints and `--report` saves.
#[derive(Debug, Clone, Serialize)]
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
/// return the results in the order given. `timeout` bounds each step.
pub async fn check(probes: Vec<Probe>, timeout: Duration) -> Vec<Check> {
    let mut tasks = tokio::task::JoinSet::new();
    for (n, probe) in probes.into_iter().enumerate() {
        tasks.spawn(async move { (n, run(probe, timeout).await) });
    }
    let mut checks: Vec<(usize, Check)> = tasks.join_all().await;
    checks.sort_by_key(|(n, _)| *n);
    checks.into_iter().map(|(_, c)| c).collect()
}

async fn run(probe: Probe, timeout: Duration) -> Check {
    let mut r = Run { steps: Vec::new(), advice: Vec::new(), timeout, from: None };
    let endpoint = match &probe.target {
        Target::Store(url) => r.store(url).await,
        Target::Database(_, Database::Postgres(c)) => {
            let endpoint = format!("{}:{} Postgres", c.host, c.port.unwrap_or(5432));
            if r.reach(&c.host, c.port.unwrap_or(5432)).await {
                r.postgres_login(&services::postgres_options(c)).await;
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
        Target::Api(_, api) => r.http(&api.url, Some(api)).await,
        Target::Http { url } => r.http(url, None).await,
        Target::Mllp { host, port } => {
            r.reach(host, *port).await;
            format!("{host}:{port} MLLP")
        }
        Target::Folder { path, created_if_missing } => {
            r.folder(path, *created_if_missing);
            path.display().to_string()
        }
        Target::Listener { host, port, protocol } => {
            r.listen(host, *port).await;
            format!("{host}:{port} {protocol}")
        }
    };
    let outcome = r.steps.iter().map(|s| s.outcome).max().unwrap_or(Outcome::Ok);
    Check { what: probe.what, endpoint, used_by: probe.used_by, outcome, steps: r.steps, advice: r.advice }
}

/// The steps of one check as they happen.
struct Run {
    steps: Vec<Step>,
    advice: Vec<String>,
    timeout: Duration,
    /// This server's address towards the target (for firewall requests and pg_hba advice).
    from: Option<IpAddr>,
}

impl Run {
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
            let attempt = tokio::time::timeout(self.timeout, TcpStream::connect(address)).await;
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

    /// The engine's database: login, then how long a durable commit takes.
    async fn store(&mut self, url: &str) -> String {
        let options: PgConnectOptions = match url.parse() {
            Ok(o) => o,
            Err(e) => {
                self.step("URL", Outcome::Fail, format!("not a Postgres URL: {e}"));
                return "(DUCTUS_DATABASE_URL)".into();
            }
        };
        let (host, port) = (options.get_host().to_string(), options.get_port());
        let endpoint = format!("{host}:{port} Postgres");
        if host.starts_with('/') {
            self.step("socket", Outcome::Ok, format!("local socket in {host}"));
        } else if !self.reach(&host, port).await {
            return endpoint;
        }
        let Some(mut connection) = self.postgres_login(&options).await else { return endpoint };
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

    async fn postgres_login(&mut self, options: &PgConnectOptions) -> Option<PgConnection> {
        let user = options.get_username().to_string();
        let database = options.get_database().unwrap_or(&user).to_string();
        let attempt = tokio::time::timeout(self.timeout, PgConnection::connect_with(options)).await;
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
        self.step("login", Outcome::Ok, format!("as `{user}`, database `{database}`{version}"));
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
        let mut request = services::http_client().get(url).timeout(self.timeout);
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

    /// A TLS handshake, verified the way the engine's HTTP client verifies (the system's
    /// trusted certificates), reporting the server's certificate either way.
    async fn tls(&mut self, host: &str, address: SocketAddr) -> bool {
        use rustls::pki_types::ServerName;
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let verifier = match rustls_platform_verifier::Verifier::new(provider.clone()) {
            Ok(v) => Recording { inner: v, seen: seen.clone() },
            Err(e) => {
                self.step("TLS", Outcome::Fail, format!("can't load this server's trusted certificates: {e}"));
                return false;
            }
        };
        let config = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("the default provider supports the default versions")
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(verifier))
            .with_no_client_auth();
        let Ok(name) = ServerName::try_from(host.to_string()) else {
            self.step("TLS", Outcome::Fail, format!("`{host}` isn't a valid name for TLS"));
            return false;
        };
        let handshake = async {
            let tcp = TcpStream::connect(address).await?;
            tokio_rustls::TlsConnector::from(Arc::new(config)).connect(name, tcp).await
        };
        let result = tokio::time::timeout(self.timeout, handshake).await;
        let leaf = seen.lock().expect("certificate lock").first().map(|c| Certificate::read(c));
        let leaf = leaf.flatten();
        let about = leaf.as_ref().map(Certificate::describe).unwrap_or_default();
        match result {
            Ok(Ok(stream)) => {
                let version = stream.get_ref().1.protocol_version().map(|v| format!("{v:?}")).unwrap_or_default();
                let version = version.replace("TLSv1_", "TLS 1.");
                let days = leaf.as_ref().map(|c| c.days_left()).unwrap_or(i64::MAX);
                if days < EXPIRY_WARNING_DAYS {
                    self.step("TLS", Outcome::Warn, format!("{version}, trusted; {about}"));
                    self.advise(format!(
                        "The certificate expires in {days} day(s): ask the server's owner to renew it before then."
                    ));
                } else {
                    self.step("TLS", Outcome::Ok, format!("{version}, trusted; {about}"));
                }
                true
            }
            Ok(Err(e)) => {
                let tls = e.get_ref().and_then(|i| i.downcast_ref::<rustls::Error>()).cloned();
                let (detail, advice) = tls_failure(host, tls.as_ref(), &e, leaf.as_ref());
                self.step("TLS", Outcome::Fail, if about.is_empty() { detail } else { format!("{detail}; {about}") });
                self.advise(advice);
                false
            }
            Err(_) => {
                self.step("TLS", Outcome::Fail, format!("no TLS answer in {}", self.waited()));
                self.advise("The port accepts connections but doesn't speak TLS (in time): is it plain http, or the wrong port?");
                false
            }
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

    async fn listen(&mut self, host: &str, port: u16) {
        match tokio::net::TcpListener::bind((host, port)).await {
            Ok(_) => self.step("port", Outcome::Ok, "free: the engine can listen here"),
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                self.step("port", Outcome::Warn, "in use: by a running engine, or another program");
                self.advise(format!(
                    "If the engine isn't running on this server, another program holds port {port}: find it with `ss -ltnp` (Linux) or `netstat -ano` (Windows)."
                ));
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
        self.inner.verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
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

/// What a report says about a certificate.
struct Certificate {
    subject: String,
    issuer: String,
    names: Vec<String>,
    not_after: chrono::DateTime<chrono::Utc>,
}

impl Certificate {
    fn read(der: &[u8]) -> Option<Self> {
        use x509_parser::prelude::*;
        let (_, cert) = X509Certificate::from_der(der).ok()?;
        let name = |n: &X509Name| {
            n.iter_common_name()
                .next()
                .and_then(|cn| cn.as_str().ok().map(str::to_string))
                .or_else(|| n.iter_organization().next().and_then(|o| o.as_str().ok().map(str::to_string)))
                .unwrap_or_else(|| n.to_string())
        };
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
        Some(Self {
            subject: name(cert.subject()),
            issuer: name(cert.issuer()),
            names,
            not_after: chrono::DateTime::from_timestamp(cert.validity().not_after.timestamp(), 0)?,
        })
    }

    fn days_left(&self) -> i64 {
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
            "Add the issuing CA to this server's trusted certificates (the engine uses the system's), or ask for a certificate from a public CA.".into(),
        ),
        C::NotValidYet | C::NotValidYetContext { .. } => (
            "the certificate isn't valid yet".into(),
            "The certificate's start date is in the future: check this server's clock, and the server's certificate.".into(),
        ),
        C::Revoked => ("the certificate was revoked".into(), "The certificate was revoked: its owner must replace it.".into()),
        other => (format!("certificate rejected: {other:?}"), "The server's certificate was rejected; show this to its owner.".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(target: Target) -> Probe {
        Probe { what: "test".into(), target, used_by: vec!["x".into()] }
    }

    async fn one(target: Target) -> Check {
        check(vec![probe(target)], Duration::from_secs(2)).await.remove(0)
    }

    fn step<'a>(c: &'a Check, name: &str) -> &'a Step {
        c.steps.iter().find(|s| s.name == name).unwrap_or_else(|| panic!("no {name} step: {c:#?}"))
    }

    #[tokio::test]
    async fn explains_refused_connections_and_names_that_dont_resolve() {
        // A port nothing listens on.
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let c = one(Target::Mllp { host: "127.0.0.1".into(), port: free }).await;
        assert_eq!(c.outcome, Outcome::Fail);
        assert_eq!(step(&c, "address").outcome, Outcome::Ok);
        assert!(step(&c, "connect").detail.ends_with("from 127.0.0.1: refused"), "{c:#?}");
        assert!(c.advice.iter().any(|a| a.contains("nothing accepts connections on port")), "{c:#?}");

        let c = one(Target::Mllp { host: "no-such-host.invalid".into(), port: 1 }).await;
        assert_eq!(step(&c, "DNS").outcome, Outcome::Fail);
        assert!(c.advice.iter().any(|a| a.contains("can't look up `no-such-host.invalid`")), "{c:#?}");

        // Something listening.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let c = one(Target::Mllp { host: "localhost".into(), port }).await;
        assert!(
            c.steps.iter().any(|s| s.name == "connect" && s.outcome == Outcome::Ok),
            "connects to 127.0.0.1: {c:#?}"
        );
        assert_ne!(c.outcome, Outcome::Fail, "{c:#?}");
        assert!(step(&c, "DNS").detail.starts_with("localhost → "), "{c:#?}");
    }

    #[tokio::test]
    async fn checks_listening_ports_and_folders() {
        let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = held.local_addr().unwrap().port();
        let c = one(Target::Listener { host: "127.0.0.1".into(), port, protocol: "MLLP" }).await;
        assert_eq!((c.outcome, c.endpoint.as_str()), (Outcome::Warn, &*format!("127.0.0.1:{port} MLLP")));
        drop(held);
        assert_eq!(
            one(Target::Listener { host: "127.0.0.1".into(), port, protocol: "MLLP" }).await.outcome,
            Outcome::Ok
        );

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
        let c = one(Target::Http { url: format!("https://localhost:{port}/orders") }).await;
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

    #[test]
    fn rounds_and_formats() {
        assert_eq!((round(412.7), round(3125.0), round(8.0)), ("410".into(), "3100".into(), "8".into()));
        assert_eq!(took(Duration::from_micros(2_440)), "2.4 ms");
        assert_eq!(took(Duration::from_millis(5_000)), "5.0 s");
    }
}
