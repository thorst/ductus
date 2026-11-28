//! `ductus bench interface` (0.8.2): an interface's real code on its messages (its fixtures, or
//! files), in a loop on as many threads as the engine would give it, every destination's function
//! on each message as a worker calls them, with its databases and APIs answered by mocks after a
//! set latency. Nothing is connected, stored or sent; no database is needed. What it tells: what
//! this code can do on this machine, and what limits it.
//!
//! The mocks are the engine's `Services` (`ductus_script::Services`) answered here: a lookup waits
//! its latency (a timer, as a real one waits on the network: no CPU) and answers the rows given
//! with `--rows`, else none. Writes are answered too (nothing is written), so an interface that
//! writes can be measured. Each destination's decision is counted: code that takes another path
//! on empty rows shows as quarantined or filtered messages instead of hiding in an average.
//!
//! Where the time goes: each function is bound by the SDK's `__invokeTimed`, which stamps the
//! time as a call starts, once the message is read, when the function's result is there and once
//! that's serialized (an op that records an `Instant`: ~0.1 µs a stamp). The function's own time
//! is its stage less what it waited on lookups (each moment counted once however many were in
//! flight). The engine never binds the timed twin, so its own path costs nothing more.
//!
//! With `real` (`--real`), the environment's own databases and APIs answer instead, through the
//! engine's services (its pools, `dedicated_pools`, TLS): reads and writes as the engine makes
//! them. Never in a production environment (`production_refused`).
//!
//! A respond-mode HTTP interface is run as its responder runs it: each fixture is a request's
//! body (or, `*.request.json`, a whole request), its `transform` answers it, and the answers are
//! counted by HTTP status.

use crate::services::{EngineServices, SharedServices};
use crate::split::{Records, Splitter};
use crate::worker::{self, Outcome};
use anyhow::{Context, Result, anyhow, bail};
use ductus_core::config::{Isolation, Source, Split};
use ductus_core::format::Format;
use ductus_core::{Environment, InterfaceConfig};
use ductus_script::{BoxFuture, FetchRequest, FetchResponse, Function, FunctionId, Isolate, Runaway, Services};
use ductus_store::MessageRef;
use serde::Serialize;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

/// How long every thread runs the code before the measured part: V8 compiles hot code as it runs.
pub const WARM_UP: Duration = Duration::from_secs(1);
/// A database lookup's latency when none is given: an indexed query on the same network.
pub const DB_LATENCY: Duration = Duration::from_millis(5);
/// An API call's (`api()` or `fetch()`) latency when none is given.
pub const API_LATENCY: Duration = Duration::from_millis(40);

/// A message to run, as its source would hand it over.
#[derive(Clone)]
pub struct Input {
    /// Where it came from, for the report: "a01.hl7", "census.csv record 3".
    pub name: String,
    pub payload: String,
}

/// For how long.
#[derive(Clone, Copy, Debug)]
pub enum Until {
    Seconds(Duration),
    /// This many messages in all, after the warm-up.
    Count(u64),
}

/// What the mocks answer, and after how long.
#[derive(Clone, Debug)]
pub struct Mocks {
    pub db: Duration,
    /// `api(name)` calls.
    pub api: Duration,
    /// `fetch()` to a URL.
    pub fetch: Duration,
    /// By resources.yaml name: overrides its kind's latency.
    pub named: BTreeMap<String, Duration>,
    /// By resources.yaml name: a database's rows (an array of objects), or an API's body. None:
    /// no rows, and `{}` as a body.
    pub rows: BTreeMap<String, serde_json::Value>,
}

impl Default for Mocks {
    fn default() -> Self {
        Self { db: DB_LATENCY, api: API_LATENCY, fetch: API_LATENCY, named: BTreeMap::new(), rows: BTreeMap::new() }
    }
}

pub struct Options {
    pub root: PathBuf,
    pub env: String,
    pub interface: String,
    /// Files to run, read whole; each is split into records as the source would (`split:
    /// records`). Empty: the interface's `fixtures/`.
    pub files: Vec<PathBuf>,
    /// None: what the engine gives it (`threads_for`).
    pub threads: Option<usize>,
    pub until: Until,
    pub mocks: Mocks,
    /// Time each stage (`__invokeTimed`); false: the engine's own `__invoke`, to measure what the
    /// stamps cost.
    pub stages: bool,
    /// The environment's real databases and APIs instead of `mocks` (never a production one:
    /// `production_refused`). Its writes are made.
    pub real: bool,
}

/// The environment names `--real` refuses, ignoring case: production's, by the usual names. A
/// load test there would put a test's load, and its writes, on the systems patients' data lives
/// in. Its own `-…`/`_…` variants (`prod-east`) are refused too.
pub const PRODUCTION: [&str; 4] = ["prod", "production", "prd", "live"];

/// Why `env` can't be load-tested against for real, or None.
pub fn production_refused(env: &str) -> Option<String> {
    let lower = env.to_ascii_lowercase();
    let named = PRODUCTION
        .iter()
        .find(|p| lower == **p || lower.strip_prefix(**p).is_some_and(|rest| rest.starts_with(['-', '_'])))?;
    Some(format!(
        "`{env}` reads as a production environment (`{named}`): a load test is never run against production's databases and APIs. Run it against a test environment (`--env test --real`), or with mocks (without `--real`)"
    ))
}

/// The name a respond-mode interface's answers are counted under (it has no destinations).
pub const RESPONSE: &str = "response";

/// What a destination decided, counted over the measured messages. A respond-mode interface's
/// answers are counted under [`RESPONSE`]: `sent` is every request answered, `statuses` by HTTP
/// status (a thrown error by what the engine answers it with: 422, 503 or 500).
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Decisions {
    pub destination: String,
    /// Respond mode: the answers by HTTP status ("200": 980, "404": 20).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub statuses: BTreeMap<u16, u64>,
    pub sent: u64,
    pub filtered: u64,
    pub quarantined: u64,
    /// A temporary failure: the engine would wait and try the message again.
    pub retried: u64,
    /// The first quarantine's or retry's reason, to say why.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_reason: Option<String>,
}

/// One mocked resource, as the code used it.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Lookup {
    /// `db`, `api` or `fetch`.
    pub kind: &'static str,
    /// Its resources.yaml name (`fetch` for a URL).
    pub name: String,
    /// Calls over the measured messages, all threads.
    pub calls: u64,
    /// Of those, writes (`db().execute`; an API's or URL's requests other than GET and HEAD):
    /// answered as done, nothing written.
    pub writes: u64,
    /// Each call's latency as mocked, in milliseconds; None: the real resource (`--real`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<f64>,
    /// What the calls waited on average, in milliseconds: mocked, its latency and a few tenths of
    /// a millisecond (the thread waking), more when the threads outnumber the cores; real, what
    /// the resource took.
    pub waited_ms: f64,
    /// Real calls that failed (`--real`): the code saw an error (a temporary one retries).
    #[serde(skip_serializing_if = "is_zero")]
    pub failed: u64,
    /// It answered rows (or a body) given with `--rows`; false: none.
    pub given_rows: bool,
}

