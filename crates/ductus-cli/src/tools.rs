//! Developer tools: synthetic traffic generator, verifying sink, status.

use anyhow::Result;
use bytes::Bytes;
use ductus_core::resources::CertificateEntry;
use ductus_core::tls::{Clients, ConnectTls, ListenTls};
use ductus_hl7::{AckCode, Message};
use ductus_runtime::tls::{Certificates, Conn};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::traffic::{self, Pace, Rng, Sent, now};

/// The engine's database, from `--database-url` or `DUCTUS_DATABASE_URL`. There is no default:
/// a default would point at somebody's own server.
pub fn database_url(given: Option<String>) -> Result<String> {
    match given.map(|u| u.trim().to_string()) {
        Some(url) if !url.is_empty() => Ok(url),
        _ => anyhow::bail!(
            "no database: set DUCTUS_DATABASE_URL (postgres://user:password@host/database) or pass --database-url; for local development `ductus dev` starts its own Postgres"
        ),
    }
}

/// Certificate files for `gen`/`sink --tls`, as the engine's `certificates:` entries would name
/// them: a CA to trust (or, for `sink`, that issues clients' certificates) and our own pair.
#[derive(Default)]
pub struct TlsFiles {
    pub ca: Option<PathBuf>,
    pub cert: Option<PathBuf>,
    pub key: Option<PathBuf>,
}

impl TlsFiles {
    /// Loaded the way the engine loads resources.yaml's `certificates:` (`ca` and `pair`), so a
    /// file the engine would refuse is refused here with the same words.
    fn load(&self) -> Result<Certificates> {
        let mut entries = BTreeMap::new();
        if let Some(ca) = &self.ca {
            entries.insert("ca".to_string(), CertificateEntry { ca_file: Some(ca.clone()), ..Default::default() });
        }
        if let (Some(cert), Some(key)) = (&self.cert, &self.key) {
            let pair =
                CertificateEntry { cert_file: Some(cert.clone()), key_file: Some(key.clone()), ..Default::default() };
            entries.insert("pair".to_string(), pair);
        }
        Certificates::load(&entries)
    }

    /// As the engine's `tls: { ca, certificate }` on a destination.
    fn connecting(&self) -> ConnectTls {
        ConnectTls { ca: self.ca.as_ref().map(|_| "ca".into()), certificate: self.cert.as_ref().map(|_| "pair".into()) }
    }
}

/// Traffic shape for `ductus gen`.
pub struct GenOptions {
    pub to: String,
    pub count: u64,
    pub patients: u64,
    /// Total messages per second across all connections; 0 = as fast as ACKs come back.
    pub rate: u64,
    pub connections: u64,
    /// Fraction of messages (0..1) for a single hot patient, to exercise hotspot handling.
    pub hot: f64,
    /// Added to sequence and patient numbers (see `ductus gen --offset`).
    pub offset: u64,
    /// Send over TLS: the receiver checked against the platform's roots and `ca`, `cert` presented.
    pub tls: Option<TlsFiles>,
    /// Connect from this local address (to try a receiver's `allow:` list).
    pub from: Option<std::net::IpAddr>,
}

pub async fn generate(opts: GenOptions) -> Result<()> {
    let patients = opts.patients.max(1);
    let connections = opts.connections.max(1);

    // Assign each message a patient, and each patient a connection: one patient's messages
    // always travel on one connection in sequence order, so the sink can verify ordering.
    let mut rng = Rng(0x9E3779B97F4A7C15);
    let mut lanes: Vec<Vec<(u64, u64)>> = vec![Vec::new(); connections as usize];
    for seq in 1..=opts.count {
        let patient = if opts.hot > 0.0 && rng.next_f64() < opts.hot { 0 } else { seq % patients };
        lanes[(patient % connections) as usize].push((opts.offset + seq, opts.offset + patient));
    }

    let tls = match &opts.tls {
        None => None,
        Some(files) => Some(files.load()?.client(&files.connecting())?),
    };
    let (host, port) = match opts.to.rsplit_once(':').map(|(h, p)| (h, p.parse::<u16>())) {
        Some((host, Ok(port))) => (host.to_string(), port),
        _ => anyhow::bail!("--to {}: give host:port", opts.to),
    };

    let start = Instant::now();
    let per_connection_rate = opts.rate as f64 / connections as f64;
    let mut tasks = Vec::new();
    for lane in lanes {
        let timeout = Duration::from_secs(10);
        let mut client = match &tls {
            Some(config) => ductus_runtime::tls::mllp_client(&host, port, timeout, config.clone()),
            None => ductus_mllp::Client::new(opts.to.clone(), timeout),
        };
        if let Some(local) = opts.from {
            client = client.from(local);
        }
        let pace = Pace { rate: per_connection_rate, start, until: None, keep_waits: false };
        tasks.push(tokio::spawn(traffic::send(client, lane, pace)));
    }
    let mut total = Sent::default();
    for task in tasks {
        total.add(task.await?);
    }

    let secs = start.elapsed().as_secs_f64();
    let Sent { retries, rejected, refused, dropped, longest, .. } = total;
    // What a sender saw: connections refused (nothing listening) and dropped (closed or reset
    // under a message), and the longest a message took from its first try to its ACK.
    println!(
        "sent {} messages over {connections} connection(s) in {secs:.1}s ({:.0}/s), retries {retries}, rejected {rejected}, refused {refused}, dropped {dropped}, longest wait {} ms",
        opts.count,
        opts.count as f64 / secs,
        longest.as_millis()
    );
    Ok(())
}

