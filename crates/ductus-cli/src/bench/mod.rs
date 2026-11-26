//! `ductus bench` (0.8.1): what this machine and its database do with the real engine, measured
//! in a few seconds per scenario, with what the numbers mean for sizing.
//!
//! Everything runs in a schema of the bench's own in the given database
//! (`ductus_store::scratch`): each scenario is a small project written to a temp folder and run
//! by an engine in this process on that schema (its own loopback ports, no API), fed by `gen`'s
//! sending loop (`traffic`), and stopped; the schema is dropped at the end, also after a failure,
//! a panic or Ctrl-C. Destinations are files in the temp folder, so the numbers are the engine's
//! and the database's, not a receiver's.

mod report;

use anyhow::{Context, Result, anyhow, bail};
use ductus_store::scratch::{self, Counter, Scratch};
use report::{Commits, Database, Report, Scenario, ms, quantile};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

use crate::traffic::{self, Pace, Sent};

pub struct Options {
    pub database_url: String,
    /// Seconds each scenario sends for, after its warm-up.
    pub seconds: u64,
    pub json: bool,
    /// Bench although an engine runs on this database.
    pub yes: bool,
}

/// Durable commits timed (as doctor's 20, more of them for a steadier p99).
const COMMITS: usize = 100;
/// Each scenario's warm-up, not counted: connections opened, statements prepared, the new
/// partition's first pages written.
const WARM_UP: Duration = Duration::from_secs(1);
/// How long delivery may take after the last ACK before the scenario fails.
const DELIVERY_WAIT: Duration = Duration::from_secs(120);
/// The bench's project's environment.
const ENV: &str = "bench";
/// Patients spread over a scenario's connections (each patient on one connection, in order).
const PATIENTS: u64 = 500;
/// Measured sequence numbers start here, apart from the warm-up's.
const MEASURED_SEQ: u64 = 1_000_000_000;

/// An MLLP ingest scenario.
struct Ingest {
    id: &'static str,
    name: &'static str,
    durability: &'static str,
    connections: u64,
}

const INGEST: [Ingest; 3] = [
    Ingest { id: "mllp-strict-1", name: "strict MLLP, 1 connection", durability: "strict", connections: 1 },
    Ingest { id: "mllp-strict-16", name: "strict MLLP, 16 connections", durability: "strict", connections: 16 },
    Ingest { id: "mllp-relaxed-16", name: "relaxed MLLP, 16 connections", durability: "relaxed", connections: 16 },
];

/// The engine running now: stopped by its scenario, or by `run` when the bench failed or was
/// interrupted under it (before the schema is dropped).
type Running = Arc<Mutex<Option<(CancellationToken, tokio::task::JoinHandle<Result<()>>)>>>;

pub async fn run(o: Options) -> Result<()> {
    let began = Instant::now();
    let found = scratch::prepare(&o.database_url, o.yes).await?;
    for name in &found.swept {
        eprintln!("dropped {name}: an earlier bench's schema, left behind over a day ago");
    }
    for (name, why) in &found.not_swept {
        eprintln!("couldn't drop {name} (an earlier bench's schema): {why}");
    }
    if found.live_engines > 0 {
        eprintln!("an engine is running on this database (--yes): the bench's numbers are lower while it has traffic");
    }
    let scratch = Scratch::create(&o.database_url).await?;
    let schema = scratch.name.clone();
    eprintln!("benching in the schema {schema} (dropped at the end, also on Ctrl-C)");
    let url = scratch.url()?;
    let running: Running = Default::default();
    let mut work = tokio::spawn(measure(url, schema.clone(), o.seconds, running.clone()));
    let measured = tokio::select! {
        ended = &mut work => ended.map_err(|e| anyhow!("the bench failed: {e}")).and_then(|r| r),
        _ = tokio::signal::ctrl_c() => {
            work.abort();
            let _ = work.await;
            Err(anyhow!("stopped (Ctrl-C)"))
        }
    };
    let stopped = stop(&running).await;
    let dropped = scratch.drop().await;
    let mut report = measured?;
    stopped?;
    dropped?;
    report.took_s = began.elapsed().as_secs_f64();
    if o.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", report.text());
    }
    Ok(())
}