/// Where one destination's function spent a message's time, on average over the calls that
/// finished (a function that threw has no result to stamp), in milliseconds.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Stages {
    pub destination: String,
    /// Reading the message as its format says (HL7 parsed, JSON read...), and its `meta`.
    pub decode_ms: f64,
    /// The function's own time: from the message read to its result, less its lookups' waits.
    pub code_ms: f64,
    /// What it waited on mocked lookups (each moment once, however many were in flight).
    pub lookups_ms: f64,
    /// Turning its result into the outgoing message.
    pub serialize_ms: f64,
    /// Calls timed.
    pub timed: u64,
}

/// One destination's stages summed on one thread.
#[derive(Debug, Clone, Copy, Default)]
struct StageSum {
    decode: Duration,
    code: Duration,
    lookups: Duration,
    serialize: Duration,
    timed: u64,
}

impl StageSum {
    fn add(&mut self, other: StageSum) {
        self.decode += other.decode;
        self.code += other.code;
        self.lookups += other.lookups;
        self.serialize += other.serialize;
        self.timed += other.timed;
    }
}

/// The time lookups were in flight on one thread, each moment once: two lookups awaited together
/// (`Promise.all`) wait as long as the slower, not their sum.
#[derive(Debug, Default)]
struct Waiting {
    in_flight: u32,
    since: Option<Instant>,
    total: Duration,
}

impl Waiting {
    fn begin(&mut self, at: Instant) {
        if self.in_flight == 0 {
            self.since = Some(at);
        }
        self.in_flight += 1;
    }

    fn end(&mut self, at: Instant) {
        self.in_flight = self.in_flight.saturating_sub(1);
        if self.in_flight == 0
            && let Some(since) = self.since.take()
        {
            self.total += at - since;
        }
    }
}

/// What the code asked of one mocked resource on one thread.
#[derive(Debug, Clone, Copy, Default)]
struct Tally {
    calls: u64,
    writes: u64,
    failed: u64,
    waited: Duration,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

type Tallies = BTreeMap<(&'static str, String), Tally>;

/// The run's result.
#[derive(Debug, Clone, Serialize)]
pub struct Load {
    pub interface: String,
    /// The environment whose variables, secrets (and with `real`, resources) the code used.
    pub env: String,
    /// Its real databases and APIs answered (`--real`); false: mocks.
    pub real: bool,
    /// A respond-mode HTTP interface: each message is a request, answered by its `transform`.
    pub respond: bool,
    pub threads: usize,
    /// Why that many (the engine's rule, or `--threads`).
    pub threads_why: String,
    /// Distinct messages, cycled.
    pub inputs: usize,
    pub messages: u64,
    pub seconds: f64,
    pub per_second: f64,
    pub per_thread_per_second: f64,
    /// Each message's time through every destination's function (and its key functions), in
    /// milliseconds.
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
    /// The interface's key functions (`ordering.key.script`, `dedupe`), all of them per message on
    /// average, in milliseconds: they run while the sender waits for its ACK. None: it has none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_ms: Option<f64>,
    /// Per destination with code, in the config's order.
    pub stages: Vec<Stages>,
    /// What a message waited on lookups, every destination's, on average, in milliseconds.
    pub lookups_ms: f64,
    /// The CPU time a message took on its thread, in milliseconds (None where the thread's CPU
    /// time isn't read: neither Linux nor Windows).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_ms: Option<f64>,
    /// A message's own time on a busy thread, in milliseconds: everything but its lookups' waits,
    /// measured by a second short run with the mocks answering at once ([`Mocks::at_once`]). A
    /// thread that waits milliseconds for every lookup runs the code several times slower than one
    /// kept busy (its CPU slows and its caches go cold meanwhile: 30 µs against 4 µs to read the
    /// same JSON, measured in WSL 2026-11-27), so this, not `wall - lookups_ms`, is what more
    /// threads or requests side by side get. None: not measured (no mocked waits, or `--real`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub own_warm_ms: Option<f64>,
    /// What one thread's JavaScript heap holds after the warm-up, its garbage collected (the
    /// largest of the threads'), in MB: the code, its module-level state, the SDK.
    pub heap_mb: f64,
    /// Whether it has an ordering key, and its lanes (`partitions`; 1 without a key): what the
    /// threads can be.
    pub keyed: bool,
    pub lanes: usize,
    pub decisions: Vec<Decisions>,
    pub lookups: Vec<Lookup>,
}

/// The threads the engine gives `config`, and why: one worker in the shared pool, `workers` when
/// dedicated; and never more than its lanes (one without an ordering key: one message at a time).
pub fn threads_for(config: &InterfaceConfig) -> (usize, String) {
    if config.responds() {
        // One responder takes the requests; it answers them side by side while they wait.
        return (1, "respond mode: one thread answers its requests".into());
    }
    let (slots, lanes) = (config.slots() as usize, config.partition_count() as usize);
    match (config.isolation, lanes) {
        (_, 1) if slots > 1 => (1, format!("{slots} workers, but no ordering key: one message at a time")),
        (_, 1) => (1, "no ordering key: one message at a time".into()),
        (Isolation::Shared, _) => (1, "the shared pool: one worker per interface".into()),
        (Isolation::Dedicated, _) if slots > lanes => (lanes, format!("{slots} workers, but {lanes} lanes")),
        (Isolation::Dedicated, _) => (slots, format!("`isolation: dedicated`, `workers: {slots}`")),
    }
}

/// Run it. Blocks: the code runs on threads of its own, each with a runtime and an isolate.
pub fn run(options: Options) -> Result<Load> {
    let env = Arc::new(Environment::load(&options.root, &options.env)?);
    let config =
        crate::load_interfaces(&options.root, &env)?.into_iter().find(|c| c.name == options.interface).with_context(
            || format!("no interface named `{}` in {}", options.interface, options.root.join("interfaces").display()),
        )?;
    let inputs = match config.responds() {
        true => requests(&config, &options.files)?,
        false => inputs(&options.root, &options.env, &config, &options.files)?,
    };
    let (threads, threads_why) = match options.threads {
        Some(n) => (n.max(1), format!("`--threads {n}`")),
        None => threads_for(&config),
    };
    let heap_mb = ductus_core::settings::Settings::load(&options.root)?.isolates.heap_mb;
    // Real resources: the engine's services on a runtime of their own (its sockets are served
    // there, not on the threads running code), kept until every thread is done.
    let real = match options.real {
        false => None,
        true => Some(Real::open(&options.root, &env, &config)?),
    };
    let config = Arc::new(config);
    let inputs = Arc::new(inputs);
    let ready = Arc::new(Barrier::new(threads + 1));
    let go = Arc::new(Barrier::new(threads + 1));
    let left = Arc::new(AtomicU64::new(match options.until {
        Until::Count(n) => n,
        Until::Seconds(_) => u64::MAX,
    }));
    let timer = Timer::start()?;
    let workers: Vec<_> = (0..threads)
        .map(|t| {
            let job = Job {
                config: config.clone(),
                inputs: inputs.clone(),
                env: env.clone(),
                mocks: options.mocks.clone(),
                real: real.as_ref().map(Real::services),
                timer: timer.clone(),
                heap_mb,
                thread: t,
                until: options.until,
                left: left.clone(),
                stages: options.stages,
            };
            let (ready, go) = (ready.clone(), go.clone());
            std::thread::Builder::new()
                .name(format!("bench-{t}"))
                .spawn(move || job.run(&ready, &go))
                .context("starting a thread for the code")
        })
        .collect::<Result<_>>()?;
    ready.wait();
    let began = Instant::now();
    go.wait();
    let mut done = Vec::new();
    for worker in workers {
        done.push(worker.join().map_err(|_| anyhow!("a thread running the code panicked"))??);
    }
    let took = began.elapsed();
    drop(real);
    let run = Run { env: options.env, real: options.real, inputs: inputs.len(), threads, threads_why, took };
    Ok(summed(&config, run, &options.mocks, done))
}

/// The environment's real resources, as a start opens them: pools connect on first use.
struct Real {
    /// Where their sockets are served; shut down (and with it every connection) at the run's end.
    io: Option<tokio::runtime::Runtime>,
    shared: Arc<SharedServices>,
    /// The interface's own pools (`dedicated_pools`), shared by its threads as by its workers.
    own: crate::services::Pools,
}

impl Real {
    fn open(root: &std::path::Path, env: &Environment, config: &InterfaceConfig) -> Result<Self> {
        let resources = ductus_core::Resources::load(root, env)
            .with_context(|| format!("loading resources.yaml for `{}` (`--real` uses its resources)", env.name))?;
        let missing = resources.missing_references([config]);
        if !missing.is_empty() {
            bail!("{}", missing.join("\n"));
        }
        let certificates = crate::tls::Certificates::load(&resources.certificates)?;
        let io = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("bench-io")
            .enable_all()
            .build()
            .context("starting the runtime for real lookups")?;
        let shared = {
            let _guard = io.enter();
            // The services keep an environment of their own (a deploy can swap it).
            Arc::new(SharedServices::new(Environment::load(root, &env.name)?, &resources, &certificates)?)
        };
        let own = {
            let _guard = io.enter();
            shared.dedicated_pools(config)
        };
        Ok(Self { io: Some(io), shared, own })
    }

    fn services(&self) -> EngineServices {
        EngineServices::new(self.shared.clone(), self.own.clone())
    }
}

impl Drop for Real {
    fn drop(&mut self) {
        // Pools close in the background; don't wait for their sockets.
        if let Some(io) = self.io.take() {
            io.shutdown_background();
        }
    }
}

/// A respond-mode interface's requests, as its HTTP source hands them to the code: each file (or
/// fixture) is a request's body, sent as the source's method to its path (no query, headers or
/// path parameters); a file named `*.request.json` is a whole request instead, `{"method",
/// "path", "params", "query", "headers", "body"}`, each part it leaves out as above (a `body`
/// that isn't a string is sent as its JSON).
fn requests(config: &InterfaceConfig, files: &[PathBuf]) -> Result<Vec<Input>> {
    let Source::Http { method, path, .. } = &config.source else { bail!("`{}` isn't an HTTP interface", config.name) };
    let files = files_or_fixtures(config, files)?;
    let mut out = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
        let name = file_name(file);
        let mut request = serde_json::json!({
            "method": method.to_ascii_uppercase(), "path": path, "params": {}, "query": {}, "headers": {}, "body": "",
        });
        if name.ends_with(".request.json") {
            let given: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&text)
                .with_context(|| format!("{}: a `.request.json` file holds a JSON object", file.display()))?;
            for (key, value) in given {
                let value = match (key.as_str(), value) {
                    ("method" | "path" | "params" | "query" | "headers", value) => value,
                    ("body", serde_json::Value::String(body)) => serde_json::Value::String(body),
                    ("body", json) => serde_json::Value::String(json.to_string()),
                    (other, _) => bail!(
                        "{}: `{other}` isn't part of a request (method, path, params, query, headers, body)",
                        file.display()
                    ),
                };
                request[key] = value;
            }
            // As the server hands them over: header names in lower case.
            if let Some(headers) = request["headers"].as_object_mut() {
                *headers = std::mem::take(headers).into_iter().map(|(k, v)| (k.to_ascii_lowercase(), v)).collect();
            }
        } else {
            request["body"] = serde_json::Value::String(text);
        }
        out.push(Input { name, payload: request.to_string() });
    }
    if out.is_empty() {
        bail!("no requests to run: give one with `--file` (a body, or a `.request.json`)");
    }
    Ok(out)
}

