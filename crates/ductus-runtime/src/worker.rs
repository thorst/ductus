//! Worker threads. Each thread owns one V8 isolate (isolates can't move between threads)
//! and runs one async task per interface slot and destination assigned to it:
//!
//! - shared pool threads host many interfaces in one isolate;
//! - dedicated threads host one slot of one interface.
//!
//! A slot owns a fixed set of lanes (partitions); for each destination, it processes their
//! pending deliveries strictly in arrival order. Because these are async tasks, a destination
//! waiting on a retry back-off or a slow receiver doesn't hold up the other destinations or
//! the other interfaces on the same thread.
//!
//! Interface code that runs away (endless loop, heap filled) is stopped by the script guard:
//! its message is quarantined, and the interface is stopped if it keeps happening
//! (`health.rs`). The other interfaces on the thread carry on.

use crate::Wake;
use crate::destination::{SendError, Sender};
use crate::health::Health;
use crate::http::HttpCall;
use crate::limiter::{Decision, Limiter};
use crate::services::EngineServices;
use anyhow::{Context, Result, anyhow};
use ductus_core::InterfaceConfig;
use ductus_core::config::Retry;
use ductus_script::{Call, FunctionId, InterfaceId, Isolate, Runaway};
use ductus_store::{DestinationId, Done, MessageRef, Pending, Status, Store};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Notify, mpsc, watch};

const BATCH: i64 = 200;
/// Safety-net poll. Ingest wakes the owning slot directly (and the wake-up is never lost),
/// so this only matters for work written by something other than this process.
const IDLE_POLL: Duration = Duration::from_secs(30);
const DB_RETRY: Duration = Duration::from_secs(1);
const FLUSH: usize = 50;

/// One slot of one interface (all its destinations), to run on a worker thread.
pub struct Job {
    pub config: Arc<InterfaceConfig>,
    pub slot: u32,
    pub wake: Arc<Wake>,
    /// Respond-mode HTTP interfaces: where their requests arrive.
    pub requests: Option<mpsc::Receiver<HttpCall>>,
    /// Shared by all slots of the interface: stopping one stops them all.
    pub health: Arc<Health>,
    /// What each destination needs beyond its config, in `config.destinations()` order.
    pub outlets: Vec<Outlet>,
}

/// One destination's shared parts.
#[derive(Clone)]
pub struct Outlet {
    /// The destination's id in the store (`Store::destination_ids`).
    pub id: DestinationId,
    /// Shared by the destination's slots: the limit is for the receiver as a whole.
    pub limiter: Option<Arc<Limiter>>,
    /// SFTP destinations: the server, from resources.yaml.
    pub server: Option<ductus_core::resources::SftpServer>,
}

/// Readiness report for one job: its interface name and whether its code loaded.
pub type Ready = (String, Result<()>);

