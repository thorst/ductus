//! Who may connect (`allow:` on MLLP and HTTP sources and the API): each listener's list is
//! checked as a connection is accepted, before any TLS handshake or read; the host names in the
//! lists are kept resolved by one task (only when some list names a host). Also TCP keepalive on
//! accepted connections.
//!
//! Background cost: nothing without host names. With them: one lookup per distinct name at
//! start and every `Names::EVERY`, and after a refusal at a listener whose list has names, one
//! more per name at most every `Names::AFTER_REFUSAL`.

use chrono::{DateTime, Utc};
use ductus_core::allow::{Allow, Rules};
use futures_util::future::BoxFuture;
use std::collections::{BTreeMap, HashMap};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;

/// Looks a host name up (forwards: name → addresses).
pub type Resolver = Arc<dyn Fn(String) -> BoxFuture<'static, std::io::Result<Vec<IpAddr>>> + Send + Sync>;

/// The system's resolver (what `getaddrinfo` answers: hosts file, then DNS).
pub fn system_resolver() -> Resolver {
    Arc::new(|name: String| {
        Box::pin(async move {
            let found = tokio::net::lookup_host((name.as_str(), 0)).await?;
            let mut addresses: Vec<IpAddr> = found.map(|a| ductus_core::allow::canonical(a.ip())).collect();
            addresses.sort();
            addresses.dedup();
            Ok(addresses)
        })
    })
}

/// What a host name in an `allow:` list resolves to now.
#[derive(Debug, Clone, Default)]
pub struct Resolved {
    /// The addresses let in for it: the last lookup that found some (kept when a later one fails,
    /// so a DNS outage doesn't cut off senders that were working).
    pub addresses: Vec<IpAddr>,
    /// When it was last looked up (None: not yet).
    pub checked_at: Option<DateTime<Utc>>,
    /// When `addresses` were found.
    pub found_at: Option<DateTime<Utc>>,
    /// Why the last lookup failed, if it did.
    pub error: Option<String>,
}

/// Every host name in the engine's `allow:` lists, kept resolved.
pub struct Names {
    state: RwLock<BTreeMap<String, Resolved>>,
    /// When each name was last looked up because of a refusal.
    rechecked: Mutex<HashMap<String, Instant>>,
    resolve: Resolver,
}

impl Names {
    /// How often every name is looked up again.
    pub const EVERY: Duration = Duration::from_secs(5 * 60);
    /// After a refusal at a listener with names, its names are looked up again at once, but at
    /// most this often each (a sender whose address changed is let in within seconds).
    pub const AFTER_REFUSAL: Duration = Duration::from_secs(60);

    pub fn new(names: impl IntoIterator<Item = String>, resolve: Resolver) -> Arc<Self> {
        let state = names.into_iter().map(|n| (n, Resolved::default())).collect();
        Arc::new(Self { state: RwLock::new(state), rechecked: Mutex::default(), resolve })
    }

    pub fn is_empty(&self) -> bool {
        self.state.read().expect("names lock").is_empty()
    }

    /// Whether `name` resolves to `ip` now.
    pub fn has(&self, name: &str, ip: IpAddr) -> bool {
        self.state.read().expect("names lock").get(name).is_some_and(|r| r.addresses.contains(&ip))
    }

    pub fn get(&self, name: &str) -> Option<Resolved> {
        self.state.read().expect("names lock").get(name).cloned()
    }

    /// Look every name up (in parallel), and log what changed.
    pub async fn resolve_all(&self) {
        let names: Vec<String> = self.state.read().expect("names lock").keys().cloned().collect();
        let lookups = names.into_iter().map(|name| {
            let resolve = self.resolve.clone();
            async move {
                let found = tokio::time::timeout(Duration::from_secs(10), resolve(name.clone())).await;
                (name, found.unwrap_or_else(|_| Err(std::io::ErrorKind::TimedOut.into())))
            }
        });
        for (name, found) in futures_util::future::join_all(lookups).await {
            self.update(&name, found);
        }
    }

    async fn resolve_one(&self, name: &str) {
        let found = tokio::time::timeout(Duration::from_secs(10), (self.resolve)(name.to_string())).await;
        self.update(name, found.unwrap_or_else(|_| Err(std::io::ErrorKind::TimedOut.into())));
    }