fn file_name(path: &std::path::Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())
}

/// The files given, else the interface's `fixtures/`, sorted.
fn files_or_fixtures(config: &InterfaceConfig, files: &[PathBuf]) -> Result<Vec<PathBuf>> {
    if !files.is_empty() {
        return Ok(files.to_vec());
    }
    let dir = config.dir.join("fixtures");
    let mut found: Vec<PathBuf> = std::fs::read_dir(&dir)
        .with_context(|| {
            format!(
                "`{}` has no fixtures to run ({} isn't there): give a message with `--file`",
                config.name,
                dir.display()
            )
        })?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file())
        .collect();
    found.sort();
    Ok(found)
}

/// The messages: each file (or fixture) as the interface's source hands it over.
fn inputs(root: &std::path::Path, env: &str, config: &InterfaceConfig, files: &[PathBuf]) -> Result<Vec<Input>> {
    let files = files_or_fixtures(config, files)?;
    let splitter = match &config.source {
        Source::File(f) | Source::Sftp(f) if f.split == Split::Records => Some(match &config.format {
            Format::Hl7 => Splitter::Hl7,
            Format::Csv(options) => Splitter::Csv { header: options.header },
            _ => Splitter::Lines,
        }),
        _ => None,
    };
    let runtime = tokio::runtime::Builder::new_current_thread().build()?;
    let mut out = Vec::new();
    for path in &files {
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let bytes = crate::try_message::as_picked_up(root, env, &config.name, bytes)?;
        let name = file_name(path);
        let Some(splitter) = splitter else {
            out.push(Input { name, payload: String::from_utf8_lossy(&bytes).into_owned() });
            continue;
        };
        runtime.block_on(async {
            let mut records = Records::open(std::io::Cursor::new(bytes), splitter, 0).await?;
            let mut n = 0;
            while let Some(record) = records.next().await? {
                n += 1;
                out.push(Input {
                    name: format!("{name} record {n}"),
                    payload: String::from_utf8_lossy(&record.payload).into_owned(),
                });
            }
            anyhow::Ok(())
        })?;
    }
    if out.is_empty() {
        bail!("no messages to run: the files given (or `{}`'s fixtures) hold none", config.name);
    }
    Ok(out)
}

/// One thread's share.
struct Job {
    config: Arc<InterfaceConfig>,
    inputs: Arc<Vec<Input>>,
    env: Arc<Environment>,
    mocks: Mocks,
    /// `--real`: the engine's services, which answer instead of the mocks.
    real: Option<EngineServices>,
    timer: Timer,
    heap_mb: usize,
    thread: usize,
    until: Until,
    /// Messages still to run (`Until::Count`), shared by every thread.
    left: Arc<AtomicU64>,
    stages: bool,
}

