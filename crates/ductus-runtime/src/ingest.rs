//! Source side: receive, persist, then acknowledge. A message is never acknowledged before
//! it's stored. `Intake` does the storing for every source (MLLP, HTTP, files...).

use crate::Wake;
use crate::gate::{Closed, Gate};
use crate::hl7_now;
use crate::keys::{self, KeyPath};
use crate::stats::{self, InterfaceStats};
use crate::tls::Conn;
use anyhow::{Context, Result};
use bytes::Bytes;
use ductus_core::config::{Durability, Source};
use ductus_core::format::Format;
use ductus_core::{InterfaceConfig, partition_of};
use ductus_hl7::{AckCode, Message};
use ductus_store::{Batch, BatchMessage, DedupeKey, DestinationId, Done, MessageRef, NewMessage, Store};
use std::sync::Arc;
use std::sync::atomic::{AtomicI16, Ordering::Relaxed};
use tokio::net::{TcpListener, TcpStream};

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
    /// Closed while draining: nothing new is stored.
    pub gate: Arc<Gate>,
    /// The interface's destinations: each stored message is queued for all of them.
    destinations: Vec<DestinationId>,
    /// The retention tier new messages go to (changes when a retention change starts).
    tier: Arc<AtomicI16>,
    ordering: Option<KeyPath>,
    dedupe: Vec<KeyPath>,
    control_id: Option<KeyPath>,
    /// What it received (shared with the API and `/metrics`).
    pub stats: Arc<InterfaceStats>,
}

