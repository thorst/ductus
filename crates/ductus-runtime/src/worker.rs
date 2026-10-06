//! Worker threads. Each thread owns one V8 isolate (isolates can't move between threads)
//! and runs one async task per interface slot assigned to it:
//!
//! - shared pool threads host many interfaces in one isolate;
//! - dedicated threads host one slot of one interface.
//!
//! A slot owns a fixed set of lanes (partitions) and processes their pending messages
//! strictly in arrival order. Because slots are async tasks, one interface waiting on a
//! retry back-off or a slow destination doesn't hold up the others on the same thread.

use crate::destination::{SendError, Sender};
use crate::limiter::{Decision, Limiter};
use crate::services::{EngineServices, SharedServices};
use anyhow::{Context, Result};
use ductus_core::InterfaceConfig;
use ductus_script::{Call, InterfaceId, Isolate};
use ductus_store::{MessageRef, Pending, Status, Store};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;

const BATCH: i64 = 200;
/// Safety-net poll. Ingest wakes the owning slot directly (and the wake-up is never lost),
/// so this only matters for work written by something other than this process.
const IDLE_POLL: Duration = Duration::from_secs(30);
const DB_RETRY: Duration = Duration::from_secs(1);
const SENT_FLUSH: usize = 50;

/// One slot of one interface, to run on a worker thread.
pub struct Job {
    pub config: Arc<InterfaceConfig>,
    pub slot: u32,
    pub wake: Arc<[Notify]>,
    pub limiter: Option<Arc<Limiter>>,
}

/// Readiness report for one job: its interface name and whether its code loaded.
pub type Ready = (String, Result<()>);

/// Start a worker thread for `jobs`. Each job reports on `ready` once its interface code has
/// loaded (or failed to); then the thread processes messages forever.
pub fn spawn_thread(
    name: String,
    jobs: Vec<Job>,
    store: Store,
    services: Arc<SharedServices>,
    ready: std::sync::mpsc::Sender<Ready>,
) -> Result<()> {
    std::thread::Builder::new()
        .name(name)
        .spawn(move || {
            let runtime =
                tokio::runtime::Builder::new_current_thread().enable_all().build().expect("building worker runtime");
            let local = tokio::task::LocalSet::new();
            local.block_on(&runtime, async move {
                let mut host = Isolate::host_with(Some(Rc::new(EngineServices(services))));
                let mut loaded = Vec::with_capacity(jobs.len());
                for job in jobs {
                    let script = match job.config.script_path() {
                        Some(path) => match host.add_interface_with(&path, &job.config.format_spec()).await {
                            Ok(id) => Some(id),
                            Err(e) => {
                                let _ = ready.send((job.config.name.clone(), Err(e)));
                                continue;
                            }
                        },
                        None => None,
                    };
                    let _ = ready.send((job.config.name.clone(), Ok(())));
                    loaded.push((job, script));
                }
                drop(ready);

                let host = Rc::new(RefCell::new(host));
                let kick = Rc::new(Notify::new());
                tokio::task::spawn_local(drive_event_loop(host.clone(), kick.clone()));
                for (job, script) in loaded {
                    let script = script.map(|id| Script { host: host.clone(), id, kick: kick.clone() });
                    tokio::task::spawn_local(Worker::new(job, script, store.clone()).run());
                }
                std::future::pending::<()>().await;
            });
        })
        .context("spawning worker thread")?;
    Ok(())
}

/// Runs the thread's JavaScript event loop whenever async transforms are waiting (on database
/// calls etc.), so many of them can be in flight at once. Sleeps when there's nothing to do.
async fn drive_event_loop(host: Rc<RefCell<Isolate>>, kick: Rc<Notify>) {
    loop {
        if let Err(e) = std::future::poll_fn(|cx| host.borrow_mut().poll_event_loop(cx)).await {
            tracing::error!(error = %e, "JavaScript event loop error");
        }
        kick.notified().await;
    }
}