/// What one thread counted, message by message.
struct Counts {
    decisions: Vec<Decisions>,
    /// Per destination, as `decisions`.
    stages: Vec<StageSum>,
    keys: Duration,
}

/// What one thread measured.
struct Done {
    latencies: Vec<Duration>,
    counts: Counts,
    calls: Tallies,
    /// Lookups in flight, each moment once.
    waited: Duration,
    cpu: Option<Duration>,
    heap: usize,
}

impl Job {
    /// Load, warm up, wait for every other thread, run; each thread reaches both barriers exactly
    /// once, also when it failed to load.
    fn run(self, ready: &Barrier, go: &Barrier) -> Result<Done> {
        let loaded = self.load();
        ready.wait();
        go.wait();
        let (runtime, mut loaded) = loaded?;
        tokio::task::LocalSet::new().block_on(&runtime, self.measure(&mut loaded))
    }

    fn load(&self) -> Result<(tokio::runtime::Runtime, Loaded)> {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        let calls = Rc::new(RefCell::new(BTreeMap::new()));
        let waiting = Rc::new(RefCell::new(Waiting::default()));
        let recorder = Recorder { calls: calls.clone(), waiting: waiting.clone() };
        let services: Rc<dyn Services> = match &self.real {
            Some(inner) => Rc::new(RealServices { inner: inner.clone(), recorder }),
            None => Rc::new(MockServices {
                env: self.env.clone(),
                mocks: self.mocks.clone(),
                timer: self.timer.clone(),
                recorder,
            }),
        };
        let mut isolate = {
            let _guard = runtime.enter();
            Isolate::new(Some(services), self.heap_mb)
        };
        // `console` goes nowhere: a line per message would measure the terminal.
        isolate.capture_console();
        if self.stages {
            isolate.time_stages();
        }
        let functions = self.config.functions();
        let keys = self.config.key_functions();
        let mut loaded = Loaded { isolate, functions: Vec::new(), keys: Vec::new(), calls, waiting, seq: 0, heap: 0 };
        let local = tokio::task::LocalSet::new();
        local.block_on(&runtime, async {
            if functions.iter().any(Option::is_some) {
                let (functions, timeout) = (Function::all(&functions), self.config.timeout);
                let added = match self.stages {
                    true => loaded.isolate.add_functions_timed(&functions, timeout).await,
                    false => loaded.isolate.add_functions(&functions, timeout).await,
                };
                loaded.functions = added.context("loading the interface's code")?.1;
            } else {
                loaded.functions = vec![None; functions.len()];
            }
            if !keys.is_empty() {
                let keys: Vec<Function> = keys.iter().map(Function::of).collect();
                loaded.keys = loaded.isolate.add_keys(&keys).await.context("loading its key functions")?.1;
            }
            // The heap is read three quarters in (collecting it can throw compiled code away),
            // then the code runs warm again.
            let mut counts = self.counts();
            for (part, read_heap) in [(WARM_UP * 3 / 4, true), (WARM_UP / 4, false)] {
                let until = Instant::now() + part;
                while Instant::now() < until {
                    self.one(&mut loaded, &mut counts).await?;
                }
                if read_heap {
                    loaded.heap = loaded.isolate.heap_used_collected();
                }
            }
            anyhow::Ok(())
        })?;
        loaded.calls.borrow_mut().clear();
        loaded.waiting.borrow_mut().total = Duration::ZERO;
        let _ = loaded.isolate.take_console();
        let _ = loaded.isolate.take_stamps();
        Ok((runtime, loaded))
    }

    fn counts(&self) -> Counts {
        let destinations = outlets(&self.config);
        Counts {
            decisions: destinations
                .iter()
                .map(|d| Decisions { destination: d.to_string(), ..Default::default() })
                .collect(),
            stages: vec![StageSum::default(); destinations.len()],
            keys: Duration::ZERO,
        }
    }

    async fn measure(&self, loaded: &mut Loaded) -> Result<Done> {
        let mut counts = self.counts();
        let mut latencies = Vec::new();
        let deadline = match self.until {
            Until::Seconds(s) => Some(Instant::now() + s),
            Until::Count(_) => None,
        };
        let cpu = thread_cpu();
        loop {
            match deadline {
                Some(at) if Instant::now() >= at => break,
                Some(_) => {}
                None => {
                    if self.left.try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1)).is_err() {
                        break;
                    }
                }
            }
            let began = Instant::now();
            self.one(loaded, &mut counts).await?;
            latencies.push(began.elapsed());
            if latencies.len() % 256 == 0 {
                let _ = loaded.isolate.take_console();
            }
        }
        let cpu = cpu.zip(thread_cpu()).map(|(began, now)| now.saturating_sub(began));
        let calls = std::mem::take(&mut *loaded.calls.borrow_mut());
        let waited = loaded.waiting.borrow().total;
        Ok(Done { latencies, counts, calls, waited, cpu, heap: loaded.heap })
    }

    /// The next message through its key functions and every destination's function, as a worker
    /// calls them, each decision counted and each stage timed.
    async fn one(&self, loaded: &mut Loaded, counts: &mut Counts) -> Result<()> {
        let n = loaded.seq;
        loaded.seq += 1;
        let input = &self.inputs[(n as usize + self.thread) % self.inputs.len()];
        for &key in &loaded.keys {
            let began = Instant::now();
            if let Err(e) = loaded.isolate.call_key(key, &input.payload) {
                bail!("{}: its key function failed: {e:#}", input.name);
            }
            counts.keys += began.elapsed();
        }
        // Ids of their own per thread, as stored messages have.
        let id = (self.thread as i64) << 40 | n as i64;
        let message = MessageRef { id, received_at: chrono::Utc::now() };
        let respond = self.config.responds();
        for (i, destination) in outlets(&self.config).iter().enumerate() {
            let result = match loaded.functions[i] {
                None => None,
                Some(function) => {
                    // A responder's transform gets the request and no `meta`.
                    let meta = (!respond).then(|| worker::meta(message, destination, None));
                    let waited = loaded.waiting.borrow().total;
                    let result = loaded.isolate.call_function(function, &input.payload, meta.as_deref()).await;
                    let waited = loaded.waiting.borrow().total - waited;
                    // A function that threw (quarantined, retried) has no serialize stamp.
                    let times = match loaded.isolate.take_stamps()[..] {
                        [(0, a), (1, b), (2, c), (3, d)] => Some((a, b, c, d)),
                        [(0, a), (1, b), (2, c)] => Some((a, b, c, c)),
                        _ => None,
                    };
                    if let Some((start, read, answered, written)) = times {
                        counts.stages[i].add(StageSum {
                            decode: read - start,
                            code: (answered - read).saturating_sub(waited),
                            lookups: waited,
                            serialize: written - answered,
                            timed: 1,
                        });
                    }
                    Some(result)
                }
            };
            let counted = &mut counts.decisions[i];
            if respond {
                answered(counted, &input.name, result)?;
                continue;
            }
            match Outcome::of(&input.payload, result, self.config.project_root()) {
                Outcome::Deliver(_) => counted.sent += 1,
                Outcome::Filter(_) => counted.filtered += 1,
                Outcome::Quarantine(why) => {
                    counted.quarantined += 1;
                    counted.first_reason.get_or_insert_with(|| format!("{}: {why}", input.name));
                }
                Outcome::Retry(why) => {
                    counted.retried += 1;
                    counted.first_reason.get_or_insert_with(|| format!("{}: {why}", input.name));
                }
                // The engine would stop it and restart the isolate: no number would mean anything.
                Outcome::Runaway(runaway) => bail!("{}: `{destination}`'s code {runaway}", input.name),
            }
        }
        Ok(())
    }
}

