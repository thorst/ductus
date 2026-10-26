//! TLS on what the engine listens on (MLLP and HTTP sources, the API), and on what it connects
//! to (MLLP and HTTP destinations, `apis:`, directories, the alerts' mail server and webhooks).
//!
//! Certificates are `certificates:` entries in resources.yaml. All of them are loaded at start,
//! and one that doesn't load (missing file, no key, a key that isn't the certificate's, expired)
//! stops the start with the reason: a typo is found at deploy, not by the first sender. Pairs
//! read from files are checked again every minute by one task (a hash of both files' contents,
//! no database work): a renewed pair is used for new connections (ours and client certificates
//! alike), a broken one is logged and the pair in use stays. Text from secrets changes with a
//! deploy (a restart).
//!
//! Every outgoing connection verifies the same way (`Trust`): the platform verifier (the system's
//! trusted certificates and its checks) plus the named CA, the name checked against the host
//! connected to, and the named pair as the client certificate. `ductus doctor` builds its
//! handshake from the same `Trust`, so it sees what the engine would.

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use ductus_core::resources::{CertificateEntry, Pem};
use ductus_core::tls::{ConnectTls, ListenTls, Presented, SubjectPattern};
use rustls::client::ResolvesClientCert;
use rustls::client::danger::ServerCertVerifier;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, RwLock};
use std::task::{Context as TaskContext, Poll};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::server::TlsStream;

/// How long a sender may take to finish its handshake.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// How often certificate files are checked for a renewal.
const RELOAD_EVERY: Duration = Duration::from_secs(60);

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::aws_lc_rs::default_provider())
}

/// What a certificate says about itself (the first in its chain).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct About {
    pub subject: String,
    /// DNS names and IP addresses it covers.
    pub names: Vec<String>,
    pub not_before: DateTime<Utc>,
    pub not_after: DateTime<Utc>,
}

impl About {
    /// What a certificate (DER) says about itself.
    pub fn read(der: &[u8]) -> Result<Self> {
        use x509_parser::prelude::*;
        let (_, cert) = X509Certificate::from_der(der).map_err(|e| anyhow!("not a certificate: {e}"))?;
        let mut names = Vec::new();
        if let Ok(Some(san)) = cert.subject_alternative_name() {
            for name in &san.value.general_names {
                match name {
                    GeneralName::DNSName(d) => names.push(d.to_string()),
                    GeneralName::IPAddress(ip) => match ip.len() {
                        4 => names.push(IpAddr::from(<[u8; 4]>::try_from(*ip).expect("4 bytes")).to_string()),
                        16 => names.push(IpAddr::from(<[u8; 16]>::try_from(*ip).expect("16 bytes")).to_string()),
                        _ => {}
                    },
                    _ => {}
                }
            }
        }
        let time = |t: ASN1Time| DateTime::from_timestamp(t.timestamp(), 0).unwrap_or_default();
        Ok(Self {
            subject: cert.subject().to_string(),
            names,
            not_before: time(cert.validity().not_before),
            not_after: time(cert.validity().not_after),
        })
    }
}

/// The subject attributes and DNS names of a client certificate, for `subjects:` patterns.
fn presented(der: &[u8]) -> Option<Presented> {
    use x509_parser::prelude::*;
    let (_, cert) = X509Certificate::from_der(der).ok()?;
    let mut out = Presented::default();
    for attribute in cert.subject().iter_attributes() {
        let short = match attribute.attr_type().to_id_string().as_str() {
            "2.5.4.3" => "CN",
            "2.5.4.10" => "O",
            "2.5.4.11" => "OU",
            "2.5.4.6" => "C",
            "2.5.4.7" => "L",
            "2.5.4.8" => "ST",
            "0.9.2342.19200300.100.1.25" => "DC",
            _ => continue,
        };
        if let Ok(value) = attribute.as_str() {
            out.subject.push((short.to_string(), value.to_string()));
        }
    }
    if let Ok(Some(san)) = cert.subject_alternative_name() {
        for name in &san.value.general_names {
            if let GeneralName::DNSName(d) = name {
                out.dns.push(d.to_string());
            }
        }
    }
    Some(out)
}

/// A pair's PEM text: certificates, key.
pub type PairPem = Arc<(Vec<u8>, Vec<u8>)>;

/// A certificate with its key, ready to present.
struct Loaded {
    key: Arc<CertifiedKey>,
    about: About,
    /// Of the PEM it was made from: a file whose contents hash the same hasn't changed.
    hash: [u8; 32],
    /// The PEM itself (certificates, key), for a client that takes only that (lettre's SMTP).
    pem: PairPem,
}

// Hand-written so the private key never ends up in logs.
impl std::fmt::Debug for Loaded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Loaded").field("about", &self.about).finish_non_exhaustive()
    }
}

fn load_pair(cert_pem: &[u8], key_pem: &[u8]) -> Result<Loaded> {
    let chain = CertificateDer::pem_slice_iter(cert_pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| anyhow!("reading the certificate: {e}"))?;
    let Some(leaf) = chain.first() else {
        bail!("no certificate in it (PEM text starting -----BEGIN CERTIFICATE-----)");
    };
    let about = About::read(leaf)?;
    let key = PrivateKeyDer::from_pem_slice(key_pem).map_err(|e| anyhow!("reading the private key: {e}"))?;
    let key = CertifiedKey::from_der(chain, key, &provider()).map_err(|e| match e {
        rustls::Error::InconsistentKeys(_) => anyhow!("the private key isn't this certificate's key"),
        e => anyhow!("the private key can't be used: {e}"),
    })?;
    Ok(Loaded {
        key: Arc::new(key),
        about,
        hash: hash(&[cert_pem, key_pem]),
        pem: Arc::new((cert_pem.to_vec(), key_pem.to_vec())),
    })
}

fn hash(parts: &[&[u8]]) -> [u8; 32] {
    let mut h = Sha256::new();
    for p in parts {
        h.update((p.len() as u64).to_le_bytes());
        h.update(p);
    }
    h.finalize().into()
}

/// A `certificates:` pair: what a listener presents. Renewed in place from its files.
#[derive(Debug)]
pub struct Pair {
    pub name: String,
    /// `cert_file` and `key_file`, when it comes from files (re-read every minute).
    files: Option<(PathBuf, PathBuf)>,
    current: RwLock<Arc<Loaded>>,
    /// The last renewal that didn't load, and why (so it's logged once, not every minute).
    failed: Mutex<Option<([u8; 32], String)>>,
    /// Told the new PEM when it is renewed: for clients that copy it in (a Postgres pool's
    /// connect options), not those that ask for it at each handshake.
    renewed: Renewed,
}

type OnRenewal = Box<dyn Fn(&PairPem) + Send + Sync>;

#[derive(Default)]
struct Renewed(Mutex<Vec<OnRenewal>>);

impl std::fmt::Debug for Renewed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} listener(s)", self.0.lock().expect("renewal lock").len())
    }
}

impl Pair {
    /// A pair read from its files here (not a `certificates:` entry: the engine database URL's
    /// `sslcert` and `sslkey`), for doctor's handshake.
    pub fn from_files(name: &str, cert: &Path, key: &Path) -> Result<Self> {
        let loaded = load_pair(&read(cert)?, &read(key)?)?;
        Ok(Self::new(name, loaded, None))
    }

    fn new(name: &str, loaded: Loaded, files: Option<(PathBuf, PathBuf)>) -> Self {
        Pair {
            name: name.to_string(),
            files,
            current: RwLock::new(Arc::new(loaded)),
            failed: Mutex::new(None),
            renewed: Renewed::default(),
        }
    }

    /// Call `f` with the new PEM each time the pair is renewed from its files.
    pub fn on_renewal(&self, f: impl Fn(&PairPem) + Send + Sync + 'static) {
        self.renewed.0.lock().expect("renewal lock").push(Box::new(f));
    }

    pub fn about(&self) -> About {
        self.current.read().expect("certificate lock").about.clone()
    }

    /// The PEM in use (certificates, key) and the hash that changes with a renewal.
    pub fn pem(&self) -> ([u8; 32], PairPem) {
        let current = self.current.read().expect("certificate lock");
        (current.hash, current.pem.clone())
    }

    /// Why the files on disk aren't in use, if they changed and didn't load.
    pub fn renewal_error(&self) -> Option<String> {
        self.failed.lock().expect("certificate lock").as_ref().map(|(_, why)| why.clone())
    }