impl Intake {
    /// `destinations`: the ids of the interface's destinations (`Store::destination_ids`);
    /// `tier`: its retention tier (`Retention::tier`).
    pub(crate) fn new(
        config: Arc<InterfaceConfig>,
        store: Store,
        wake: Arc<Wake>,
        gate: Arc<Gate>,
        destinations: Vec<DestinationId>,
        tier: Arc<AtomicI16>,
        stats: Arc<InterfaceStats>,
    ) -> Result<Self> {
        let compile = |path: &str| {
            keys::compile(&config.format, path).with_context(|| format!("interface `{}`: key `{path}`", config.name))
        };
        let ordering = config.ordering.key.as_deref().map(compile).transpose()?;
        let dedupe = config.dedupe.iter().flat_map(|d| d.keys.iter()).map(|k| compile(k)).collect::<Result<_>>()?;
        let control_id = (config.format == Format::Hl7).then(|| compile("MSH-10")).transpose()?;
        Ok(Self { config, store, wake, gate, destinations, tier, ordering, dedupe, control_id, stats })
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

    fn tier(&self) -> ductus_store::Tier {
        self.tier.load(Relaxed)
    }

    fn relaxed(&self) -> bool {
        self.config.durability == Durability::Relaxed
    }

    /// Wake only the lane's owners: one worker per destination. (Also after a resend, which puts
    /// a delivery back in the lane's queue.)
    pub(crate) fn wake(&self, partition: i32) {
        self.wake.lane(partition);
    }

    /// Queue an operator's corrected copy of message `original` for `destinations` (see
    /// `Store::copy_edited`). Its lane, ordering key and control id are read from the edited
    /// payload, as for any message; it's never deduplicated (sending it is the point) and isn't
    /// counted as received (no sender sent it). Not held back by the drain gate: like a resend,
    /// it adds to what a drain waits for, and someone asked for it.
    pub async fn copy_edited(
        &self,
        original: i64,
        payload: &[u8],
        destinations: &[DestinationId],
        by: ductus_store::Actor<'_>,
    ) -> Result<ductus_store::Edited> {
        let keys = self.keys(payload);
        let copy = ductus_store::Copy {
            original,
            interface: &self.config.name,
            partition_key: keys.partition,
            ordering_key: keys.ordering_key.as_deref(),
            control_id: keys.control_id.as_deref(),
            payload,
            tier: self.tier(),
            destinations,
        };
        let edited = self.store.copy_edited(copy, by).await?;
        if matches!(edited, ductus_store::Edited::Copied { .. }) {
            self.wake(keys.partition);
        }
        Ok(edited)
    }

    /// Count what storing gave: `queued` new messages, `duplicates`, or a failure (not a closed
    /// gate: that's a drain, not a problem).
    fn count<T>(&self, result: &Result<T>, queued: impl Fn(&T) -> (u64, u64)) {
        match result {
            Ok(r) => {
                let (queued, duplicates) = queued(r);
                self.stats.received.fetch_add(queued, Relaxed);
                self.stats.duplicates.fetch_add(duplicates, Relaxed);
                if queued + duplicates > 0 {
                    self.stats.last_received.now();
                }
            }
            Err(e) if e.is::<Closed>() => {}
            Err(_) => stats::add(&self.stats.store_failures),
        }
    }

    /// Store a message (deduplicating if configured) and wake its worker. Fails with `Closed`
    /// while draining.
    pub async fn accept(&self, payload: &[u8]) -> Result<Accepted> {
        let result = self.accept_one(payload).await;
        self.count(&result, |a| if matches!(a, Accepted::Queued(_)) { (1, 0) } else { (0, 1) });
        result
    }

    async fn accept_one(&self, payload: &[u8]) -> Result<Accepted> {
        let _pass = self.gate.enter()?;
        let keys = self.keys(payload);
        let new = NewMessage {
            interface: &self.config.name,
            partition_key: keys.partition,
            ordering_key: keys.ordering_key.as_deref(),
            control_id: keys.control_id.as_deref(),
            payload,
            relaxed: self.relaxed(),
            destinations: &self.destinations,
            tier: self.tier(),
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
        let result = self.accept_many(payloads, state).await;
        self.count(&result, |&queued| (queued as u64, (payloads.len() - queued) as u64));
        result
    }

    async fn accept_many(&self, payloads: &[Vec<u8>], state: Option<(&str, &str)>) -> Result<usize> {
        let _pass = self.gate.enter()?;
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
                    origin: None,
                })
                .collect(),
            dedupe_window: self.config.dedupe.as_ref().map(|d| d.window).unwrap_or_default(),
            relaxed: self.relaxed(),
            state,
            destinations: &self.destinations,
            tier: self.tier(),
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

    /// Messages another interface's destination routed here, stored together with that
    /// destination's statuses (`done`) in one transaction (`Store::finish_routed`): each is
    /// queued here exactly once. This interface's keys, dedupe and destinations apply.
    ///
    /// Not held back by the drain gate: these were received (by the interface routing them)
    /// before the drain, and a drain waits for them to be delivered.
    pub async fn accept_routed(&self, destination: DestinationId, done: &[Done], routed: &[Routed]) -> Result<()> {
        if routed.is_empty() {
            return self.store.finish(destination, done).await;
        }
        let keys: Vec<Keys> = routed.iter().map(|r| self.keys(&r.payload)).collect();
        let batch = Batch {
            interface: &self.config.name,
            messages: keys
                .iter()
                .zip(routed)
                .map(|(k, r)| BatchMessage {
                    partition_key: k.partition,
                    ordering_key: k.ordering_key.as_deref(),
                    control_id: k.control_id.as_deref(),
                    payload: &r.payload,
                    dedupe_key: k.dedupe.as_deref(),
                    origin: Some(r.origin),
                })
                .collect(),
            dedupe_window: self.config.dedupe.as_ref().map(|d| d.window).unwrap_or_default(),
            relaxed: true, // see `Store::finish_routed`
            state: None,
            destinations: &self.destinations,
            tier: self.tier(),
        };
        let queued = self.store.finish_routed(destination, done, &batch).await;
        // Not queued here: a duplicate, or already routed by an earlier try of this flush.
        self.count(&queued, |q| (q.iter().filter(|q| **q).count() as u64, 0));
        for (k, _) in keys.iter().zip(&queued?).filter(|(_, queued)| **queued) {
            self.wake(k.partition);
        }
        Ok(())
    }

    /// Queue a scheduled run (see `Store::insert_scheduled`). False if it was skipped because the
    /// previous run is still pending; `skipped_state` is saved instead of `state` then.
    pub async fn accept_scheduled(&self, payload: &[u8], key: &str, state: &str, skipped_state: &str) -> Result<bool> {
        let _pass = self.gate.enter()?;
        let new = NewMessage {
            interface: &self.config.name,
            partition_key: 0,
            ordering_key: None,
            control_id: None,
            payload,
            relaxed: self.relaxed(),
            destinations: &self.destinations,
            tier: self.tier(),
        };
        let queued = self.store.insert_scheduled(new, key, state, skipped_state).await.map(|q| q.is_some());
        self.count(&queued, |&q| (u64::from(q), 0));
        let queued = queued?;
        if queued {
            self.wake(0);
        }
        Ok(queued)
    }
}

/// A message a destination routes to another interface: what it sent, and the id of the
/// message it came from.
#[derive(Debug, Clone)]
pub struct Routed {
    pub payload: Vec<u8>,
    pub origin: i64,
}

struct Keys {
    partition: i32,
    ordering_key: Option<String>,
    control_id: Option<String>,
    dedupe: Option<String>,
}

/// Start an MLLP listener feeding `intake`, over TLS with `tls`; connections from addresses
/// `admission` refuses are closed before the handshake or any read. While its gate is closed
/// (draining) the listener is closed, so senders see the engine as down and keep their messages;
/// it opens again when the gate does.
pub async fn spawn_mllp(
    intake: Arc<Intake>,
    tls: Option<Arc<rustls::ServerConfig>>,
    admission: crate::tls::Admission,
) -> Result<()> {
    let config = intake.config.clone();
    let Source::Mllp { host, port, .. } = &config.source else { return Ok(()) };
    let (host, port) = (host.clone(), *port);
    let bind = move || {
        let host = host.clone();
        async move { TcpListener::bind((host.as_str(), port)).await.with_context(|| format!("binding {host}:{port}")) }
    };
    // The first bind is part of startup: a port in use stops the engine.
    let mut listener = Some(bind().await.with_context(|| format!("interface `{}`", config.name))?);

    let handler = {
        let intake = intake.clone();
        move |frame: Bytes| {
            let intake = intake.clone();
            async move { receive_hl7(&intake, &frame).await.map(Bytes::from) }
        }
    };
    let acceptor = tls.map(tokio_rustls::TlsAcceptor::from);
    let label = format!("interface `{}` (MLLP)", config.name);
    let open = move |tcp: TcpStream, peer: std::net::SocketAddr| {
        let (acceptor, label, admission) = (acceptor.clone(), label.clone(), admission.clone());
        async move {
            crate::allow::keepalive(&tcp);
            if admission.is_some_and(|a| !a.admits(peer)) {
                return None;
            }
            match acceptor {
                None => Some(Conn::Plain(tcp)),
                Some(a) => crate::tls::handshake(&a, &label, tcp, peer).await.map(|s| Conn::Tls(Box::new(s))),
            }
        }
    };
    tokio::spawn(async move {
        let name = &intake.config.name;
        loop {
            intake.gate.opened().await;
            let listener = match listener.take() {
                Some(l) => l,
                None => match bind().await {
                    Ok(l) => l,
                    Err(e) => {
                        tracing::error!(interface = %name, error = %format!("{e:#}"), "can't listen again after draining; retrying");
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        continue;
                    }
                },
            };
            // Debug: one line per interface is a thousand lines at every start, stop and drain of
            // a big engine; the `started` line counts them, and `ductus ports` lists them.
            tracing::debug!(interface = %name, address = %listener.local_addr().map(|a| a.to_string()).unwrap_or_default(), "listening (MLLP)");
            let stop = ductus_mllp::CancellationToken::new();
            let serve = ductus_mllp::serve_with(listener, open.clone(), handler.clone(), stop.clone());
            tokio::pin!(serve);
            tokio::select! {
                result = &mut serve => {
                    if let Err(e) = result {
                        tracing::error!(interface = %name, error = %e, "listener stopped; listening again");
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    }
                }
                _ = intake.gate.closed() => {
                    stop.cancel();
                    let _ = serve.await;
                    tracing::debug!(interface = %name, "MLLP listener closed: not taking new messages");
                }
            }
        }
    });
    Ok(())
}

/// Store an HL7 message and build its ACK. `None`: it came in after the gate closed, so the
/// connection is closed without an answer (an AE could make some senders skip the message).
async fn receive_hl7(intake: &Intake, frame: &[u8]) -> Option<String> {
    let raw = String::from_utf8_lossy(frame);
    let message = match Message::parse(&raw) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(interface = %intake.config.name, error = %e, "rejected unparseable message");
            return Some(ductus_hl7::reject_unparseable(&e.to_string(), &hl7_now()));
        }
    };
    Some(match intake.accept(frame).await {
        // Duplicates are ACKed AA too, so the sender stops resending.
        Ok(_) => message.ack(AckCode::Accept, "", &hl7_now()),
        Err(e) if e.is::<Closed>() => return None,
        Err(e) => {
            // AE tells the sender to retry: nothing was stored, so nothing is lost.
            tracing::error!(interface = %intake.config.name, error = %e, "failed to store message");
            message.ack(AckCode::Error, "temporarily unable to store message", &hl7_now())
        }
    })
}
