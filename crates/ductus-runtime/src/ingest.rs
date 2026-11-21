//! Source side: receive, persist, then acknowledge. A message is never acknowledged before
//! it's stored. `Intake` does the storing for every source (MLLP, HTTP, files...).

use crate::Wake;
use crate::gate::{Closed, Gate};
use crate::hl7_now;
use crate::keys::{self, KeyPath};
use crate::lanes::Lanes;
use crate::stats::{self, EmptyWindow, InterfaceStats};
use anyhow::{Context, Result};
use bytes::Bytes;
use ductus_core::InterfaceConfig;
use ductus_core::config::Durability;
use ductus_core::format::Format;
use ductus_core::monitor::{KeyKind, KeysSeen};
use ductus_hl7::{AckCode, Message};
use ductus_store::{Batch, BatchMessage, DedupeKey, DestinationId, Done, MessageRef, NewMessage, Store};
use std::sync::atomic::{AtomicI16, Ordering::Relaxed};
use std::sync::{Arc, RwLock};
use tokio::net::TcpStream;

/// What happened to an accepted message.
pub enum Accepted {
    /// Stored and queued for delivery.
    Queued(MessageRef),
    /// Seen within the dedupe window: stored (status `duplicate`) but not queued.
    Duplicate,
}

/// Stores incoming messages for one interface and wakes the worker that owns each one.
///
/// What it stores with (`Settings`: the interface's config, the keys compiled from it, its lanes,
/// retention tier, destinations and their workers' wake-ups) is one snapshot, read once per store
/// and swapped whole by a deploy in place (`retarget` for destinations, `reset` for a restart,
/// 0.7.5.2.4), so its source (an MLLP listener, a file pickup, an HTTP route) and the interfaces
/// routing to it keep the same intake through any change.
pub struct Intake {
    /// The interface's name (never changes).
    name: String,
    store: Store,
    settings: RwLock<Arc<Settings>>,
    /// Closed while draining: nothing new is stored.
    pub gate: Arc<Gate>,
    /// What it received (shared with the API and `/metrics`).
    pub stats: Arc<InterfaceStats>,
}

/// What an intake stores with (`Intake`): held for the whole of a store, so a swap knows when
/// nothing stores with the old one any more.
pub(crate) struct Settings {
    config: Arc<InterfaceConfig>,
    /// `ordering.key`'s parts.
    ordering: Option<Vec<KeyPath>>,
    dedupe: Vec<KeyPath>,
    control_id: Option<KeyPath>,
    /// Its lanes: messages go to the newest generation's (`crate::lanes`).
    lanes: Arc<Lanes>,
    /// The retention tier new messages go to (changes when a retention change starts).
    tier: Arc<AtomicI16>,
    targets: Targets,
    /// Which of the latest messages had empty keys (`monitor.empty_keys`): new with new keys, so a
    /// fixed path clears the interface's *keyless* at once.
    empty: Arc<EmptyKeys>,
}

/// The windows of `monitor.empty_keys`, one per key the interface has.
struct EmptyKeys {
    ordering: Option<EmptyWindow>,
    dedupe: Option<EmptyWindow>,
}

impl Settings {
    /// `destinations`: the ids of the interface's destinations (`Store::destination_ids`), with
    /// their workers' wake-ups; `tier`: its retention tier (`Retention::tier`). An error when a key
    /// path doesn't fit its format.
    pub(crate) fn new(
        config: Arc<InterfaceConfig>,
        lanes: Arc<Lanes>,
        tier: Arc<AtomicI16>,
        targets: Targets,
    ) -> Result<Self> {
        let compile = |parts: &[ductus_core::keys::KeyPart]| {
            keys::compile_all(&config.format, parts).with_context(|| format!("interface `{}`", config.name))
        };
        let ordering = config.ordering.key.as_ref().map(|k| compile(&k.0)).transpose()?;
        let dedupe = compile(config.dedupe.as_ref().map_or(&[][..], |d| &d.keys))?;
        let control_id = match config.format {
            Format::Hl7 => compile(&["MSH-10".into()])?.pop(),
            _ => None,
        };
        let window = || EmptyWindow::new(config.monitor.empty_keys.last);
        let empty = Arc::new(EmptyKeys {
            ordering: ordering.as_ref().map(|_| window()),
            dedupe: (!dedupe.is_empty()).then(window),
        });
        Ok(Self { config, ordering, dedupe, control_id, lanes, tier, targets, empty })
    }