    /// The chain in use, leaf first (as senders get it).
    pub fn chain(&self) -> Vec<CertificateDer<'static>> {
        self.current.read().expect("certificate lock").key.cert.clone()
    }

    /// Read the files again; use them if they changed and load. True if what the pair shows
    /// changed: renewed, or its renewal error appeared, changed or went.
    async fn reload(&self) -> bool {
        let before = self.renewal_error();
        let renewed = self.reload_files().await;
        renewed || self.renewal_error() != before
    }

    /// True if renewed.
    async fn reload_files(&self) -> bool {
        let Some((cert, key)) = &self.files else { return false };
        let read = async { Ok::<_, std::io::Error>((tokio::fs::read(cert).await?, tokio::fs::read(key).await?)) };
        let (outcome, seen) = match read.await {
            Ok((c, k)) => {
                let seen = hash(&[&c, &k]);
                if seen == self.current.read().expect("certificate lock").hash {
                    *self.failed.lock().expect("certificate lock") = None;
                    return false;
                }
                (load_pair(&c, &k), seen)
            }
            Err(e) => (Err(anyhow!("can't read {} or {}: {e}", cert.display(), key.display())), [0; 32]),
        };
        let outcome = outcome.and_then(|loaded| {
            if loaded.about.not_after < Utc::now() {
                bail!("the new certificate expired on {}", loaded.about.not_after.format("%Y-%m-%d"));
            }
            Ok(loaded)
        });
        match outcome {
            Ok(loaded) => {
                tracing::info!(
                    certificate = %self.name,
                    subject = %loaded.about.subject,
                    expires = %loaded.about.not_after.format("%Y-%m-%d"),
                    "certificate renewed: new connections use it"
                );
                let pem = loaded.pem.clone();
                *self.current.write().expect("certificate lock") = Arc::new(loaded);
                *self.failed.lock().expect("certificate lock") = None;
                for f in self.renewed.0.lock().expect("renewal lock").iter() {
                    f(&pem);
                }
                true
            }
            Err(e) => {
                let why = format!("{e:#}");
                let mut failed = self.failed.lock().expect("certificate lock");
                if failed.as_ref().is_none_or(|(h, w)| *h != seen || *w != why) {
                    tracing::error!(
                        certificate = %self.name,
                        error = %why,
                        "the certificate files changed but can't be used: still presenting the one loaded before"
                    );
                }
                *failed = Some((seen, why));
                false
            }
        }
    }
}

impl ResolvesServerCert for Pair {
    fn resolve(&self, _hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(self.current.read().expect("certificate lock").key.clone())
    }
}

/// As our client certificate: always the one in use (a renewal applies to new connections).
impl ResolvesClientCert for Pair {
    fn resolve(&self, _hints: &[&[u8]], _schemes: &[rustls::SignatureScheme]) -> Option<Arc<CertifiedKey>> {
        Some(self.current.read().expect("certificate lock").key.clone())
    }

    fn has_certs(&self) -> bool {
        true
    }
}

/// A `certificates:` CA: what servers or clients it issued are checked against.
#[derive(Debug)]
struct Authority {
    store: Arc<rustls::RootCertStore>,
    certificates: Vec<CertificateDer<'static>>,
    /// As written, for a client that takes PEM (sqlx's `ssl_root_cert_from_pem`).
    pem: Vec<u8>,
}

/// Every `certificates:` entry, loaded.
#[derive(Debug, Default)]
pub struct Certificates {
    pairs: BTreeMap<String, Arc<Pair>>,
    cas: BTreeMap<String, Authority>,
    /// Entries that didn't load, and why (only `load_what_loads` keeps any).
    broken: BTreeMap<String, String>,
    /// Client settings already built, by what they name: destinations naming the same CA and
    /// certificate share one (and its session cache; the platform's roots are read once).
    clients: Mutex<HashMap<ConnectTls, Arc<rustls::ClientConfig>>>,
}

impl Certificates {
    /// Load every entry. One that doesn't load, or a pair that has expired, is an error naming
    /// it and why.
    pub fn load(entries: &BTreeMap<String, CertificateEntry>) -> Result<Self> {
        let out = Self::load_what_loads(entries);
        match out.broken.values().next() {
            Some(why) => bail!("{why}"),
            None => Ok(out),
        }
    }

    /// Load every entry that loads; the others are kept with why, and connections naming them
    /// fail with it (`ductus doctor` shows that rather than stopping).
    pub fn load_what_loads(entries: &BTreeMap<String, CertificateEntry>) -> Self {
        let mut out = Self::default();
        for (name, entry) in entries {
            if let Err(e) = out.add(name, entry) {
                out.broken
                    .insert(name.clone(), format!("{:#}", e.context(format!("certificate `{name}` (resources.yaml)"))));
            }
        }
        out
    }

    fn add(&mut self, name: &str, entry: &CertificateEntry) -> Result<()> {
        match entry.pem() {
            Some(Pem::Files { cert, key }) => {
                let loaded = load_pair(&read(cert)?, &read(key)?)?;
                self.add_pair(name, loaded, Some((cert.to_path_buf(), key.to_path_buf())))
            }
            Some(Pem::Text { cert, key }) => self.add_pair(name, load_pair(cert.as_bytes(), key.as_bytes())?, None),
            Some(Pem::CaText(ca)) => self.add_ca(name, ca.as_bytes()),
            Some(Pem::CaFile(file)) => self.add_ca(name, &read(file)?),
            None => bail!("neither a certificate with its key nor a CA"),
        }
    }

    fn add_ca(&mut self, name: &str, pem: &[u8]) -> Result<()> {
        let (store, certificates) = roots(pem)?;
        self.cas.insert(name.to_string(), Authority { store: Arc::new(store), certificates, pem: pem.to_vec() });
        Ok(())
    }

    fn add_pair(&mut self, name: &str, loaded: Loaded, files: Option<(PathBuf, PathBuf)>) -> Result<()> {
        let now = Utc::now();
        if loaded.about.not_after < now {
            bail!(
                "it expired on {} ({}): renew it, or replace the files",
                loaded.about.not_after.format("%Y-%m-%d"),
                loaded.about.subject
            );
        }
        if loaded.about.not_before > now {
            tracing::warn!(
                certificate = name,
                valid_from = %loaded.about.not_before,
                "certificate isn't valid yet: senders will refuse it until then (is this server's clock right?)"
            );
        }
        self.pairs.insert(name.to_string(), Arc::new(Pair::new(name, loaded, files)));
        Ok(())
    }

    pub fn pairs(&self) -> impl Iterator<Item = &Arc<Pair>> {
        self.pairs.values()
    }

    /// The TLS settings for a listener's `tls:` (names checked by `ductus_core::tls::check`).
    /// `alpn`: the protocols it speaks (`http/1.1` for HTTP; none for MLLP).
    pub fn server(&self, tls: &ListenTls, alpn: &[&[u8]]) -> Result<Arc<rustls::ServerConfig>> {
        let pair = self
            .pairs
            .get(&tls.certificate)
            .with_context(|| format!("no certificate with its key named `{}`", tls.certificate))?
            .clone();
        let builder = rustls::ServerConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .context("TLS 1.2 and 1.3")?;
        let builder = match &tls.clients {
            None => builder.with_no_client_auth(),
            Some(clients) => {
                let ca = self.cas.get(&clients.ca).with_context(|| format!("no CA named `{}`", clients.ca))?;
                let inner = rustls::server::WebPkiClientVerifier::builder_with_provider(ca.store.clone(), provider())
                    .build()
                    .with_context(|| format!("CA `{}`", clients.ca))?;
                let subjects = clients.subjects.iter().map(|s| SubjectPattern::parse(s)).collect::<Result<Vec<_>>>()?;
                builder.with_client_cert_verifier(Arc::new(ClientCheck { inner, subjects }))
            }
        };
        let mut config = builder.with_cert_resolver(pair);
        config.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
        Ok(Arc::new(config))
    }

    /// What a connection naming `tls`'s CA and certificate trusts and presents. The names were
    /// checked by `ductus_core::tls::check_outbound`; one that didn't load fails with why.
    pub fn trust(&self, tls: &ConnectTls) -> Result<Trust> {
        let roots = match &tls.ca {
            None => Vec::new(),
            Some(name) => match self.cas.get(name) {
                Some(ca) => ca.certificates.clone(),
                None => bail!("{}", self.why_not(name, "CA")),
            },
        };
        let client = match &tls.certificate {
            None => None,
            Some(name) => match self.pairs.get(name) {
                Some(pair) => Some(pair.clone()),
                None => bail!("{}", self.why_not(name, "certificate with its key")),
            },
        };
        Ok(Trust { roots, client })
    }

    fn why_not(&self, name: &str, kind: &str) -> String {
        match self.broken.get(name) {
            Some(why) => why.clone(),
            None => format!("no {kind} named `{name}` in resources.yaml's `certificates:`"),
        }
    }

    /// The client settings for `tls`, built once and shared by every connection naming the same.
    pub fn client(&self, tls: &ConnectTls) -> Result<Arc<rustls::ClientConfig>> {
        if let Some(config) = self.clients.lock().expect("clients lock").get(tls) {
            return Ok(config.clone());
        }
        let config = Arc::new(self.trust(tls)?.config()?);
        self.clients.lock().expect("clients lock").insert(tls.clone(), config.clone());
        Ok(config)
    }

    /// An HTTP client with `tls`'s settings; reqwest's own (the same platform verifier) when it
    /// names nothing.
    pub fn http(&self, tls: &ConnectTls, builder: reqwest::ClientBuilder) -> Result<reqwest::Client> {
        http_client(self.named(tls)?.as_ref(), builder)
    }

    /// The client settings for what a connection names, when it names anything.
    pub fn named(&self, tls: &ConnectTls) -> Result<Option<Arc<rustls::ClientConfig>>> {
        tls.is_set().then(|| self.client(tls)).transpose()
    }
}

/// An HTTP client using `config` (the settings `Certificates::client` built), or reqwest's own
/// (the same platform verifier) without one.
pub fn http_client(
    config: Option<&Arc<rustls::ClientConfig>>,
    builder: reqwest::ClientBuilder,
) -> Result<reqwest::Client> {
    let builder = match config {
        Some(config) => {
            let mut config = (**config).clone();
            config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
            builder.tls_backend_preconfigured(config)
        }
        None => builder,
    };
    builder.build().context("building the HTTP client")
}

