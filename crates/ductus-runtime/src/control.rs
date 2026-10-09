//! The running engine as the REST API sees it and steers it: its interfaces and their health,
//! queue counts, drain mode, and pausing a destination.
//!
//! **Drain mode** stops taking new messages and keeps delivering what's stored, until nothing
//! is pending: for server moves, upgrades, and config changes that need an empty queue. MLLP
//! listeners close (senders see the engine as down and keep their messages), queued HTTP
//! answers 503 with `Retry-After`, file pickup and schedules pause. Respond-mode HTTP keeps
//! answering: it stores nothing. "Drained" means the gates are closed, nothing is being
//! stored, and no delivery is pending; with `stop`, the engine then exits.

use crate::activity::{Activity, Pause, Paused};
use crate::gate::Gate;
use crate::health::Health;
use anyhow::Result;
use chrono::{DateTime, Utc};
use ductus_core::impact::CodeUses;
use ductus_core::monitor::{self, Diagnosis, Facts, InterfaceState};
use ductus_core::{InterfaceConfig, Resources};
use ductus_store::{DestinationId, QueueCount, Store};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{OnceCell, watch};
use tokio_util::sync::CancellationToken;

/// How often the monitor counts queues while someone is watching (or draining).
const MONITOR_EVERY: Duration = Duration::from_secs(2);

/// One running interface.
pub struct Running {
    pub config: Arc<InterfaceConfig>,
    pub health: Arc<Health>,
    pub gate: Arc<Gate>,
    /// What it received and delivered since the engine started.
    pub stats: Arc<crate::stats::InterfaceStats>,
    /// Its destinations' names and store ids, in config order.
    pub destinations: Vec<(String, DestinationId)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrainState {
    /// Taking messages as usual.
    Accepting,
    /// Not taking new messages; delivering what's stored.
    Draining { since: DateTime<Utc>, stop: bool },
    /// Nothing left to deliver (and not taking new messages).
    Drained { since: DateTime<Utc>, stop: bool },
}

/// Queue counts at a moment, by destination id.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    pub at: DateTime<Utc>,
    pub queues: HashMap<DestinationId, QueueCount>,
    /// When the oldest waiting message was received, for destinations with any.
    pub oldest: HashMap<DestinationId, DateTime<Utc>>,
}

impl Snapshot {
    /// How long the oldest message waiting for a destination has waited.
    pub fn oldest_age(&self, destination: DestinationId) -> Option<Duration> {
        self.oldest.get(&destination).map(|t| (self.at - *t).to_std().unwrap_or_default())
    }

    pub fn queue(&self, destination: DestinationId) -> QueueCount {
        self.queues.get(&destination).copied().unwrap_or(QueueCount {
            destination_id: destination,
            pending: 0,
            quarantined: 0,
        })
    }
}

pub struct Engine {
    pub started_at: DateTime<Utc>,
    pub environment: String,
    pub root: PathBuf,
    pub database_url: String,
    pub store: Store,
    pub resources: Resources,
    pub interfaces: Vec<Running>,
    pub retention: Arc<crate::retention::Retention>,
    /// Calls to each database and API in resources.yaml (`fetch` for URLs called directly).
    pub calls: Arc<crate::stats::ResourceCalls>,
    /// When each interface was first watched, and every destination's pause switch.
    pub activity: Activity,
    drain: watch::Sender<DrainState>,
    monitor: watch::Sender<Arc<Snapshot>>,
    /// What each interface's code uses, worked out on first request (it needs the TypeScript
    /// analysis, which takes a moment; code doesn't change while the engine runs).
    code: OnceCell<CodeUses>,
    /// Cancelled when the engine should stop (drained with `stop`).
    pub shutdown: CancellationToken,
}

/// What the engine is made of, from startup.
pub struct Parts {
    pub environment: String,
    pub root: PathBuf,
    pub database_url: String,
    pub store: Store,
    pub resources: Resources,
    pub interfaces: Vec<Running>,
    pub retention: Arc<crate::retention::Retention>,
    pub calls: Arc<crate::stats::ResourceCalls>,
    pub activity: Activity,
}