/// An interface's code in the thread's shared isolate. The isolate is borrowed only for
/// synchronous calls, never across an await, so tasks on the thread never contend.
struct Script {
    host: Rc<RefCell<Isolate>>,
    id: InterfaceId,
    /// Wakes the event-loop driver when an async transform starts.
    kick: Rc<Notify>,
}

impl Script {
    async fn transform(&self, input: &str) -> Result<Option<String>> {
        let started = self.host.borrow_mut().begin(self.id, input)?;
        match started {
            Call::Done(output) => Ok(output),
            Call::Pending(pending) => {
                self.kick.notify_one();
                let value = pending.await;
                self.host.borrow_mut().settle(value)
            }
        }
    }
}

struct Worker {
    config: Arc<InterfaceConfig>,
    lanes: Vec<i32>,
    script: Option<Script>,
    sender: Sender,
    store: Store,
    wake: Arc<[Notify]>,
    limiter: Option<Arc<Limiter>>,
    index: usize,
    /// Delivered but not yet marked sent. Flushed every SENT_FLUSH messages and at the end of
    /// each batch, so a crash re-sends at most that many (at-least-once delivery).
    sent: Vec<MessageRef>,
}

/// What the interface logic decided to do with a message.
enum Outcome {
    Deliver(String),
    /// Dropped on purpose (`return null` or `kill(reason)`): kept for audit, never delivered.
    Filter(Option<String>),
    Quarantine(String),
    /// A temporary failure inside the transform (e.g. its database is down): try again later.
    Retry(String),
}

impl Worker {
    fn new(job: Job, script: Option<Script>, store: Store) -> Self {
        Self {
            lanes: job.config.partitions_for_worker(job.slot),
            sender: Sender::new(&job.config.destination, &job.config.dir),
            config: job.config,
            script,
            store,
            wake: job.wake,
            limiter: job.limiter,
            index: job.slot as usize,
            sent: Vec::with_capacity(SENT_FLUSH),
        }
    }

    async fn run(mut self) {
        if self.lanes.is_empty() {
            return; // more workers than lanes: nothing to own
        }
        loop {
            let batch = match self.store.pending(&self.config.name, &self.lanes, BATCH).await {
                Ok(batch) => batch,
                Err(e) => {
                    tracing::error!(interface = %self.config.name, error = %e, "reading queue failed");
                    tokio::time::sleep(DB_RETRY).await;
                    continue;
                }
            };
            if batch.is_empty() {
                tokio::select! {
                    _ = self.wake[self.index].notified() => {}
                    _ = tokio::time::sleep(IDLE_POLL) => {}
                }
                continue;
            }
            for message in batch {
                self.process(message).await;
            }
            // Must happen before the next read, or those messages would be read again as pending.
            self.flush_sent().await;
        }
    }

