//! The engine's own entries in the audit log (`ductus_store::audit`): each start (and how the
//! last run on this host ended), each stop, interfaces the runaway guard stops, and the hourly
//! job that trims the log and clears copies of payloads from old message actions. What people do
//! through the API is recorded where they do it (`api/`).

use crate::control::Engine;
use ductus_store::{Audit, By, PreviousRun};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedReceiver;

/// Rows per statement when trimming: small, so each holds its rows briefly.
const BATCH: i64 = 5_000;
/// Batches per table per run; an hourly job that finds more continues the next hour.
const MAX_BATCHES: u32 = 100;
/// The longest retention tier, in days: for actions on interfaces that are no longer configured.
const LONGEST_TIER_DAYS: i32 = 49;

/// A name for this run of the engine: a stop carries the start's, so a start without one is a crash.
pub fn new_instance() -> String {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    format!("{nanos:x}-{:x}", std::process::id())
}

/// This machine's name, as well as it can be told without asking the system for more.
pub fn host_name() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .ok()
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok().map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

/// Record this start, then keep the log: interface stops as they happen, trimming every hour.
pub async fn start(
    engine: &Arc<Engine>,
    mut stopped: UnboundedReceiver<(String, String)>,
    retention_days: Vec<(String, i32)>,
) {
    let host = host_name();
    let (previous, previous_start) = match engine.store.previous_run(&host).await {
        Ok(PreviousRun::First) => ("first", None),
        Ok(PreviousRun::Stopped(at)) => ("stopped", Some(at)),
        Ok(PreviousRun::Unclean(at)) => {
            tracing::warn!(%host, started = %at, "the last run of the engine on this host did not stop cleanly (it was killed, crashed or lost power); messages in hand are processed again, as the delivery guarantee says");
            ("unclean", Some(at))
        }
        Err(e) => {
            tracing::error!(error = %format!("{e:#}"), "could not read how the engine last stopped");
            ("unknown", None)
        }
    };
    let detail = json!({
        "version": env!("CARGO_PKG_VERSION"), "host": host, "instance": engine.instance,
        "environment": engine.environment, "previous": previous, "previous_start": previous_start,
    });
    record(engine, Audit::new(By::engine(), "engine.start").detail(detail)).await;

    let store = engine.store.clone();
    tokio::spawn(async move {
        while let Some((interface, reason)) = stopped.recv().await {
            let audit =
                Audit::new(By::engine(), "interface.stopped").interface(&interface).detail(json!({ "reason": reason }));
            if let Err(e) = store.record(&audit).await {
                tracing::error!(error = %format!("{e:#}"), %interface, "could not write the audit log (interface stopped)");
            }
        }
    });

    let engine = Arc::downgrade(engine);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(60)).await;
        loop {
            let Some(engine) = engine.upgrade() else { return };
            trim(&engine, &retention_days).await;
            drop(engine);
            tokio::time::sleep(Duration::from_secs(3600)).await;
        }
    });
}

/// Record that the engine stopped (`forced`: it did not finish what was in hand in time).
pub async fn stopped(engine: &Engine, why: &str, forced: bool) {
    let detail = json!({ "instance": engine.instance, "why": why, "forced": forced });
    record(engine, Audit::new(By::engine(), "engine.stop").detail(detail)).await;
}

async fn record(engine: &Engine, audit: Audit<'_>) {
    if let Err(e) = engine.store.record(&audit).await {
        tracing::error!(action = audit.action, error = %format!("{e:#}"), "could not write the audit log");
    }
}

/// Delete what is past `audit.keep` (audit rows and the records of engines that ran that long
/// ago), and clear payload copies kept with actions on messages that are gone.
async fn trim(engine: &Engine, retention_days: &[(String, i32)]) {
    match engine.store.clear_action_outputs(retention_days, LONGEST_TIER_DAYS, BATCH, MAX_BATCHES).await {
        Ok(0) => {}
        Ok(n) => tracing::info!(
            cleared = n,
            "cleared copies of payloads from message actions older than their messages' retention"
        ),
        Err(e) => tracing::error!(error = %format!("{e:#}"), "clearing payload copies from message actions failed"),
    }
    match engine.store.trim_audit(engine.audit.keep, BATCH, MAX_BATCHES).await {
        Ok((0, 0)) => {}
        Ok((actions, rows)) => tracing::info!(actions, rows, "deleted audit records past `audit.keep`"),
        Err(e) => tracing::error!(error = %format!("{e:#}"), "trimming the audit log failed"),
    }
    // Engines' records live as long as the audit rows that name their runs.
    if let Err(e) = engine.store.trim_engines(engine.audit.keep).await {
        tracing::error!(error = %format!("{e:#}"), "trimming the engines' records failed");
    }
}