    /// The same, queueing for `targets` instead.
    fn retargeted(&self, targets: Targets) -> Self {
        Self {
            config: self.config.clone(),
            ordering: self.ordering.clone(),
            dedupe: self.dedupe.clone(),
            control_id: self.control_id.clone(),
            lanes: self.lanes.clone(),
            tier: self.tier.clone(),
            targets,
            empty: self.empty.clone(),
        }
    }

    /// Count the keys of messages just stored (`keys`, one per message): the interface's totals
    /// and the windows `monitor.empty_keys` reads.
    fn watch<'k>(&self, stats: &InterfaceStats, keys: impl IntoIterator<Item = &'k Keys>) {
        for k in keys {
            if let (Some(window), Some(key)) = (&self.empty.ordering, &k.ordering_key) {
                window.record(key.is_empty());
                if key.is_empty() {
                    stats::add(&stats.ordering_keys_empty);
                }
            }
            if let Some(window) = &self.empty.dedupe {
                window.record(k.dedupe.is_none());
                if k.dedupe.is_none() {
                    stats::add(&stats.dedupe_keys_empty);
                }
            }
        }
    }

    /// How many of the latest messages had empty keys, per key.
    fn keys_seen(&self) -> Vec<KeysSeen> {
        let seen = |kind, window: &EmptyWindow, paths: Vec<String>| KeysSeen {
            kind,
            paths,
            empty: window.empty(),
            seen: window.seen(),
        };
        let written = |parts: &[ductus_core::keys::KeyPart]| parts.iter().map(ToString::to_string).collect();
        let ordering = self.empty.ordering.as_ref().map(|w| {
            seen(KeyKind::Ordering, w, self.config.ordering.key.as_ref().map_or_else(Vec::new, |k| written(&k.0)))
        });
        let dedupe =
            self.empty.dedupe.as_ref().map(|w| {
                seen(KeyKind::Dedupe, w, self.config.dedupe.as_ref().map_or_else(Vec::new, |d| written(&d.keys)))
            });
        ordering.into_iter().chain(dedupe).collect()
    }

    /// Read a message's lane, ordering key, control ID and dedupe key (the message parsed once for
    /// all of them).
    fn keys(&self, payload: &[u8]) -> Keys {
        let text = String::from_utf8_lossy(payload);
        let document = keys::Document::new(&self.config.format, &text);
        let read = |k: &KeyPath| document.read(k).unwrap_or_default();
        let ordering_key = self.ordering.as_ref().map(|parts| document.read_all(parts).unwrap_or_default());
        let values: Vec<String> = self.dedupe.iter().map(read).collect();
        Keys {
            partition: self.lanes.lane_of(ordering_key.as_deref().unwrap_or("")),
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

    fn dedupe_window(&self) -> std::time::Duration {
        self.config.dedupe.as_ref().map(|d| d.window).unwrap_or_default()
    }
}

impl Intake {
    pub(crate) fn new(store: Store, gate: Arc<Gate>, stats: Arc<InterfaceStats>, settings: Settings) -> Self {
        let name = settings.config.name.clone();
        Self { name, store, settings: RwLock::new(Arc::new(settings)), gate, stats }
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    /// The interface's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// How many of its latest messages had empty keys, per key it has (`monitor.empty_keys`).
    pub fn keys_seen(&self) -> Vec<KeysSeen> {
        self.settings().keys_seen()
    }

    /// What it stores with now. Held for the whole of a store (see `swap`).
    fn settings(&self) -> Arc<Settings> {
        self.settings.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Store with `settings` from now on. Returns once every store that began with the old ones
    /// has finished (committed or failed), so what follows (matching what's stored to new
    /// destinations, new workers reading their lanes) misses nothing. Answers the old ones.
    async fn swap(&self, settings: Arc<Settings>) -> Arc<Settings> {
        let old = std::mem::replace(&mut *self.settings.write().unwrap_or_else(|p| p.into_inner()), settings);
        // A store holds its settings for a database round trip or two: milliseconds.
        while Arc::strong_count(&old) > 1 {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        old
    }

    /// Queue messages for `targets`' destinations and wake their workers from now on (a deploy in
    /// place that adds or removes destinations). Returns once every store that began with the
    /// old ones has finished, so matching what's stored to the new destinations after this misses
    /// nothing (`Store::reconcile`).
    pub(crate) async fn retarget(&self, targets: Targets) {
        let next = self.settings().retargeted(targets);
        self.swap(Arc::new(next)).await;
    }

    /// Store with `settings` from now on (a deploy in place restarted the interface on new
    /// settings: format, keys, durability, lanes, retention, destinations), once every store with
    /// the old ones has finished. Answers the old ones, which `reset` takes back if the restart
    /// fails.
    pub(crate) async fn reset(&self, settings: Arc<Settings>) -> Arc<Settings> {
        self.swap(settings).await
    }

    /// Wake only the lane's owners: one worker per destination. (Also after a resend, which puts
    /// a delivery back in the lane's queue.)
    pub(crate) fn wake(&self, partition: i32) {
        self.settings().targets.wake.lane(partition);
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
        let settings = self.settings();
        let keys = settings.keys(payload);
        let copy = ductus_store::Copy {
            original,
            interface: &self.name,
            partition_key: keys.partition,
            ordering_key: keys.ordering_key.as_deref(),
            control_id: keys.control_id.as_deref(),
            payload,
            tier: settings.tier(),
            destinations,
        };
        let edited = self.store.copy_edited(copy, by).await?;
        if matches!(edited, ductus_store::Edited::Copied { .. }) {
            settings.targets.wake.lane(keys.partition);
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
        let settings = self.settings();
        let keys = settings.keys(payload);
        let targets = &settings.targets;
        let new = NewMessage {
            interface: &self.name,
            partition_key: keys.partition,
            ordering_key: keys.ordering_key.as_deref(),
            control_id: keys.control_id.as_deref(),
            payload,
            relaxed: settings.relaxed(),
            destinations: &targets.destinations,
            tier: settings.tier(),
        };
        let accepted = match (&settings.config.dedupe, &keys.dedupe) {
            (Some(d), Some(key)) => {
                let stored = self.store.insert_deduped(new, DedupeKey { key, window: d.window }).await?;
                if stored.duplicate {
                    tracing::debug!(interface = %self.name, %key, "duplicate");
                    Accepted::Duplicate
                } else {
                    Accepted::Queued(stored.message)
                }
            }
            _ => Accepted::Queued(self.store.insert(new).await?),
        };
        settings.watch(&self.stats, std::slice::from_ref(&keys));
        if matches!(accepted, Accepted::Queued(_)) {
            targets.wake.lane(keys.partition);
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
        let settings = self.settings();
        let keys: Vec<Keys> = payloads.iter().map(|p| settings.keys(p)).collect();
        let targets = &settings.targets;
        let batch = Batch {
            interface: &self.name,
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
            dedupe_window: settings.dedupe_window(),
            relaxed: settings.relaxed(),
            state,
            destinations: &targets.destinations,
            tier: settings.tier(),
        };
        let duplicates = self.store.insert_batch(&batch).await?;
        settings.watch(&self.stats, &keys);
        let mut woken = vec![false; targets.wake.slots()];
        let mut queued = 0;
        for (k, _) in keys.iter().zip(&duplicates).filter(|(_, duplicate)| !**duplicate) {
            queued += 1;
            let slot = k.partition as usize % targets.wake.slots();
            if !std::mem::replace(&mut woken[slot], true) {
                targets.wake.lane(k.partition);
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
        let settings = self.settings();
        let keys: Vec<Keys> = routed.iter().map(|r| settings.keys(&r.payload)).collect();
        let targets = &settings.targets;
        let batch = Batch {
            interface: &self.name,
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
            dedupe_window: settings.dedupe_window(),
            relaxed: true, // see `Store::finish_routed`
            state: None,
            destinations: &targets.destinations,
            tier: settings.tier(),
        };
        let queued = self.store.finish_routed(destination, done, &batch).await;
        // Not queued here: a duplicate, or already routed by an earlier try of this flush.
        self.count(&queued, |q| (q.iter().filter(|q| **q).count() as u64, 0));
        let queued = queued?;
        let stored = || keys.iter().zip(&queued).filter(|(_, queued)| **queued).map(|(k, _)| k);
        settings.watch(&self.stats, stored());
        for k in stored() {
            targets.wake.lane(k.partition);
        }
        Ok(())
    }

    /// Queue a scheduled run (see `Store::insert_scheduled`). False if it was skipped because the
    /// previous run is still pending; `skipped_state` is saved instead of `state` then.
    pub async fn accept_scheduled(&self, payload: &[u8], key: &str, state: &str, skipped_state: &str) -> Result<bool> {
        let _pass = self.gate.enter()?;
        let settings = self.settings();
        let lane = settings.lanes.lane_of("");
        let targets = &settings.targets;
        let new = NewMessage {
            interface: &self.name,
            partition_key: lane,
            ordering_key: None,
            control_id: None,
            payload,
            relaxed: settings.relaxed(),
            destinations: &targets.destinations,
            tier: settings.tier(),
        };
        let queued = self.store.insert_scheduled(new, key, state, skipped_state).await.map(|q| q.is_some());
        self.count(&queued, |&q| (u64::from(q), 0));
        let queued = queued?;
        if queued {
            targets.wake.lane(lane);
        }
        Ok(queued)
    }
}

/// Where an intake's messages go: each is queued for every one of `destinations` (the ids of the
/// interface's destinations, in config order) and wakes their workers.
pub(crate) struct Targets {
    pub destinations: Vec<DestinationId>,
    pub wake: Arc<Wake>,
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

/// Start an MLLP listener on `host:port` feeding `intake`, accepting as `accepting` says at each
/// connection (its TLS and `allow:`, which a deploy in place can swap: connections from
/// addresses it refuses are closed before the handshake or any read). While its gate is closed
/// (draining) the listener is closed, so senders see the engine as down and keep their messages;
/// it opens again when the gate does. On `stop` (the engine ended, or a deploy in place removed
/// the interface or moved its port: `sources::Sources`) it closes as for a drain (each connection
/// finishes the message in hand) and its task, returned, ends.
pub async fn spawn_mllp(
    intake: Arc<Intake>,
    (host, port): (String, u16),
    accepting: Arc<crate::accepting::Accepting>,
    stop: tokio_util::sync::CancellationToken,
) -> Result<tokio::task::JoinHandle<()>> {
    let bind = {
        let host = host.clone();
        move || {
            let host = host.clone();
            async move { crate::systemd::listen(&host, port).await.with_context(|| format!("binding {host}:{port}")) }
        }
    };
    // The first bind is part of startup: a port in use stops the engine.
    let mut listener = Some(bind().await.with_context(|| format!("interface `{}`", intake.name()))?);

    let handler = {
        let intake = intake.clone();
        move |frame: Bytes| {
            let intake = intake.clone();
            async move { receive_hl7(&intake, &frame).await.map(Bytes::from) }
        }
    };
    let open = move |tcp: TcpStream, peer: std::net::SocketAddr| {
        let accepting = accepting.clone();
        async move { accepting.accept(tcp, peer).await }
    };
    // Until `stop`: after the engine's stop its gate stays closed, and this would wait forever.
    Ok(tokio::spawn(async move {
        let name = intake.name();
        loop {
            tokio::select! {
                _ = intake.gate.opened() => {}
                _ = stop.cancelled() => return,
            }
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
            let serving = ductus_mllp::CancellationToken::new();
            let serve = ductus_mllp::serve_with(listener, open.clone(), handler.clone(), serving.clone());
            tokio::pin!(serve);
            tokio::select! {
                result = &mut serve => {
                    if let Err(e) = result {
                        tracing::error!(interface = %name, error = %e, "listener stopped; listening again");
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    }
                }
                _ = intake.gate.closed() => {
                    // A drain: closed for real (a stop keeps the socket in systemd's store).
                    crate::systemd::release(&host, port);
                    serving.cancel();
                    let _ = serve.await;
                    tracing::debug!(interface = %name, "MLLP listener closed: not taking new messages");
                }
                // The socket isn't released here: at the engine's end it stays kept for the next
                // start (or the engine taking over); a removal released it before this.
                _ = stop.cancelled() => {
                    serving.cancel();
                    let _ = serve.await;
                    return;
                }
            }
        }
    }))
}

/// Store an HL7 message and build its ACK. `None`: it came in after the gate closed, so the
/// connection is closed without an answer (an AE could make some senders skip the message).
async fn receive_hl7(intake: &Intake, frame: &[u8]) -> Option<String> {
    let raw = String::from_utf8_lossy(frame);
    let message = match Message::parse(&raw) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(interface = %intake.name(), error = %e, "rejected unparseable message");
            return Some(ductus_hl7::reject_unparseable(&e.to_string(), &hl7_now()));
        }
    };
    Some(match intake.accept(frame).await {
        // Duplicates are ACKed AA too, so the sender stops resending.
        Ok(_) => message.ack(AckCode::Accept, "", &hl7_now()),
        Err(e) if e.is::<Closed>() => return None,
        Err(e) => {
            // AE tells the sender to retry: nothing was stored, so nothing is lost.
            tracing::error!(interface = %intake.name(), error = %e, "failed to store message");
            message.ack(AckCode::Error, "temporarily unable to store message", &hl7_now())
        }
    })
}