/// Start a worker thread for `jobs`, with one isolate of `heap_mb`. Each job reports on
/// `ready` once its interface code has loaded (or failed to); then the thread processes
/// messages forever.
pub fn spawn_thread(
    name: String,
    jobs: Vec<Job>,
    store: Store,
    services: EngineServices,
    heap_mb: usize,
    ready: std::sync::mpsc::Sender<Ready>,
) -> Result<()> {
    let io = tokio::runtime::Handle::current(); // the main runtime (see `Worker::db`)
    std::thread::Builder::new()
        .name(name.clone())
        .spawn(move || {
            let runtime =
                tokio::runtime::Builder::new_current_thread().enable_all().build().expect("building worker runtime");
            let local = tokio::task::LocalSet::new();
            local.block_on(&runtime, async move {
                let mut host = Isolate::new(Some(Rc::new(services)), heap_mb);
                let mut loaded = Vec::with_capacity(jobs.len());
                for job in jobs {
                    let config = &job.config;
                    let script = match config.script_path() {
                        Some(path) => {
                            let functions = config.functions();
                            let functions: Vec<(&str, &str)> =
                                functions.iter().map(|(name, formats)| (name.as_str(), formats.as_str())).collect();
                            match host.add_functions(&path, &functions, config.timeout).await {
                                Ok(loaded) => Some(loaded),
                                Err(e) => {
                                    let _ = ready.send((job.config.name.clone(), Err(e)));
                                    continue;
                                }
                            }
                        }
                        None => None,
                    };
                    let _ = ready.send((job.config.name.clone(), Ok(())));
                    loaded.push((job, script));
                }
                drop(ready);

                let host = Rc::new(RefCell::new(host));
                let kick = Rc::new(Notify::new());
                let interrupted = Rc::new(watch::Sender::new(None));
                tokio::task::spawn_local(drive_event_loop(name, host.clone(), kick.clone(), interrupted.clone()));
                for (mut job, script) in loaded {
                    // The script's functions, in `config.functions()` order: one per destination.
                    let script = |i: usize| {
                        script.as_ref().map(|(interface, functions)| Script {
                            host: host.clone(),
                            interface: *interface,
                            function: functions[i],
                            kick: kick.clone(),
                            interrupted: interrupted.clone(),
                        })
                    };
                    if let Some(requests) = job.requests.take() {
                        let worker = Responder { config: job.config, script: script(0), health: job.health };
                        tokio::task::spawn_local(worker.answer(requests));
                        continue;
                    }
                    for (i, outlet) in job.outlets.iter().enumerate() {
                        let worker = Worker::new(&job, i, outlet.clone(), script(i), store.clone(), io.clone());
                        tokio::task::spawn_local(worker.run());
                    }
                }
                std::future::pending::<()>().await;
            });
        })
        .context("spawning worker thread")?;
    Ok(())
}

/// Runs the thread's JavaScript event loop whenever async transforms are waiting (on database
/// calls etc.), so many of them can be in flight at once. Sleeps when there's nothing to do.
///
/// If code resumed after an `await` runs away, V8 drops every queued continuation on the
/// isolate, so no async transform in flight will ever finish: `interrupted` tells them all.
async fn drive_event_loop(
    thread: String,
    host: Rc<RefCell<Isolate>>,
    kick: Rc<Notify>,
    interrupted: Rc<watch::Sender<Option<Runaway>>>,
) {
    loop {
        if let Err(e) = std::future::poll_fn(|cx| host.borrow_mut().poll_event_loop(cx)).await {
            match e.downcast_ref::<Runaway>() {
                Some(runaway) => {
                    if runaway.interface.is_none() {
                        tracing::error!(%thread, error = %runaway, "runaway code in a shared module (not in any interface's folder) was stopped; can't tell whose it was, so every transform in flight on this thread will be retried");
                    }
                    interrupted.send_replace(Some(*runaway));
                }
                None => tracing::error!(error = %e, "JavaScript event loop error"),
            }
        }
        kick.notified().await;
    }
}

/// One of an interface's functions in the thread's shared isolate. The isolate is borrowed
/// only for synchronous calls, never across an await, so tasks on the thread never contend.
#[derive(Clone)]
struct Script {
    host: Rc<RefCell<Isolate>>,
    interface: InterfaceId,
    function: FunctionId,
    /// Wakes the event-loop driver when an async transform starts.
    kick: Rc<Notify>,
    /// Set when runaway code was stopped in the event loop (see `drive_event_loop`).
    interrupted: Rc<watch::Sender<Option<Runaway>>>,
}

impl Script {
    /// Run the function on `input`; `meta` (JSON) is its second argument.
    async fn transform(&self, input: &str, meta: Option<&str>) -> Result<Option<String>> {
        let started = self.host.borrow_mut().begin_function(self.function, input, meta)?;
        match started {
            Call::Done(output) => Ok(output),
            Call::Pending(pending) => {
                let mut interrupted = self.interrupted.subscribe();
                self.kick.notify_one();
                tokio::select! {
                    value = pending => self.host.borrow_mut().settle(value),
                    // Whose code it was is known by interface, not by function: every call of
                    // that interface in flight takes the blame.
                    _ = interrupted.changed() => match *interrupted.borrow() {
                        Some(runaway) if runaway.interface == Some(self.interface) => Err(runaway.into()),
                        _ => Err(anyhow!(
                            "{} interrupted: runaway code elsewhere on this thread was stopped",
                            ductus_script::TRANSIENT
                        )),
                    },
                }
            }
        }
    }
}

