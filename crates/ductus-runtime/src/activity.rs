//! Monitoring state that survives restarts and failover: when each interface last received
//! something and each destination last sent or failed (kept in `stats.rs` as it happens, saved
//! here every few seconds), and destinations paused on purpose (saved at once).
//!
//! Saving is one task and two statements every `SAVE_EVERY`, whatever the traffic: the cost
//! doesn't grow with volume, and a crash loses at most those seconds of "last seen".

use crate::stats::InterfaceStats;
use anyhow::Result;
use chrono::{DateTime, Utc};
use ductus_store::{DestinationId, PauseRow, Seen, Store};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

const SAVE_EVERY: Duration = Duration::from_secs(5);

/// A destination paused on purpose: who did it and when, and why once someone says.
#[derive(Debug, Clone, PartialEq)]
pub struct Paused {
    pub at: DateTime<Utc>,
    pub by: String,
    pub reason: Option<String>,
    pub reason_by: Option<String>,
    pub reason_at: Option<DateTime<Utc>>,
}

impl Paused {
    pub fn from_row(row: PauseRow) -> Option<Self> {
        Some(Self {
            at: row.paused_at?,
            by: row.paused_by.unwrap_or_default(),
            reason: row.pause_reason,
            reason_by: row.reason_by,
            reason_at: row.reason_at,
        })
    }
}

/// A destination's pause switch, shared by its workers (every slot) and the API.
#[derive(Debug, Default)]
pub struct Pause(watch::Sender<Option<Paused>>);

impl Pause {
    pub fn get(&self) -> Option<Paused> {
        self.0.borrow().clone()
    }

    pub fn is_paused(&self) -> bool {
        self.0.borrow().is_some()
    }

    /// Set from a stored row (all `None`: not paused).
    pub fn set(&self, row: PauseRow) {
        self.0.send_replace(Paused::from_row(row));
    }

    /// Returns once the destination isn't paused (at once if it isn't).
    pub async fn until_resumed(&self) {
        let mut state = self.0.subscribe();
        let _ = state.wait_for(Option::is_none).await;
    }
}

/// An interface to watch: its counts (with their last-seen times) and its destinations' ids, in
/// `config.destinations()` order.
pub struct Watched {
    pub stats: Arc<InterfaceStats>,
    pub destinations: Vec<DestinationId>,
}

/// What was loaded at startup.
pub struct Activity {
    /// When an engine first ran each interface: deadlines before it don't count.
    pub watched_since: HashMap<String, DateTime<Utc>>,
    /// Every destination's pause switch.
    pub pauses: HashMap<DestinationId, Arc<Pause>>,
}

impl Activity {
    /// Load what's stored into the counts' last-seen times and the pause switches, then save
    /// changes every few seconds for as long as the engine runs.
    pub async fn start(store: Store, watched: Vec<Watched>) -> Result<Self> {
        let names: Vec<&str> = watched.iter().map(|w| w.stats.name.as_str()).collect();
        let ids: Vec<DestinationId> = watched.iter().flat_map(|w| w.destinations.iter().copied()).collect();
        let (interfaces, destinations) = store.activity(&names, &ids).await?;
        let by_id: HashMap<DestinationId, _> = destinations.into_iter().map(|d| (d.destination_id, d)).collect();
        let mut pauses = HashMap::new();
        for w in &watched {
            for (id, stats) in w.destinations.iter().zip(&w.stats.destinations) {
                let pause = Arc::new(Pause::default());
                if let Some(stored) = by_id.get(id) {
                    if let Some(t) = stored.last_sent {
                        stats.last_sent.at(t);
                    }
                    if let (Some(at), Some(error)) = (stored.last_error_at, &stored.last_error) {
                        *stats.last_error.lock().unwrap_or_else(|p| p.into_inner()) = Some((at, error.clone()));
                    }
                    pause.set(stored.pause.clone());
                    if let Some(p) = pause.get() {
                        tracing::warn!(interface = %w.stats.name, destination = %stats.name, by = %p.by, since = %p.at, "destination is paused: nothing is sent until `ductus resume`");
                    }
                }
                pauses.insert(*id, pause);
            }
        }
        let mut watched_since = HashMap::new();
        for i in interfaces {
            if let Some(w) = watched.iter().find(|w| w.stats.name == i.interface)
                && let Some(t) = i.last_received
            {
                w.stats.last_received.at(t);
            }
            watched_since.insert(i.interface, i.watched_since);
        }
        tokio::spawn(save(store, watched));
        Ok(Self { watched_since, pauses })
    }
}

/// Every few seconds, save the last-seen times that changed.
async fn save(store: Store, watched: Vec<Watched>) {
    // What was saved last (or loaded): only changes are written.
    let mut received: HashMap<String, Option<DateTime<Utc>>> =
        watched.iter().map(|w| (w.stats.name.clone(), w.stats.last_received.get())).collect();
    // (last sent, last error's time) per destination.
    type Saved = (Option<DateTime<Utc>>, Option<DateTime<Utc>>);
    let mut seen: HashMap<DestinationId, Saved> = HashMap::new();
    for w in &watched {
        for (id, s) in w.destinations.iter().zip(&w.stats.destinations) {
            seen.insert(*id, (s.last_sent.get(), s.last_error().map(|e| e.0)));
        }
    }
    loop {
        tokio::time::sleep(SAVE_EVERY).await;
        let mut changed_received = Vec::new();
        let mut changed_seen = Vec::new();
        for w in &watched {
            let now = w.stats.last_received.get();
            if let Some(t) = now
                && received.get(&w.stats.name) != Some(&now)
            {
                changed_received.push((w.stats.name.clone(), t));
            }
            for (id, s) in w.destinations.iter().zip(&w.stats.destinations) {
                let (sent, error) = (s.last_sent.get(), s.last_error());
                if seen.get(id) != Some(&(sent, error.as_ref().map(|e| e.0))) {
                    changed_seen.push(Seen { destination: *id, last_sent: sent, last_error: error });
                }
            }
        }
        if changed_received.is_empty() && changed_seen.is_empty() {
            continue;
        }
        let names: Vec<(&str, DateTime<Utc>)> = changed_received.iter().map(|(n, t)| (n.as_str(), *t)).collect();
        match store.save_activity(&names, &changed_seen).await {
            Ok(()) => {
                for (name, t) in changed_received {
                    received.insert(name, Some(t));
                }
                for s in changed_seen {
                    seen.insert(s.destination, (s.last_sent, s.last_error.map(|e| e.0)));
                }
            }
            // Tried again next time: the times are still in memory.
            Err(e) => tracing::warn!(error = %format!("{e:#}"), "saving last activity failed"),
        }
    }
}