#[derive(Default)]
struct SinkStats {
    received: u64,
    seen: HashSet<u64>,
    last_per_mrn: HashMap<String, u64>,
    duplicates: u64,
    out_of_order: u64,
}

/// `sink --tls`: the pair it presents (`cert`/`key`), and with `ca` it asks senders for a
/// certificate that CA issued, matching one of `subjects` if any (the engine's `clients:`).
pub struct SinkTls {
    pub files: TlsFiles,
    pub subjects: Vec<String>,
}

pub async fn sink(port: u16, reject: Vec<String>, tls: Option<SinkTls>) -> Result<()> {
    let acceptor = match &tls {
        None => None,
        Some(t) => {
            let clients = t.files.ca.as_ref().map(|_| Clients { ca: "ca".into(), subjects: t.subjects.clone() });
            let config = t.files.load()?.server(&ListenTls { certificate: "pair".into(), clients }, &[])?;
            Some(tokio_rustls::TlsAcceptor::from(config))
        }
    };
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    let stats = Arc::new(Mutex::new(SinkStats::default()));
    let reject = Arc::new(reject);
    let over = match &tls {
        None => String::new(),
        Some(SinkTls { files: TlsFiles { ca: Some(ca), .. }, .. }) => {
            format!(" (MLLP over TLS, senders need a certificate from {})", ca.display())
        }
        Some(_) => " (MLLP over TLS)".into(),
    };
    if reject.is_empty() {
        println!("sink listening on {port}{over}; Ctrl-C to stop");
    } else {
        println!("sink listening on {port}{over}, rejecting messages with {}; Ctrl-C to stop", reject.join(" or "));
    }

    let handler_stats = stats.clone();
    let handler = move |frame: Bytes| {
        let stats = handler_stats.clone();
        let reject = reject.clone();
        async move {
            let raw = String::from_utf8_lossy(&frame);
            let Ok(message) = Message::parse(&raw) else {
                return Some(Bytes::from(ductus_hl7::reject_unparseable("not HL7", &now())));
            };
            if let Some(text) = reject.iter().find(|t| raw.contains(t.as_str())) {
                println!("rejected {} (contains {text})", message.control_id());
                return Some(Bytes::from(message.ack(AckCode::Reject, "rejected by the receiver", &now())));
            }
            if let Ok(seq) = message.control_id().parse::<u64>() {
                let mrn = message.value("PID-3.1").to_string();
                let mut s = stats.lock().unwrap();
                s.received += 1;
                if !s.seen.insert(seq) {
                    s.duplicates += 1;
                } else if s.last_per_mrn.get(&mrn).is_some_and(|&last| seq < last) {
                    s.out_of_order += 1;
                }
                let last = s.last_per_mrn.entry(mrn).or_insert(0);
                *last = (*last).max(seq);
            }
            Some(Bytes::from(message.ack(AckCode::Accept, "", &now())))
        }
    };
    // Handshakes as the engine's listeners make them (refusals logged once a minute per sender).
    let open = move |tcp: tokio::net::TcpStream, peer| {
        let acceptor = acceptor.clone();
        async move {
            match acceptor {
                None => Some(Conn::Plain(tcp)),
                Some(a) => ductus_runtime::tls::handshake(&a, "sink", tcp, peer).await.map(|s| Conn::Tls(Box::new(s))),
            }
        }
    };
    tokio::spawn(ductus_mllp::serve_with(listener, open, handler, ductus_mllp::CancellationToken::new()));

    let report = |s: &SinkStats| {
        format!(
            "received {} | unique {} | duplicates {} | out of order {} | patients {}",
            s.received,
            s.seen.len(),
            s.duplicates,
            s.out_of_order,
            s.last_per_mrn.len()
        )
    };
    let mut ticker = tokio::time::interval(Duration::from_secs(5));
    let mut last_received = u64::MAX;
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let s = stats.lock().unwrap();
                if s.received != last_received {
                    println!("{}", report(&s));
                    last_received = s.received;
                }
            }
            _ = tokio::signal::ctrl_c() => {
                println!("final: {}", report(&stats.lock().unwrap()));
                return Ok(());
            }
        }
    }
}

pub async fn status(database_url: &str) -> Result<()> {
    let store = ductus_store::Store::connect(database_url, 2).await?;
    let counts = store.counts().await?;
    if counts.is_empty() {
        println!("no messages");
    }
    for (interface, status, n) in counts {
        println!("{interface:<20} {:<12} {n}", status.label());
    }
    // Per destination, for interfaces with several (one destination says nothing new).
    let deliveries = store.delivery_counts().await?;
    let several = |interface: &str| {
        let mut names: Vec<_> = deliveries.iter().filter(|(i, ..)| i == interface).map(|(_, d, ..)| d).collect();
        names.dedup();
        names.len() > 1
    };
    let rows: Vec<_> = deliveries.iter().filter(|(i, ..)| several(i)).collect();
    if !rows.is_empty() {
        println!("\nby destination:");
        for (interface, destination, status, n) in rows {
            println!("{:<20} {:<12} {n}", format!("{interface} → {destination}"), status.label());
        }
    }
    Ok(())
}