/// What decisions are counted under: its destinations, or [`RESPONSE`] in respond mode (in the
/// order of `functions()`).
fn outlets(config: &InterfaceConfig) -> Vec<String> {
    match config.responds() {
        true => vec![RESPONSE.to_string()],
        false => config.destinations().map(|(d, _)| d.to_string()).collect(),
    }
}

/// A respond-mode answer counted by its HTTP status, as the engine would answer the caller: the
/// response the code made, or for a thrown error 422, 503 or 500 (`http::status_of_error`).
fn answered(counted: &mut Decisions, input: &str, result: Option<Result<Option<String>>>) -> Result<()> {
    let Some(result) = result else { bail!("respond mode without a `script:`: there is no code to run") };
    let status = match result {
        Ok(Some(envelope)) => {
            serde_json::from_str::<serde_json::Value>(&envelope).ok().and_then(|e| e["status"].as_u64()).unwrap_or(500)
                as u16
        }
        // The engine answers a response it can't read with 500.
        Ok(None) => 500,
        Err(e) => {
            if let Some(runaway) = e.downcast_ref::<Runaway>() {
                bail!("{input}: the code {runaway}");
            }
            let error = format!("{e:#}");
            counted.first_reason.get_or_insert_with(|| format!("{input}: {}", error.lines().next().unwrap_or("")));
            crate::http::status_of_error(&error).as_u16()
        }
    };
    counted.sent += 1;
    *counted.statuses.entry(status).or_default() += 1;
    Ok(())
}

struct Loaded {
    isolate: Isolate,
    functions: Vec<Option<FunctionId>>,
    keys: Vec<FunctionId>,
    calls: Rc<RefCell<Tallies>>,
    waiting: Rc<RefCell<Waiting>>,
    /// Messages run on this thread so far: picks the next input and makes its id.
    seq: u64,
    /// V8's heap after the warm-up, collected, in bytes.
    heap: usize,
}

/// This thread's CPU time so far (None where it isn't read: neither Linux nor Windows). Windows
/// counts it in scheduler ticks (~15.6 ms), so a second's run reads within a few percent.
pub fn thread_cpu() -> Option<Duration> {
    #[cfg(target_os = "linux")]
    {
        let mut time = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        // SAFETY: clock_gettime writes the timespec it's given and nothing else.
        let read = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut time) } == 0;
        read.then(|| Duration::new(time.tv_sec as u64, time.tv_nsec as u32))
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::FILETIME;
        use windows_sys::Win32::System::Threading::{GetCurrentThread, GetThreadTimes};
        let zero = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
        let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
        // SAFETY: the current thread's pseudo-handle, and four FILETIMEs for it to write.
        let read =
            unsafe { GetThreadTimes(GetCurrentThread(), &mut created, &mut exited, &mut kernel, &mut user) } != 0;
        // 100 ns units.
        let units = |t: FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
        read.then(|| Duration::from_nanos((units(kernel) + units(user)) * 100))
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        None
    }
}

/// What the run was, for its report.
struct Run {
    env: String,
    real: bool,
    inputs: usize,
    threads: usize,
    threads_why: String,
    took: Duration,
}

/// Every thread's numbers as one.
fn summed(config: &InterfaceConfig, run: Run, mocks: &Mocks, done: Vec<Done>) -> Load {
    let Run { env, real, inputs, threads, threads_why, took } = run;
    let mut latencies: Vec<Duration> = Vec::new();
    let mut decisions: Vec<Decisions> =
        outlets(config).into_iter().map(|destination| Decisions { destination, ..Default::default() }).collect();
    let mut stages = vec![StageSum::default(); decisions.len()];
    let mut calls = Tallies::new();
    let (mut keys, mut waited, mut heap) = (Duration::ZERO, Duration::ZERO, 0);
    let mut cpu = Some(Duration::ZERO);
    for d in done {
        latencies.extend(d.latencies);
        keys += d.counts.keys;
        waited += d.waited;
        heap = heap.max(d.heap);
        cpu = cpu.zip(d.cpu).map(|(a, b)| a + b);
        for (sum, one) in stages.iter_mut().zip(d.counts.stages) {
            sum.add(one);
        }
        for (sum, one) in decisions.iter_mut().zip(d.counts.decisions) {
            sum.sent += one.sent;
            sum.filtered += one.filtered;
            sum.quarantined += one.quarantined;
            sum.retried += one.retried;
            for (status, n) in one.statuses {
                *sum.statuses.entry(status).or_default() += n;
            }
            if sum.first_reason.is_none() {
                sum.first_reason = one.first_reason;
            }
        }
        for (k, one) in d.calls {
            let sum = calls.entry(k).or_default();
            sum.calls += one.calls;
            sum.writes += one.writes;
            sum.failed += one.failed;
            sum.waited += one.waited;
        }
    }
    latencies.sort_unstable();
    let ms = |q: f64| match latencies.len() {
        0 => 0.0,
        n => latencies[((n - 1) as f64 * q).round() as usize].as_secs_f64() * 1000.0,
    };
    let messages = latencies.len() as u64;
    let per_second = messages as f64 / took.as_secs_f64().max(1e-9);
    let per_message = |d: Duration| d.as_secs_f64() * 1000.0 / messages.max(1) as f64;
    let functions = config.functions();
    let stages = outlets(config)
        .into_iter()
        .zip(stages)
        .zip(&functions)
        .filter(|(_, function)| function.is_some())
        .map(|((destination, sum), _)| {
            let mean = |d: Duration| d.as_secs_f64() * 1000.0 / sum.timed.max(1) as f64;
            Stages {
                destination,
                decode_ms: mean(sum.decode),
                code_ms: mean(sum.code),
                lookups_ms: mean(sum.lookups),
                serialize_ms: mean(sum.serialize),
                timed: sum.timed,
            }
        })
        .collect();
    let lookups = calls
        .into_iter()
        .map(|((kind, name), tally)| Lookup {
            kind,
            latency_ms: (!real).then(|| mocks.latency(kind, &name).as_secs_f64() * 1000.0),
            waited_ms: tally.waited.as_secs_f64() * 1000.0 / tally.calls.max(1) as f64,
            failed: tally.failed,
            given_rows: !real && mocks.rows.contains_key(&name),
            name,
            calls: tally.calls,
            writes: tally.writes,
        })
        .collect();
    Load {
        interface: config.name.clone(),
        env,
        real,
        respond: config.responds(),
        threads,
        threads_why,
        inputs,
        messages,
        seconds: took.as_secs_f64(),
        per_second,
        per_thread_per_second: per_second / threads as f64,
        p50_ms: ms(0.50),
        p95_ms: ms(0.95),
        p99_ms: ms(0.99),
        max_ms: ms(1.0),
        key_ms: (!config.key_functions().is_empty()).then(|| per_message(keys)),
        stages,
        lookups_ms: per_message(waited),
        cpu_ms: cpu.map(per_message),
        own_warm_ms: None,
        heap_mb: heap as f64 / (1024.0 * 1024.0),
        keyed: config.ordering.key.is_some(),
        lanes: config.partition_count() as usize,
        decisions,
        lookups,
    }
}