/// Answers a respond-mode HTTP interface's requests (no queue, no destinations).
struct Responder {
    config: Arc<InterfaceConfig>,
    script: Option<Script>,
    health: Arc<Health>,
}

impl Responder {
    /// Run each HTTP request through the transform, concurrently (one local task per request,
    /// so slow lookups don't hold up other callers).
    async fn answer(self, mut requests: mpsc::Receiver<HttpCall>) {
        let Some(script) = self.script else { return };
        let name: Rc<str> = self.config.name.as_str().into();
        while let Some(call) = requests.recv().await {
            if let Some(reason) = self.health.stopped() {
                let _ = call.reply.send(Err(anyhow!("{} interface stopped: {reason}", ductus_script::TRANSIENT)));
                continue;
            }
            let (script, health, name) = (script.clone(), self.health.clone(), name.clone());
            tokio::task::spawn_local(async move {
                let result = script.transform(&call.envelope, None).await;
                match result.as_ref().err().and_then(|e| e.downcast_ref::<Runaway>()) {
                    Some(runaway) => ran_away(&name, &health, runaway),
                    None => health.ran(),
                }
                let _ = call.reply.send(result.map(Option::unwrap_or_default));
            });
        }
    }
}

/// Works through one destination's queue in one slot's lanes.
struct Worker {
    config: Arc<InterfaceConfig>,
    /// The destination's name.
    destination: Arc<str>,
    /// Its id in the store.
    destination_id: DestinationId,
    lanes: Vec<i32>,
    script: Option<Script>,
    sender: Sender,
    store: Store,
    io: tokio::runtime::Handle,
    wake: Arc<Wake>,
    slot: usize,
    index: usize,
    limiter: Option<Arc<Limiter>>,
    retry: Retry,
    save_output: bool,
    /// Sent or filtered, not yet saved. Flushed every FLUSH messages and at the end of
    /// each batch, so a crash re-sends at most that many (at-least-once delivery).
    done: Vec<Done>,
    health: Arc<Health>,
}

/// What the interface logic decided to do with a message.
enum Outcome {
    Deliver(String),
    /// Dropped on purpose (`return null` or `kill(reason)`): kept for audit, never delivered.
    Filter(Option<String>),
    Quarantine(String),
    /// A temporary failure inside the transform (e.g. its database is down): try again later.
    Retry(String),
    /// The code ran away and was stopped.
    Runaway(Runaway),
}

impl Worker {
    /// The worker for `job`'s destination number `index`.
    fn new(
        job: &Job,
        index: usize,
        outlet: Outlet,
        script: Option<Script>,
        store: Store,
        io: tokio::runtime::Handle,
    ) -> Self {
        let config = job.config.clone();
        let (name, destination) = config.destinations().nth(index).expect("one outlet per destination");
        Self {
            destination: name.into(),
            destination_id: outlet.id,
            lanes: config.partitions_for_worker(job.slot),
            sender: Sender::new(&config, destination, outlet.server),
            retry: config.retry_for(destination).clone(),
            save_output: destination.save_output,
            script,
            store,
            io,
            wake: job.wake.clone(),
            slot: job.slot as usize,
            index,
            limiter: outlet.limiter,
            done: Vec::with_capacity(FLUSH),
            health: job.health.clone(),
            config,
        }
    }

    async fn run(mut self) {
        if self.lanes.is_empty() {
            return; // more workers than lanes: nothing to own
        }
        // A stopped interface's messages stay pending until it's fixed and restarted.
        while self.health.stopped().is_none() {
            let (destination, lanes) = (self.destination_id, self.lanes.clone());
            let read = |store: Store| async move { store.pending(destination, &lanes, BATCH).await };
            let batch = match self.db(read).await {
                Ok(batch) => batch,
                Err(e) => {
                    tracing::error!(interface = %self.config.name, destination = %self.destination, error = %e, "reading queue failed");
                    tokio::time::sleep(DB_RETRY).await;
                    continue;
                }
            };
            if batch.is_empty() {
                tokio::select! {
                    _ = self.wake.get(self.slot, self.index).notified() => {}
                    _ = tokio::time::sleep(IDLE_POLL) => {}
                }
                continue;
            }
            for message in batch {
                if self.health.stopped().is_some() {
                    break;
                }
                self.process(message).await;
            }
            // Must happen before the next read, or those messages would be read again as pending.
            self.flush().await;
        }
    }