    fn update(&self, name: &str, found: std::io::Result<Vec<IpAddr>>) {
        let now = Utc::now();
        let mut state = self.state.write().expect("names lock");
        let Some(r) = state.get_mut(name) else { return };
        let first = r.checked_at.is_none();
        r.checked_at = Some(now);
        match found {
            Ok(addresses) if !addresses.is_empty() => {
                if addresses != r.addresses {
                    let now_ = list(&addresses);
                    if first {
                        tracing::debug!(name, addresses = %now_, "allow: resolved");
                    } else if r.addresses.is_empty() {
                        tracing::info!(name, addresses = %now_, "allow: resolves again; senders from it are let in");
                    } else {
                        tracing::info!(name, addresses = %now_, was = %list(&r.addresses), "allow: name now has other addresses");
                    }
                }
                if r.error.is_some() && r.addresses == addresses {
                    tracing::info!(name, addresses = %list(&addresses), "allow: resolves again");
                }
                r.addresses = addresses;
                r.found_at = Some(now);
                r.error = None;
            }
            outcome => {
                let why = match outcome {
                    Ok(_) => "no addresses".to_string(),
                    Err(e) if e.kind() == std::io::ErrorKind::TimedOut => "no answer in 10s".to_string(),
                    Err(e) => e.to_string(),
                };
                // Logged when it starts failing (or fails differently), not at every try.
                if r.error.as_deref() != Some(why.as_str()) {
                    match &r.found_at {
                        Some(at) => tracing::warn!(
                            name,
                            error = %why,
                            "allow: name didn't resolve; still letting in {} (found {})",
                            list(&r.addresses),
                            at.format("%H:%M")
                        ),
                        None => tracing::warn!(
                            name,
                            error = %why,
                            "allow: name doesn't resolve; nothing is let in for it until it does"
                        ),
                    }
                }
                r.error = Some(why);
            }
        }
    }

    /// After a refusal: look these names up again now, each at most every `AFTER_REFUSAL`.
    pub fn recheck(self: &Arc<Self>, names: &[String]) {
        let now = Instant::now();
        let due: Vec<String> = {
            let mut rechecked = self.rechecked.lock().expect("names lock");
            let mut due = Vec::new();
            for name in names {
                if rechecked.get(name).is_none_or(|at| now.duration_since(*at) >= Self::AFTER_REFUSAL) {
                    rechecked.insert(name.clone(), now);
                    due.push(name.clone());
                }
            }
            due
        };
        for name in due {
            let me = self.clone();
            tokio::spawn(async move { me.resolve_one(&name).await });
        }
    }

    /// Look every name up every `EVERY` until `stop` (one task; none without names).
    pub fn start(self: &Arc<Self>, stop: CancellationToken) {
        self.start_every(Self::EVERY, stop);
    }

    fn start_every(self: &Arc<Self>, every: Duration, stop: CancellationToken) {
        if self.is_empty() {
            return;
        }
        let me = Arc::downgrade(self);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(every) => {}
                    _ = stop.cancelled() => return,
                }
                let Some(names) = me.upgrade() else { return };
                names.resolve_all().await;
            }
        });
    }
}

fn list(addresses: &[IpAddr]) -> String {
    addresses.iter().map(|a| a.to_string()).collect::<Vec<_>>().join(", ")
}

/// One listener's `allow:` list, checked as each connection is accepted.
pub struct Admission {
    /// How the log names the listener (`interface `adt` (MLLP)`, `the REST API`).
    pub listener: String,
    pub host: String,
    pub port: u16,
    allow: Allow,
    rules: Rules,
    names: Arc<Names>,
    refused: AtomicU64,
    last_refused: Mutex<Option<(IpAddr, DateTime<Utc>)>>,
}

impl Admission {
    /// A listener's list; `names` must hold its host names (`Names::new` with every list's).
    pub fn new(listener: &str, host: &str, port: u16, allow: &Allow, names: Arc<Names>) -> anyhow::Result<Arc<Self>> {
        let rules = allow.rules()?;
        let admission = Arc::new(Self {
            listener: listener.to_string(),
            host: host.to_string(),
            port,
            allow: allow.clone(),
            rules,
            names,
            refused: AtomicU64::new(0),
            last_refused: Mutex::new(None),
        });
        let mut all = registry().lock().expect("admissions lock");
        all.retain(|a| a.strong_count() > 0);
        all.push(Arc::downgrade(&admission));
        Ok(admission)
    }