impl Mocks {
    /// The same answers with no latency: for the code's own time on a thread kept busy
    /// ([`Load::own_warm_ms`]).
    pub fn at_once(&self) -> Mocks {
        Mocks {
            db: Duration::ZERO,
            api: Duration::ZERO,
            fetch: Duration::ZERO,
            named: self.named.keys().map(|n| (n.clone(), Duration::ZERO)).collect(),
            rows: self.rows.clone(),
        }
    }

    fn latency(&self, kind: &str, name: &str) -> Duration {
        self.named.get(name).copied().unwrap_or(match kind {
            "db" => self.db,
            "api" => self.api,
            _ => self.fetch,
        })
    }
}

/// Wakes mocked calls at their deadline, spending no CPU while they wait: 0.05 ms late on a busy
/// thread, ~0.3 ms after a 5 ms idle (the CPU waking; measured in WSL, 2026-11-27), as a real
/// lookup's answer wakes the thread. tokio's own timer counts whole milliseconds and rounds up: a
/// 1 ms mock waited 2, and a message with two lookups was milliseconds slower than its code. One
/// thread for the run, shared by every thread running code; it ends when the last handle goes.
#[derive(Clone)]
struct Timer(Arc<TimerShared>);

struct TimerShared {
    due: std::sync::Mutex<TimerState>,
    wakeup: Wakeup,
}

/// How the timer's thread sleeps until the next deadline or a new one. A condvar, except on
/// Windows: there a condvar's (and `thread::sleep`'s) timeout ends on the scheduler's tick, ~15.6
/// ms, so a 1 ms mock waited 14 (CI, 2026-11-27); a high-resolution waitable timer and an event
/// for a new deadline end it on time, and nothing polls.
struct Wakeup {
    #[cfg(not(windows))]
    changed: std::sync::Condvar,
    #[cfg(windows)]
    win: windows_wakeup::Handles,
}

impl Wakeup {
    fn new() -> Result<Self> {
        Ok(Self {
            #[cfg(not(windows))]
            changed: std::sync::Condvar::new(),
            #[cfg(windows)]
            win: windows_wakeup::Handles::new()?,
        })
    }

    /// A deadline was added, or the run is over: the sleeper looks again.
    fn notify(&self) {
        #[cfg(not(windows))]
        self.changed.notify_one();
        #[cfg(windows)]
        self.win.notify();
    }

    /// Give up `state` and sleep for at most `for_`, or until [`Wakeup::notify`].
    fn sleep(&self, state: std::sync::MutexGuard<'_, TimerState>, for_: Duration) {
        #[cfg(not(windows))]
        {
            let _unused = self.changed.wait_timeout(state, for_).unwrap_or_else(|e| e.into_inner());
        }
        #[cfg(windows)]
        {
            // The event is auto-reset and stays set: a deadline added between here and the wait
            // ends the wait at once, so none is missed.
            drop(state);
            self.win.sleep(for_);
        }
    }
}

#[cfg(windows)]
mod windows_wakeup {
    use anyhow::{Result, bail};
    use std::time::Duration;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::Threading::{
        CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CreateEventW, CreateWaitableTimerExW, INFINITE, SetEvent,
        SetWaitableTimer, TIMER_ALL_ACCESS, WaitForMultipleObjects,
    };

    pub struct Handles {
        timer: HANDLE,
        event: HANDLE,
    }

    // SAFETY: both are kernel object handles, usable from any thread; closed once, in drop.
    unsafe impl Send for Handles {}
    unsafe impl Sync for Handles {}

    impl Handles {
        pub fn new() -> Result<Self> {
            use std::ptr::null;
            // SAFETY: no attributes, no name; the flags and access are the documented ones.
            let mut timer = unsafe {
                CreateWaitableTimerExW(null(), null(), CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, TIMER_ALL_ACCESS)
            };
            if timer.is_null() {
                // Before Windows 10 1803 there's no high-resolution timer: the tick's, then.
                // SAFETY: as above, without the flag.
                timer = unsafe { CreateWaitableTimerExW(null(), null(), 0, TIMER_ALL_ACCESS) };
            }
            // SAFETY: an unnamed auto-reset event, not set.
            let event = unsafe { CreateEventW(null(), 0, 0, null()) };
            if timer.is_null() || event.is_null() {
                bail!("can't make the mocks' timer: {}", std::io::Error::last_os_error());
            }
            Ok(Self { timer, event })
        }

        pub fn notify(&self) {
            // SAFETY: our own event handle, open until drop.
            unsafe { SetEvent(self.event) };
        }

        pub fn sleep(&self, for_: Duration) {
            // Relative due times are negative, in 100 ns units.
            let due = -((for_.as_nanos() / 100).clamp(1, i64::MAX as u128) as i64);
            let handles = [self.timer, self.event];
            // SAFETY: our own handles; no completion routine; the array outlives the wait.
            unsafe {
                SetWaitableTimer(self.timer, &due, 0, None, std::ptr::null(), 0);
                WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE);
            }
        }
    }

    impl Drop for Handles {
        fn drop(&mut self) {
            // SAFETY: opened in `new`, closed only here.
            unsafe {
                CloseHandle(self.timer);
                CloseHandle(self.event);
            }
        }
    }
}

#[derive(Default)]
struct TimerState {
    /// By deadline, then by when it was asked (`seq`), so two at the same instant both fire.
    due: BTreeMap<(Instant, u64), tokio::sync::oneshot::Sender<()>>,
    seq: u64,
    stop: bool,
}

impl Timer {
    fn start() -> Result<Self> {
        let shared = Arc::new(TimerShared { due: Default::default(), wakeup: Wakeup::new()? });
        let weak = Arc::downgrade(&shared);
        std::thread::Builder::new()
            .name("bench-timer".into())
            .spawn(move || Self::fire(weak))
            .context("starting the mocks' timer")?;
        Ok(Self(shared))
    }

    /// Done at `at` (at once when it's past).
    async fn wait(&self, at: Instant) {
        let (tell, told) = tokio::sync::oneshot::channel();
        {
            let mut state = self.0.due.lock().unwrap_or_else(|e| e.into_inner());
            state.seq += 1;
            let seq = state.seq;
            state.due.insert((at, seq), tell);
        }
        self.0.wakeup.notify();
        let _ = told.await;
    }

