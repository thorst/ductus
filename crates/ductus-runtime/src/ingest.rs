//! Source side: receive, persist, then acknowledge. A message is never acknowledged before
//! it's stored. `Intake` does the storing for every source (MLLP, HTTP, files...).

use crate::Wake;
use crate::hl7_now;
use crate::keys::{self, KeyPath};
use anyhow::{Context, Result};
use bytes::Bytes;
use ductus_core::config::{Durability, Source};
use ductus_core::format::Format;
use ductus_core::{InterfaceConfig, partition_of};
use ductus_hl7::{AckCode, Message};
use ductus_store::{Batch, BatchMessage, DedupeKey, DestinationId, MessageRef, NewMessage, Store};
use std::sync::Arc;
use tokio::net::TcpListener;

/// What happened to an accepted message.
pub enum Accepted {
    /// Stored and queued for delivery.
    Queued(MessageRef),
    /// Seen within the dedupe window: stored (status `duplicate`) but not queued.
    Duplicate,
}

/// Stores incoming messages for one interface and wakes the worker that owns each one.
pub struct Intake {
    pub config: Arc<InterfaceConfig>,
    store: Store,
    wake: Arc<Wake>,
    /// The interface's destinations: each stored message is queued for all of them.
    destinations: Vec<DestinationId>,
    ordering: Option<KeyPath>,
    dedupe: Vec<KeyPath>,
    control_id: Option<KeyPath>,
}

