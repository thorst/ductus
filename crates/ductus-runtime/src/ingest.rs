//! Source side: receive, persist, then ACK. A message is never ACKed before it's stored.

use crate::hl7_now;
use anyhow::{Context, Result};
use bytes::Bytes;
use ductus_core::config::Source;
use ductus_core::{InterfaceConfig, partition_of};
use ductus_hl7::{AckCode, Message, Path};
use ductus_store::{NewMessage, Store};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::Notify;

pub async fn spawn(config: Arc<InterfaceConfig>, store: Store, wake: Arc<Notify>) -> Result<()> {
    let key_path: Option<Path> = config
        .ordering
        .key
        .as_deref()
        .map(str::parse)
        .transpose()
        .with_context(|| format!("interface `{}`: ordering.key", config.name))?;

    let Source::Mllp { host, port } = &config.source;
    let listener = TcpListener::bind((host.as_str(), *port))
        .await
        .with_context(|| format!("interface `{}`: binding {host}:{port}", config.name))?;
    tracing::info!(interface = %config.name, %host, port, "listening (MLLP)");

    let handler = move |frame: Bytes| {
        let config = config.clone();
        let store = store.clone();
        let wake = wake.clone();
        let key_path = key_path.clone();
        async move {
            let ack = receive(&config, &store, key_path.as_ref(), &frame).await;
            wake.notify_waiters();
            Bytes::from(ack)
        }
    };
    tokio::spawn(async move {
        if let Err(e) = ductus_mllp::serve(listener, handler).await {
            tracing::error!(error = %e, "listener stopped");
        }
    });
    Ok(())
}

async fn receive(config: &InterfaceConfig, store: &Store, key_path: Option<&Path>, frame: &[u8]) -> String {
    let raw = String::from_utf8_lossy(frame);
    let message = match Message::parse(&raw) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(interface = %config.name, error = %e, "rejected unparseable message");
            return ductus_hl7::reject_unparseable(&e.to_string(), &hl7_now());
        }
    };

    let key = key_path.map(|p| message.get(p));
    let partitions = config.partition_count();
    let partition_key = partition_of(key.unwrap_or(""), partitions);
    let control_id = Some(message.control_id()).filter(|c| !c.is_empty());

    let stored = store
        .insert(NewMessage { interface: &config.name, partition_key, ordering_key: key, control_id, payload: frame })
        .await;

    match stored {
        Ok(_) => message.ack(AckCode::Accept, "", &hl7_now()),
        Err(e) => {
            // AE tells the sender to retry: nothing was stored, so nothing is lost.
            tracing::error!(interface = %config.name, error = %e, "failed to store message");
            message.ack(AckCode::Error, "temporarily unable to store message", &hl7_now())
        }
    }
}