    /// The timer's thread: sends each deadline's signal when it comes, sleeping until the next.
    fn fire(shared: std::sync::Weak<TimerShared>) {
        loop {
            let Some(shared) = shared.upgrade() else { return };
            let mut state = shared.due.lock().unwrap_or_else(|e| e.into_inner());
            if state.stop {
                return;
            }
            let now = Instant::now();
            while let Some(entry) = state.due.first_entry() {
                if entry.key().0 > now {
                    break;
                }
                let _ = entry.remove().send(());
            }
            // Wake at the next deadline, or within 100 ms to see whether the run is over.
            let next = state.due.keys().next().map_or(Duration::from_millis(100), |&(at, _)| at - now);
            shared.wakeup.sleep(state, next);
        }
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        // The run's own handle (the last but the threads', which are gone by then) stops it.
        if Arc::strong_count(&self.0) == 1 {
            self.0.due.lock().unwrap_or_else(|e| e.into_inner()).stop = true;
            self.0.wakeup.notify();
        }
    }
}

/// Tallies each lookup on one thread, mocked or real: its call, whether it wrote, what it waited.
struct Recorder {
    calls: Rc<RefCell<Tallies>>,
    waiting: Rc<RefCell<Waiting>>,
}

impl Recorder {
    fn time<T: 'static>(&self, kind: &'static str, name: &str, write: bool, call: BoxFuture<T>) -> BoxFuture<T> {
        let key = (kind, name.to_string());
        let (calls, waiting) = (self.calls.clone(), self.waiting.clone());
        Box::pin(async move {
            let began = Instant::now();
            waiting.borrow_mut().begin(began);
            let result = call.await;
            waiting.borrow_mut().end(Instant::now());
            let mut calls = calls.borrow_mut();
            let tally = calls.entry(key).or_default();
            tally.calls += 1;
            tally.writes += u64::from(write);
            tally.failed += u64::from(result.is_err());
            tally.waited += began.elapsed();
            result
        })
    }
}

/// A request's kind and name as the report counts it: `api` and its resources.yaml name, or
/// `fetch` for a URL; and whether it writes (anything but GET and HEAD).
fn fetched(request: &FetchRequest) -> (&'static str, String, bool) {
    let write = !matches!(request.method.to_ascii_uppercase().as_str(), "GET" | "HEAD");
    match &request.api {
        Some(api) => ("api", api.clone(), write),
        None => ("fetch", "fetch".to_string(), write),
    }
}

/// The engine's services, answered here: see the module's comment.
struct MockServices {
    env: Arc<Environment>,
    mocks: Mocks,
    timer: Timer,
    recorder: Recorder,
}

impl MockServices {
    /// `value` after the resource's latency, the call and its wait tallied.
    fn answer<T: 'static>(&self, kind: &'static str, name: &str, write: bool, value: T) -> BoxFuture<T> {
        let (timer, at) = (self.timer.clone(), Instant::now() + self.mocks.latency(kind, name));
        self.recorder.time(
            kind,
            name,
            write,
            Box::pin(async move {
                timer.wait(at).await;
                Ok(value)
            }),
        )
    }
}

/// The engine's own services (`--real`), each call tallied as the mocks' are.
struct RealServices {
    inner: EngineServices,
    recorder: Recorder,
}

impl Services for RealServices {
    fn db_query(&self, resource: &str, sql: &str, params: Vec<serde_json::Value>) -> BoxFuture<Vec<serde_json::Value>> {
        self.recorder.time("db", resource, false, self.inner.db_query(resource, sql, params))
    }

    fn db_execute(&self, resource: &str, sql: &str, params: Vec<serde_json::Value>) -> BoxFuture<u64> {
        self.recorder.time("db", resource, true, self.inner.db_execute(resource, sql, params))
    }

    fn fetch(&self, request: FetchRequest) -> BoxFuture<FetchResponse> {
        let (kind, name, write) = fetched(&request);
        self.recorder.time(kind, &name, write, self.inner.fetch(request))
    }

    fn variable(&self, name: &str) -> Option<String> {
        self.inner.variable(name)
    }

    fn secret(&self, name: &str) -> Result<Option<String>> {
        self.inner.secret(name)
    }
}

impl Services for MockServices {
    fn db_query(
        &self,
        resource: &str,
        _sql: &str,
        _params: Vec<serde_json::Value>,
    ) -> BoxFuture<Vec<serde_json::Value>> {
        let rows = match self.mocks.rows.get(resource) {
            Some(serde_json::Value::Array(rows)) => rows.clone(),
            Some(row) => vec![row.clone()],
            None => Vec::new(),
        };
        self.answer("db", resource, false, rows)
    }

    fn db_execute(&self, resource: &str, _sql: &str, _params: Vec<serde_json::Value>) -> BoxFuture<u64> {
        self.answer("db", resource, true, 1)
    }

    fn fetch(&self, request: FetchRequest) -> BoxFuture<FetchResponse> {
        let (kind, name, write) = fetched(&request);
        let body = self.mocks.rows.get(&name).map_or_else(
            || "{}".to_string(),
            |b| match b {
                serde_json::Value::String(text) => text.clone(),
                json => json.to_string(),
            },
        );
        let headers = BTreeMap::from([("content-type".to_string(), "application/json".to_string())]);
        self.answer(kind, &name, write, FetchResponse { status: 200, headers, body })
    }

    fn variable(&self, name: &str) -> Option<String> {
        self.env.variable(name)
    }

    fn secret(&self, name: &str) -> Result<Option<String>> {
        self.env.secret(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(yaml: &str) -> InterfaceConfig {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("interface.yaml"), yaml).unwrap();
        InterfaceConfig::load(dir.path()).unwrap()
    }

    #[test]
    fn threads_are_what_the_engine_gives_and_never_more_than_the_lanes() {
        let base = "name: t\nsource: { mllp: { port: 6661 } }\ndestinations: { out: { file: { dir: out } } }\n";
        let cases = [
            ("", 1, "no ordering key: one message at a time"),
            ("isolation: dedicated\nworkers: 8\n", 1, "8 workers, but no ordering key: one message at a time"),
            ("ordering: { key: PID-3.1 }\n", 1, "the shared pool: one worker per interface"),
            ("ordering: { key: PID-3.1 }\nisolation: dedicated\n", 4, "`isolation: dedicated`, `workers: 4`"),
            (
                "ordering: { key: PID-3.1, partitions: 2 }\nisolation: dedicated\nworkers: 8\n",
                2,
                "8 workers, but 2 lanes",
            ),
        ];
        for (extra, threads, why) in cases {
            assert_eq!(threads_for(&config(&format!("{base}{extra}"))), (threads, why.to_string()), "{extra}");
        }
    }

    /// The mocks' latency is what was asked to within a fraction of a millisecond, on several
    /// waits at once (tokio's timer alone made 1 ms into 2).
    #[test]
    fn the_timer_wakes_each_wait_at_its_deadline() {
        let timer = Timer::start().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let waits: Vec<Duration> = runtime.block_on(async {
            let one = |ms: u64| {
                let timer = timer.clone();
                async move {
                    let began = Instant::now();
                    timer.wait(began + Duration::from_millis(ms)).await;
                    began.elapsed() - Duration::from_millis(ms)
                }
            };
            let mut late = Vec::new();
            for _ in 0..20 {
                let (a, b, c) = tokio::join!(one(1), one(1), one(5));
                late.extend([a, b, c]);
            }
            late
        });
        let mut late = waits;
        late.sort();
        // The median late by under half a millisecond (typically 0.05–0.1 ms).
        assert!(late[late.len() / 2] < Duration::from_micros(500), "{late:?}");
    }

    /// Lookups in flight together count once: two 5 ms waits side by side, then one alone, are
    /// 10 ms of waiting, not 15.
    #[test]
    fn waits_side_by_side_count_once() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut waiting = Waiting::default();
        waiting.begin(at(0));
        waiting.begin(at(1));
        waiting.end(at(5));
        waiting.end(at(6));
        waiting.begin(at(10));
        waiting.end(at(14));
        assert_eq!(waiting.total, Duration::from_millis(10));
        assert_eq!(waiting.in_flight, 0);
    }