    /// Whether a connection from `peer` may go on. A refused one is counted, logged once a minute
    /// per address, and its listener's names looked up again; the caller drops it unread.
    /// The engine's own checks (doctor, registered with `tls::connect_own`) always may.
    pub fn admits(&self, peer: SocketAddr) -> bool {
        if self.rules.permits(peer.ip(), |name, ip| self.names.has(name, ip)) {
            return true;
        }
        if crate::tls::Refusals::global().is_our_own(peer) {
            return true;
        }
        let ip = ductus_core::allow::canonical(peer.ip());
        self.refused.fetch_add(1, Ordering::Relaxed);
        *self.last_refused.lock().expect("admission lock") = Some((ip, Utc::now()));
        crate::tls::Refusals::global().not_allowed(&self.listener, SocketAddr::new(ip, peer.port()), &self.describe());
        if !self.rules.names.is_empty() {
            self.names.recheck(&self.rules.names);
        }
        false
    }

    /// The list with each name's addresses: `10.1.20.0/24, epic-bridge01 (10.1.30.22)`.
    pub fn describe(&self) -> String {
        self.allow
            .0
            .iter()
            .map(|entry| {
                let entry = entry.trim();
                match self.names.get(&entry.to_ascii_lowercase()) {
                    Some(r) if r.addresses.is_empty() => format!("{entry} (doesn't resolve)"),
                    Some(r) => format!("{entry} ({})", list(&r.addresses)),
                    None => entry.to_string(),
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The list as written.
    pub fn allow(&self) -> &Allow {
        &self.allow
    }

    /// Each host name in the list with what it resolves to now.
    pub fn names(&self) -> Vec<(String, Resolved)> {
        self.rules.names.iter().map(|n| (n.clone(), self.names.get(n).unwrap_or_default())).collect()
    }

    /// Connections refused since the engine started, and the last one's address and time.
    pub fn refused(&self) -> (u64, Option<(IpAddr, DateTime<Utc>)>) {
        (self.refused.load(Ordering::Relaxed), *self.last_refused.lock().expect("admission lock"))
    }
}

/// Every listener's admission in this process (weakly: they go with their engine), so the API
/// and doctor can find a port's.
fn registry() -> &'static Mutex<Vec<Weak<Admission>>> {
    static ALL: std::sync::OnceLock<Mutex<Vec<Weak<Admission>>>> = std::sync::OnceLock::new();
    ALL.get_or_init(Mutex::default)
}

/// The admission of a listener on `port` in this process, if it has an `allow:` list.
pub fn on_port(port: u16) -> Option<Arc<Admission>> {
    registry().lock().expect("admissions lock").iter().filter_map(Weak::upgrade).find(|a| a.port == port)
}

/// TCP keepalive on an accepted connection: a sender that vanished (powered off, cable pulled,
/// a firewall that dropped the connection) is noticed in about 30 s, not the OS's ~2 hours; and
/// quiet connections stay open through firewalls that forget idle ones. The kernel sends the
/// probes, so it costs the engine nothing.
pub fn keepalive(tcp: &TcpStream) {
    let settings = socket2::TcpKeepalive::new()
        .with_time(KEEPALIVE_IDLE)
        .with_interval(KEEPALIVE_INTERVAL)
        .with_retries(KEEPALIVE_PROBES);
    if let Err(e) = socket2::SockRef::from(tcp).set_tcp_keepalive(&settings) {
        tracing::debug!(error = %e, "couldn't set TCP keepalive on a connection");
    }
}

/// Idle time before the first probe, the time between probes, and how many go unanswered before
/// the connection is given up: 15 + 5 × 3 = 30 s.
pub const KEEPALIVE_IDLE: Duration = Duration::from_secs(15);
pub const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(5);
pub const KEEPALIVE_PROBES: u32 = 3;

/// What `allow:` lists exist in a project, and the names to keep resolved for them.
pub fn names_in<'a>(lists: impl IntoIterator<Item = &'a Allow>) -> Vec<String> {
    let mut names: Vec<String> = lists.into_iter().filter_map(|a| a.rules().ok()).flat_map(|r| r.names).collect();
    names.sort();
    names.dedup();
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    type Table = Arc<Mutex<HashMap<String, Vec<IpAddr>>>>;

    /// A resolver answering from a table the test changes, counting lookups.
    fn table(entries: &[(&str, &str)]) -> (Resolver, Table, Arc<AtomicUsize>) {
        let map: HashMap<String, Vec<IpAddr>> =
            entries.iter().map(|(n, a)| (n.to_string(), vec![a.parse().unwrap()])).collect();
        let map = Arc::new(Mutex::new(map));
        let calls = Arc::new(AtomicUsize::new(0));
        let (m, c) = (map.clone(), calls.clone());
        let resolver: Resolver = Arc::new(move |name: String| {
            c.fetch_add(1, Ordering::SeqCst);
            let found = m.lock().unwrap().get(&name).cloned();
            Box::pin(async move { found.ok_or_else(|| std::io::Error::other("not found")) })
        });
        (resolver, map, calls)
    }

    fn peer(ip: &str) -> SocketAddr {
        SocketAddr::new(ip.parse().unwrap(), 40000)
    }

    #[tokio::test]
    async fn names_resolve_at_start_and_again_after_a_refusal() {
        let (resolver, map, calls) = table(&[("epic.local", "10.1.30.22")]);
        let allow = Allow(vec!["10.1.20.0/24".into(), "epic.local".into()]);
        let names = Names::new(names_in([&allow]), resolver);
        names.resolve_all().await;
        let admission = Admission::new("test-allow-names", "0.0.0.0", 1, &allow, names.clone()).unwrap();
        assert!(admission.admits(peer("10.1.20.9")));
        assert!(admission.admits(peer("10.1.30.22")), "through the name");
        assert!(!admission.admits(peer("10.1.30.23")));
        assert_eq!(admission.refused().0, 1);
        assert_eq!(admission.describe(), "10.1.20.0/24, epic.local (10.1.30.22)");

        // The sender moved (a failover): the refusal looks the name up again, at once.
        map.lock().unwrap().insert("epic.local".into(), vec!["10.1.30.24".parse().unwrap()]);
        let before = calls.load(Ordering::SeqCst);
        assert!(!admission.admits(peer("10.1.30.24")));
        for _ in 0..100 {
            if admission.admits(peer("10.1.30.24")) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(admission.admits(peer("10.1.30.24")), "let in after the lookup");
        assert!(!admission.admits(peer("10.1.30.22")), "the old address is gone");
        // ...but no more than once a minute however many are refused.
        let after = calls.load(Ordering::SeqCst);
        assert_eq!(after, before + 1);
        for _ in 0..20 {
            admission.admits(peer("10.9.9.9"));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(calls.load(Ordering::SeqCst), after);
        let (refused, last) = admission.refused();
        assert!(refused >= 23, "every refusal counted: {refused}");
        assert_eq!(last.map(|(ip, _)| ip.to_string()).as_deref(), Some("10.9.9.9"));
    }

    #[tokio::test]
    async fn a_failed_lookup_keeps_the_addresses_it_had() {
        let (resolver, map, _) = table(&[("epic.local", "10.1.30.22")]);
        let names = Names::new(["epic.local".to_string(), "gone.local".to_string()], resolver);
        names.resolve_all().await;
        assert!(names.has("epic.local", "10.1.30.22".parse().unwrap()));
        let gone = names.get("gone.local").unwrap();
        assert!(gone.addresses.is_empty() && gone.error.is_some() && gone.checked_at.is_some());

        map.lock().unwrap().clear(); // DNS is down
        names.resolve_all().await;
        let epic = names.get("epic.local").unwrap();
        assert!(names.has("epic.local", "10.1.30.22".parse().unwrap()), "still let in");
        assert!(epic.error.is_some() && epic.found_at < epic.checked_at);
    }

    #[tokio::test]
    async fn names_are_looked_up_on_the_timer_and_only_with_names() {
        let (resolver, map, _) = table(&[("epic.local", "10.1.30.22")]);
        let names = Names::new(["epic.local".to_string()], resolver);
        names.resolve_all().await;
        map.lock().unwrap().insert("epic.local".into(), vec!["10.1.30.23".parse().unwrap()]);
        let stop = CancellationToken::new();
        names.start_every(Duration::from_millis(20), stop.clone());
        for _ in 0..100 {
            if names.has("epic.local", "10.1.30.23".parse().unwrap()) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(names.has("epic.local", "10.1.30.23".parse().unwrap()));
        stop.cancel();

        // A resolver of its own: the task above, cancelled, may still be finishing a lookup.
        let (resolver, _, calls) = table(&[("epic.local", "10.1.30.22")]);
        let none = Names::new(Vec::<String>::new(), resolver);
        none.start_every(Duration::from_millis(1), CancellationToken::new());
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 0, "no names, no task, no lookups");
    }

    #[tokio::test]
    async fn our_own_checks_are_let_in() {
        let (resolver, ..) = table(&[]);
        let allow = Allow(vec!["10.1.20.0/24".into()]);
        let admission = Admission::new("test-allow-own", "0.0.0.0", 2, &allow, Names::new([], resolver)).unwrap();
        let doctor = peer("127.0.0.1");
        assert!(!admission.admits(doctor));
        crate::tls::Refusals::global().our_own(doctor);
        assert!(admission.admits(doctor));
        assert!(on_port(2).is_some_and(|a| a.listener == "test-allow-own"));
    }
}