impl Certificates {
    /// Check the pairs read from files every minute until `stop`. What comes out are the names
    /// of the pairs whose state changed in a check (renewed, or a renewal that can't be used
    /// appeared or went): nothing at all while nothing changes. Closed at once when no pair
    /// comes from files.
    pub fn watch(
        self: &Arc<Self>,
        stop: tokio_util::sync::CancellationToken,
    ) -> tokio::sync::mpsc::Receiver<Vec<String>> {
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        if !self.pairs.values().any(|p| p.files.is_some()) {
            return rx;
        }
        let me = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(RELOAD_EVERY) => {}
                    _ = stop.cancelled() => return,
                }
                let changed = me.reload().await;
                // Nobody listening (an engine without listeners' rows to keep) is fine.
                if !changed.is_empty() && tx.try_send(changed).is_err() {
                    tracing::debug!("certificate changes not passed on: nobody is reading them");
                }
            }
        });
        rx
    }

    /// Check every file-backed pair once; the names of those whose state changed (renewed, or
    /// their renewal error appeared, changed or went).
    pub async fn reload(&self) -> Vec<String> {
        let mut changed = Vec::new();
        for pair in self.pairs.values() {
            if pair.reload().await {
                changed.push(pair.name.clone());
            }
        }
        changed
    }

    /// The pair named, if it loaded.
    pub fn pair(&self, name: &str) -> Option<&Arc<Pair>> {
        self.pairs.get(name)
    }

    /// A CA entry's PEM, or why there is none (for clients that take PEM: Postgres).
    pub fn ca_pem(&self, name: &str) -> Result<&[u8]> {
        match self.cas.get(name) {
            Some(ca) => Ok(&ca.pem),
            None => bail!("{}", self.why_not(name, "CA")),
        }
    }

    /// The pair named, or why there is none.
    pub fn pair_or_why(&self, name: &str) -> Result<&Arc<Pair>> {
        self.pairs.get(name).ok_or_else(|| anyhow!("{}", self.why_not(name, "certificate with its key")))
    }

    /// The CAs' certificates, every `certificates:` CA entry's (doctor: whether a chain we serve
    /// ends at one of them).
    pub fn authorities(&self) -> impl Iterator<Item = (&str, &CertificateDer<'static>)> {
        self.cas.iter().flat_map(|(name, ca)| ca.certificates.iter().map(move |c| (name.as_str(), c)))
    }
}

/// A self-signed certificate for `names` (DNS names or IP addresses) valid for `days`, with its
/// key: (certificate PEM, key PEM). For labs and tests; senders must be told to trust it.
pub fn self_signed(names: &[String], days: u32) -> Result<(String, String)> {
    use chrono::Datelike;
    if names.is_empty() {
        bail!("give at least one name the certificate is for (the host senders connect to)");
    }
    let key = rcgen::KeyPair::generate().context("making a key")?;
    let mut params = rcgen::CertificateParams::new(names.to_vec()).context("the names")?;
    params.distinguished_name.push(rcgen::DnType::CommonName, names[0].clone());
    let date = |t: DateTime<Utc>| {
        let d = t.date_naive();
        rcgen::date_time_ymd(d.year(), d.month() as u8, d.day() as u8)
    };
    params.not_before = date(Utc::now() - chrono::Duration::days(1));
    params.not_after = date(Utc::now() + chrono::Duration::days(i64::from(days.max(1))));
    let cert = params.self_signed(&key).context("signing")?;
    Ok((cert.pem(), key.serialize_pem()))
}

/// What a new key is (`ductus cert request --key`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyType {
    /// RSA 2048: what every CA takes (Microsoft AD CS templates often still ask for RSA).
    Rsa,
    /// ECDSA P-256: smaller and faster, where the CA takes it.
    Ecdsa,
}

/// A new key and a certificate signing request for `names` (DNS names or IP addresses), with
/// `subject` (`O=General Hospital, OU=Interfaces`; its CN is the first name unless it gives one),
/// asking for a certificate a server and a client can both use: (CSR PEM, key PEM). The key
/// never leaves this server; the CSR goes to the CA.
pub fn request(names: &[String], subject: &str, key: KeyType) -> Result<(String, String)> {
    if names.is_empty() {
        bail!("give at least one name the certificate is for (--dns, the host senders connect to)");
    }
    let key = match key {
        KeyType::Rsa => rcgen::KeyPair::generate_for(&rcgen::PKCS_RSA_SHA256),
        KeyType::Ecdsa => rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256),
    }
    .context("making a key")?;
    let mut params = rcgen::CertificateParams::new(names.to_vec()).context("the names")?;
    let mut common_name = false;
    for part in subject.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let Some((attribute, value)) = part.split_once('=') else {
            bail!("subject `{subject}`: write each part as ATTR=value, like O=General Hospital");
        };
        let kind = match attribute.trim().to_ascii_uppercase().as_str() {
            "CN" => {
                common_name = true;
                rcgen::DnType::CommonName
            }
            "O" => rcgen::DnType::OrganizationName,
            "OU" => rcgen::DnType::OrganizationalUnitName,
            "C" => rcgen::DnType::CountryName,
            "ST" => rcgen::DnType::StateOrProvinceName,
            "L" => rcgen::DnType::LocalityName,
            other => bail!("subject `{subject}`: `{other}` isn't one of CN, O, OU, C, ST, L"),
        };
        params.distinguished_name.push(kind, value.trim());
    }
    if !common_name {
        params.distinguished_name.push(rcgen::DnType::CommonName, names[0].clone());
    }
    params.extended_key_usages =
        vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth, rcgen::ExtendedKeyUsagePurpose::ClientAuth];
    let csr = params.serialize_request(&key).context("making the request")?;
    Ok((csr.pem().context("writing the request")?, key.serialize_pem()))
}

/// Whether `key_pem` is the key of the first certificate in `cert_pem`: the certificate's
/// details, or why not (in the words the engine's start would use).
pub fn check_pair(cert_pem: &[u8], key_pem: &[u8]) -> Result<About> {
    load_pair(cert_pem, key_pem).map(|loaded| loaded.about)
}

fn read(file: &Path) -> Result<Vec<u8>> {
    std::fs::read(file).with_context(|| format!("reading {}", file.display()))
}

fn roots(pem: &[u8]) -> Result<(rustls::RootCertStore, Vec<CertificateDer<'static>>)> {
    let mut store = rustls::RootCertStore::empty();
    let mut certificates = Vec::new();
    for cert in CertificateDer::pem_slice_iter(pem) {
        let cert = cert.map_err(|e| anyhow!("reading the CA: {e}"))?;
        store.add(cert.clone()).map_err(|e| anyhow!("the CA: {e}"))?;
        certificates.push(cert);
    }
    if store.is_empty() {
        bail!("no certificate in it: give the CA's certificate as PEM text (-----BEGIN CERTIFICATE-----...)");
    }
    Ok((store, certificates))
}

/// What an outgoing connection trusts (the platform's, plus a CA's certificates) and the client
/// certificate it presents.
#[derive(Debug, Clone, Default)]
pub struct Trust {
    roots: Vec<CertificateDer<'static>>,
    client: Option<Arc<Pair>>,
}

impl Trust {
    /// What a Postgres URL's `sslrootcert` and `sslcert`/`sslkey` files say (the engine's own
    /// database), read now.
    pub fn from_files(root_cert: Option<&Path>, client: Option<(&Path, &Path)>) -> Result<Self> {
        let roots = match root_cert {
            Some(file) => roots(&read(file)?).with_context(|| format!("sslrootcert {}", file.display()))?.1,
            None => Vec::new(),
        };
        let client = match client {
            Some((cert, key)) => Some(Arc::new(
                Pair::from_files("sslcert", cert, key).with_context(|| format!("sslcert {}", cert.display()))?,
            )),
            None => None,
        };
        Ok(Self { roots, client })
    }

    /// The certificates trusted besides the platform's, for a client that takes only those.
    pub fn extra_roots(&self) -> &[CertificateDer<'static>] {
        &self.roots
    }

    /// The client certificate's pair, if one is presented.
    pub fn client_pair(&self) -> Option<&Arc<Pair>> {
        self.client.as_ref()
    }

    /// The platform verifier, also trusting the extra roots.
    pub fn verifier(&self) -> Result<Verifier> {
        let platform = if self.roots.is_empty() {
            rustls_platform_verifier::Verifier::new(provider())
        } else {
            rustls_platform_verifier::Verifier::new_with_extra_roots(self.roots.iter().cloned(), provider())
        };
        Ok(Verifier {
            platform: platform.context("loading this server's trusted certificates")?,
            roots: self.roots.clone(),
            system: std::sync::OnceLock::new(),
            judge: Judge::Ours,
        })
    }

    /// Client settings that check the server with `verifier` (the platform's, or doctor's
    /// recording wrapper around it) and present the client certificate.
    pub fn config_with(&self, verifier: Arc<dyn ServerCertVerifier>) -> rustls::ClientConfig {
        let builder = rustls::ClientConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .expect("the default provider supports the default versions")
            .dangerous()
            .with_custom_certificate_verifier(verifier);
        match &self.client {
            Some(pair) => builder.with_client_cert_resolver(pair.clone()),
            None => builder.with_no_client_auth(),
        }
    }

    pub fn config(&self) -> Result<rustls::ClientConfig> {
        Ok(self.config_with(Arc::new(self.verifier()?)))
    }
}

