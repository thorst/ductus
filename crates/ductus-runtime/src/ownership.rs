//! One owner per interface across engines (0.6.2; `ductus_store` engines.rs).
//!
//! At start, before anything is changed for its interfaces (their lanes, reconcile, retention,
//! the workers), the engine takes its lock and claims its interfaces (`Owner::claim`). An
//! interface run by another live engine refuses the start: two engines running one interface
//! would both send its messages (duplicates), and per-key order would break. (A change of an
//! interface's lanes is never refused since 0.7.4.4.4: `crate::lanes`.)
//!
//! While running, `Owner::watch` checks the lock connection every few seconds. If it is lost (a
//! database restart, a failover), workers, file pickup and schedules pause (`Ownership`) and the
//! engine takes its lock and claims again; intake keeps storing (any number of engines can). If
//! another engine took an interface meanwhile, this one stops.
//!
//! Respond-mode HTTP interfaces aren't claimed: they have no queue, and several engines behind a
//! load balancer answering the same requests is a fine way to run them.
//!
//! A takeover (`crate::handover`) takes its lock at once but claims only once the engine it takes
//! over from let go (`Owner::prepare`, then `Owner::claim_now`); until then its workers wait as
//! after a lost lock. It checks first what a start would refuse, so a takeover that couldn't
//! start fails while the old engine still runs.

use anyhow::Result;
use chrono::{DateTime, Utc};
use ductus_store::{Audit, By, EngineLock, NewEngine, Owned, Store};
use serde_json::json;
use std::collections::BTreeMap;
use std::fmt::Write;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

/// How often the lock connection is checked, and how long it may take to answer. Well inside
/// the ~25 s the server takes to notice a vanished engine (keepalives on that connection), so an
/// engine cut off from the database pauses before another can claim its interfaces.
const CHECK_EVERY: Duration = Duration::from_secs(2);
const CHECK_TIMEOUT: Duration = Duration::from_secs(3);

/// The process exit code of a refused start: a person has to act, so service managers are told
/// not to restart it (`RestartPreventExitStatus=3` in the systemd unit).
pub const REFUSED_EXIT: i32 = 3;

/// A start refused (or an engine stopped) by these guards, with what to do about it.
#[derive(Debug)]
pub struct Refused(pub String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refused {}

/// Whether this engine holds its interfaces right now: false while its lock connection is lost.
/// Workers, file pickup and schedules wait while it is false.
#[derive(Debug)]
pub struct Ownership(watch::Sender<bool>);

impl Default for Ownership {
    fn default() -> Self {
        Self(watch::Sender::new(true))
    }
}

impl Ownership {
    fn new(held: bool) -> Self {
        Self(watch::Sender::new(held))
    }

    pub fn is_held(&self) -> bool {
        *self.0.borrow()
    }

    /// Returns once held (at once if it is).
    pub async fn until_held(&self) {
        let mut state = self.0.subscribe();
        let _ = state.wait_for(|held| *held).await;
    }

    fn set(&self, held: bool) {
        self.0.send_replace(held);
    }
}

/// This engine, as the database knows it.
pub struct Me {
    pub instance: String,
    pub host: String,
    pub environment: String,
}

impl Me {
    fn row(&self) -> NewEngine<'_> {
        NewEngine {
            instance: &self.instance,
            host: &self.host,
            pid: std::process::id(),
            version: env!("CARGO_PKG_VERSION"),
            environment: &self.environment,
        }
    }
}

/// The engine's lock and claims.
pub struct Owner {
    me: Me,
    key: i64,
    lock: tokio::sync::Mutex<Option<EngineLock>>,
    /// The interfaces it claimed.
    interfaces: Vec<String>,
    pub ownership: Arc<Ownership>,
    /// Cancelled when another engine took an interface while the lock was lost (`why`).
    taken: CancellationToken,
    why: std::sync::Mutex<Option<String>>,
}

impl Owner {
    /// Take the engine's lock and claim `interfaces` (the queued ones). Refused (a `Refused`
    /// error) when a live engine runs any of them: then nothing is claimed, the refusal is in the
    /// audit log, and the engine leaves no record.
    pub async fn claim(store: &Store, me: Me, interfaces: Vec<String>) -> Result<Self> {
        let started = std::time::Instant::now();
        let lock = store.lock_engine(&me.row(), None).await?;
        let owned = store.claim(&me.instance, &interfaces).await?;
        let owner = Self {
            key: lock.key,
            lock: tokio::sync::Mutex::new(Some(lock)),
            interfaces,
            ownership: Arc::new(Ownership::default()),
            taken: CancellationToken::new(),
            why: std::sync::Mutex::new(None),
            me,
        };
        if !owned.is_empty() {
            let detail = json!({
                "reason": "owned",
                "instance": owner.me.instance,
                "interfaces": owned.iter().map(|o| &o.interface).collect::<Vec<_>>(),
                "owners": owners_detail(&owned),
            });
            return Err(owner.refuse(store, detail, owned_message(&owned)).await);
        }
        tracing::debug!(interfaces = owner.interfaces.len(), took = ?started.elapsed(), "interfaces claimed");
        Ok(owner)
    }

