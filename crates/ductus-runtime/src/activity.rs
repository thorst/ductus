//! Monitoring state that survives restarts and failover: when each interface last received
//! something and each destination last sent or failed (kept in `stats.rs` as it happens, saved
//! here every few seconds), and destinations paused on purpose (saved at once).
//!
//! Saving is one task and two statements every `SAVE_EVERY`, whatever the traffic: the cost
//! doesn't grow with volume, and a crash loses at most those seconds of "last seen".
//!
//! The same task samples each destination's counts (`Trends`): the last few minutes' rates for
//! the diagnosis, and, at the end of each hour, what's normal for that hour of the week (saved,
//! one statement an hour).

use crate::stats::{DestinationStats, InterfaceStats};
use anyhow::Result;
use chrono::{DateTime, Utc};
use ductus_core::monitor::{Baseline, Recent, hour_of_week};
use ductus_store::{BaselineRow, DestinationId, PauseRow, Seen, Store};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

const SAVE_EVERY: Duration = Duration::from_secs(5);
/// How far back "recent" rates look: long enough to smooth a few slow sends, short enough to
/// show a change within minutes.
const WINDOW: Duration = Duration::from_secs(300);
/// An hour is learned from only if the engine watched at least this much of it (one that
/// started a minute before the hour ended says nothing about the hour).
const LEARN_FROM: Duration = Duration::from_secs(15 * 60);

/// One destination's counts at a moment.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Sample {
    at: DateTime<Utc>,
    /// The interface's (every message goes to each of its destinations).
    received: u64,
    done: u64,
    sends: u64,
    send_time: Duration,
}

impl Sample {
    fn of(received: u64, d: &DestinationStats, at: DateTime<Utc>) -> Self {
        Self { at, received, done: d.done(), sends: d.send.count(), send_time: d.send.sum() }
    }
}

/// A destination's recent rates and its normal per hour of the week.
#[derive(Debug)]
pub struct Trends {
    samples: VecDeque<Sample>,
    /// When it last finished a message (or when watching started).
    progress: DateTime<Utc>,
    /// The first sample of the hour being watched.
    hour_start: Option<Sample>,
    pub baseline: Baseline,
}

impl Trends {
    pub fn new(baseline: Baseline, now: DateTime<Utc>) -> Self {
        Self { samples: VecDeque::new(), progress: now, hour_start: None, baseline }
    }

    /// Add a sample; returns true when an hour ended and the baseline learned from it.
    fn sample(&mut self, s: Sample) -> bool {
        if self.samples.back().is_some_and(|last| s.done > last.done) {
            self.progress = s.at;
        }
        let mut learned = false;
        match (self.hour_start, self.samples.back()) {
            (Some(start), Some(&last)) if hour_of_week(s.at) != hour_of_week(start.at) => {
                // `last` is the hour's last sample: the hour ran from `start` to it.
                let watched = (last.at - start.at).to_std().unwrap_or_default();
                if watched >= LEARN_FROM {
                    let minutes = watched.as_secs_f64() / 60.0;
                    let inflow = (last.received - start.received) as f64 / minutes;
                    let sends = last.sends - start.sends;
                    let send_ms =
                        (sends > 0).then(|| (last.send_time - start.send_time).as_secs_f64() * 1000.0 / sends as f64);
                    self.baseline.learn(hour_of_week(start.at), inflow, send_ms);
                    learned = true;
                }
                self.hour_start = Some(s);
            }
            (None, _) => self.hour_start = Some(s),
            _ => {}
        }
        self.samples.push_back(s);
        while self.samples.front().is_some_and(|f| s.at - f.at > chrono::Duration::from_std(WINDOW).unwrap_or_default())
        {
            self.samples.pop_front();
        }
        learned
    }