    /// Handle one message to completion. Blocks this worker's lanes until the message is
    /// delivered, filtered or quarantined: that's the price of ordering.
    async fn process(&mut self, message: Pending) {
        let reference = message.reference();
        let input = String::from_utf8_lossy(&message.payload);
        let mut attempts = message.attempts;
        let mut backoff = Duration::from_millis(self.config.retry.initial_backoff_ms);
        loop {
            let outcome = match &self.script {
                None => Outcome::Deliver(input.to_string()),
                Some(script) => match script.transform(&input).await {
                    Ok(Some(output)) => match output.strip_prefix(ductus_script::KILL) {
                        Some(reason) => Outcome::Filter(Some(format!("killed: {reason}"))),
                        None => Outcome::Deliver(output),
                    },
                    Ok(None) => Outcome::Filter(None),
                    Err(e) => {
                        let error = format!("{e:#}");
                        if let Some((_, reason)) = error.split_once(ductus_script::QUARANTINE) {
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

            return match outcome {
                Outcome::Filter(reason) => self.finish(reference, Status::Filtered, reason.as_deref()).await,
                Outcome::Quarantine(error) => {
                    tracing::warn!(interface = %self.config.name, id = reference.id, %error, "quarantined");
                    self.finish(reference, Status::Quarantined, Some(&error)).await
                }
                Outcome::Deliver(output) => self.deliver(reference, attempts, output.as_bytes()).await,
                Outcome::Retry(error) => {
                    // Keep this lane's order: the message stays first in line until it succeeds.
                    attempts = self.record_failure(reference, &error).await.unwrap_or(attempts + 1);
                    tracing::warn!(interface = %self.config.name, id = reference.id, attempts, %error, "temporary failure; retrying in {backoff:?}");
                    self.flush_sent().await;
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_millis(self.config.retry.max_backoff_ms));
                    continue;
                }
            };
        }
    }

    async fn deliver(&mut self, reference: MessageRef, mut attempts: i32, payload: &[u8]) {
        if let Some(limiter) = self.limiter.clone() {
            loop {
                match limiter.acquire(std::time::Instant::now()) {
                    Decision::Proceed => break,
                    Decision::Wait(delay) => {
                        self.flush_sent().await;
                        tokio::time::sleep(delay).await;
                    }
                    Decision::Drop => {
                        return self.finish(reference, Status::Filtered, Some("dropped: over rate limit")).await;
                    }
                }
            }
        }
        let retry = self.config.retry.clone();
        let mut backoff = Duration::from_millis(retry.initial_backoff_ms);
        loop {
            match self.sender.send(reference.id, payload).await {
                Ok(()) => {
                    self.sent.push(reference);
                    if self.sent.len() >= SENT_FLUSH {
                        self.flush_sent().await;
                    }
                    return;
                }
                Err(SendError::Permanent(error)) => {
                    tracing::warn!(interface = %self.config.name, id = reference.id, %error, "destination rejected; quarantined");
                    return self.finish(reference, Status::Quarantined, Some(&error)).await;
                }
                Err(SendError::Transient(error)) => {
                    attempts = self.record_failure(reference, &error).await.unwrap_or(attempts + 1);
                    if retry.max_attempts.is_some_and(|max| attempts as u32 >= max) {
                        let error = format!("gave up after {attempts} attempts: {error}");
                        tracing::warn!(interface = %self.config.name, id = reference.id, %error, "quarantined");
                        return self.finish(reference, Status::Quarantined, Some(&error)).await;
                    }
                    tracing::warn!(interface = %self.config.name, id = reference.id, attempts, %error, "delivery failed; retrying in {backoff:?}");
                    self.flush_sent().await; // don't hold delivered statuses through a long outage
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_millis(retry.max_backoff_ms));
                }
            }
        }
    }

    async fn record_failure(&self, reference: MessageRef, error: &str) -> Option<i32> {
        match self.store.record_failure(reference, error).await {
            Ok(attempts) => Some(attempts),
            Err(e) => {
                tracing::error!(interface = %self.config.name, error = %e, "recording failure failed");
                None
            }
        }
    }

    async fn flush_sent(&mut self) {
        if self.sent.is_empty() {
            return;
        }
        while let Err(e) = self.store.set_status_many(&self.sent, Status::Sent).await {
            tracing::error!(interface = %self.config.name, error = %e, "saving sent statuses failed; retrying");
            tokio::time::sleep(DB_RETRY).await;
        }
        self.sent.clear();
    }

    /// Persist the final status. Retries until the database accepts it: if we gave up here,
    /// the message would be re-sent after a restart.
    async fn finish(&self, reference: MessageRef, status: Status, error: Option<&str>) {
        while let Err(e) = self.store.set_status(reference, status, error).await {
            tracing::error!(interface = %self.config.name, id = reference.id, error = %e, "saving status failed; retrying");
            tokio::time::sleep(DB_RETRY).await;
        }
    }
}
