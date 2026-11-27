//! `ductus bench` (0.8.1): what this machine and its database do with the real engine, measured
//! in a few seconds per scenario, with what the numbers mean for sizing.
//!
//! Everything runs in a schema of the bench's own in the given database
//! (`ductus_store::scratch`): each scenario is a small project written to a temp folder and run
//! by an engine in this process on that schema (its own loopback ports, no API), fed by `gen`'s
//! sending loop (`traffic`), an HTTP client or a file, and stopped; the schema is dropped at the
//! end, also after a failure, a panic or Ctrl-C. Destinations are files in the temp folder, so
//! the numbers are the engine's and the database's, not a receiver's. Two scenarios run without
//! that engine: transforms (V8 alone, `transforms`) and memory (an engine in a process of its
//! own, `memory`).
//!
//! Every scenario runs once per pass and the passes alternate (all scenarios, then all again),
//! so a disk's or a neighbour's slow moment shows as a wider range instead of one scenario's
//! number.

mod compare;
pub mod interface;
mod memory;
mod report;

pub use compare::compare;
mod transforms;

use anyhow::{Context, Result, anyhow, bail};
use ductus_store::scratch::{self, Counter, Scratch};
use report::{Commits, Database, Measured, Report, ms, quantile, secs};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

use crate::traffic::{self, Pace, Sent};

pub struct Options {
    pub database_url: String,
    /// Seconds each timed scenario sends for, after its warm-up.
    pub seconds: u64,
    /// One pass instead of three.
    pub quick: bool,
    pub json: bool,
    /// Bench although an engine runs on this database.
    pub yes: bool,
}

/// Passes without `--quick`.
const PASSES: usize = 3;
/// Durable commits timed (as doctor's 20, more of them for a steadier p99).
const COMMITS: usize = 100;
/// Each scenario's warm-up, not counted: connections opened, statements prepared, the new
/// partition's first pages written, V8's compiling of hot code.
pub(crate) const WARM_UP: Duration = Duration::from_secs(1);
/// How long delivery may take after the last ACK before a fan-out scenario fails.
const DELIVERY_WAIT: Duration = Duration::from_secs(120);
/// The bench's project's environment.
pub(crate) const ENV: &str = "bench";
/// Patients spread over a scenario's connections (each patient on one connection, in order).
const PATIENTS: u64 = 500;
/// Measured sequence numbers start here, apart from the warm-up's.
const MEASURED_SEQ: u64 = 1_000_000_000;
/// Fan-out: destinations per scenario, and the messages each sends (a count, not a time: every
/// one is delivered to every destination before it ends, and 25 destinations take longest).
const FANOUT: [u64; 3] = [1, 5, 25];
const FANOUT_MESSAGES: u64 = 2_000;
const FANOUT_CONNECTIONS: u64 = 16;
/// File ingest: rows in the CSV (about 50 bytes each), and in its warm-up file.
const FILE_ROWS: u64 = 100_000;
const FILE_WARM_ROWS: u64 = 2_000;
/// HTTP respond mode: callers, each on a kept-alive connection of its own.
const HTTP_CONNECTIONS: [u64; 2] = [1, 16];

/// An MLLP ingest scenario.
struct Ingest {
    id: &'static str,
    name: &'static str,
    durability: &'static str,
    connections: u64,
}

