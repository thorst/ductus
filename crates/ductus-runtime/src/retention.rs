//! Retention as the engine runs it: each interface's configured retention is applied at
//! startup (one change at a time; shorter ones wait for a confirmation), changes run in the
//! background, and expired partitions are dropped every hour, their unfinished messages moved
//! to the held area first. See `ductus_store::retention`.

use anyhow::Result;
use chrono::{DateTime, Utc};
use ductus_store::{HeldArea, Kept, RetentionState, Store, Tier};
use std::sync::atomic::{AtomicI16, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::Notify;

/// Messages per batch of a change: small, so each statement holds its rows briefly.
const BATCH: i64 = 1_000;
/// How often expired partitions are dropped.
const DROP_EVERY: Duration = Duration::from_secs(3600);

/// One interface whose messages are stored (respond-mode interfaces store none).
pub struct Managed {
    pub name: String,
    /// What interface.yaml (or ductus.yaml) says.
    pub configured: Tier,
    /// The tier its new messages go to; intake reads it for every insert.
    pub tier: Arc<AtomicI16>,
}

pub struct Retention {
    store: Store,
    pub interfaces: Vec<Managed>,
    /// Wakes the change runner (a confirmation came in).
    kick: Arc<Notify>,
    /// Wakes the partition dropper (a change finished: partitions it emptied can go).
    drop_now: Arc<Notify>,
    /// The last drop run.
    last_drop: Mutex<Option<LastDrop>>,
}

/// What the last hourly run found.
#[derive(Debug, Clone)]
pub struct LastDrop {
    pub at: DateTime<Utc>,
    /// Expired partitions it had to keep, and why.
    pub kept: Vec<Kept>,
    /// The held area afterwards.
    pub held: HeldArea,
}

/// One interface's retention as configured.
pub struct Configured {
    pub name: String,
    pub days: Tier,
    /// `quarantined: purge`.
    pub purge_quarantined: bool,
}

impl Retention {
    /// Apply every interface's configured retention and start the background work.
    pub async fn start(store: Store, configured: Vec<Configured>) -> Result<Arc<Self>> {
        let mut interfaces = Vec::with_capacity(configured.len());
        let wanted: Vec<(String, Tier)> = configured.iter().map(|c| (c.name.clone(), c.days)).collect();
        let states = store.apply_retention_all(&wanted).await?;
        for (c, state) in configured.iter().zip(states) {
            report(&state, c.days);
            // A change that just began is a decision about patient data worth keeping a record of.
            if state.changing_from.is_some() && Utc::now() - state.changed_at < chrono::TimeDelta::minutes(1) {
                let detail =
                    serde_json::json!({ "from": state.changing_from, "to": state.days, "waiting": state.waiting });
                let audit = ductus_store::Audit::new(ductus_store::By::engine(), "retention.change")
                    .interface(&c.name)
                    .detail(detail);
                if let Err(e) = store.record(&audit).await {
                    tracing::error!(error = %format!("{e:#}"), interface = %c.name, "could not write the audit log (retention change)");
                }
            }
            interfaces.push(Managed {
                name: c.name.clone(),
                configured: c.days,
                tier: Arc::new(AtomicI16::new(state.days)),
            });
        }
        let purge: Vec<(String, bool)> = configured.into_iter().map(|c| (c.name, c.purge_quarantined)).collect();
        store.set_quarantined(&purge).await?;
        let retention = Arc::new(Self {
            store,
            interfaces,
            kick: Arc::new(Notify::new()),
            drop_now: Arc::new(Notify::new()),
            last_drop: Mutex::new(None),
        });
        tokio::spawn(run_changes(Arc::downgrade(&retention)));
        tokio::spawn(drop_expired(Arc::downgrade(&retention)));
        Ok(retention)
    }

    pub fn get(&self, interface: &str) -> Option<&Managed> {
        self.interfaces.iter().find(|m| m.name == interface)
    }

    /// The tier an interface's new messages go to (shared with its intake).
    pub fn tier(&self, interface: &str) -> Arc<AtomicI16> {
        self.get(interface)
            .map(|m| m.tier.clone())
            .unwrap_or_else(|| Arc::new(AtomicI16::new(ductus_core::Retention::DEFAULT.days() as Tier)))
    }

    /// This engine's interfaces' retention, as stored.
    pub async fn states(&self) -> Result<Vec<RetentionState>> {
        let mut states = self.store.retention().await?;
        states.retain(|s| self.get(&s.interface).is_some());
        Ok(states)
    }

    /// How many messages the interface's waiting, shorter retention would delete now.
    pub async fn would_delete(&self, state: &RetentionState) -> Result<Option<i64>> {
        match state.waiting {
            Some(w) if w < state.days => Ok(Some(self.store.retention_would_delete(&state.interface, w).await?)),
            _ => Ok(None),
        }
    }

    /// Confirm an interface's waiting, shorter retention: it's applied now, or once the running
    /// change finishes. `None` if nothing waits for a confirmation.
    pub async fn confirm(&self, interface: &str, by: &str) -> Result<Option<RetentionState>> {
        let Some(m) = self.get(interface) else { return Ok(None) };
        let Some(confirmed) = self.store.confirm_retention(interface).await? else { return Ok(None) };
        tracing::warn!(%interface, retention = confirmed.confirmed.unwrap_or_default(), by, "shorter retention confirmed: messages past it will be deleted");
        let state = self.store.apply_retention(interface, m.configured).await?;
        m.tier.store(state.days, Relaxed);
        self.kick.notify_one();
        Ok(Some(state))
    }

    /// When expired partitions were last dropped, those that had to stay, and the held area.
    pub fn last_drop(&self) -> Option<LastDrop> {
        self.last_drop.lock().unwrap().clone()
    }

    /// The hourly run: drop expired partitions; move the unfinished messages of the ones that
    /// had to stay to the held area, then drop those too; drop the held area's finished months.
    async fn tidy(&self) -> Result<LastDrop> {
        let mut expired = self.store.drop_expired().await?;
        let mut moved_any = false;
        for kept in expired.kept.iter().filter(|k| k.to_hold()) {
            match self.hold(&kept.partition).await {
                Ok(moved) => {
                    moved_any |= moved > 0;
                    tracing::info!(
                        partition = %kept.partition,
                        moved,
                        pending = kept.pending,
                        quarantined = kept.quarantined,
                        "expired partition: messages still needed moved to the held area"
                    );
                }
                Err(e) => {
                    tracing::warn!(partition = %kept.partition, error = %format!("{e:#}"), "moving messages to the held area failed; trying again in an hour")
                }
            }
        }
        if moved_any {
            let again = self.store.drop_expired().await?;
            expired.dropped.extend(again.dropped);
            expired.kept = again.kept;
        }
        if !expired.dropped.is_empty() {
            tracing::info!(partitions = %expired.dropped.join(", "), "expired partitions dropped");
        }
        let finished = self.store.drop_finished_held().await?;
        if !finished.is_empty() {
            tracing::info!(partitions = %finished.join(", "), "held area: months with nothing left to deliver dropped");
        }
        let held = self.store.held_area().await?;
        Ok(LastDrop { at: Utc::now(), kept: expired.kept, held })
    }

    /// Move an expired partition's unfinished messages to the held area, in batches, busy at
    /// most half the time (it can be millions after a long outage). Returns how many moved.
    async fn hold(&self, partition: &str) -> Result<u64> {
        let (mut after, mut moved) = (0, 0);
        loop {
            let started = Instant::now();
            let (n, last) = self.store.hold_step(partition, after, BATCH).await?;
            moved += n;
            let Some(last) = last else { return Ok(moved) };
            after = last;
            tokio::time::sleep(started.elapsed().max(Duration::from_millis(20))).await;
        }
    }

    /// Run one interface's change to the end: move (or delete) its rows in other tiers, in
    /// batches, busy at most half the time; then apply what's configured now (a value that
    /// waited may start the next change).
    async fn run_change(&self, m: &Managed, state: &RetentionState) -> Result<()> {
        let (name, days) = (&m.name, state.days);
        let rows = self.store.start_retention_change(name, days).await?;
        tracing::info!(interface = %name, from = state.changing_from.unwrap_or_default(), to = days, rows, "retention change: moving messages into the new tier (deleting those past it)");
        let (mut deleted, mut moved) = (0, 0);
        loop {
            let started = Instant::now();
            let step = self.store.retention_step(name, days, BATCH).await?;
            if step.is_empty() {
                let waiting = self.store.retention_waiting_for_delivery(name, days).await?;
                if waiting == 0 {
                    break;
                }
                // They move once delivered.
                tokio::time::sleep(Duration::from_secs(10)).await;
                continue;
            }
            deleted += step.deleted;
            moved += step.moved;
            tokio::time::sleep(started.elapsed().max(Duration::from_millis(20))).await;
        }
        let left = self.store.finish_retention_change(name).await?;
        tracing::info!(interface = %name, retention = days, moved, deleted, kept_in_old_tier = left, "retention change done");
        if left > 0 {
            tracing::info!(interface = %name, count = left, "messages past the new retention that were pending or quarantined stay in their old tier (or the held area) until they're finished");
        }
        self.drop_now.notify_one();
        let next = self.store.apply_retention(name, m.configured).await?;
        m.tier.store(next.days, Relaxed);
        report(&next, m.configured);
        Ok(())
    }
}

/// Log what applying a retention did, when there's something to say.
fn report(state: &RetentionState, configured: Tier) {
    let name = &state.interface;
    if let Some(from) = state.changing_from {
        tracing::info!(interface = %name, from, to = state.days, "retention change running");
    }
    if state.needs_confirmation() {
        tracing::warn!(
            interface = %name,
            applied = state.days,
            configured,
            "shorter retention not applied: it deletes messages, so it needs a confirmation (`ductus retention confirm {name}`)"
        );
    } else if let Some(waiting) = state.waiting {
        tracing::warn!(interface = %name, applied = state.days, configured = waiting, "retention change still running: the new value waits for it");
    }
}

/// Runs changes as they come (at startup, after confirmations, after a change finishes). Ends
/// with the engine.
async fn run_changes(retention: std::sync::Weak<Retention>) {
    loop {
        let Some(r) = retention.upgrade() else { return };
        match r.store.retention().await {
            Ok(states) => {
                for state in states.iter().filter(|s| s.changing_from.is_some()) {
                    let Some(m) = r.get(&state.interface) else { continue };
                    if let Err(e) = r.run_change(m, state).await {
                        tracing::warn!(interface = %m.name, error = %format!("{e:#}"), "retention change interrupted; trying again in a minute");
                    }
                }
            }
            Err(e) => tracing::warn!(error = %format!("{e:#}"), "reading retention failed"),
        }
        let kick = r.kick.clone();
        drop(r); // don't keep the engine's retention alive while waiting
        tokio::select! {
            _ = kick.notified() => {}
            _ = tokio::time::sleep(Duration::from_secs(60)) => {}
        }
    }
}

/// Drops expired partitions (`Retention::tidy`) every hour and after each retention change, the
/// first time just after startup.
async fn drop_expired(retention: std::sync::Weak<Retention>) {
    let mut wait = Duration::from_secs(5);
    loop {
        let Some(now) = retention.upgrade().map(|r| r.drop_now.clone()) else { return };
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = now.notified() => {}
        }
        wait = DROP_EVERY;
        let Some(r) = retention.upgrade() else { return };
        match r.tidy().await {
            Ok(run) => {
                for kept in &run.kept {
                    tracing::warn!(
                        partition = %kept.partition,
                        pending = kept.pending,
                        quarantined = kept.quarantined,
                        kept_for = %kept.kept_for.join(", "),
                        "expired partition kept: it still holds messages someone needs"
                    );
                }
                let quarantined: i64 = run.held.interfaces.iter().map(|i| i.quarantined).sum();
                if quarantined > 0 {
                    tracing::warn!(
                        quarantined,
                        "quarantined messages past their retention are kept in the held area until someone resolves them"
                    );
                }
                *r.last_drop.lock().unwrap() = Some(run);
            }
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "dropping expired partitions failed; trying again in an hour")
            }
        }
    }
}
