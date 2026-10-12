//! Developer tools: synthetic traffic generator, verifying sink, status.

use anyhow::Result;
use bytes::Bytes;
use ductus_hl7::{AckCode, Message};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The engine's database, from `--database-url` or `DUCTUS_DATABASE_URL`. There is no default:
/// a default would point at somebody's own server.
pub fn database_url(given: Option<String>) -> Result<String> {
    match given.map(|u| u.trim().to_string()) {
        Some(url) if !url.is_empty() => Ok(url),
        _ => anyhow::bail!(
            "no database: set DUCTUS_DATABASE_URL (postgres://user:password@host/database) or pass              --database-url; for local development `ductus dev` starts its own Postgres"
        ),
    }
}

const LAST: &[&str] =
    &["SMITH", "JOHNSON", "WILLIAMS", "BROWN", "JONES", "GARCIA", "MILLER", "DAVIS", "LOPEZ", "WILSON"];
const FIRST: &[&str] =
    &["JAMES", "MARY", "ROBERT", "PATRICIA", "JOHN", "JENNIFER", "MICHAEL", "LINDA", "DAVID", "ELENA"];

fn now() -> String {
    chrono::Local::now().format("%Y%m%d%H%M%S").to_string()
}

/// A synthetic ADT^A08. MSH-10 carries the global sequence number so the sink can check order.
fn adt(seq: u64, patient: u64) -> String {
    let mrn = 100_000 + patient;
    let last = LAST[(patient % LAST.len() as u64) as usize];
    let first = FIRST[(patient / LAST.len() as u64 % FIRST.len() as u64) as usize];
    let sex = if patient.is_multiple_of(2) { "F" } else { "M" };
    let ts = now();
    format!(
        "MSH|^~\\&|EPIC|HOSP|DUCTUS|HOSP|{ts}||ADT^A08|{seq}|P|2.5\r\
         EVN|A08|{ts}\r\
         PID|1||{mrn}^^^MRN||{last}^{first}||19{:02}0101|{sex}\r\
         PV1|1|I|4W^{}^A\r",
        patient % 100,
        400 + patient % 40,
    )
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
}

/// Small deterministic PRNG (xorshift64*), so runs are reproducible.
struct Rng(u64);

impl Rng {
    fn next_f64(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545F4914F6CDD1D) >> 11) as f64 / (1u64 << 53) as f64
    }
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

    let start = Instant::now();
    let per_connection_rate = opts.rate as f64 / connections as f64;
    let mut tasks = Vec::new();
    for lane in lanes {
        let to = opts.to.clone();
        tasks.push(tokio::spawn(async move { send_lane(&to, lane, per_connection_rate, start).await }));
    }
    let (mut retries, mut rejected) = (0u64, 0u64);
    for task in tasks {
        let (r, j) = task.await?;
        retries += r;
        rejected += j;
    }

    let secs = start.elapsed().as_secs_f64();
    println!(
        "sent {} messages over {connections} connection(s) in {secs:.1}s ({:.0}/s), retries {retries}, rejected {rejected}",
        opts.count,
        opts.count as f64 / secs
    );
    Ok(())
}

/// Send one connection's messages in order, resending each until it's ACKed.
/// Returns (retries, rejected).
async fn send_lane(to: &str, messages: Vec<(u64, u64)>, rate: f64, start: Instant) -> (u64, u64) {
    let mut client = ductus_mllp::Client::new(to, Duration::from_secs(10));
    let interval = (rate > 0.0).then(|| Duration::from_secs_f64(1.0 / rate));
    let (mut retries, mut rejected) = (0u64, 0u64);
    for (i, (seq, patient)) in messages.into_iter().enumerate() {
        let message = adt(seq, patient);
        // A lost ACK means we can't know whether it was stored, so resend.
        loop {
            match client.send(message.as_bytes()).await {
                Ok(ack) => {
                    let ack = String::from_utf8_lossy(&ack);
                    match Message::parse(&ack).map(|a| a.value("MSA-1").to_string()).as_deref() {
                        Ok("AA") => break,
                        Ok("AR") => {
                            rejected += 1;
                            break;
                        }
                        _ => retries += 1,
                    }
                }
                Err(_) => {
                    retries += 1;
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
        }
        if let Some(interval) = interval {
            tokio::time::sleep_until((start + interval * (i as u32 + 1)).into()).await;
        }
    }
    (retries, rejected)
}

#[derive(Default)]
struct SinkStats {
    received: u64,
    seen: HashSet<u64>,
    last_per_mrn: HashMap<String, u64>,
    duplicates: u64,
    out_of_order: u64,
}

pub async fn sink(port: u16) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    let stats = Arc::new(Mutex::new(SinkStats::default()));
    println!("sink listening on {port}; Ctrl-C to stop");

    let handler_stats = stats.clone();
    let handler = move |frame: Bytes| {
        let stats = handler_stats.clone();
        async move {
            let raw = String::from_utf8_lossy(&frame);
            let Ok(message) = Message::parse(&raw) else {
                return Bytes::from(ductus_hl7::reject_unparseable("not HL7", &now()));
            };
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
            Bytes::from(message.ack(AckCode::Accept, "", &now()))
        }
    };
    tokio::spawn(ductus_mllp::serve(listener, handler));

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