const INGEST: [Ingest; 3] = [
    Ingest { id: "mllp-strict-1", name: "strict, 1 connection", durability: "strict", connections: 1 },
    Ingest { id: "mllp-strict-16", name: "strict, 16 connections", durability: "strict", connections: 16 },
    Ingest { id: "mllp-relaxed-16", name: "relaxed, 16 connections", durability: "relaxed", connections: 16 },
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
    let passes = if o.quick { 1 } else { PASSES };
    let mut work = tokio::spawn(measure(url, schema.clone(), o.seconds, passes, running.clone()));
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
async fn measure(url: String, schema: String, seconds: u64, passes: usize, running: Running) -> Result<Report> {
    let mut counter = Counter::connect(&url).await?;
    let (version, synchronous_commit) = counter.server().await?;
    let round_trip = counter.round_trip(20).await?;
    eprintln!("timing {COMMITS} durable commits…");
    let times = counter.commits(COMMITS).await?;
    let commits = Commits { count: COMMITS, median_ms: ms(quantile(&times, 0.5)), p99_ms: ms(quantile(&times, 0.99)) };
    let database = where_it_is(&url, version, ms(round_trip), synchronous_commit, schema);
    let mut runs = Vec::new();
    for pass in 1..=passes {
        let mut bench = Pass {
            url: &url,
            pass,
            passes,
            seconds: Duration::from_secs(seconds),
            counter: &mut counter,
            running: &running,
        };
        for s in &INGEST {
            bench.say(&format!("MLLP {}: {seconds} s", s.name));
            runs.push(bench.ingest(s).await?);
        }
        for n in FANOUT {
            bench.say(&format!("fan-out to {n}: {} messages", report::whole(FANOUT_MESSAGES as f64)));
            runs.push(bench.fanout(n).await?);
        }
        bench.say(&format!("file ingest: a CSV of {} rows", report::whole(FILE_ROWS as f64)));
        runs.push(bench.file().await?);
        bench.say(&format!("HTTP respond mode: {seconds} s each on 1 and 16 connections"));
        runs.extend(bench.http().await?);
        bench.say(&format!("transforms: {seconds} s each on 1 thread and on all"));
        runs.extend(transforms::run(bench.seconds).await?);
        bench.say("memory: an engine with 100, then 1,000 idle interfaces");
        match memory::run(&url).await? {
            Some(m) => runs.push(m),
            None if pass == 1 => eprintln!("  (skipped: this system's process memory isn't read)"),
            None => {}
        }
    }
    counter.close().await;
    let scenarios = report::gather(runs);
    let advice = report::advise(&commits, &database, &scenarios);
    Ok(Report {
        ductus: env!("CARGO_PKG_VERSION").into(),
        machine: report::machine(),
        database,
        passes,
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

/// One pass's scenarios that run the engine in this process.
struct Pass<'a> {
    url: &'a str,
    pass: usize,
    passes: usize,
    seconds: Duration,
    counter: &'a mut Counter,
    running: &'a Running,
}

impl Pass<'_> {
    /// Progress on stderr: `pass 2/3  fan-out to 5: 2,000 messages…`.
    fn say(&self, what: &str) {
        if self.passes > 1 {
            eprintln!("pass {}/{}  {what}…", self.pass, self.passes);
        } else {
            eprintln!("{what}…");
        }
    }

    /// The interface's name in this pass (each pass's rows apart from the others').
    fn name(&self, id: &str) -> String {
        format!("bench-{id}-{}", self.pass)
    }

    /// MLLP ingest: an interface receiving on a port of its own and delivering to a file per
    /// message, fed on `connections` connections for `seconds` after the warm-up. Delivery is
    /// counted over the same seconds, not waited for: where it was slower than receiving, the
    /// rate says so (and the backlog is dropped with the schema).
    async fn ingest(&mut self, s: &Ingest) -> Result<Measured> {
        let dir = tempfile::tempdir().context("making the bench's project folder")?;
        let port = free_port()?;
        let name = self.name(s.id);
        let interface = format!(
            "name: {name}\nsource: {{ mllp: {{ host: 127.0.0.1, port: {port} }} }}\nordering: {{ key: PID-3.1 }}\ndurability: {}\ndestinations: {{ out: {{ file: {{ dir: out }} }} }}\n",
            s.durability
        );
        project(dir.path(), &name, &interface)?;
        start(dir.path(), self.url, self.running).await?;
        let to = format!("127.0.0.1:{port}");
        let warm = send(&to, s.connections, Budget::For(WARM_UP), 0).await;
        let (stored, waiting) = self.counter.messages(&name).await?;
        let delivered_before = stored - waiting;
        let measured = send(&to, s.connections, Budget::For(self.seconds), MEASURED_SEQ).await;
        let (stored, waiting) = self.counter.messages(&name).await?;
        stop(self.running).await?;
        let answered = warm.sent.answered + measured.sent.answered;
        answered_and_stored(&format!("MLLP {}", s.name), &measured.sent, answered, stored)?;
        let mut waits = measured.sent.waits;
        waits.sort();
        let took = measured.elapsed.as_secs_f64();
        let delivered = (stored - waiting - delivered_before).max(0);
        Ok(Measured {
            id: s.id.into(),
            name: s.name.into(),
            numbers: vec![
                ("stored_per_second", (measured.sent.answered as f64 / took).round()),
                ("ack_p50_ms", ms(quantile(&waits, 0.5))),
                ("ack_p99_ms", ms(quantile(&waits, 0.99))),
                ("delivered_per_second", (delivered as f64 / took).round()),
            ],
        })
    }

    /// Fan-out: strict MLLP on 16 connections to an interface with `destinations` file
    /// destinations, a set count of messages, timed until every one is at every destination.
    async fn fanout(&mut self, destinations: u64) -> Result<Measured> {
        let dir = tempfile::tempdir().context("making the bench's project folder")?;
        let port = free_port()?;
        let id = format!("fanout-{destinations}");
        let name = self.name(&id);
        let mut interface = format!(
            "name: {name}\nsource: {{ mllp: {{ host: 127.0.0.1, port: {port} }} }}\nordering: {{ key: PID-3.1 }}\ndestinations:\n"
        );
        for d in 1..=destinations {
            interface += &format!("  d{d}: {{ file: {{ dir: out/d{d} }} }}\n");
        }
        project(dir.path(), &name, &interface)?;
        start(dir.path(), self.url, self.running).await?;
        let to = format!("127.0.0.1:{port}");
        let what = format!("fan-out to {destinations}");
        // Warmed up with a few messages each, all delivered before the measured ones go.
        let warm = send(&to, FANOUT_CONNECTIONS, Budget::Count(10 * FANOUT_CONNECTIONS), 0).await;
        self.delivered(&what, &name, Instant::now()).await?;
        let began = Instant::now();
        let measured = send(&to, FANOUT_CONNECTIONS, Budget::Count(FANOUT_MESSAGES), MEASURED_SEQ).await;
        let answered = warm.sent.answered + measured.sent.answered;
        let stored = self.delivered(&what, &name, Instant::now()).await;
        let all_delivered = began.elapsed();
        stop(self.running).await?;
        answered_and_stored(&what, &measured.sent, answered, stored?)?;
        let deliveries = measured.sent.answered * destinations;
        Ok(Measured {
            id: id.clone(),
            name: match destinations {
                1 => "1 destination".into(),
                n => format!("{n} destinations"),
            },
            numbers: vec![
                ("stored_per_second", (measured.sent.answered as f64 / measured.elapsed.as_secs_f64()).round()),
                ("all_delivered_s", secs(all_delivered)),
                ("deliveries_per_second", (deliveries as f64 / all_delivered.as_secs_f64()).round()),
            ],
        })
    }

    /// Wait until the interface has no message waiting for a destination; the count stored
    /// (fewer than were ACKed is the caller's to say).
    async fn delivered(&mut self, what: &str, name: &str, since: Instant) -> Result<i64> {
        loop {
            let (stored, waiting) = self.counter.messages(name).await?;
            if waiting == 0 {
                return Ok(stored);
            }
            if since.elapsed() > DELIVERY_WAIT {
                bail!("{what}: {waiting} messages still undelivered {} s after the last ACK", DELIVERY_WAIT.as_secs());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// File ingest: a CSV dropped in the interface's folder, `split: records`, a row a message,
    /// timed from its first row stored to its last (the folder's poll isn't the engine's speed).
    /// The transform drops each row, so what's measured is reading and storing.
    async fn file(&mut self) -> Result<Measured> {
        let dir = tempfile::tempdir().context("making the bench's project folder")?;
        let name = self.name("file-csv");
        let interface = format!(
            "name: {name}\nsource: {{ file: {{ dir: in, pattern: \"*.csv\", poll: 100ms, split: records }} }}\nformat: csv\nscript: transform.ts\ndestinations: {{ out: {{ file: {{ dir: out }} }} }}\n"
        );
        project(dir.path(), &name, &interface)?;
        let folder = dir.path().join("interfaces").join(&name);
        std::fs::write(folder.join("transform.ts"), "export function transform() {\n  return null;\n}\n")?;
        let input = folder.join("in");
        std::fs::create_dir_all(&input)?;
        start(dir.path(), self.url, self.running).await?;
        let rows = async {
            drop_csv(&input, "warm", 0, FILE_WARM_ROWS)?;
            self.stored(&name, FILE_WARM_ROWS as i64).await?;
            drop_csv(&input, "rows", FILE_WARM_ROWS, FILE_ROWS)?;
            self.stored(&name, (FILE_WARM_ROWS + FILE_ROWS) as i64).await
        }
        .await;
        stop(self.running).await?;
        let (first, last, counted) = rows?;
        let took = last.duration_since(first).as_secs_f64().max(0.001);
        Ok(Measured {
            id: "file-csv".into(),
            name: format!("{} rows", report::whole(FILE_ROWS as f64)),
            numbers: vec![("rows_per_second", (counted as f64 / took).round())],
        })
    }

    /// Wait until `total` rows are stored; when the first beyond what was there was seen, when
    /// the last was, and how many came between.
    async fn stored(&mut self, name: &str, total: i64) -> Result<(Instant, Instant, i64)> {
        let (before, _) = self.counter.messages(name).await?;
        let began = Instant::now();
        let mut first = None;
        loop {
            let (stored, _) = self.counter.messages(name).await?;
            let now = Instant::now();
            if stored > before && first.is_none() {
                first = Some((now, stored));
            }
            if stored >= total {
                let (at, seen) = first.unwrap_or((began, before));
                return Ok((at, now, stored - seen));
            }
            if began.elapsed() > DELIVERY_WAIT {
                bail!("file ingest: {stored} of {total} rows stored after {} s", DELIVERY_WAIT.as_secs());
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// HTTP respond mode: a GET answered by a transform (nothing stored), on one kept-alive
    /// connection and then on 16, each for `seconds` after a warm-up.
    async fn http(&mut self) -> Result<Vec<Measured>> {
        let dir = tempfile::tempdir().context("making the bench's project folder")?;
        let port = free_port()?;
        let name = self.name("http-respond");
        let interface = format!(
            "name: {name}\nsource: {{ http: {{ host: 127.0.0.1, port: {port}, path: \"/patients/{{mrn}}\", method: GET, mode: respond }} }}\nformat: json\nscript: transform.ts\n"
        );
        project(dir.path(), &name, &interface)?;
        std::fs::write(
            dir.path().join("interfaces").join(&name).join("transform.ts"),
            "import type { HttpRequest } from \"@ductus/sdk\";\n\n\
             export function transform(req: HttpRequest) {\n  return { mrn: req.params.mrn, active: true };\n}\n",
        )?;
        start(dir.path(), self.url, self.running).await?;
        let url = format!("http://127.0.0.1:{port}/patients/");
        let mut runs = Vec::new();
        let mut measured = Ok(());
        for connections in HTTP_CONNECTIONS {
            let calls = async {
                calls(&url, connections, WARM_UP).await?;
                calls(&url, connections, self.seconds).await
            };
            match calls.await {
                Ok((count, mut waits, took)) => {
                    waits.sort();
                    runs.push(Measured {
                        id: format!("http-respond-{connections}"),
                        name: match connections {
                            1 => "1 connection".into(),
                            n => format!("{n} connections"),
                        },
                        numbers: vec![
                            ("requests_per_second", (count as f64 / took.as_secs_f64()).round()),
                            ("p50_ms", ms(quantile(&waits, 0.5))),
                            ("p99_ms", ms(quantile(&waits, 0.99))),
                        ],
                    });
                }
                Err(e) => {
                    measured = Err(e);
                    break;
                }
            }
        }
        stop(self.running).await?;
        measured.map(|()| runs)
    }
}

/// Every ACKed message stored (what the ACK promised), and the engine answered throughout.
fn answered_and_stored(what: &str, measured: &Sent, answered: u64, stored: i64) -> Result<()> {
    if measured.given_up > 0 || measured.answered == 0 {
        bail!(
            "{what}: the bench's engine stopped answering ({} answered, {} given up)",
            measured.answered,
            measured.given_up
        );
    }
    if stored < answered as i64 {
        bail!("{what}: the engine ACKed {answered} messages but stored {stored}: please report this");
    }
    Ok(())
}

/// A CSV of `rows` rows (ids from `first`) in `dir`, written beside and renamed in, so the
/// folder's poll never sees half of it.
fn drop_csv(dir: &Path, name: &str, first: u64, rows: u64) -> Result<()> {
    let mut csv = String::with_capacity(rows as usize * 56 + 32);
    csv += "id,mrn,name,code,value\n";
    for i in first..first + rows {
        csv += &format!("{i},{},PATIENT^{},GLU,{}\n", 100_000 + i % PATIENTS, i % 97, 70 + i % 130);
    }
    let part = dir.join(format!("{name}.part"));
    std::fs::write(&part, csv).context("writing the bench's CSV")?;
    std::fs::rename(&part, dir.join(format!("{name}.csv"))).context("moving the bench's CSV into the folder")?;
    Ok(())
}

/// GETs to `url` + an MRN on `connections` callers, each with a connection of its own kept
/// alive, for `time`: how many were answered, each one's wait, how long until the last finished.
async fn calls(url: &str, connections: u64, time: Duration) -> Result<(u64, Vec<Duration>, Duration)> {
    let start = Instant::now();
    let until = start + time;
    let callers: Vec<_> = (0..connections)
        .map(|caller| {
            let url = url.to_string();
            tokio::spawn(async move {
                let client = reqwest::Client::builder()
                    .no_proxy()
                    .pool_max_idle_per_host(1)
                    .timeout(Duration::from_secs(10))
                    .build()
                    .context("making the bench's HTTP client")?;
                let mut waits = Vec::new();
                let mut k = caller;
                while Instant::now() < until {
                    let asked = Instant::now();
                    let answer = client.get(format!("{url}{}", 100_000 + k % PATIENTS)).send().await;
                    let answer = answer.context("calling the bench's respond-mode interface")?;
                    if !answer.status().is_success() {
                        bail!(
                            "the bench's respond-mode interface answered {}: {}",
                            answer.status(),
                            answer.text().await.unwrap_or_default()
                        );
                    }
                    answer.bytes().await.context("reading the bench's respond-mode answer")?;
                    waits.push(asked.elapsed());
                    k += connections;
                }
                Ok(waits)
            })
        })
        .collect();
    let mut waits = Vec::new();
    for caller in callers {
        waits.extend(caller.await.map_err(|e| anyhow!("a bench caller failed: {e}"))??);
    }
    Ok((waits.len() as u64, waits, start.elapsed()))
}

/// What all of a run's connections saw, and how long until the last one finished.
struct Sending {
    sent: Sent,
    elapsed: Duration,
}

/// How much a run sends: for a time, or a count over all its connections.
enum Budget {
    For(Duration),
    Count(u64),
}

/// Send synthetic ADT to `to` on `connections` connections, as fast as ACKs come back, for the
/// budget. Sequence numbers from `first` up (unique across a scenario's runs); each patient on
/// one connection, so ordering holds as with real senders.
async fn send(to: &str, connections: u64, budget: Budget, first: u64) -> Sending {
    let start = Instant::now();
    let (until, per_lane) = match budget {
        Budget::For(time) => (Some(start + time), u64::MAX),
        Budget::Count(n) => (None, n.div_ceil(connections)),
    };
    let patients = (PATIENTS / connections).max(1);
    let lanes: Vec<_> = (0..connections)
        .map(|lane| {
            let client = ductus_mllp::Client::new(to.to_string(), Duration::from_secs(10));
            let messages = (0u64..per_lane)
                .map(move |k| (first + k * connections + lane + 1, lane + connections * (k % patients)));
            let pace = Pace { rate: 0.0, start, until, keep_waits: true };
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

/// What every bench project has: ductus.yaml and the bench environment.
pub(crate) fn project_files(root: &Path) -> Result<()> {
    std::fs::create_dir_all(root.join("environments"))?;
    std::fs::write(root.join("ductus.yaml"), "{}\n")?;
    std::fs::write(root.join("environments").join(format!("{ENV}.yaml")), "variables: {}\n")?;
    Ok(())
}

/// A project with one interface, in `root`.
fn project(root: &Path, name: &str, interface_yaml: &str) -> Result<()> {
    let folder = root.join("interfaces").join(name);
    std::fs::create_dir_all(&folder)?;
    project_files(root)?;
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