    /// A takeover's start: the lock and the engine's record, nothing claimed (the engine it takes
    /// over from, `from`, still runs them), and `Ownership` not held. Refused as a start would be
    /// when another live engine runs any of `interfaces`.
    pub async fn prepare(store: &Store, me: Me, interfaces: Vec<String>, from: &str) -> Result<Self> {
        let lock = store.lock_engine(&me.row(), None).await?;
        let owner = Self {
            key: lock.key,
            lock: tokio::sync::Mutex::new(Some(lock)),
            interfaces,
            ownership: Arc::new(Ownership::new(false)),
            taken: CancellationToken::new(),
            why: std::sync::Mutex::new(None),
            me,
        };
        let owned = store.live_owners(&owner.interfaces, &[&owner.me.instance, from]).await?;
        if !owned.is_empty() {
            let detail = json!({
                "reason": "owned",
                "instance": owner.me.instance,
                "interfaces": owned.iter().map(|o| &o.interface).collect::<Vec<_>>(),
                "owners": owners_detail(&owned),
            });
            return Err(owner.refuse(store, detail, owned_message(&owned)).await);
        }
        Ok(owner)
    }

    /// A takeover, once the engine it took over from let go: claim, as a start does (refused only
    /// if a third engine took one of them meanwhile).
    pub async fn claim_now(&self, store: &Store) -> Result<()> {
        let owned = store.claim(&self.me.instance, &self.interfaces).await?;
        if owned.is_empty() {
            return Ok(());
        }
        let detail = json!({
            "reason": "owned",
            "instance": self.me.instance,
            "interfaces": owned.iter().map(|o| &o.interface).collect::<Vec<_>>(),
            "owners": owners_detail(&owned),
        });
        Err(self.refuse(store, detail, owned_message(&owned)).await)
    }

    /// The interfaces are this engine's: workers, file pickup and schedules go.
    pub fn hold(&self) {
        self.ownership.set(true);
    }

    pub fn instance(&self) -> &str {
        &self.me.instance
    }

    /// Write the refusal to the audit log, let go of everything, and make the error to return.
    async fn refuse(&self, store: &Store, detail: serde_json::Value, message: String) -> anyhow::Error {
        if let Err(e) = store.record(&Audit::new(By::engine(), "engine.refused").detail(detail)).await {
            tracing::error!(error = %format!("{e:#}"), "could not write the audit log (start refused)");
        }
        if let Err(e) = store.forget_engine(&self.me.instance).await {
            tracing::warn!(error = %format!("{e:#}"), "could not remove the refused engine's record");
        }
        if let Some(lock) = self.lock.lock().await.take() {
            lock.release().await;
        }
        Refused(message).into()
    }

