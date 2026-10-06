//! Source side: receive, persist, then acknowledge. A message is never acknowledged before
//! it's stored. `Intake` does the storing for every source (MLLP, HTTP, files...).

use crate::hl7_now;
use crate::keys::{self, KeyPath};
use anyhow::{Context, Result};
use bytes::Bytes;
use ductus_core::config::{Durability, Source};
use ductus_core::format::Format;
use ductus_core::{InterfaceConfig, partition_of};
use ductus_hl7::{AckCode, Message};
use ductus_store::{DedupeKey, MessageRef, NewMessage, Store};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::Notify;

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
    wake: Arc<[Notify]>,
    ordering: Option<KeyPath>,
    dedupe: Vec<KeyPath>,
    control_id: Option<KeyPath>,
}

impl Intake {
    pub fn new(config: Arc<InterfaceConfig>, store: Store, wake: Arc<[Notify]>) -> Result<Self> {
        let compile = |path: &str| {
            keys::compile(&config.format, path).with_context(|| format!("interface `{}`: key `{path}`", config.name))
        };
        let ordering = config.ordering.key.as_deref().map(compile).transpose()?;
        let dedupe = config.dedupe.iter().flat_map(|d| d.keys.iter()).map(|k| compile(k)).collect::<Result<_>>()?;
        let control_id = (config.format == Format::Hl7).then(|| compile("MSH-10")).transpose()?;
        Ok(Self { config, store, wake, ordering, dedupe, control_id })
    }

    /// Store a message (deduplicating if configured) and wake its worker.
    pub async fn accept(&self, payload: &[u8]) -> Result<Accepted> {
        let text = String::from_utf8_lossy(payload);
        let read = |k: &KeyPath| keys::extract(&self.config.format, k, &text);

        let key = self.ordering.as_ref().map(read);
        let partition_key = partition_of(key.as_deref().unwrap_or(""), self.config.partition_count());
        let control_id = self.control_id.as_ref().map(read).filter(|c| !c.is_empty());
        let new = NewMessage {
            interface: &self.config.name,
            partition_key,
            ordering_key: key.as_deref(),
            control_id: control_id.as_deref(),
            payload,
            relaxed: self.config.durability == Durability::Relaxed,
        };

        let values: Vec<String> = self.dedupe.iter().map(read).collect();
        let accepted = match &self.config.dedupe {
            Some(d) if values.iter().any(|v| !v.is_empty()) => {
                // Unit separator: can't appear in these values, so distinct lists never collide.
                let dedupe_key = values.join("\u{1f}");
                let stored = self.store.insert_deduped(new, DedupeKey { key: &dedupe_key, window: d.window }).await?;
                if stored.duplicate {
                    tracing::debug!(interface = %self.config.name, key = %dedupe_key, "duplicate");
                    Accepted::Duplicate
                } else {
                    Accepted::Queued(stored.message)
                }
            }
            // Nothing to identify the message by: store it normally.
            _ => Accepted::Queued(self.store.insert(new).await?),
        };
        if matches!(accepted, Accepted::Queued(_)) {
            // Wake only the lane's owner (same assignment as InterfaceConfig::partitions_for_worker).
            // notify_one keeps a permit if the worker is busy, so the wake-up is never lost.
            self.wake[partition_key as usize % self.wake.len()].notify_one();
        }
        Ok(accepted)
    }
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