/// Everything measured, in the schema `url` searches.
async fn measure(url: String, schema: String, seconds: u64, running: Running) -> Result<Report> {
    let mut counter = Counter::connect(&url).await?;
    let (version, synchronous_commit) = counter.server().await?;
    let round_trip = counter.round_trip(20).await?;
    eprintln!("timing {COMMITS} durable commits…");
    let times = counter.commits(COMMITS).await?;
    let commits = Commits { count: COMMITS, median_ms: ms(quantile(&times, 0.5)), p99_ms: ms(quantile(&times, 0.99)) };
    let database = where_it_is(&url, version, ms(round_trip), synchronous_commit, schema);
    let mut scenarios = Vec::new();
    for s in &INGEST {
        eprintln!("{}: {seconds} s…", s.name);
        scenarios.push(ingest(s, &url, Duration::from_secs(seconds), &mut counter, &running).await?);
    }
    counter.close().await;
    let advice = report::advise(&commits, &database, &scenarios);
    Ok(Report {
        ductus: env!("CARGO_PKG_VERSION").into(),
        machine: report::machine(),
        database,
        seconds_per_scenario: seconds,
        commits,
        scenarios,
        advice,
        took_s: 0.0,
    })
}

fn where_it_is(url: &str, version: String, round_trip_ms: f64, synchronous_commit: String, schema: String) -> Database {
    let host = ductus_core::pgtls::UrlTls::read(url).map_or_else(|| "?".into(), |u| u.host);
    let location = if host.starts_with('/') {
        "socket"
    } else if ductus_core::pgtls::local(&host) {
        "local"
    } else {
        "network"
    };
    Database { version, location: location.into(), host, round_trip_ms, synchronous_commit, schema }
}