    /// The last few minutes (none until there's half a minute of samples).
    pub fn recent(&self, now: DateTime<Utc>) -> Option<Recent> {
        let (first, last) = (self.samples.front()?, self.samples.back()?);
        let covered = (last.at - first.at).to_std().ok()?;
        if covered < Duration::from_secs(30) {
            return None;
        }
        let sends = last.sends - first.sends;
        Some(Recent {
            minutes: covered.as_secs_f64() / 60.0,
            received: last.received - first.received,
            done: last.done - first.done,
            send_ms: (sends > 0).then(|| (last.send_time - first.send_time).as_secs_f64() * 1000.0 / sends as f64),
            since_progress: (now - self.progress).to_std().unwrap_or_default(),
        })
    }
}

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
    /// Every destination's recent rates and normal.
    pub trends: HashMap<DestinationId, Arc<Mutex<Trends>>>,
    /// The task that saves them (see `saved`).
    saver: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Activity {
    /// A destination's last few minutes, and what's normal now: (recent, inflow per minute, send ms).
    pub fn trend(&self, destination: DestinationId, now: DateTime<Utc>) -> (Option<Recent>, Option<f64>, Option<f64>) {
        let Some(trends) = self.trends.get(&destination) else { return (None, None, None) };
        let t = trends.lock().unwrap_or_else(|p| p.into_inner());
        let hour = hour_of_week(now);
        (t.recent(now), t.baseline.inflow(hour), t.baseline.send_ms(hour))
    }
}

