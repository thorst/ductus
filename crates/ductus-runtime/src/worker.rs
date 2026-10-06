//! Workers: each owns a fixed set of lanes (partitions) and processes their pending
//! messages strictly in arrival order. A V8 isolate can't move between threads,
//! so every worker is its own OS thread with its own isolate and async runtime.

use crate::destination::{SendError, Sender};
use crate::limiter::{Decision, Limiter};
use anyhow::{Context, Result};
use ductus_core::InterfaceConfig;
use ductus_script::Isolate;
use ductus_store::{MessageRef, Pending, Status, Store};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;

const BATCH: i64 = 200;
const IDLE_POLL: Duration = Duration::from_millis(250);
const DB_RETRY: Duration = Duration::from_secs(1);
const SENT_FLUSH: usize = 50;

/// Start a worker thread. It reports on `ready` once its interface code has loaded (or failed to).
pub fn spawn(
    config: Arc<InterfaceConfig>,
    index: u32,
    store: Store,
    wake: Arc<[Notify]>,
    limiter: Option<Arc<Limiter>>,
    ready: std::sync::mpsc::Sender<Result<()>>,
) -> Result<()> {
    std::thread::Builder::new()
        .name(format!("{}-w{index}", config.name))
        .spawn(move || {
            let runtime =
                tokio::runtime::Builder::new_current_thread().enable_all().build().expect("building worker runtime");
            runtime.block_on(async move {
                match Worker::new(config, index, store, wake, limiter).await {
                    Ok(worker) => {
                        let _ = ready.send(Ok(()));
                        drop(ready);
                        worker.run().await
                    }
                    Err(e) => {
                        let _ = ready.send(Err(e));
                    }
                }
            });
        })
        .context("spawning worker thread")?;
    Ok(())
}

struct Worker {
    config: Arc<InterfaceConfig>,
    lanes: Vec<i32>,
    isolate: Option<Isolate>,
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
    Filter,
    Quarantine(String),
}

impl Worker {
    async fn new(
        config: Arc<InterfaceConfig>,
        index: u32,
        store: Store,
        wake: Arc<[Notify]>,
        limiter: Option<Arc<Limiter>>,
    ) -> Result<Self> {
        let isolate = match config.script_path() {
            Some(path) => Some(Isolate::interface(&path).await?),
            None => None,
        };
        Ok(Self {
            lanes: config.partitions_for_worker(index),
            sender: Sender::new(&config.destination, &config.dir),
            config,
            isolate,
            store,
            wake,
            limiter,
            index: index as usize,
            sent: Vec::with_capacity(SENT_FLUSH),
        })
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
        let outcome = match self.isolate.as_mut() {
            None => Outcome::Deliver(input.into_owned()),
            Some(isolate) => match isolate.transform(&input) {
                Ok(Some(output)) => Outcome::Deliver(output),
                Ok(None) => Outcome::Filter,
                Err(e) => Outcome::Quarantine(format!("transform failed: {e:#}")),
            },
        };

        match outcome {
            Outcome::Filter => self.finish(reference, Status::Filtered, None).await,
            Outcome::Quarantine(error) => {
                tracing::warn!(interface = %self.config.name, id = reference.id, %error, "quarantined");
                self.finish(reference, Status::Quarantined, Some(&error)).await
            }
            Outcome::Deliver(output) => self.deliver(reference, message.attempts, output.as_bytes()).await,
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
