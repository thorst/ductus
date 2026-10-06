//! Developer tools: synthetic traffic generator, verifying sink, status.

use anyhow::Result;
use bytes::Bytes;
use ductus_hl7::{AckCode, Message};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const DEFAULT_DB: &str = "postgres://todd:ductus@localhost/ductus_dev";

const LAST: &[&str] = &["SMITH", "JOHNSON", "WILLIAMS", "BROWN", "JONES", "GARCIA", "MILLER", "DAVIS", "LOPEZ", "WILSON"];
const FIRST: &[&str] = &["JAMES", "MARY", "ROBERT", "PATRICIA", "JOHN", "JENNIFER", "MICHAEL", "LINDA", "DAVID", "ELENA"];

fn now() -> String {
    chrono::Local::now().format("%Y%m%d%H%M%S").to_string()
}

/// A synthetic ADT^A08. MSH-10 carries the global sequence number so the sink can check order.
fn adt(seq: u64, patient: u64) -> String {
    let mrn = 100_000 + patient;
    let last = LAST[(patient % LAST.len() as u64) as usize];
    let first = FIRST[(patient / LAST.len() as u64 % FIRST.len() as u64) as usize];
    let sex = if patient % 2 == 0 { "F" } else { "M" };
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

pub async fn generate(to: &str, count: u64, patients: u64, rate: u64) -> Result<()> {
    let mut client = ductus_mllp::Client::new(to, Duration::from_secs(10));
    let start = Instant::now();
    let interval = (rate > 0).then(|| Duration::from_secs_f64(1.0 / rate as f64));
    let (mut retries, mut rejected) = (0u64, 0u64);

    for seq in 1..=count {
        let message = adt(seq, seq % patients.max(1));
        // Resend until ACKed: a lost ACK means we can't know if it was stored.
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
            let due = start + interval * seq as u32;
            tokio::time::sleep_until(due.into()).await;
        }
        if seq % 1000 == 0 {
            eprintln!("sent {seq}/{count}");
        }
    }

    let secs = start.elapsed().as_secs_f64();
    println!(
        "sent {count} messages in {secs:.1}s ({:.0}/s), retries {retries}, rejected {rejected}",
        count as f64 / secs
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
    Ok(())
}