    /// Handle one message to completion for this destination. Blocks this worker's lanes until
    /// the message is delivered, filtered or quarantined: that's the price of ordering.
    async fn process(&mut self, message: Pending) {
        let reference = message.reference();
        let input = String::from_utf8_lossy(&message.payload);
        let meta = serde_json::json!({
            "id": message.id,
            "receivedAt": message.received_at.to_rfc3339(),
            "destination": &*self.destination,
        })
        .to_string();
        let mut attempts = message.attempts;
        let mut backoff = Duration::from_millis(self.retry.initial_backoff_ms);
        loop {
            let outcome = match &self.script {
                None => Outcome::Deliver(input.to_string()),
                Some(script) => match script.transform(&input, Some(&meta)).await {
                    Ok(Some(output)) => match output.strip_prefix(ductus_script::KILL) {
                        Some(reason) => Outcome::Filter(Some(format!("killed: {reason}"))),
                        None => Outcome::Deliver(output),
                    },
                    Ok(None) => Outcome::Filter(None),
                    Err(e) => {
                        let error = format!("{e:#}");
                        if let Some(runaway) = e.downcast_ref::<Runaway>() {
                            Outcome::Runaway(*runaway)
                        } else if let Some((_, reason)) = error.split_once(ductus_script::QUARANTINE) {
                            // A deliberate quarantine: keep just the reason, not the stack trace.
                            Outcome::Quarantine(reason.lines().next().unwrap_or("").trim().to_string())
                        } else if error.contains(ductus_script::TRANSIENT) {
                            Outcome::Retry(format!(
                                "transform failed: {}",
                                error.replace(ductus_script::TRANSIENT, "").trim()
                            ))
                        } else {
                            Outcome::Quarantine(format!("transform failed: {error}"))
                        }
                    }
                },
            };

            match &outcome {
                Outcome::Runaway(runaway) => ran_away(&self.config.name, &self.health, runaway),
                Outcome::Retry(_) => {} // didn't finish: says nothing about the code
                _ => self.health.ran(),
            }
            return match outcome {
                Outcome::Runaway(runaway) => {
                    let error = format!("transform {runaway}");
                    self.finish(reference, Status::Quarantined, Some(&error)).await
                }
                Outcome::Filter(reason) => self.done(Done::with(reference, Status::Filtered, reason.as_deref())).await,
                Outcome::Quarantine(error) => {
                    tracing::warn!(interface = %self.config.name, destination = %self.destination, id = reference.id, %error, "quarantined");
                    self.finish(reference, Status::Quarantined, Some(&error)).await
                }
                Outcome::Deliver(output) => self.deliver(reference, attempts, output.into_bytes()).await,
                Outcome::Retry(error) => {
                    // Keep this lane's order: the message stays first in line until it succeeds.
                    attempts = self.record_failure(reference, &error).await.unwrap_or(attempts + 1);
                    tracing::warn!(interface = %self.config.name, destination = %self.destination, id = reference.id, attempts, %error, "temporary failure; retrying in {backoff:?}");
                    self.flush().await;
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_millis(self.retry.max_backoff_ms));
                    continue;
                }
            };
        }
    }

    async fn deliver(&mut self, reference: MessageRef, mut attempts: i32, payload: Vec<u8>) {
        if let Some(limiter) = self.limiter.clone() {
            loop {
                match limiter.acquire(std::time::Instant::now()) {
                    Decision::Proceed => break,
                    Decision::Wait(delay) => {
                        self.flush().await;
                        tokio::time::sleep(delay).await;
                    }
                    Decision::Drop => {
                        let dropped = Done::with(reference, Status::Filtered, Some("dropped: over rate limit"));
                        return self.done(dropped).await;
                    }
                }
            }
        }
        let retry = self.retry.clone();
        let mut backoff = Duration::from_millis(retry.initial_backoff_ms);
        loop {
            match self.sender.send(reference.id, &payload).await {
                Ok(()) => {
                    let output = self.save_output.then_some(payload);
                    return self.done(Done::sent(reference, output)).await;
                }
                Err(SendError::Permanent(error)) => {
                    tracing::warn!(interface = %self.config.name, destination = %self.destination, id = reference.id, %error, "destination rejected; quarantined");
                    return self.finish(reference, Status::Quarantined, Some(&error)).await;
                }
                Err(SendError::Transient(error)) => {
                    attempts = self.record_failure(reference, &error).await.unwrap_or(attempts + 1);
                    if retry.max_attempts.is_some_and(|max| attempts as u32 >= max) {
                        let error = format!("gave up after {attempts} attempts: {error}");
                        tracing::warn!(interface = %self.config.name, destination = %self.destination, id = reference.id, %error, "quarantined");
                        return self.finish(reference, Status::Quarantined, Some(&error)).await;
                    }
                    tracing::warn!(interface = %self.config.name, destination = %self.destination, id = reference.id, attempts, %error, "delivery failed; retrying in {backoff:?}");
                    self.flush().await; // don't hold delivered statuses through a long outage
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_millis(retry.max_backoff_ms));
                }
            }
        }
    }

    /// Run a store call on the main runtime. Connections are pooled and shared with intake,
    /// and a connection's socket is served by the runtime that opened it: one opened on this
    /// thread would hold up intake's commits (and so the senders' ACKs) whenever this thread
    /// is busy in JavaScript.
    async fn db<T, F>(&self, call: impl FnOnce(Store) -> F) -> Result<T>
    where
        T: Send + 'static,
        F: Future<Output = Result<T>> + Send + 'static,
    {
        self.io.spawn(call(self.store.clone())).await.context("database task failed")?
    }

    async fn record_failure(&self, reference: MessageRef, error: &str) -> Option<i32> {
        let (error, destination) = (error.to_string(), self.destination_id);
        match self.db(|store| async move { store.record_failure(reference, destination, &error).await }).await {
            Ok(attempts) => Some(attempts),
            Err(e) => {
                tracing::error!(interface = %self.config.name, destination = %self.destination, error = %e, "recording failure failed");
                None
            }
        }
    }

    /// A message is done for this destination (sent or filtered): saved with the next flush.
    /// If the engine stops before then, it's processed again: the same result, or a re-send.
    async fn done(&mut self, done: Done) {
        self.done.push(done);
        if self.done.len() >= FLUSH {
            self.flush().await;
        }
    }

    async fn flush(&mut self) {
        if self.done.is_empty() {
            return;
        }
        let flush = || {
            let (done, destination) = (self.done.clone(), self.destination_id);
            self.db(move |store| async move { store.finish(destination, &done).await })
        };
        while let Err(e) = flush().await {
            tracing::error!(interface = %self.config.name, destination = %self.destination, error = %e, "saving statuses failed; retrying");
            tokio::time::sleep(DB_RETRY).await;
        }
        self.done.clear();
    }

    /// Persist a final status now (quarantines: someone should see them at once). Retries until
    /// the database accepts it: if we gave up here, the message would be processed again after
    /// a restart.
    async fn finish(&self, reference: MessageRef, status: Status, error: Option<&str>) {
        let save = || {
            let (done, destination) = (Done::with(reference, status, error), self.destination_id);
            self.db(move |store| async move { store.finish(destination, &[done]).await })
        };
        while let Err(e) = save().await {
            tracing::error!(interface = %self.config.name, destination = %self.destination, id = reference.id, error = %e, "saving status failed; retrying");
            tokio::time::sleep(DB_RETRY).await;
        }
    }
}

/// Record that an interface's code ran away; say so loudly if that stops the interface.
fn ran_away(interface: &str, health: &Health, runaway: &Runaway) {
    tracing::error!(%interface, error = %runaway, "runaway script stopped");
    if let Some(reason) = health.runaway(runaway) {
        tracing::error!(%interface, %reason, "interface stopped: its messages wait until the code is fixed and the engine restarted");
    }
}