impl Engine {
    pub fn new(parts: Parts) -> Arc<Self> {
        let Parts { environment, root, database_url, store, resources, interfaces, retention, calls, activity } = parts;
        let engine = Arc::new(Self {
            started_at: Utc::now(),
            environment,
            root,
            database_url,
            store,
            resources,
            interfaces,
            retention,
            calls,
            activity,
            drain: watch::Sender::new(DrainState::Accepting),
            monitor: watch::Sender::new(Arc::default()),
            code: OnceCell::new(),
            shutdown: CancellationToken::new(),
        });
        tokio::spawn(monitor(Arc::downgrade(&engine)));
        engine
    }

    pub fn interface(&self, name: &str) -> Option<&Running> {
        self.interfaces.iter().find(|i| i.config.name == name)
    }

    fn destination_ids(&self) -> impl Iterator<Item = DestinationId> + '_ {
        self.interfaces.iter().flat_map(|i| i.destinations.iter().map(|(_, id)| *id))
    }

    /// A destination's pause, if it's paused.
    pub fn paused(&self, destination: DestinationId) -> Option<Paused> {
        self.activity.pauses.get(&destination).and_then(|p| p.get())
    }

    /// An interface's destination by name: `Err` says what's wrong in plain words (no such
    /// interface or destination).
    pub fn destination(&self, interface: &str, destination: &str) -> Result<(&Running, DestinationId), String> {
        let i = self.interface(interface).ok_or_else(|| format!("no interface named `{interface}`"))?;
        match i.destinations.iter().find(|(name, _)| name == destination) {
            Some((_, id)) => Ok((i, *id)),
            None if i.destinations.is_empty() => Err(format!("`{interface}` has no destinations (respond mode)")),
            None => {
                let names: Vec<&str> = i.destinations.iter().map(|(n, _)| n.as_str()).collect();
                Err(format!("`{interface}` has no destination `{destination}` (it has {})", names.join(", ")))
            }
        }
    }

    fn pause_switch(&self, destination: DestinationId) -> Result<&Arc<Pause>> {
        self.activity.pauses.get(&destination).ok_or_else(|| anyhow::anyhow!("destination {destination} isn't watched"))
    }

    /// Pause a destination: from now on its workers send nothing and retry nothing (a send under
    /// way finishes). No questions asked: a reason is optional, and can be added later by
    /// pausing again with one. Saved first, so it holds across a restart.
    pub async fn pause(&self, destination: DestinationId, by: &str, reason: Option<&str>) -> Result<Paused> {
        let row = self.store.pause(destination, by, reason).await?;
        let switch = self.pause_switch(destination)?;
        switch.set(row);
        self.monitor.send_modify(|_| {}); // live updates show it at once
        switch.get().ok_or_else(|| anyhow::anyhow!("the pause wasn't saved"))
    }

    /// Resume a paused destination; returns the pause it ended (`None`: it wasn't paused).
    pub async fn unpause(&self, destination: DestinationId) -> Result<Option<Paused>> {
        let switch = self.pause_switch(destination)?;
        let ended = switch.get();
        let row = self.store.unpause(destination).await?;
        switch.set(Default::default());
        self.monitor.send_modify(|_| {});
        // Paused by another engine sharing the database: what was stored.
        Ok(ended.or_else(|| Paused::from_row(row)))
    }

    /// This engine's queue counts now (one quick query). Destinations the database knows from
    /// elsewhere (another engine, an older config) are left out, so they never look like a
    /// change here.
    pub async fn snapshot(&self) -> Result<Snapshot> {
        let mine: std::collections::HashSet<DestinationId> = self.destination_ids().collect();
        let counts = self.store.queue_counts().await?;
        let queues: HashMap<_, _> =
            counts.into_iter().filter(|c| mine.contains(&c.destination_id)).map(|c| (c.destination_id, c)).collect();
        let waiting: Vec<DestinationId> = queues.values().filter(|q| q.pending > 0).map(|q| q.destination_id).collect();
        let oldest = self.store.oldest_pending(&waiting).await?;
        let oldest = oldest.into_iter().map(|o| (o.destination_id, o.received_at)).collect();
        Ok(Snapshot { at: Utc::now(), queues, oldest })
    }

    /// An interface's own state: what it should have received (`monitor:`), or stopped.
    pub fn interface_state(&self, i: &Running, now: DateTime<Utc>) -> (InterfaceState, String) {
        let watched_since = self.activity.watched_since.get(&i.config.name).copied().unwrap_or(self.started_at);
        let stopped = i.health.stopped();
        monitor::interface_state(&i.config.monitor, i.stats.last_received.get(), watched_since, stopped, now)
    }

    /// A destination's state and why (`n`: its place in `i.destinations`).
    pub fn diagnose(&self, i: &Running, n: usize, snapshot: &Snapshot, now: DateTime<Utc>) -> Diagnosis {
        let (Some((_, id)), Some(stats), Some((_, config))) =
            (i.destinations.get(n), i.stats.destinations.get(n), i.config.destinations().nth(n))
        else {
            return monitor::diagnose(&Facts::default(), now);
        };
        let queue = snapshot.queue(*id);
        let (recent, normal_inflow, normal_send_ms) = self.activity.trend(*id, now);
        let last_error = stats.last_error();
        let paused = self.paused(*id).map(|p| {
            let day = |t: DateTime<Utc>| t.with_timezone(&chrono::Local).date_naive();
            let at = if day(p.at) == day(now) { "%H:%M" } else { "%Y-%m-%d %H:%M" };
            let at = p.at.with_timezone(&chrono::Local).format(at);
            match p.reason {
                Some(reason) => format!("paused by {} at {at}: {reason}", p.by),
                None => format!("paused by {} at {at} (no reason yet)", p.by),
            }
        });
        let facts = Facts {
            waiting: queue.pending.max(0) as u64,
            oldest: snapshot.oldest_age(*id),
            quarantined: queue.quarantined.max(0) as u64,
            last_sent: stats.last_sent.get(),
            last_error: last_error.as_ref().map(|(at, e)| (*at, e.as_str())),
            paused,
            stopped: i.health.stopped(),
            recent,
            behind: i.config.behind_for(config),
            normal_inflow,
            normal_send_ms,
        };
        monitor::diagnose(&facts, now)
    }

    /// Queue counts as they change, every few seconds while anyone is subscribed.
    pub fn watch_queues(&self) -> watch::Receiver<Arc<Snapshot>> {
        // Wake the monitor at once for its first subscriber; subscribe after, so the new
        // subscriber doesn't take the nudge for a change.
        self.monitor.send_modify(|_| {});
        self.monitor.subscribe()
    }

    pub fn drain_state(&self) -> DrainState {
        *self.drain.borrow()
    }

    pub fn watch_drain(&self) -> watch::Receiver<DrainState> {
        self.drain.subscribe()
    }

    /// Stop taking messages and deliver what's stored; with `stop`, exit once drained.
    /// Draining again only updates `stop`.
    pub fn drain(&self, stop: bool) {
        let now = Utc::now();
        self.drain.send_modify(|s| {
            *s = match *s {
                DrainState::Accepting => DrainState::Draining { since: now, stop },
                DrainState::Draining { since, .. } => DrainState::Draining { since, stop },
                DrainState::Drained { since, .. } => DrainState::Drained { since, stop },
            }
        });
        for i in &self.interfaces {
            i.gate.set_open(false);
        }
        tracing::warn!(stop, "draining: not taking new messages; delivering what's stored");
        if stop && matches!(self.drain_state(), DrainState::Drained { .. }) {
            self.shutdown.cancel();
        }
    }

    /// Take messages again.
    pub fn resume(&self) {
        if self.drain.send_replace(DrainState::Accepting) != DrainState::Accepting {
            tracing::info!("resumed: taking new messages again");
        }
        for i in &self.interfaces {
            i.gate.set_open(true);
        }
    }

    /// Pending deliveries of this engine's interfaces, if nothing can add more: every gate
    /// closed and nothing being stored. `None` while a store is still in flight.
    async fn left_to_deliver(&self) -> Result<Option<i64>> {
        if self.interfaces.iter().any(|i| i.gate.is_open() || i.gate.in_flight() > 0) {
            return Ok(None);
        }
        let snapshot = self.snapshot().await?;
        // A paused destination keeps its messages on purpose: it doesn't hold the drain up.
        Ok(Some(self.destination_ids().filter(|d| self.paused(*d).is_none()).map(|d| snapshot.queue(d).pending).sum()))
    }

    /// What each interface's code uses (`db`/`api` names), worked out once.
    pub async fn code_uses(&self) -> Result<&CodeUses> {
        self.code
            .get_or_try_init(|| async {
                let root = self.root.clone();
                let configs: Vec<InterfaceConfig> = self.interfaces.iter().map(|i| (*i.config).clone()).collect();
                // The analysis runs the TypeScript compiler in V8: on a thread of its own, so
                // it never holds up the runtime that serves intake.
                let (tx, rx) = tokio::sync::oneshot::channel();
                std::thread::Builder::new().name("code-analysis".into()).spawn(move || {
                    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build();
                    let result = runtime.map_err(anyhow::Error::from).and_then(|rt| {
                        let dirs: Vec<&std::path::Path> = configs.iter().map(|c| c.dir.as_path()).collect();
                        let refs: Vec<&InterfaceConfig> = configs.iter().collect();
                        rt.block_on(crate::project::code_uses(&root, &dirs, &refs))
                    });
                    let _ = tx.send(result);
                })?;
                rx.await?
            })
            .await
    }
}