/// The platform verifier, made the same on every OS for a receiver presenting a CA's certificate
/// (CA:TRUE) as its own: accepted when that exact certificate is itself trusted (named in `ca:`,
/// or installed as trusted on this server), its dates and the host name still checked. Windows's
/// own check does this already (a trusted root is trusted wherever it appears, as in Java and
/// .NET); webpki, which the platform verifier is on Linux, refuses every CA:TRUE end entity.
/// Trusting one exact certificate is at least as strict as trusting a CA. A CA:TRUE certificate
/// that is only issued by a trusted CA is refused, on Windows too (which would accept it).
#[derive(Debug)]
pub struct Verifier {
    platform: rustls_platform_verifier::Verifier,
    roots: Vec<CertificateDer<'static>>,
    /// This server's trusted certificates, read the first time a CA's certificate is presented
    /// (the platform verifier keeps its own copy, out of reach).
    system: std::sync::OnceLock<Vec<CertificateDer<'static>>>,
    judge: Judge,
}

/// Whose verdict a `Verifier` gives.
#[derive(Debug)]
enum Judge {
    /// The engine's own connections: the platform's, with a trusted CA:TRUE certificate accepted
    /// (see above).
    Ours,
    /// The platform verifier's alone.
    Platform,
    /// webpki's, over the same roots.
    Webpki(Arc<rustls::client::WebPkiServerVerifier>),
}

impl Verifier {
    /// The platform verifier's verdict alone, as alert mail sees it (lettre, which verifies with
    /// the platform verifier and takes no verifier of ours): for doctor, which must say what that
    /// connection would.
    pub fn platform_only(self) -> Self {
        Self { judge: Judge::Platform, ..self }
    }

    /// webpki's verdict over the same roots (the `ca:`s and the system's), as Postgres (sqlx) and
    /// SQL Server (tiberius) see it on every OS: for doctor.
    pub fn webpki_only(self) -> Result<Self> {
        let mut roots = rustls::RootCertStore::empty();
        roots.add_parsable_certificates(self.roots.iter().cloned());
        roots.add_parsable_certificates(rustls_native_certs::load_native_certs().certs);
        let webpki = rustls::client::WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider())
            .build()
            .context("loading this server's trusted certificates")?;
        Ok(Self { judge: Judge::Webpki(webpki), ..self })
    }

    fn trusts_exactly(&self, cert: &CertificateDer<'_>) -> bool {
        self.roots.iter().any(|r| r.as_ref() == cert.as_ref())
            || self
                .system
                .get_or_init(|| rustls_native_certs::load_native_certs().certs)
                .iter()
                .any(|r| r.as_ref() == cert.as_ref())
    }

    /// The checks webpki would still make of a certificate trusted as it is: the name, the dates.
    fn trusted_as_is(
        cert: &CertificateDer<'_>,
        server_name: &rustls::pki_types::ServerName<'_>,
        now: rustls::pki_types::UnixTime,
    ) -> Result<(), rustls::Error> {
        use rustls::CertificateError as C;
        use x509_parser::prelude::*;
        rustls::client::verify_server_name(&rustls::server::ParsedCertificate::try_from(cert)?, server_name)?;
        let (_, x509) =
            X509Certificate::from_der(cert).map_err(|_| rustls::Error::InvalidCertificate(C::BadEncoding))?;
        if not_valid_yet(cert, now) {
            return Err(rustls::Error::InvalidCertificate(C::NotValidYet));
        }
        if now.as_secs() as i64 > x509.validity().not_after.timestamp() {
            return Err(rustls::Error::InvalidCertificate(C::Expired));
        }
        Ok(())
    }
}

/// Whether a certificate (DER) is a CA's (CA:TRUE).
pub(crate) fn is_ca(der: &[u8]) -> bool {
    use x509_parser::prelude::*;
    X509Certificate::from_der(der).is_ok_and(|(_, c)| c.is_ca())
}

fn not_valid_yet(der: &[u8], now: rustls::pki_types::UnixTime) -> bool {
    use x509_parser::prelude::*;
    X509Certificate::from_der(der).is_ok_and(|(_, c)| (now.as_secs() as i64) < c.validity().not_before.timestamp())
}

impl ServerCertVerifier for Verifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &rustls::pki_types::ServerName<'_>,
        ocsp_response: &[u8],
        now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        use rustls::CertificateError as C;
        let verdict = match &self.judge {
            Judge::Webpki(webpki) => {
                return webpki.verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now);
            }
            Judge::Platform => {
                return self.platform.verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now);
            }
            Judge::Ours => self.platform.verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now),
        };
        match verdict {
            Err(rustls::Error::InvalidCertificate(C::Other(e)))
                if is_ca_as_leaf(&e) && self.trusts_exactly(end_entity) =>
            {
                Self::trusted_as_is(end_entity, server_name, now)?;
                Ok(rustls::client::danger::ServerCertVerified::assertion())
            }
            // Windows accepts a CA's certificate that a trusted CA issued, which webpki refuses:
            // refused here too, so both say the same.
            Ok(_) if is_ca(end_entity) && !self.trusts_exactly(end_entity) => Err(rustls::Error::InvalidCertificate(
                C::Other(rustls::OtherError(Arc::new(webpki::Error::CaUsedAsEndEntity))),
            )),
            // Windows reports a certificate that isn't valid yet as expired.
            Err(rustls::Error::InvalidCertificate(C::Expired | C::ExpiredContext { .. }))
                if not_valid_yet(end_entity, now) =>
            {
                Err(rustls::Error::InvalidCertificate(C::NotValidYet))
            }
            verdict => verdict,
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.platform.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.platform.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.platform.supported_verify_schemes()
    }

    fn requires_raw_public_keys(&self) -> bool {
        self.platform.requires_raw_public_keys()
    }

    fn root_hint_subjects(&self) -> Option<&[rustls::DistinguishedName]> {
        self.platform.root_hint_subjects()
    }
}

/// An MLLP client over TLS to `host:port`, checking the receiver's certificate covers `host`.
pub fn mllp_client(host: &str, port: u16, timeout: Duration, config: Arc<rustls::ClientConfig>) -> ductus_mllp::Client {
    let connector = tokio_rustls::TlsConnector::from(config);
    let name = rustls::pki_types::ServerName::try_from(host.trim_matches(['[', ']']).to_string())
        .map_err(|_| format!("`{host}` isn't a name TLS can check"));
    ductus_mllp::Client::with_connect(format!("{host}:{port}"), timeout, move |tcp| {
        let (connector, name) = (connector.clone(), name.clone());
        async move { connector.connect(name.map_err(std::io::Error::other)?, tcp).await }
    })
}

/// Why a TLS connection we made failed, in plain words (a send's error: the message retries).
/// `None` when the error isn't about TLS.
pub fn outbound_failure(error: &std::io::Error) -> Option<String> {
    let tls = error.get_ref().and_then(|e| e.downcast_ref::<rustls::Error>())?;
    use rustls::CertificateError as C;
    use rustls::Error as E;
    Some(match tls {
        E::InvalidCertificate(C::UnknownIssuer) => {
            "TLS: the receiver's certificate isn't from a CA we trust (name its CA in `tls.ca`, or install it on this server)".into()
        }
        E::InvalidCertificate(C::NotValidForName | C::NotValidForNameContext { .. }) => {
            "TLS: the receiver's certificate doesn't cover the host we connect to".into()
        }
        E::InvalidCertificate(C::Expired | C::ExpiredContext { .. }) => "TLS: the receiver's certificate has expired".into(),
        E::InvalidCertificate(C::NotValidYet | C::NotValidYetContext { .. }) => {
            "TLS: the receiver's certificate isn't valid yet (is this server's clock right?)".into()
        }
        E::InvalidCertificate(C::Other(e)) if is_ca_as_leaf(e) => {
            "TLS: the receiver presents a CA's certificate (CA:TRUE) as its own: it needs a server certificate (`openssl req -x509` makes a CA's by default)".into()
        }
        E::InvalidCertificate(other) => format!("TLS: the receiver's certificate was refused ({other:?})"),
        E::InvalidMessage(_) => "TLS: the receiver isn't speaking TLS (a plain MLLP port?)".into(),
        E::PeerIncompatible(_) => "TLS: no TLS version or cipher in common (TLS 1.2 is our minimum)".into(),
        E::AlertReceived(alert) => {
            use rustls::AlertDescription as A;
            match alert {
                A::CertificateRequired => {
                    "TLS: the receiver wants a client certificate (name one in `tls.certificate`)".into()
                }
                A::UnknownCA | A::BadCertificate | A::CertificateUnknown | A::AccessDenied => format!(
                    "TLS: the receiver refused our client certificate ({alert:?}): is it one its CA issued, and still valid?"
                ),
                A::HandshakeFailure | A::ProtocolVersion | A::InsufficientSecurity => {
                    format!("TLS: the receiver ended the handshake ({alert:?}): no TLS version or cipher in common?")
                }
                other => format!("TLS: the receiver ended the connection ({other:?})"),
            }
        }
        other => format!("TLS: {other}"),
    })
}

/// A certificate refused for being a CA's where a server's or client's belongs.
pub(crate) fn is_ca_as_leaf(e: &rustls::OtherError) -> bool {
    matches!(e.0.downcast_ref::<webpki::Error>(), Some(webpki::Error::CaUsedAsEndEntity))
}

/// Client certificates: the CA's check, then the subject patterns.
#[derive(Debug)]
struct ClientCheck {
    inner: Arc<dyn rustls::server::danger::ClientCertVerifier>,
    subjects: Vec<SubjectPattern>,
}