impl Activity {
    /// Load what's stored into the counts' last-seen times and the pause switches, then save
    /// changes every few seconds until `stop` (and once more then).
    pub async fn start(store: Store, watched: Vec<Watched>, stop: CancellationToken) -> Result<Self> {
        let names: Vec<&str> = watched.iter().map(|w| w.stats.name.as_str()).collect();
        let ids: Vec<DestinationId> = watched.iter().flat_map(|w| w.destinations.iter().copied()).collect();
        let (interfaces, destinations) = store.activity(&names, &ids).await?;
        let mut by_id: HashMap<DestinationId, _> = destinations.into_iter().map(|d| (d.destination_id, d)).collect();
        let mut pauses = HashMap::new();
        let mut trends = HashMap::new();
        let now = Utc::now();
        for w in &watched {
            for (id, stats) in w.destinations.iter().zip(&w.stats.destinations) {
                let pause = Arc::new(Pause::default());
                let mut baseline = Baseline::default();
                if let Some(stored) = by_id.get_mut(id) {
                    baseline = Baseline::from_stored(stored.baseline_inflow.take(), stored.baseline_send_ms.take());
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
                trends.insert(*id, Arc::new(Mutex::new(Trends::new(baseline, now))));
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
        let saver = tokio::spawn(save(store, watched, trends.clone(), stop));
        Ok(Self { watched_since, pauses, trends, saver: Mutex::new(Some(saver)) })
    }

    /// Read the pauses again: a takeover's, once the engine it took over from let go (a
    /// destination paused or resumed there meanwhile is so here too).
    pub async fn refresh_pauses(&self, store: &Store) -> Result<()> {
        let ids: Vec<DestinationId> = self.pauses.keys().copied().collect();
        let (_, destinations) = store.activity(&[], &ids).await?;
        for d in destinations {
            if let Some(pause) = self.pauses.get(&d.destination_id) {
                pause.set(d.pause);
            }
        }
        Ok(())
    }

    /// After the engine's stop: wait for the last save.
    pub async fn saved(&self) {
        let saver = self.saver.lock().unwrap_or_else(|p| p.into_inner()).take();
        if let Some(saver) = saver {
            let _ = saver.await;
        }
    }
}

/// Every few seconds, sample each destination's counts and save the last-seen times that
/// changed; once an hour, save what was learned about it.
async fn save(
    store: Store,
    watched: Vec<Watched>,
    trends: HashMap<DestinationId, Arc<Mutex<Trends>>>,
    stop: CancellationToken,
) {
    let mut unsaved: Vec<BaselineRow> = Vec::new();
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
        // Stopping: one last save, so a restart starts from now rather than a few seconds ago.
        let last = tokio::select! {
            _ = tokio::time::sleep(SAVE_EVERY) => false,
            _ = stop.cancelled() => true,
        };
        let now = Utc::now();
        for w in &watched {
            let arrived = crate::stats::get(&w.stats.received);
            for (id, s) in w.destinations.iter().zip(&w.stats.destinations) {
                let Some(trend) = trends.get(id) else { continue };
                let mut t = trend.lock().unwrap_or_else(|p| p.into_inner());
                if t.sample(Sample::of(arrived, s, now)) {
                    unsaved.retain(|r| r.destination != *id);
                    let b = &t.baseline;
                    unsaved.push(BaselineRow {
                        destination: *id,
                        inflow: b.inflow.clone(),
                        send_ms: b.send_ms.clone(),
                    });
                }
            }
        }
        if !unsaved.is_empty() {
            match store.save_baselines(&unsaved).await {
                Ok(()) => unsaved.clear(),
                // Tried again next time; learning goes on in memory.
                Err(e) => tracing::warn!(error = %format!("{e:#}"), "saving what's normal for destinations failed"),
            }
        }
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
            if last {
                return;
            }
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
        if last {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Local, TimeZone};

    fn local(text: &str) -> DateTime<Utc> {
        let naive = chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S").unwrap();
        Local.from_local_datetime(&naive).earliest().unwrap().with_timezone(&Utc)
    }

    /// Samples every 5 s from `from` for `secs`: `per_min` arriving and finished, sends of `ms`.
    fn feed(t: &mut Trends, from: DateTime<Utc>, secs: i64, per_min: u64, ms: u64, start: &mut Sample) -> bool {
        let mut learned = false;
        for i in (0..secs).step_by(5) {
            let n = per_min / 12; // per 5 s
            *start = Sample {
                at: from + chrono::Duration::seconds(i),
                received: start.received + n,
                done: start.done + n,
                sends: start.sends + n,
                send_time: start.send_time + Duration::from_millis(ms * n),
            };
            learned |= t.sample(*start);
        }
        learned
    }

    #[test]
    fn recent_rates_and_each_hour_learned() {
        // 2026-10-05 is a Monday: 07:00 is hour 7 of the week.
        let mut t = Trends::new(Baseline::default(), local("2026-10-05 07:00:00"));
        let mut s =
            Sample { at: local("2026-10-05 07:00:00"), received: 0, done: 0, sends: 0, send_time: Duration::ZERO };
        assert!(t.recent(s.at).is_none(), "nothing sampled yet");
        assert!(!feed(&mut t, local("2026-10-05 07:30:00"), 30 * 60, 120, 10, &mut s));
        let r = t.recent(local("2026-10-05 07:59:55")).unwrap();
        assert!((r.minutes - 5.0).abs() < 0.1, "the window: {}", r.minutes);
        assert!((r.received as f64 / r.minutes - 120.0).abs() < 1.0);
        assert_eq!(r.send_ms, Some(10.0));
        assert!(r.since_progress < Duration::from_secs(10));

        // The next hour's first sample: 07:00-08:00 is learned (30 minutes watched).
        assert!(feed(&mut t, local("2026-10-05 08:00:00"), 5, 120, 10, &mut s));
        assert!((t.baseline.inflow(7).unwrap() - 120.0).abs() < 1.0);
        assert_eq!(t.baseline.send_ms(7), Some(10.0));

        // Nothing finished for two minutes: progress stays where it was.
        let still = s;
        for i in 1..=24 {
            t.sample(Sample { at: still.at + chrono::Duration::seconds(5 * i), ..still });
        }
        let r = t.recent(still.at + chrono::Duration::seconds(120)).unwrap();
        assert_eq!(r.since_progress, Duration::from_secs(120));

        // An hour watched for less than 15 minutes teaches nothing.
        let mut late = Trends::new(Baseline::default(), local("2026-10-05 09:50:00"));
        let mut s = Sample { at: local("2026-10-05 09:50:00"), ..s };
        feed(&mut late, local("2026-10-05 09:50:00"), 10 * 60 + 5, 60, 5, &mut s);
        assert_eq!(late.baseline, Baseline::default());
    }
}