    /// Check the lock every few seconds until `stop`; see the module.
    pub fn watch(self: &Arc<Self>, store: Store, stop: CancellationToken) {
        let owner = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(CHECK_EVERY) => {}
                    _ = stop.cancelled() => return,
                }
                let lost = {
                    let mut lock = owner.lock.lock().await;
                    match lock.as_mut() {
                        None => return, // released
                        Some(l) => l.check(CHECK_TIMEOUT).await.err(),
                    }
                };
                let Some(error) = lost else { continue };
                owner.ownership.set(false);
                tracing::error!(error = %format!("{error:#}"), "lost the connection that holds this engine's interfaces: sending, file pickup and schedules pause until it is back (receiving goes on)");
                if !owner.regain(&store, &stop).await {
                    return;
                }
            }
        });
    }

    /// Take the lock again and claim again. False when the engine should stop (another took an
    /// interface over, or it is stopping anyway).
    async fn regain(&self, store: &Store, stop: &CancellationToken) -> bool {
        let mut announced = false;
        loop {
            tokio::select! {
                _ = tokio::time::sleep(CHECK_EVERY) => {}
                _ = stop.cancelled() => return false,
            }
            let lock = match store.lock_engine(&self.me.row(), Some(self.key)).await {
                Ok(lock) => lock,
                Err(e) => {
                    if !announced {
                        tracing::warn!(error = %format!("{e:#}"), "can't take the engine's lock again yet; retrying every {CHECK_EVERY:?}");
                        announced = true;
                    }
                    continue;
                }
            };
            *self.lock.lock().await = Some(lock);
            match store.claim(&self.me.instance, &self.interfaces).await {
                Ok(owned) if owned.is_empty() => {
                    self.ownership.set(true);
                    tracing::info!("the engine's lock is back: sending, file pickup and schedules go on");
                    return true;
                }
                Ok(owned) => {
                    let message = format!(
                        "while this engine was cut off from the database, another took over its interfaces; stopping.\n{}",
                        owned_message(&owned)
                    );
                    tracing::error!("{message}");
                    *self.why.lock().unwrap() = Some(message);
                    self.taken.cancel();
                    return false;
                }
                Err(e) => tracing::warn!(error = %format!("{e:#}"), "claiming the interfaces again failed; retrying"),
            }
        }
    }

    /// Resolves when another engine took this one's interfaces over (see `regain`).
    pub async fn taken_over(&self) {
        self.taken.cancelled().await
    }

    /// Why the engine had to stop, if another took it over.
    pub fn taken_why(&self) -> Option<String> {
        self.why.lock().unwrap().clone()
    }

    /// A clean stop, after the workers finished: let go of the interfaces and the lock.
    pub async fn release(&self, store: &Store) {
        if let Err(e) = store.release_engine(&self.me.instance).await {
            tracing::warn!(error = %format!("{e:#}"), "could not record the engine's stop (its interfaces are free anyway: its lock goes with it)");
        }
        if let Some(lock) = self.lock.lock().await.take() {
            lock.release().await;
        }
    }
}

/// The refusal for interfaces owned by live engines, grouped by engine.
pub fn owned_message(owned: &[Owned]) -> String {
    let mut by_engine: BTreeMap<(DateTime<Utc>, &str), Vec<&Owned>> = BTreeMap::new();
    for o in owned {
        by_engine.entry((o.started_at, o.instance.as_str())).or_default().push(o);
    }
    let mut text = format!(
        "another engine is running {} of this engine's interfaces on this database:\n",
        if owned.len() == 1 { "one".to_string() } else { owned.len().to_string() }
    );
    for rows in by_engine.values() {
        let names: Vec<&str> = rows.iter().map(|o| o.interface.as_str()).collect();
        let shown = if names.len() > 10 {
            format!("{} and {} more", names[..10].join(", "), names.len() - 10)
        } else {
            names.join(", ")
        };
        let e = rows[0];
        let _ = writeln!(
            text,
            "  {shown}\n    owned by: {}, pid {}, ductus {}, started {} (instance {})",
            e.host,
            e.pid,
            e.version,
            e.started_at.format("%Y-%m-%d %H:%M:%S UTC"),
            e.instance
        );
    }
    text.push_str(
        "Two engines running one interface would both send its messages: duplicates, and per-patient order breaks.\n\
         Stop that engine first, or remove these interfaces from this one's project.",
    );
    text
}

fn owners_detail(owned: &[Owned]) -> Vec<serde_json::Value> {
    let mut seen = std::collections::BTreeSet::new();
    owned
        .iter()
        .filter(|o| seen.insert(o.instance.clone()))
        .map(|o| {
            json!({ "instance": o.instance, "host": o.host, "pid": o.pid, "version": o.version, "started_at": o.started_at.to_rfc3339() })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_refusal_names_the_other_engine() {
        let at = DateTime::parse_from_rfc3339("2026-10-20T09:14:03Z").unwrap().with_timezone(&Utc);
        let owned: Vec<Owned> = (0..12)
            .map(|i| Owned {
                interface: format!("feed-{i:02}"),
                instance: "abc-1".into(),
                host: "host-b".into(),
                pid: 4120,
                version: "0.6.0".into(),
                started_at: at,
            })
            .collect();
        let text = owned_message(&owned);
        assert!(text.starts_with("another engine is running 12 of this engine's interfaces"), "{text}");
        assert!(text.contains("feed-09 and 2 more\n    owned by: host-b, pid 4120, ductus 0.6.0, started 2026-10-20 09:14:03 UTC (instance abc-1)"), "{text}");
        assert!(text.contains("Stop that engine first"), "{text}");
    }

    #[tokio::test]
    async fn ownership_waits_until_held() {
        let o = Arc::new(Ownership::default());
        assert!(o.is_held());
        o.set(false);
        let waiter = tokio::spawn({
            let o = o.clone();
            async move { o.until_held().await }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!waiter.is_finished());
        o.set(true);
        tokio::time::timeout(Duration::from_secs(1), waiter).await.unwrap().unwrap();
    }
}