impl Intake {
    /// `destinations`: the ids of the interface's destinations (`Store::destination_ids`).
    pub fn new(
        config: Arc<InterfaceConfig>,
        store: Store,
        wake: Arc<Wake>,
        destinations: Vec<DestinationId>,
    ) -> Result<Self> {
        let compile = |path: &str| {
            keys::compile(&config.format, path).with_context(|| format!("interface `{}`: key `{path}`", config.name))
        };
        let ordering = config.ordering.key.as_deref().map(compile).transpose()?;
        let dedupe = config.dedupe.iter().flat_map(|d| d.keys.iter()).map(|k| compile(k)).collect::<Result<_>>()?;
        let control_id = (config.format == Format::Hl7).then(|| compile("MSH-10")).transpose()?;
        Ok(Self { config, store, wake, destinations, ordering, dedupe, control_id })
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Read a message's lane, ordering key, control ID and dedupe key.
    fn keys(&self, payload: &[u8]) -> Keys {
        let text = String::from_utf8_lossy(payload);
        let read = |k: &KeyPath| keys::extract(&self.config.format, k, &text);
        let ordering_key = self.ordering.as_ref().map(read);
        let values: Vec<String> = self.dedupe.iter().map(read).collect();
        Keys {
            partition: partition_of(ordering_key.as_deref().unwrap_or(""), self.config.partition_count()),
            ordering_key,
            control_id: self.control_id.as_ref().map(read).filter(|c| !c.is_empty()),
            // Nothing to identify the message by: no dedupe. The unit separator can't appear in
            // these values, so distinct lists never collide.
            dedupe: values.iter().any(|v| !v.is_empty()).then(|| values.join("\u{1f}")),
        }
    }

    fn relaxed(&self) -> bool {
        self.config.durability == Durability::Relaxed
    }

    /// Wake only the lane's owners: one worker per destination.
    fn wake(&self, partition: i32) {
        self.wake.lane(partition);
    }

    /// Store a message (deduplicating if configured) and wake its worker.
    pub async fn accept(&self, payload: &[u8]) -> Result<Accepted> {
        let keys = self.keys(payload);
        let new = NewMessage {
            interface: &self.config.name,
            partition_key: keys.partition,
            ordering_key: keys.ordering_key.as_deref(),
            control_id: keys.control_id.as_deref(),
            payload,
            relaxed: self.relaxed(),
            destinations: &self.destinations,
        };
        let accepted = match (&self.config.dedupe, &keys.dedupe) {
            (Some(d), Some(key)) => {
                let stored = self.store.insert_deduped(new, DedupeKey { key, window: d.window }).await?;
                if stored.duplicate {
                    tracing::debug!(interface = %self.config.name, %key, "duplicate");
                    Accepted::Duplicate
                } else {
                    Accepted::Queued(stored.message)
                }
            }
            _ => Accepted::Queued(self.store.insert(new).await?),
        };
        if matches!(accepted, Accepted::Queued(_)) {
            self.wake(keys.partition);
        }
        Ok(accepted)
    }

    /// Store many messages and the source's progress in one transaction (see `Store::insert_batch`).
    /// Returns how many were queued (the rest were duplicates).
    pub async fn accept_batch(&self, payloads: &[Vec<u8>], state: Option<(&str, &str)>) -> Result<usize> {
        let keys: Vec<Keys> = payloads.iter().map(|p| self.keys(p)).collect();
        let batch = Batch {
            interface: &self.config.name,
            messages: keys
                .iter()
                .zip(payloads)
                .map(|(k, payload)| BatchMessage {
                    partition_key: k.partition,
                    ordering_key: k.ordering_key.as_deref(),
                    control_id: k.control_id.as_deref(),
                    payload,
                    dedupe_key: k.dedupe.as_deref(),
                })
                .collect(),
            dedupe_window: self.config.dedupe.as_ref().map(|d| d.window).unwrap_or_default(),
            relaxed: self.relaxed(),
            state,
            destinations: &self.destinations,
        };
        let duplicates = self.store.insert_batch(&batch).await?;
        let mut woken = vec![false; self.wake.slots()];
        let mut queued = 0;
        for (k, _) in keys.iter().zip(&duplicates).filter(|(_, duplicate)| !**duplicate) {
            queued += 1;
            let slot = k.partition as usize % self.wake.slots();
            if !std::mem::replace(&mut woken[slot], true) {
                self.wake(k.partition);
            }
        }
        Ok(queued)
    }

    /// Queue a scheduled run (see `Store::insert_scheduled`). False if it was skipped because the
    /// previous run is still pending; `skipped_state` is saved instead of `state` then.
    pub async fn accept_scheduled(&self, payload: &[u8], key: &str, state: &str, skipped_state: &str) -> Result<bool> {
        let new = NewMessage {
            interface: &self.config.name,
            partition_key: 0,
            ordering_key: None,
            control_id: None,
            payload,
            relaxed: self.relaxed(),
            destinations: &self.destinations,
        };
        let queued = self.store.insert_scheduled(new, key, state, skipped_state).await?.is_some();
        if queued {
            self.wake(0);
        }
        Ok(queued)
    }
}

struct Keys {
    partition: i32,
    ordering_key: Option<String>,
    control_id: Option<String>,
    dedupe: Option<String>,
}

/// Start an MLLP listener feeding `intake`.
pub async fn spawn_mllp(intake: Arc<Intake>) -> Result<()> {
    let config = intake.config.clone();
    let Source::Mllp { host, port } = &config.source else { return Ok(()) };
    let listener = TcpListener::bind((host.as_str(), *port))
        .await
        .with_context(|| format!("interface `{}`: binding {host}:{port}", config.name))?;
    tracing::info!(interface = %config.name, %host, port, "listening (MLLP)");

    let handler = move |frame: Bytes| {
        let intake = intake.clone();
        async move { Bytes::from(receive_hl7(&intake, &frame).await) }
    };
    tokio::spawn(async move {
        if let Err(e) = ductus_mllp::serve(listener, handler).await {
            tracing::error!(error = %e, "listener stopped");
        }
    });
    Ok(())
}

/// Store an HL7 message and build its ACK.
async fn receive_hl7(intake: &Intake, frame: &[u8]) -> String {
    let raw = String::from_utf8_lossy(frame);
    let message = match Message::parse(&raw) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(interface = %intake.config.name, error = %e, "rejected unparseable message");
            return ductus_hl7::reject_unparseable(&e.to_string(), &hl7_now());
        }
    };
    match intake.accept(frame).await {
        // Duplicates are ACKed AA too, so the sender stops resending.
        Ok(_) => message.ack(AckCode::Accept, "", &hl7_now()),
        Err(e) => {
            // AE tells the sender to retry: nothing was stored, so nothing is lost.
            tracing::error!(interface = %intake.config.name, error = %e, "failed to store message");
            message.ack(AckCode::Error, "temporarily unable to store message", &hl7_now())
        }
    }
}