/// A client certificate the CA issued, with a subject no pattern allows.
#[derive(Debug)]
struct SubjectRefused(String);

impl std::fmt::Display for SubjectRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "client certificate `{}` isn't one `subjects` allows", self.0)
    }
}

impl std::error::Error for SubjectRefused {}

impl rustls::server::danger::ClientCertVerifier for ClientCheck {
    fn root_hint_subjects(&self) -> &[rustls::DistinguishedName] {
        self.inner.root_hint_subjects()
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::server::danger::ClientCertVerified, rustls::Error> {
        let verified = self.inner.verify_client_cert(end_entity, intermediates, now)?;
        let who = presented(end_entity).unwrap_or_default();
        if ductus_core::tls::allowed(&self.subjects, &who) {
            return Ok(verified);
        }
        let subject = who.subject.iter().map(|(a, v)| format!("{a}={v}")).collect::<Vec<_>>().join(", ");
        Err(rustls::Error::InvalidCertificate(rustls::CertificateError::Other(rustls::OtherError(Arc::new(
            SubjectRefused(subject),
        )))))
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

/// Refused handshakes, logged at most once a minute per listener and sender address (a
/// misconfigured sender retries every few seconds, and the log isn't for counting them).
#[derive(Default)]
pub struct Refusals {
    last: Mutex<HashMap<(String, IpAddr), Instant>>,
    /// Connections doctor makes to our own listeners (by the address they come from): it sends
    /// no client certificate and hangs up once it has the chain, which isn't a sender refused.
    /// Logged, it would also hide a real refusal from this address for a minute.
    own: Mutex<HashMap<SocketAddr, Instant>>,
}

impl Refusals {
    const EVERY: Duration = Duration::from_secs(60);

    pub fn global() -> &'static Refusals {
        static REFUSALS: std::sync::OnceLock<Refusals> = std::sync::OnceLock::new();
        REFUSALS.get_or_init(Refusals::default)
    }

    /// A connection from `local` is this engine's own check: its handshake isn't logged.
    pub fn our_own(&self, local: SocketAddr) {
        let now = Instant::now();
        let mut own = self.own.lock().expect("refusals lock");
        own.retain(|_, at| now.duration_since(*at) < HANDSHAKE_TIMEOUT * 2);
        own.insert(local, now);
    }

    /// Whether a connection from `peer` is one of this engine's own checks (kept for its
    /// handshake, which `note` judges next).
    pub fn is_our_own(&self, peer: SocketAddr) -> bool {
        self.own.lock().expect("refusals lock").contains_key(&peer)
    }

    /// Log a refused handshake, unless this sender's was logged in the last minute or it's our
    /// own check. True if logged.
    fn note(&self, listener: &str, peer: SocketAddr, error: &std::io::Error) -> bool {
        if self.own.lock().expect("refusals lock").remove(&peer).is_some() {
            tracing::debug!(listener, %peer, reason = %why(error), "our own check's handshake ended");
            return false;
        }
        if !self.due(listener, peer.ip()) {
            return false;
        }
        tracing::warn!(listener, peer = %peer.ip(), reason = %why(error), "TLS handshake refused (logged once a minute per sender)");
        true
    }

    /// Log a connection closed because its address isn't on the listener's `allow:` list
    /// (`allowed`: the list, names with their addresses), once a minute per sender. True if logged.
    pub fn not_allowed(&self, listener: &str, peer: SocketAddr, allowed: &str) -> bool {
        if !self.due(listener, peer.ip()) {
            return false;
        }
        tracing::warn!(listener, peer = %peer.ip(), allowed, "connection refused: not on the allow list (logged once a minute per sender)");
        true
    }

    /// Whether a refusal from `ip` at `listener` should be logged now (none in the last minute).
    fn due(&self, listener: &str, ip: IpAddr) -> bool {
        let now = Instant::now();
        let mut last = self.last.lock().expect("refusals lock");
        if last.len() > 10_000 {
            last.retain(|_, at| now.duration_since(*at) < Self::EVERY);
        }
        let key = (listener.to_string(), ip);
        if last.get(&key).is_some_and(|at| now.duration_since(*at) < Self::EVERY) {
            return false;
        }
        last.insert(key, now);
        true
    }
}

/// Connect to `address` as one of the engine's own checks: the local address is registered
/// before connecting, so our listeners let it in (`allow:`) and don't log its handshake.
pub async fn connect_own(address: SocketAddr) -> std::io::Result<TcpStream> {
    let socket = || ductus_mllp::net::socket(address);
    // Our listeners are on this machine: connecting from the listener's own address (loopback
    // for 0.0.0.0) gives a local address known before connecting. Elsewhere that bind fails, and
    // the address is only known once connected (nothing of ours is listening there anyway).
    let own = socket()?;
    if own.bind(SocketAddr::new(address.ip(), 0)).is_ok() {
        Refusals::global().our_own(own.local_addr()?);
        return own.connect(address).await;
    }
    let tcp = socket()?.connect(address).await?;
    Refusals::global().our_own(tcp.local_addr()?);
    Ok(tcp)
}

/// A refused handshake in plain words.
fn why(error: &std::io::Error) -> String {
    let Some(tls) = error.get_ref().and_then(|e| e.downcast_ref::<rustls::Error>()) else {
        return match error.kind() {
            std::io::ErrorKind::TimedOut => format!("no handshake within {}s", HANDSHAKE_TIMEOUT.as_secs()),
            std::io::ErrorKind::UnexpectedEof => "the sender closed the connection during the handshake".into(),
            _ => error.to_string(),
        };
    };
    match tls {
        rustls::Error::InvalidMessage(rustls::InvalidMessage::InvalidContentType) => {
            "the sender isn't speaking TLS (a plain MLLP or HTTP sender on a TLS port?)".into()
        }
        rustls::Error::NoCertificatesPresented => "the sender presented no client certificate".into(),
        rustls::Error::PeerIncompatible(_) => {
            "no TLS version or cipher in common (TLS 1.2 is the minimum: an old sender on TLS 1.0/1.1?)".into()
        }
        rustls::Error::InvalidCertificate(rustls::CertificateError::UnknownIssuer) => {
            "the client certificate isn't from the CA this listener trusts".into()
        }
        rustls::Error::InvalidCertificate(rustls::CertificateError::Expired) => {
            "the client certificate has expired".into()
        }
        rustls::Error::InvalidCertificate(rustls::CertificateError::Other(e)) if is_ca_as_leaf(e) => {
            "the client certificate is a CA's (CA:TRUE), not one issued to a client".into()
        }
        rustls::Error::InvalidCertificate(rustls::CertificateError::Other(e)) => e.to_string(),
        rustls::Error::AlertReceived(alert) => {
            format!("the sender ended the handshake ({alert:?}): it doesn't trust our certificate?")
        }
        e => e.to_string(),
    }
}

/// A TLS handshake on an accepted connection, within the timeout; refusals are logged.
pub async fn handshake(
    acceptor: &tokio_rustls::TlsAcceptor,
    listener: &str,
    tcp: TcpStream,
    peer: SocketAddr,
) -> Option<TlsStream<TcpStream>> {
    let outcome = match tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(tcp)).await {
        Ok(outcome) => outcome,
        Err(_) => Err(std::io::ErrorKind::TimedOut.into()),
    };
    match outcome {
        Ok(stream) => Some(stream),
        Err(e) => {
            Refusals::global().note(listener, peer, &e);
            None
        }
    }
}

/// A connection a listener accepted: plain, or TLS after its handshake.
pub enum Conn {
    Plain(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
}

impl AsyncRead for Conn {
    fn poll_read(self: Pin<&mut Self>, cx: &mut TaskContext<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Conn::Plain(s) => Pin::new(s).poll_read(cx, buf),
            Conn::Tls(s) => Pin::new(s.as_mut()).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Conn {
    fn poll_write(self: Pin<&mut Self>, cx: &mut TaskContext<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
        match self.get_mut() {
            Conn::Plain(s) => Pin::new(s).poll_write(cx, buf),
            Conn::Tls(s) => Pin::new(s.as_mut()).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Conn::Plain(s) => Pin::new(s).poll_flush(cx),
            Conn::Tls(s) => Pin::new(s.as_mut()).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Conn::Plain(s) => Pin::new(s).poll_shutdown(cx),
            Conn::Tls(s) => Pin::new(s.as_mut()).poll_shutdown(cx),
        }
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<std::io::Result<usize>> {
        match self.get_mut() {
            Conn::Plain(s) => Pin::new(s).poll_write_vectored(cx, bufs),
            Conn::Tls(s) => Pin::new(s.as_mut()).poll_write_vectored(cx, bufs),
        }
    }

    fn is_write_vectored(&self) -> bool {
        match self {
            Conn::Plain(s) => s.is_write_vectored(),
            Conn::Tls(s) => s.is_write_vectored(),
        }
    }
}

/// An HTTP listener (axum), plain or TLS. With TLS, handshakes run on tasks of their own, so a
/// slow or silent client holds up no one else; finished ones are handed to the server.
pub struct Listener {
    local: SocketAddr,
    inner: Inner,
}

/// Who may connect: the listener's `allow:` list, checked before anything else.
pub type Admission = Option<Arc<crate::allow::Admission>>;

enum Inner {
    Plain(TcpListener, Admission),
    Tls { ready: tokio::sync::mpsc::Receiver<(Conn, SocketAddr)>, task: tokio::task::AbortHandle },
}

/// A connection just accepted: keepalive on, and whether its address may connect.
fn admitted(admission: &Admission, tcp: &TcpStream, peer: SocketAddr) -> bool {
    crate::allow::keepalive(tcp);
    admission.as_ref().is_none_or(|a| a.admits(peer))
}

impl Drop for Listener {
    fn drop(&mut self) {
        if let Inner::Tls { task, .. } = &self.inner {
            task.abort();
        }
    }
}

impl Listener {
    /// Listen on `tcp`; `name` labels refused handshakes in the log. Connections from addresses
    /// `admission` refuses are closed before the handshake or any read.
    pub fn new(
        tcp: TcpListener,
        tls: Option<Arc<rustls::ServerConfig>>,
        name: &str,
        admission: Admission,
    ) -> std::io::Result<Self> {
        let local = tcp.local_addr()?;
        let Some(tls) = tls else { return Ok(Self { local, inner: Inner::Plain(tcp, admission) }) };
        let acceptor = tokio_rustls::TlsAcceptor::from(tls);
        let (tx, ready) = tokio::sync::mpsc::channel(64);
        let name = name.to_string();
        let task = tokio::spawn(async move {
            loop {
                let (stream, peer) = tokio::select! {
                    accepted = tcp.accept() => match accepted {
                        Ok(a) => a,
                        Err(e) => {
                            accept_error(e).await;
                            continue;
                        }
                    },
                    _ = tx.closed() => return,
                };
                if !admitted(&admission, &stream, peer) {
                    continue;
                }
                let (acceptor, tx, name) = (acceptor.clone(), tx.clone(), name.clone());
                tokio::spawn(async move {
                    if let Some(tls) = handshake(&acceptor, &name, stream, peer).await {
                        let _ = tx.send((Conn::Tls(Box::new(tls)), peer)).await;
                    }
                });
            }
        })
        .abort_handle();
        Ok(Self { local, inner: Inner::Tls { ready, task } })
    }
}

/// As axum's own listener: errors about one connection are skipped; others (out of file
/// descriptors) wait a second so the loop doesn't spin.
async fn accept_error(e: std::io::Error) {
    use std::io::ErrorKind::*;
    if !matches!(e.kind(), ConnectionRefused | ConnectionAborted | ConnectionReset) {
        tracing::error!(error = %e, "accepting a connection failed");
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

impl axum::serve::Listener for Listener {
    type Io = Conn;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Conn, SocketAddr) {
        match &mut self.inner {
            Inner::Plain(tcp, admission) => loop {
                match TcpListener::accept(tcp).await {
                    Ok((stream, peer)) if admitted(admission, &stream, peer) => return (Conn::Plain(stream), peer),
                    Ok(_) => {}
                    Err(e) => accept_error(e).await,
                }
            },
            Inner::Tls { ready, .. } => match ready.recv().await {
                Some(conn) => conn,
                None => std::future::pending().await,
            },
        }
    }

    fn local_addr(&self) -> std::io::Result<SocketAddr> {
        Ok(self.local)
    }
}

/// Certificates made at test time (a CA, what it issues), for the runtime's TLS tests.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    pub struct Ca {
        pub cert: rcgen::Certificate,
        pub issuer: rcgen::Issuer<'static, rcgen::KeyPair>,
    }

    pub fn ca(name: &str) -> Ca {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        params.distinguished_name.push(rcgen::DnType::CommonName, name);
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let cert = params.self_signed(&key).unwrap();
        Ca { cert, issuer: rcgen::Issuer::new(params, key) }
    }

    /// An intermediate CA `root` issued.
    pub fn intermediate(root: &Ca, name: &str) -> Ca {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        params.distinguished_name.push(rcgen::DnType::CommonName, name);
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let cert = params.signed_by(&key, &root.issuer).unwrap();
        Ca { cert, issuer: rcgen::Issuer::new(params, key) }
    }

    /// A certificate `ca` issued: (PEM chain, PEM key).
    pub fn leaf(ca: &Ca, subject: &[(rcgen::DnType, &str)], names: &[&str], days: i64) -> (String, String) {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params =
            rcgen::CertificateParams::new(names.iter().map(|n| n.to_string()).collect::<Vec<_>>()).unwrap();
        for (t, v) in subject {
            params.distinguished_name.push(t.clone(), *v);
        }
        let day = |d: i64| {
            use chrono::Datelike;
            let d = (Utc::now() + chrono::Duration::days(d)).date_naive();
            rcgen::date_time_ymd(d.year(), d.month() as u8, d.day() as u8)
        };
        params.not_before = day(-30);
        params.not_after = day(days);
        let cert = params.signed_by(&key, &ca.issuer).unwrap();
        (cert.pem(), key.serialize_pem())
    }

    /// A pair from PEM text.
    pub fn entry(cert: &str, key: &str) -> CertificateEntry {
        CertificateEntry { cert: Some(cert.into()), key: Some(key.into()), ..Default::default() }
    }

    /// A CA from PEM text.
    pub fn ca_entry(ca: &Ca) -> CertificateEntry {
        CertificateEntry { ca: Some(ca.cert.pem()), ..Default::default() }
    }

    /// A receiver's certificates: its own (for localhost and 127.0.0.1, from `server_ca`), and
    /// clients' from `client_ca` (as `server-ca`, `ours` and `client-ca`), plus ours as a client
    /// (`client`, `CN=ductus-client`, from `client_ca`) and one from a CA nobody trusts
    /// (`stranger`). All loaded.
    pub fn receiver_and_client(server_ca: &Ca, client_ca: &Ca) -> Certificates {
        let other = ca("other CA");
        let names = ["localhost", "127.0.0.1"];
        let ours = leaf(server_ca, &[(rcgen::DnType::CommonName, "receiver")], &names, 90);
        let client = leaf(client_ca, &[(rcgen::DnType::CommonName, "ductus-client")], &[], 90);
        let stranger = leaf(&other, &[(rcgen::DnType::CommonName, "ductus-client")], &[], 90);
        let entries = BTreeMap::from([
            ("server-ca".to_string(), ca_entry(server_ca)),
            ("client-ca".to_string(), ca_entry(client_ca)),
            ("ours".to_string(), entry(&ours.0, &ours.1)),
            ("client".to_string(), entry(&client.0, &client.1)),
            ("stranger".to_string(), entry(&stranger.0, &stranger.1)),
        ]);
        Certificates::load(&entries).unwrap()
    }

    /// `tls:` for a receiver presenting `ours` and, with `clients`, asking for one `client-ca` issued.
    pub fn receiving(clients: bool) -> ListenTls {
        let clients = clients.then(|| ductus_core::tls::Clients { ca: "client-ca".into(), subjects: vec![] });
        ListenTls { certificate: "ours".into(), clients }
    }

    pub fn connecting(ca: Option<&str>, certificate: Option<&str>) -> ConnectTls {
        ConnectTls { ca: ca.map(Into::into), certificate: certificate.map(Into::into) }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;
    use ductus_core::tls::Clients;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A CA's certificate trusted as it is must still be for the host and within its dates; with
    /// `platform_only` (doctor, for alert mail) it's the platform's verdict, which on Windows
    /// already accepts it; with `webpki_only` (Postgres, SQL Server) it's refused everywhere.
    #[test]
    fn a_ca_certificate_trusted_as_it_is_still_has_its_name_and_dates_checked() {
        use rustls::CertificateError as C;
        use rustls::pki_types::{ServerName, UnixTime};
        let day = |d: i64| {
            use chrono::Datelike;
            let d = (Utc::now() + chrono::Duration::days(d)).date_naive();
            rcgen::date_time_ymd(d.year(), d.month() as u8, d.day() as u8)
        };
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(vec!["localhost".to_string()]).unwrap();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        (params.not_before, params.not_after) = (day(-10), day(30));
        let cert = params.self_signed(&key).unwrap();
        let trust = Trust { roots: vec![cert.der().clone()], client: None };
        let verify = |verifier: &Verifier, host: &str, days: i64| {
            let at = (Utc::now() + chrono::Duration::days(days)).timestamp() as u64;
            let name = ServerName::try_from(host.to_string()).unwrap();
            verifier.verify_server_cert(
                cert.der(),
                &[],
                &name,
                &[],
                UnixTime::since_unix_epoch(Duration::from_secs(at)),
            )
        };
        let refused = |r: Result<_, rustls::Error>| match r {
            Err(rustls::Error::InvalidCertificate(c)) => c,
            other => panic!("expected a refusal: {other:?}"),
        };

        let verifier = trust.verifier().unwrap();
        assert!(verify(&verifier, "localhost", 0).is_ok());
        assert!(matches!(
            refused(verify(&verifier, "elsewhere.example", 0)),
            C::NotValidForName | C::NotValidForNameContext { .. }
        ));
        assert!(matches!(refused(verify(&verifier, "localhost", 60)), C::Expired | C::ExpiredContext { .. }));
        assert!(matches!(refused(verify(&verifier, "localhost", -20)), C::NotValidYet | C::NotValidYetContext { .. }));

        let platform = trust.verifier().unwrap().platform_only();
        if cfg!(windows) {
            assert!(verify(&platform, "localhost", 0).is_ok(), "Windows's own verdict");
        } else {
            assert!(matches!(refused(verify(&platform, "localhost", 0)), C::Other(e) if is_ca_as_leaf(&e)));
        }
        // webpki's (Postgres and SQL Server): refused on every OS.
        let webpki = trust.verifier().unwrap().webpki_only().unwrap();
        assert!(matches!(refused(verify(&webpki, "localhost", 0)), C::Other(e) if is_ca_as_leaf(&e)));

        // A CA's certificate a trusted CA issued: refused everywhere (Windows alone would take it).
        let issuer = ca("receiver's CA");
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(vec!["localhost".to_string()]).unwrap();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let issued = params.signed_by(&key, &issuer.issuer).unwrap();
        let trust = Trust { roots: vec![issuer.cert.der().clone()], client: None };
        let name = ServerName::try_from("localhost").unwrap();
        let verdict = trust.verifier().unwrap().verify_server_cert(issued.der(), &[], &name, &[], UnixTime::now());
        assert!(matches!(refused(verdict), C::Other(e) if is_ca_as_leaf(&e)));
    }

    /// A sender's refusals are logged once a minute; our own check's handshake isn't one, and
    /// doesn't use up that minute for a real sender on the same address.
    #[test]
    fn refusals_are_logged_once_a_minute_and_not_for_our_own_check() {
        let refusals = Refusals::default();
        let error = std::io::Error::from(std::io::ErrorKind::UnexpectedEof);
        let (doctor, sender, later): (SocketAddr, SocketAddr, SocketAddr) = (
            "127.0.0.1:50001".parse().unwrap(),
            "127.0.0.1:50002".parse().unwrap(),
            "127.0.0.1:50003".parse().unwrap(),
        );
        refusals.our_own(doctor);
        assert!(!refusals.note("adt", doctor, &error), "our own check");
        assert!(refusals.note("adt", sender, &error), "the sender on that address is still logged");
        assert!(!refusals.note("adt", later, &error), "once a minute per address");
        assert!(refusals.note("orders", later, &error), "per listener");
        assert!(refusals.note("billing", doctor, &error), "an address is our own once only");
    }

    /// The request names what the CA needs (subject, names, use as server and client) and carries
    /// the new key's public half; the key is one the engine loads.
    #[test]
    fn a_request_carries_the_names_and_the_new_keys_public_half() {
        use x509_parser::prelude::*;
        let names = ["mllp.hospital.org".to_string(), "10.1.2.3".to_string()];
        for key_type in [KeyType::Rsa, KeyType::Ecdsa] {
            let (csr, key) = request(&names, "O=General Hospital, OU=Interfaces", key_type).unwrap();
            assert!(csr.starts_with("-----BEGIN CERTIFICATE REQUEST-----"), "{csr}");
            let der = rustls::pki_types::CertificateSigningRequestDer::from_pem_slice(csr.as_bytes()).unwrap();
            let (_, req) = X509CertificationRequest::from_der(der.as_ref()).unwrap();
            let info = &req.certification_request_info;
            assert_eq!(info.subject.to_string(), "CN=mllp.hospital.org, O=General Hospital, OU=Interfaces");
            let mut seen = Vec::new();
            let mut usages = None;
            for ext in req.requested_extensions().into_iter().flatten() {
                match ext {
                    ParsedExtension::SubjectAlternativeName(san) => {
                        for n in &san.general_names {
                            match n {
                                GeneralName::DNSName(d) => seen.push(d.to_string()),
                                GeneralName::IPAddress(ip) => seen.push(format!("{ip:?}")),
                                _ => {}
                            }
                        }
                    }
                    ParsedExtension::ExtendedKeyUsage(eku) => usages = Some((eku.server_auth, eku.client_auth)),
                    _ => {}
                }
            }
            assert_eq!(seen, ["mllp.hospital.org", "[10, 1, 2, 3]"], "{key_type:?}");
            assert_eq!(usages, Some((true, true)), "{key_type:?}");
            use rcgen::PublicKeyData;
            let public = rcgen::KeyPair::from_pem(&key).unwrap().subject_public_key_info();
            assert_eq!(info.subject_pki.raw, public.as_slice(), "{key_type:?}: the CSR is the key's");
        }
        let err = request(&names, "OU", KeyType::Ecdsa).unwrap_err().to_string();
        assert!(err.contains("write each part as ATTR=value"), "{err}");
        let err = request(&names, "EMAIL=a@b", KeyType::Ecdsa).unwrap_err().to_string();
        assert!(err.contains("`EMAIL` isn't one of CN, O, OU, C, ST, L"), "{err}");
        assert!(request(&[], "", KeyType::Ecdsa).is_err());
    }

    fn client_config(ca: &Ca, client: Option<(&str, &str)>) -> Arc<rustls::ClientConfig> {
        let mut roots = rustls::RootCertStore::empty();
        roots.add(ca.cert.der().clone()).unwrap();
        let builder = rustls::ClientConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots);
        Arc::new(match client {
            None => builder.with_no_client_auth(),
            Some((cert, key)) => builder
                .with_client_auth_cert(
                    CertificateDer::pem_slice_iter(cert.as_bytes()).map(Result::unwrap).collect(),
                    PrivateKeyDer::from_pem_slice(key.as_bytes()).unwrap(),
                )
                .unwrap(),
        })
    }

    /// An echo server behind `Listener` (what the HTTP sources and API use); its address.
    async fn echo(config: Arc<rustls::ServerConfig>) -> SocketAddr {
        echo_with(config, None).await
    }

    async fn echo_with(config: Arc<rustls::ServerConfig>, admission: Admission) -> SocketAddr {
        let mut listener =
            Listener::new(TcpListener::bind("127.0.0.1:0").await.unwrap(), Some(config), "test", admission).unwrap();
        let addr = axum::serve::Listener::local_addr(&listener).unwrap();
        tokio::spawn(async move {
            loop {
                let (mut conn, _) = axum::serve::Listener::accept(&mut listener).await;
                tokio::spawn(async move {
                    let mut buf = [0u8; 64];
                    while let Ok(n) = conn.read(&mut buf).await {
                        if n == 0 || conn.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                    }
                });
            }
        });
        addr
    }

    /// Connect with `config`, say hello, read it back.
    async fn talk(addr: SocketAddr, config: Arc<rustls::ClientConfig>) -> Result<(String, About)> {
        let tcp = TcpStream::connect(addr).await?;
        let name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
        let mut tls = tokio_rustls::TlsConnector::from(config).connect(name, tcp).await?;
        let seen = About::read(&tls.get_ref().1.peer_certificates().unwrap()[0])?;
        tls.write_all(b"hello").await?;
        let mut buf = [0u8; 5];
        tls.read_exact(&mut buf).await?;
        Ok((String::from_utf8_lossy(&buf).into(), seen))
    }

    #[tokio::test]
    async fn serves_tls_and_checks_client_certificates_and_subjects() {
        let (server_ca, client_ca, other_ca) = (ca("server CA"), ca("client CA"), ca("other CA"));
        let (cert, key) = leaf(&server_ca, &[(rcgen::DnType::CommonName, "engine")], &["localhost"], 90);
        let entries = BTreeMap::from([
            ("ours".to_string(), entry(&cert, &key)),
            ("clients".to_string(), CertificateEntry { ca: Some(client_ca.cert.pem()), ..Default::default() }),
        ]);
        let certificates = Certificates::load(&entries).unwrap();
        let plain = certificates.server(&ListenTls { certificate: "ours".into(), clients: None }, &[]).unwrap();
        let addr = echo(plain).await;
        let (echoed, seen) = talk(addr, client_config(&server_ca, None)).await.unwrap();
        assert_eq!((echoed.as_str(), seen.subject.as_str()), ("hello", "CN=engine"));
        assert!(talk(addr, client_config(&other_ca, None)).await.is_err(), "a client that doesn't trust it");

        let mtls = ListenTls {
            certificate: "ours".into(),
            clients: Some(Clients { ca: "clients".into(), subjects: vec!["CN=bridges*".into()] }),
        };
        let addr = echo(certificates.server(&mtls, &[]).unwrap()).await;
        let bridges = leaf(&client_ca, &[(rcgen::DnType::CommonName, "bridges01")], &[], 90);
        let stranger = leaf(&client_ca, &[(rcgen::DnType::CommonName, "someone")], &[], 90);
        let forged = leaf(&other_ca, &[(rcgen::DnType::CommonName, "bridges01")], &[], 90);
        let with = |c: &(String, String)| client_config(&server_ca, Some((c.0.as_str(), c.1.as_str())));
        assert_eq!(talk(addr, with(&bridges)).await.unwrap().0, "hello");
        // TLS 1.3 sends the client's certificate after the client thinks it's done: the refusal
        // shows as the connection closing when it reads.
        assert!(talk(addr, with(&stranger)).await.is_err(), "a subject no pattern allows");
        assert!(talk(addr, with(&forged)).await.is_err(), "issued by another CA");
        assert!(talk(addr, client_config(&server_ca, None)).await.is_err(), "no client certificate");
    }

    /// Post-quantum key exchange (X25519 + ML-KEM-768, the hybrid browsers use) between our own
    /// ends, and still a plain X25519 handshake with a peer that doesn't offer it.
    #[tokio::test]
    async fn key_exchange_is_post_quantum_and_falls_back() {
        let server_ca = ca("server CA");
        let (cert, key) = leaf(&server_ca, &[(rcgen::DnType::CommonName, "engine")], &["localhost"], 90);
        let certificates = Certificates::load(&BTreeMap::from([("ours".to_string(), entry(&cert, &key))])).unwrap();
        let config = certificates.server(&ListenTls { certificate: "ours".into(), clients: None }, &[]).unwrap();
        let addr = echo(config).await;
        let group = |client: Arc<rustls::ClientConfig>| async move {
            let tcp = TcpStream::connect(addr).await.unwrap();
            let name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
            let tls = tokio_rustls::TlsConnector::from(client).connect(name, tcp).await.unwrap();
            tls.get_ref().1.negotiated_key_exchange_group().map(|g| g.name())
        };
        assert_eq!(group(client_config(&server_ca, None)).await, Some(rustls::NamedGroup::X25519MLKEM768));

        let mut old = rustls::crypto::aws_lc_rs::default_provider();
        old.kx_groups = vec![rustls::crypto::aws_lc_rs::kx_group::X25519];
        let mut roots = rustls::RootCertStore::empty();
        roots.add(server_ca.cert.der().clone()).unwrap();
        let old = rustls::ClientConfig::builder_with_provider(Arc::new(old))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        assert_eq!(group(Arc::new(old)).await, Some(rustls::NamedGroup::X25519), "a peer without it");
    }

    /// Linux: every 127.x address is this machine, so a test can connect from one not allowed.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn refuses_addresses_off_the_allow_list_before_the_handshake() {
        let server_ca = ca("server CA");
        let (cert, key) = leaf(&server_ca, &[(rcgen::DnType::CommonName, "engine")], &["localhost"], 90);
        let certificates = Certificates::load(&BTreeMap::from([("ours".to_string(), entry(&cert, &key))])).unwrap();
        let config = certificates.server(&ListenTls { certificate: "ours".into(), clients: None }, &[]).unwrap();
        let allow = ductus_core::allow::Allow(vec!["127.0.0.1".into()]);
        let names = crate::allow::Names::new([], crate::allow::system_resolver());
        let admission = crate::allow::Admission::new("test-before-handshake", "127.0.0.1", 0, &allow, names).unwrap();
        let addr = echo_with(config, Some(admission.clone())).await;
        assert_eq!(talk(addr, client_config(&server_ca, None)).await.unwrap().0, "hello", "127.0.0.1 is allowed");

        // From 127.0.0.2: the start of a ClientHello gets not one byte back (no ServerHello, no
        // alert): the connection is closed unread.
        let socket = tokio::net::TcpSocket::new_v4().unwrap();
        socket.bind("127.0.0.2:0".parse().unwrap()).unwrap();
        let mut tcp = socket.connect(addr).await.unwrap();
        let _ = tcp.write_all(&[0x16, 0x03, 0x01, 0x00, 0x05, 1, 0, 0, 1, 3]).await;
        let mut buf = [0u8; 16];
        let read = tokio::time::timeout(Duration::from_secs(5), tcp.read(&mut buf)).await.expect("closed at once");
        assert!(matches!(read, Ok(0) | Err(_)), "nothing sent back: {read:?}");
        let (refused, last) = admission.refused();
        assert_eq!((refused, last.map(|(ip, _)| ip.to_string())), (1, Some("127.0.0.2".to_string())));

        // Logged once a minute per listener and address, however often it retries.
        let refusals = Refusals::default();
        let from = "10.1.40.7:50000".parse().unwrap();
        assert!(refusals.not_allowed("adt", from, "10.1.20.0/24"));
        assert!(!refusals.not_allowed("adt", "10.1.40.7:50001".parse().unwrap(), "10.1.20.0/24"));
        assert!(refusals.not_allowed("lab", from, "10.1.20.0/24"), "another listener's line is its own");
    }

    #[test]
    fn accepted_connections_get_keepalive() {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let client = TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
            let (accepted, _) = listener.accept().await.unwrap();
            assert!(!socket2::SockRef::from(&accepted).keepalive().unwrap());
            crate::allow::keepalive(&accepted);
            assert!(socket2::SockRef::from(&accepted).keepalive().unwrap());
            drop(client);
        });
    }