    #[test]
    fn a_thread_s_cpu_time_counts_its_work_not_its_sleep() {
        let Some(began) = thread_cpu() else { return };
        std::thread::sleep(Duration::from_millis(50));
        let slept = thread_cpu().unwrap() - began;
        let until = Instant::now() + Duration::from_millis(50);
        let mut x = 0u64;
        while Instant::now() < until {
            x = std::hint::black_box(x.wrapping_add(1));
        }
        let worked = thread_cpu().unwrap() - began - slept;
        assert!(slept < Duration::from_millis(10), "{slept:?}");
        // Windows counts in ~15.6 ms ticks.
        assert!(worked > Duration::from_millis(30), "{worked:?}");
    }

    /// What the stamps cost (0.8.2.2's check: under 2% of a transform): the guides' ORU to FHIR
    /// transform on one thread, timed and not, alternated. `cargo test -p ductus-runtime --release
    /// --lib stage_timing -- --ignored --nocapture`.
    #[test]
    #[ignore = "a measurement: run it by hand"]
    fn stage_timing_costs_little() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/guides");
        let measure = |stages| {
            let options = Options {
                root: root.clone(),
                env: "dev".into(),
                interface: "results-to-fhir".into(),
                files: Vec::new(),
                threads: Some(1),
                until: Until::Seconds(Duration::from_secs(3)),
                mocks: Mocks::default(),
                stages,
                real: false,
            };
            run(options).unwrap().per_second
        };
        let (mut timed, mut plain) = (Vec::new(), Vec::new());
        for _ in 0..5 {
            plain.push(measure(false));
            timed.push(measure(true));
        }
        let median = |v: &mut Vec<f64>| {
            v.sort_by(f64::total_cmp);
            v[v.len() / 2]
        };
        let (t, p) = (median(&mut timed), median(&mut plain));
        let cost = (p - t) / p * 100.0;
        println!("plain {p:.0} msg/s, timed {t:.0} msg/s: the stamps cost {cost:.2}% (runs: {plain:?} / {timed:?})");
        assert!(cost < 2.0, "{cost:.2}%");
    }

    #[test]
    fn production_is_refused_by_its_usual_names() {
        for env in ["prod", "PROD", "production", "prd", "live", "prod-east", "Prod_2"] {
            assert!(production_refused(env).is_some_and(|why| why.contains("never run against production")), "{env}");
        }
        for env in ["dev", "test", "uat", "staging", "product", "prodtest", "preprod", "delivery"] {
            assert_eq!(production_refused(env), None, "{env}");
        }
    }

    #[test]
    fn a_respond_mode_request_is_the_body_or_a_whole_request() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("interface.yaml"),
            "name: lookup\nsource: { http: { port: 7001, path: \"/patients/{mrn}\", method: GET, mode: respond } }\nformat: json\nscript: t.ts\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("t.ts"), "export function transform() { return \"\"; }\n").unwrap();
        let config = InterfaceConfig::load(dir.path()).unwrap();
        let body = dir.path().join("a.json");
        std::fs::write(&body, r#"{"x":1}"#).unwrap();
        let whole = dir.path().join("b.request.json");
        std::fs::write(
            &whole,
            r#"{"params": {"mrn": "123"}, "headers": {"X-Trace": "t"}, "body": {"y": 2}, "query": {"full": "1"}}"#,
        )
        .unwrap();
        let got: Vec<serde_json::Value> = requests(&config, &[body, whole.clone()])
            .unwrap()
            .into_iter()
            .map(|i| serde_json::from_str(&i.payload).unwrap())
            .collect();
        assert_eq!(
            got[0],
            serde_json::json!({"method": "GET", "path": "/patients/{mrn}", "params": {}, "query": {}, "headers": {}, "body": "{\"x\":1}"})
        );
        assert_eq!(
            got[1],
            serde_json::json!({"method": "GET", "path": "/patients/{mrn}", "params": {"mrn": "123"}, "query": {"full": "1"}, "headers": {"x-trace": "t"}, "body": "{\"y\":2}"})
        );
        std::fs::write(&whole, r#"{"verb": "GET"}"#).unwrap();
        let refused = requests(&config, &[whole]).err().unwrap().to_string();
        assert!(refused.contains("`verb` isn't part of a request"), "{refused}");
    }

    #[test]
    fn a_respond_mode_answer_counts_under_its_status() {
        let mut counted = Decisions::default();
        answered(&mut counted, "a", Some(Ok(Some(r#"{"status":404,"headers":{},"body":""}"#.into())))).unwrap();
        answered(&mut counted, "b", Some(Ok(Some(r#"{"status":200,"headers":{},"body":""}"#.into())))).unwrap();
        let thrown = |marker: &str| Some(Err(anyhow!("{marker} no such patient")));
        answered(&mut counted, "c", thrown(ductus_script::QUARANTINE)).unwrap();
        answered(&mut counted, "d", thrown(ductus_script::TRANSIENT)).unwrap();
        answered(&mut counted, "e", Some(Err(anyhow!("TypeError: x is undefined")))).unwrap();
        assert_eq!(counted.statuses, BTreeMap::from([(200, 1), (404, 1), (422, 1), (500, 1), (503, 1)]));
        assert_eq!(counted.sent, 5);
        assert!(counted.first_reason.as_deref().is_some_and(|r| r.starts_with("c: ")), "{counted:?}");
        assert!(answered(&mut counted, "f", None).is_err(), "no code, nothing to measure");
    }

    #[test]
    fn a_named_resource_s_latency_wins_over_its_kind_s() {
        let mut mocks = Mocks::default();
        mocks.named.insert("empi".into(), Duration::from_millis(120));
        assert_eq!(mocks.latency("api", "empi"), Duration::from_millis(120));
        assert_eq!(mocks.latency("api", "other"), API_LATENCY);
        assert_eq!(mocks.latency("db", "provider_db"), DB_LATENCY);
        assert_eq!(mocks.latency("fetch", "fetch"), API_LATENCY);
    }
}