/// One MLLP scenario: an interface receiving on a port of its own and delivering to a file per
/// message, fed on `connections` connections for `seconds` after the warm-up.
async fn ingest(
    s: &Ingest,
    url: &str,
    seconds: Duration,
    counter: &mut Counter,
    running: &Running,
) -> Result<Scenario> {
    let dir = tempfile::tempdir().context("making the bench's project folder")?;
    let port = free_port()?;
    let name = format!("bench-{}", s.id);
    let interface = format!(
        "name: {name}\nsource: {{ mllp: {{ host: 127.0.0.1, port: {port} }} }}\nordering: {{ key: PID-3.1 }}\ndurability: {}\ndestinations: {{ out: {{ file: {{ dir: out }} }} }}\n",
        s.durability
    );
    project(dir.path(), &name, &interface)?;
    start(dir.path(), url, running).await?;
    let to = format!("127.0.0.1:{port}");
    let began = Instant::now();
    let warm = send(&to, s.connections, WARM_UP, 0).await;
    let measured = send(&to, s.connections, seconds, MEASURED_SEQ).await;
    let last_answer = Instant::now();
    let answered = warm.sent.answered + measured.sent.answered;
    if measured.sent.given_up > 0 || measured.sent.answered == 0 {
        bail!(
            "{}: the bench's engine stopped answering ({} answered, {} given up)",
            s.name,
            measured.sent.answered,
            measured.sent.given_up
        );
    }
    // Every answered message stored, and at its file: what the ACK promised.
    let (delivered_after, delivered_per_second) = loop {
        let (stored, pending) = counter.messages(&name).await?;
        if stored < answered as i64 && pending == 0 {
            bail!("{}: the engine ACKed {answered} messages but stored {stored}: please report this", s.name);
        }
        if pending == 0 {
            break (last_answer.elapsed(), stored as f64 / began.elapsed().as_secs_f64());
        }
        if last_answer.elapsed() > DELIVERY_WAIT {
            bail!("{}: {pending} messages still undelivered {} s after the last ACK", s.name, DELIVERY_WAIT.as_secs());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    stop(running).await?;
    let mut waits = measured.sent.waits;
    waits.sort();
    let secs = measured.elapsed.as_secs_f64();
    Ok(Scenario {
        id: s.id.into(),
        name: s.name.into(),
        connections: s.connections,
        durability: s.durability.into(),
        messages: measured.sent.answered,
        seconds: (secs * 100.0).round() / 100.0,
        per_second: (measured.sent.answered as f64 / secs).round(),
        ack_p50_ms: ms(quantile(&waits, 0.5)),
        ack_p99_ms: ms(quantile(&waits, 0.99)),
        delivered_after_ms: ms(delivered_after),
        delivered_per_second: delivered_per_second.round(),
    })
}

/// What all of a run's connections saw, and how long until the last one finished.
struct Sending {
    sent: Sent,
    elapsed: Duration,
}

/// Send synthetic ADT to `to` on `connections` connections for `time`, as fast as ACKs come
/// back. Sequence numbers from `first` up (unique across a scenario's runs); each patient on one
/// connection, so ordering holds as with real senders.
async fn send(to: &str, connections: u64, time: Duration, first: u64) -> Sending {
    let start = Instant::now();
    let until = start + time;
    let per_lane = (PATIENTS / connections).max(1);
    let lanes: Vec<_> = (0..connections)
        .map(|lane| {
            let client = ductus_mllp::Client::new(to.to_string(), Duration::from_secs(10));
            let messages =
                (0u64..).map(move |k| (first + k * connections + lane + 1, lane + connections * (k % per_lane)));
            let pace = Pace { rate: 0.0, start, until: Some(until), keep_waits: true };
            tokio::spawn(traffic::send(client, messages, pace))
        })
        .collect();
    let mut sent = Sent::default();
    for lane in lanes {
        if let Ok(lane) = lane.await {
            sent.add(lane);
        }
    }
    Sending { sent, elapsed: start.elapsed() }
}

/// A project with one interface, in `root`.
fn project(root: &Path, name: &str, interface_yaml: &str) -> Result<()> {
    let folder = root.join("interfaces").join(name);
    std::fs::create_dir_all(&folder)?;
    std::fs::create_dir_all(root.join("environments"))?;
    std::fs::write(root.join("ductus.yaml"), "{}\n")?;
    std::fs::write(root.join("environments").join(format!("{ENV}.yaml")), "variables: {}\n")?;
    std::fs::write(folder.join("interface.yaml"), interface_yaml)?;
    Ok(())
}

/// A loopback port nothing listens on now.
fn free_port() -> Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0").context("finding a free port")?.local_addr()?.port())
}

/// Start an engine in this process on `root`'s project and the schema `url` searches; returns
/// once every interface is up. It is kept in `running` until [`stop`].
async fn start(root: &Path, url: &str, running: &Running) -> Result<()> {
    let stop = CancellationToken::new();
    let (started, up) = tokio::sync::oneshot::channel();
    let options = ductus_runtime::EngineOptions {
        env: ENV.into(),
        api_listen: Some("off".into()),
        stop_request: Some(stop.clone()),
        on_started: Some(Box::new(move || {
            let _ = started.send(());
        })),
        ..Default::default()
    };
    let (root, url) = (root.to_path_buf(), url.to_string());
    let task = tokio::spawn(async move { ductus_runtime::run(&root, &url, options).await });
    *running.lock().unwrap() = Some((stop, task));
    // The callback is dropped unsent when the run ends before it's up: then the run says why.
    match tokio::time::timeout(Duration::from_secs(120), up).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => {
            let (_, task) = running.lock().unwrap().take().expect("the engine just started");
            Err(match task.await {
                Ok(Err(e)) => e.context("the bench's engine didn't start"),
                Ok(Ok(())) => anyhow!("the bench's engine ended before it started"),
                Err(e) => anyhow!("the bench's engine failed: {e}"),
            })
        }
        Err(_) => bail!("the bench's engine didn't start within 2 minutes"),
    }
}

/// Stop the engine running now, if any, and wait for its end.
async fn stop(running: &Running) -> Result<()> {
    let Some((stop, task)) = running.lock().unwrap().take() else { return Ok(()) };
    stop.cancel();
    match tokio::time::timeout(Duration::from_secs(60), task).await {
        Ok(Ok(ended)) => ended.context("the bench's engine failed as it stopped"),
        Ok(Err(e)) => Err(anyhow!("the bench's engine failed: {e}")),
        Err(_) => Err(anyhow!("the bench's engine didn't stop within a minute")),
    }
}