    #[tokio::test]
    async fn a_broken_or_expired_certificate_stops_the_start_with_the_reason() {
        let server_ca = ca("server CA");
        let (cert, key) = leaf(&server_ca, &[(rcgen::DnType::CommonName, "engine")], &["localhost"], 90);
        let (_, other_key) = leaf(&server_ca, &[(rcgen::DnType::CommonName, "other")], &["localhost"], 90);
        let (old, old_key) = leaf(&server_ca, &[(rcgen::DnType::CommonName, "old")], &["localhost"], -1);
        let err = |e: CertificateEntry| {
            format!("{:#}", Certificates::load(&BTreeMap::from([("x".to_string(), e)])).unwrap_err())
        };
        assert!(err(entry(&cert, &other_key)).contains("the private key isn't this certificate's key"));
        assert!(err(entry("nothing", &key)).contains("no certificate in it"));
        assert!(err(entry(&cert, "nothing")).contains("reading the private key"));
        assert!(err(entry(&old, &old_key)).contains("certificate `x` (resources.yaml): it expired on"));
        let missing = CertificateEntry {
            cert_file: Some("/nonexistent/x.pem".into()),
            key_file: Some("/nonexistent/x.key".into()),
            ..Default::default()
        };
        assert!(err(missing).contains("reading /nonexistent/x.pem"));
        let ca = CertificateEntry { ca: Some("nothing".into()), ..Default::default() };
        assert!(err(ca).contains("no certificate in it"));
    }