/// Counts queues every few seconds while anyone watches them, and moves a drain to drained.
/// Ends with the engine.
async fn monitor(engine: std::sync::Weak<Engine>) {
    loop {
        let Some(e) = engine.upgrade() else { return };
        let watched = e.monitor.receiver_count() > 0;
        let draining = matches!(e.drain_state(), DrainState::Draining { .. });
        if watched {
            match e.snapshot().await {
                Ok(snapshot) => {
                    e.monitor.send_if_modified(|s| {
                        // Same counts: keep the old snapshot, so watchers only hear of changes.
                        let changed = s.queues != snapshot.queues;
                        if changed {
                            *s = Arc::new(snapshot);
                        }
                        changed
                    });
                }
                Err(e) => tracing::warn!(error = %format!("{e:#}"), "counting queues failed"),
            }
        }
        if draining {
            match e.left_to_deliver().await {
                Ok(Some(0)) => {
                    let mut stop = false;
                    e.drain.send_modify(|s| {
                        if let DrainState::Draining { stop: s_stop, .. } = *s {
                            stop = s_stop;
                            *s = DrainState::Drained { since: Utc::now(), stop };
                        }
                    });
                    tracing::warn!(stop, "drained: nothing left to deliver");
                    if stop {
                        e.shutdown.cancel();
                    }
                }
                Ok(_) => {}
                Err(err) => tracing::warn!(error = %format!("{err:#}"), "checking the drain failed"),
            }
        }
        let mut changes = e.monitor.subscribe();
        let mut drain = e.drain.subscribe();
        drop(e); // don't keep the engine alive while sleeping
        tokio::select! {
            _ = tokio::time::sleep(if draining { Duration::from_millis(500) } else { MONITOR_EVERY }) => {}
            _ = changes.changed(), if !watched => {} // a first subscriber came
            _ = drain.changed() => {}
        }
    }
}
