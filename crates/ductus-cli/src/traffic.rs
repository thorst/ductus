//! Synthetic HL7 over MLLP, shared by `ductus gen` (a count, printed) and `ductus bench` (for a
//! time, measured): the messages, the sending loop, and what a sender saw.

use ductus_hl7::Message;
use std::time::{Duration, Instant};

const LAST: &[&str] =
    &["SMITH", "JOHNSON", "WILLIAMS", "BROWN", "JONES", "GARCIA", "MILLER", "DAVIS", "LOPEZ", "WILSON"];
const FIRST: &[&str] =
    &["JAMES", "MARY", "ROBERT", "PATRICIA", "JOHN", "JENNIFER", "MICHAEL", "LINDA", "DAVID", "ELENA"];

pub fn now() -> String {
    chrono::Local::now().format("%Y%m%d%H%M%S").to_string()
}

/// A synthetic ADT^A08. MSH-10 carries the global sequence number so the sink can check order.
pub fn adt(seq: u64, patient: u64) -> String {
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

/// Small deterministic PRNG (xorshift64*), so runs are reproducible.
pub struct Rng(pub u64);

impl Rng {
    pub fn next_f64(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545F4914F6CDD1D) >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// How one connection sends.
pub struct Pace {
    /// Messages per second on this connection; 0: as fast as answers come back.
    pub rate: f64,
    /// When sending started (the rate's schedule counts from it).
    pub start: Instant,
    /// Take no new message after this (the one under way is finished): `ductus bench`'s time box.
    /// A message that can't be sent by [`GIVE_UP_AFTER`] past it is given up, so a receiver
    /// that stopped answering ends the run instead of holding it.
    pub until: Option<Instant>,
    /// Keep each message's wait for its answer ([`Sent::waits`]).
    pub keep_waits: bool,
}

/// How long past [`Pace::until`] a message may keep being retried.
pub const GIVE_UP_AFTER: Duration = Duration::from_secs(10);

/// One connection's account of its sending.
#[derive(Default)]
pub struct Sent {
    /// Messages answered AA or AR (the receiver decided).
    pub answered: u64,
    pub retries: u64,
    pub rejected: u64,
    pub refused: u64,
    pub dropped: u64,
    /// Messages given up past the time box (never answered).
    pub given_up: u64,
    pub longest: Duration,
    /// Each answered message's wait from its first try to its answer (with `keep_waits`).
    pub waits: Vec<Duration>,
}

impl Sent {
    /// Another connection's account added to this one.
    pub fn add(&mut self, other: Sent) {
        self.answered += other.answered;
        self.retries += other.retries;
        self.rejected += other.rejected;
        self.refused += other.refused;
        self.dropped += other.dropped;
        self.given_up += other.given_up;
        self.longest = self.longest.max(other.longest);
        self.waits.extend(other.waits);
    }
}

/// Send one connection's messages (`(sequence, patient)`) in order, resending each until it's
/// answered (a lost ACK means we can't know whether it was stored).
pub async fn send(mut client: ductus_mllp::Client, messages: impl IntoIterator<Item = (u64, u64)>, pace: Pace) -> Sent {
    let interval = (pace.rate > 0.0).then(|| Duration::from_secs_f64(1.0 / pace.rate));
    let mut sent = Sent::default();
    // Said once each time it changes, so a refused handshake doesn't retry in silence.
    let mut last_error = String::new();
    'messages: for (i, (seq, patient)) in messages.into_iter().enumerate() {
        if pace.until.is_some_and(|until| Instant::now() >= until) {
            break;
        }
        let message = adt(seq, patient);
        let first_try = Instant::now();
        loop {
            match client.send(message.as_bytes()).await {
                Ok(ack) => {
                    let ack = String::from_utf8_lossy(&ack);
                    match Message::parse(&ack).map(|a| a.value("MSA-1").to_string()).as_deref() {
                        Ok("AA") => break,
                        Ok("AR") => {
                            sent.rejected += 1;
                            break;
                        }
                        _ => {
                            sent.retries += 1;
                            if past_giving_up(&pace) {
                                sent.given_up += 1;
                                break 'messages;
                            }
                        }
                    }
                }
                Err(e) => {
                    use std::io::ErrorKind::*;
                    // A connection closed under us is tried again on a new one almost at once, as
                    // senders do; anything else every 0.5 s.
                    let pause = match e.kind() {
                        ConnectionRefused => {
                            sent.refused += 1;
                            500
                        }
                        ConnectionReset | ConnectionAborted | BrokenPipe | UnexpectedEof => {
                            sent.dropped += 1;
                            50
                        }
                        _ => 500,
                    };
                    // The engine's words, with gen's options where they name the engine's.
                    let why = ductus_runtime::tls::outbound_failure(&e)
                        .map(|w| w.replace("`tls.certificate`", "--cert").replace("`tls.ca`", "--ca"))
                        .unwrap_or_else(|| e.to_string());
                    if why != last_error {
                        eprintln!("sending failed, retrying: {why}");
                        last_error = why;
                    }
                    sent.retries += 1;
                    if past_giving_up(&pace) {
                        sent.given_up += 1;
                        break 'messages;
                    }
                    tokio::time::sleep(Duration::from_millis(pause)).await;
                }
            }
        }
        let waited = first_try.elapsed();
        sent.answered += 1;
        sent.longest = sent.longest.max(waited);
        if pace.keep_waits {
            sent.waits.push(waited);
        }
        if let Some(interval) = interval {
            tokio::time::sleep_until((pace.start + interval * (i as u32 + 1)).into()).await;
        }
    }
    sent
}

fn past_giving_up(pace: &Pace) -> bool {
    pace.until.is_some_and(|until| Instant::now() >= until + GIVE_UP_AFTER)
}