    #[tokio::test]
    async fn a_renewed_pair_is_used_and_a_broken_one_is_ignored() {
        let server_ca = ca("server CA");
        let dir = tempfile::tempdir().unwrap();
        let (cert_file, key_file) = (dir.path().join("c.pem"), dir.path().join("c.key"));
        let write = |pair: &(String, String)| {
            std::fs::write(&cert_file, &pair.0).unwrap();
            std::fs::write(&key_file, &pair.1).unwrap();
        };
        let first = leaf(&server_ca, &[(rcgen::DnType::CommonName, "first")], &["localhost"], 30);
        write(&first);
        let entries = BTreeMap::from([(
            "ours".to_string(),
            CertificateEntry {
                cert_file: Some(cert_file.clone()),
                key_file: Some(key_file.clone()),
                ..Default::default()
            },
        )]);
        let certificates = Certificates::load(&entries).unwrap();
        let config = certificates.server(&ListenTls { certificate: "ours".into(), clients: None }, &[]).unwrap();
        let addr = echo(config).await;
        let subject = || async { talk(addr, client_config(&server_ca, None)).await.unwrap().1.subject };
        assert_eq!(subject().await, "CN=first");
        assert!(certificates.reload().await.is_empty(), "unchanged: nothing to do");

        let second = leaf(&server_ca, &[(rcgen::DnType::CommonName, "second")], &["localhost"], 90);
        write(&second);
        assert_eq!(certificates.reload().await, ["ours"]);
        assert_eq!(subject().await, "CN=second", "new connections get the renewed one");

        // Half a renewal (a new certificate, the old key): kept on the one that works. Its error
        // is a change once (the pair's row shows it), not at every check after.
        let third = leaf(&server_ca, &[(rcgen::DnType::CommonName, "third")], &["localhost"], 90);
        std::fs::write(&cert_file, &third.0).unwrap();
        assert_eq!(certificates.reload().await, ["ours"]);
        assert!(certificates.reload().await.is_empty(), "the same error: nothing new");
        let pair = certificates.pairs().next().unwrap();
        assert!(pair.renewal_error().unwrap().contains("the private key isn't this certificate's key"));
        assert_eq!(subject().await, "CN=second");
        // The key follows: renewed.
        std::fs::write(&key_file, &third.1).unwrap();
        assert_eq!(certificates.reload().await, ["ours"]);
        assert_eq!((subject().await, pair.renewal_error()), ("CN=third".to_string(), None));
        // As a client certificate, the pair in use is the one presented.
        let presented = ResolvesClientCert::resolve(&**pair, &[], &[]).unwrap();
        assert_eq!(About::read(&presented.cert[0]).unwrap().subject, "CN=third");
    }

