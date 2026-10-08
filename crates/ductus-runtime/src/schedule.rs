//! Schedule sources. Each run is queued as a message carrying the run's details
//! (`{"due": ..., "previous": ...}`); the interface's worker runs `transform` on it like any other
//! message, so a run gets the same guarantees: it survives a crash (and runs after restart),
//! it's retried on temporary failures, and it's in the message log.
//!
//! Runs are one at a time: if the previous run is still pending when the next one is due, the
//! new one is skipped. A run missed while the engine was down runs once on start.

use crate::gate::Closed;
use crate::ingest::Intake;
use anyhow::{Context, Result};
use chrono::{DateTime, Local, Utc};
use ductus_core::schedule::Timing;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

const STATE_KEY: &str = "schedule";
/// Re-check the clock at least this often while waiting, so a clock change or a suspended VM
/// doesn't delay a run by long.
const MAX_SLEEP: Duration = Duration::from_secs(60);
const RETRY: Duration = Duration::from_secs(5);

#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    /// The next run is the first time after this.
    anchor: Option<DateTime<Utc>>,
    /// Due time of the last run that was queued: the `previous` the next run sees.
    previous: Option<DateTime<Utc>>,
}

pub async fn run(intake: Arc<Intake>, timing: Timing) {
    let name = intake.config.name.clone();
    tracing::info!(interface = %name, "schedule started");
    loop {
        match next_run(&intake, &timing).await {
            Ok(()) => {}
            // Draining: the run waits for the gate to open (and runs once then, however late).
            Err(e) if e.is::<Closed>() => {}
            Err(e) => {
                tracing::error!(interface = %name, error = %format!("{e:#}"), "schedule failed; retrying");
                tokio::time::sleep(RETRY).await;
            }
        }
    }
}

/// Wait for the next run and queue it.
async fn next_run(intake: &Intake, timing: &Timing) -> Result<()> {
    let name = &intake.config.name;
    let state: State = match intake.store().source_state(name, STATE_KEY).await? {
        Some(json) => serde_json::from_str(&json).context("reading schedule state")?,
        None => State::default(),
    };
    let due = timing.next_due(state.anchor, Utc::now())?;
    loop {
        let wait = (due - Utc::now()).to_std().unwrap_or_default();
        if wait.is_zero() {
            break;
        }
        tokio::time::sleep(wait.min(MAX_SLEEP)).await;
    }

    intake.gate.opened().await;
    let now = Utc::now();
    let local = |t: DateTime<Utc>| t.with_timezone(&Local).to_rfc3339();
    let payload = serde_json::json!({ "due": local(due), "previous": state.previous.map(local) }).to_string();
    let anchor = Some(timing.anchor_after(due, now)?);
    let queued = serde_json::to_string(&State { anchor, previous: Some(due) })?;
    let skipped = serde_json::to_string(&State { anchor, previous: state.previous })?;
    if intake.accept_scheduled(payload.as_bytes(), STATE_KEY, &queued, &skipped).await? {
        let late = (now - due).num_seconds();
        if late > 60 {
            tracing::info!(interface = %name, due = %local(due), "running a run missed while the engine was down");
        } else {
            tracing::debug!(interface = %name, due = %local(due), "scheduled run queued");
        }
    } else {
        tracing::warn!(interface = %name, due = %local(due), "previous run still in progress; this run skipped");
    }
    Ok(())
}