    #[test]
    fn self_signed_certificates_load_and_cover_their_names() {
        let (cert, key) = self_signed(&["engine.lab".into(), "127.0.0.1".into()], 30).unwrap();
        let certificates = Certificates::load(&BTreeMap::from([("lab".to_string(), entry(&cert, &key))])).unwrap();
        let about = certificates.pairs().next().unwrap().about();
        assert_eq!(
            (about.subject.as_str(), about.names.clone()),
            ("CN=engine.lab", vec!["engine.lab".to_string(), "127.0.0.1".to_string()])
        );
        assert!((29..=31).contains(&(about.not_after - Utc::now()).num_days()));
        assert!(self_signed(&[], 30).is_err());
    }

    #[test]
    fn refusals_say_why_in_plain_words() {
        let tls = |e: rustls::Error| std::io::Error::new(std::io::ErrorKind::InvalidData, e);
        assert!(
            why(&tls(rustls::Error::InvalidMessage(rustls::InvalidMessage::InvalidContentType)))
                .contains("isn't speaking TLS")
        );
        assert!(why(&tls(rustls::Error::NoCertificatesPresented)).contains("no client certificate"));
        assert!(
            why(&tls(rustls::Error::PeerIncompatible(rustls::PeerIncompatible::Tls12NotOffered)))
                .contains("TLS 1.2 is the minimum")
        );
        assert!(why(&std::io::ErrorKind::TimedOut.into()).contains("no handshake within 10s"));
    }
}
