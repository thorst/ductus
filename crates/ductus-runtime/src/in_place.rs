//! What the engine keeps of each running interface to change it while it runs (a deploy in
//! place, 0.7.5): the parts its workers share and the threads its slots run on. A start builds
//! them once (`Kept::new`); `Kept::replace` gives an interface new code and settings by building
//! new jobs from them and sending each thread its slots' (`worker::Command::Replace`).
//!
//! What it keeps across a change: the interface's health, counts, lanes and gate, and each
//! destination that stays its id, counts, pause switch and (with the same rate limit) its
//! allowance. What it rebuilds: the config and each destination's outlet (TLS, API, SFTP server,
//! route), from the certificates and resources the engine started with (a change to those is a
//! handover: `changes::plan`).
//!
//! **Destinations added or removed** (or put in another order: each worker's wake-up is found by
//! its destination's place) also change where the interface's messages are queued, in this order:
//! 1. A new destination gets its id in the store and what's stored about it (a pause, last sent,
//!    what's normal: `Activity::watch`): it starts paused if it was, under that name, before.
//! 2. The threads switch to the new jobs, which wait on new wake-ups. Messages stored meanwhile
//!    still go to the old destinations: nothing is lost if a thread can't load the new code and
//!    the interface is put back.
//! 3. The intake queues for the new destinations (`Intake::retarget`, which waits for every store
//!    that began with the old ones), then what's waiting is matched to them as at a start
//!    (`Store::reconcile`: a new destination gets a delivery for each message waiting, a removed
//!    one's pending deliveries are marked filtered) and every new worker reads again.
//!
//! While an interface's lanes are changing (an `ordering` change not settled yet) its destinations
//! can't change between two messages: each lane's drain is tracked per destination. `replace`
//! refuses that with `NeedsHandover`; `Engine::replace_interface` restarts the interface alone
//! instead (its lanes built again for its new destinations, 0.7.5.2.4).
//!
//! **A restart** (`Engine::restart_interface`) is `stop` (every worker of it stopped, its parts and
//! a respond-mode interface's waiting requests answered), then `add` with its new parts.

use crate::Wake;
use crate::activity::{Activity, Pause, Watched};
use crate::control::RunningDestination;
use crate::health::Health;
use crate::ingest::{Intake, Targets};
use crate::lanes::Lanes;
use crate::limiter::Limiter;
use crate::services::SharedServices;
use crate::stats::{DestinationStats, InterfaceStats};
use crate::worker::{Budget, Command, Job, Outlet, Renewal, Requests, Waiting};
use anyhow::{Context, Result, anyhow, bail};
use ductus_core::config::{Destination, DestinationConfig, Isolation};
use ductus_core::{InterfaceConfig, Resources};
use ductus_script::CodeFile;
use ductus_store::{DestinationId, Store};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, mpsc::UnboundedSender, oneshot};

/// Every running interface's parts, and what their jobs share.
pub struct Kept {
    /// One lock per interface: deploys of different interfaces go at once, two of the same one
    /// in turn. A deploy adds and removes interfaces here too (0.7.5.2).
    interfaces: std::sync::RwLock<HashMap<String, Arc<Mutex<Interface>>>>,
    shared: Shared,
    threads: Threads,
    /// The generation code applied now loads under (`next_generation`; 0: the start's).
    generation: std::sync::atomic::AtomicU64,
}

/// What every interface's jobs and outlets are built with.
pub(crate) struct Shared {
    pub store: Store,
    pub ownership: Arc<crate::ownership::Ownership>,
    pub waiting: Arc<Waiting>,
    pub certificates: Arc<crate::tls::Certificates>,
    pub resources: Resources,
    /// Every queued interface's intake (routes store into their target's): changed as interfaces
    /// are added and removed.
    pub intakes: std::sync::RwLock<HashMap<String, Arc<Intake>>>,
    /// Where an interface's health reports its stop (the audit log): an added one's too.
    pub stopped: crate::health::StoppedTx,
}

/// The worker threads, to place an interface a deploy adds and to join at the engine's stop.
pub(crate) struct Threads {
    /// The shared pool's threads (made as the start needs them, then as added interfaces do, up
    /// to `Spawn::pool_threads`), each with how many slots it runs.
    pub pool: std::sync::Mutex<Vec<PoolThread>>,
    /// What new threads are made with; None: none can be (tests).
    pub spawn: Option<Spawn>,
    /// Every worker thread's handle (`Kept::take_threads`).
    pub handles: std::sync::Mutex<Vec<std::thread::JoinHandle<()>>>,
    /// Where threads past their budget of superseded code ask to be renewed, until `renew_beside`
    /// takes it.
    pub renewals: std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<Renewal>>>,
}

/// One of the shared pool's threads.
pub(crate) struct PoolThread {
    pub commands: UnboundedSender<Command>,
    /// The slots it runs, of every interface on it: an added interface's go where there are fewest.
    pub slots: usize,
}

/// What a new worker thread is made with.
pub(crate) struct Spawn {
    pub services: Arc<SharedServices>,
    pub heap_mb: usize,
    /// How many threads the shared pool may have.
    pub pool_threads: usize,
    /// The engine's stop, the workers' (`spawn_thread`'s `stop`).
    pub stop: tokio_util::sync::CancellationToken,
    /// Each thread's budget of superseded code (`isolates.superseded_mb`), and where it asks.
    pub superseded_bytes: usize,
    pub renew: tokio::sync::mpsc::UnboundedSender<Renewal>,
}

impl Spawn {
    /// The budget of a new thread whose commands are `commands`.
    fn budget(&self, commands: &UnboundedSender<Command>) -> Option<Budget> {
        Some(Budget { bytes: self.superseded_bytes, renew: self.renew.clone(), me: commands.downgrade() })
    }
}

/// What a code version's files are, by path: their hashes (`fingerprint`).
pub(crate) type Fingerprint = Vec<(std::path::PathBuf, [u8; 32])>;

/// The paths and hashes of `files`.
pub(crate) fn fingerprint(files: &[CodeFile]) -> Fingerprint {
    files.iter().map(|f| (f.path.clone(), f.hash)).collect()
}

/// One interface's parts, shared by its slots' jobs.
pub(crate) struct Interface {
    pub config: Arc<InterfaceConfig>,
    /// Its workers' wake-ups, one per (slot, destination).
    pub wake: Arc<Wake>,
    pub health: Arc<Health>,
    pub stats: Arc<InterfaceStats>,
    pub lanes: Arc<Lanes>,
    /// Its destinations' outlets, in `config.destinations()` order.
    pub outlets: Vec<Outlet>,
    /// Where its messages are stored (none in respond mode).
    pub intake: Option<Arc<Intake>>,
    /// Its code version's id, which its workers save with every decision: the start's, then each
    /// deploy's (its workers wait for it before saving one).
    pub decided: crate::deployment::Decided,
    /// Where its slots run: each thread's commands and the slots on it.
    pub threads: Vec<Placed>,
    /// Removed by a deploy (`Kept::remove`): a change that waited for its lock finds it gone.
    pub removed: bool,
    /// Stops its lanes' watch for settling (`lanes::retire_when_settled`): cancelled when a
    /// restart in place gives it new lanes, or it is removed.
    pub retiring: tokio_util::sync::CancellationToken,
    /// The generation its code loaded under (`ductus_script::Isolate::set_generation`): the
    /// start's 0, then the deploy's or reload's that changed it last. Code put back after a failed
    /// change loads under its old one, and so gets its old modules back.
    pub generation: u64,
    /// The files its code loaded (its code version's), by hash: a renewal loading it again on a
    /// new thread must find the same (`Kept::renew`).
    pub code: Fingerprint,
}

/// Some of an interface's slots, on one worker thread.
pub(crate) struct Placed {
    pub commands: UnboundedSender<Command>,
    pub slots: Vec<u32>,
}

/// What one destination keeps when its settings change.
pub(crate) struct KeptDestination {
    pub id: DestinationId,
    pub stats: Arc<DestinationStats>,
    pub pause: Arc<Pause>,
    /// Its allowance so far, kept when its rate limit is the same.
    pub limiter: Option<Arc<Limiter>>,
}

/// An interface as `Kept::replace` left it: what the engine shows of it from then on.
pub struct Replaced {
    pub config: Arc<InterfaceConfig>,
    pub destinations: Vec<RunningDestination>,
    /// The files its new code loaded (its code version's).
    pub files: Vec<CodeFile>,
}

/// A change that can't be applied while the interface runs: the handover applies it. Says why.
#[derive(Debug)]
pub struct NeedsHandover(pub String);

impl std::fmt::Display for NeedsHandover {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NeedsHandover {}

impl Shared {
    /// Queued interface `name`'s intake, if it runs here.
    pub fn intake(&self, name: &str) -> Option<Arc<Intake>> {
        self.intakes.read().unwrap_or_else(|e| e.into_inner()).get(name).cloned()
    }

    /// `config`'s destination `name`'s outlet. TLS settings are built from the engine's
    /// certificates (destinations naming the same CA and certificate share them).
    pub fn outlet(
        &self,
        config: &InterfaceConfig,
        name: &str,
        d: &DestinationConfig,
        kept: KeptDestination,
    ) -> Result<Outlet> {
        let tls = match &d.connector {
            Destination::Mllp { tls: Some(t), .. } => Some(self.certificates.client(t)),
            Destination::Http { api: Some(api), .. } => {
                self.resources.apis.get(api).and_then(|a| self.certificates.named(&a.tls()).transpose())
            }
            Destination::Http { ca, certificate, .. } => {
                self.certificates.named(&ductus_core::tls::ConnectTls::new(ca, certificate)).transpose()
            }
            _ => None,
        };
        let tls = tls.transpose().with_context(|| format!("{}: TLS", config.label_destination(name)))?;
        let route = match d.route() {
            Some(target) => Some(self.intake(target).with_context(|| {
                format!("{}: routes to `{target}`, which isn't a queued interface here", config.label_destination(name))
            })?),
            None => None,
        };
        Ok(Outlet {
            id: kept.id,
            tls,
            stats: kept.stats,
            pause: kept.pause,
            limiter: kept.limiter.or_else(|| {
                config.rate_limit_for(d).map(|r| Arc::new(Limiter::new(r.clone(), std::time::Instant::now())))
            }),
            server: match &d.connector {
                Destination::Sftp { server, .. } => self.resources.sftp.get(server).cloned(),
                _ => None,
            },
            api: match &d.connector {
                Destination::Http { api: Some(api), .. } => self.resources.apis.get(api).cloned(),
                _ => None,
            },
            route,
        })
    }
}

impl Interface {
    /// Slot `slot`'s job (a respond-mode interface's first gets `requests`). `waiting`: the
    /// start's list of interfaces with messages waiting (its workers skip their first read when
    /// theirs had none); a replacement's is empty, so its workers read first (the workers they
    /// replace may have left messages unread).
    pub fn job(
        &self,
        shared: &Shared,
        slot: u32,
        requests: Option<crate::worker::Requests>,
        waiting: &Arc<Waiting>,
    ) -> Job {
        Job {
            config: self.config.clone(),
            slot,
            wake: self.wake.clone(),
            requests,
            health: self.health.clone(),
            outlets: self.outlets.clone(),
            stats: self.stats.clone(),
            ownership: shared.ownership.clone(),
            waiting: waiting.clone(),
            decided: self.decided.clone(),
            lanes: self.lanes.clone(),
            generation: self.generation,
        }
    }

    /// Every slot's job, by the thread it runs on (replacements: see `job`).
    fn jobs(&self, shared: &Shared) -> Vec<(UnboundedSender<Command>, Vec<Job>)> {
        let fresh = Arc::default();
        let jobs = |p: &Placed| p.slots.iter().map(|&slot| self.job(shared, slot, None, &fresh)).collect();
        self.threads.iter().map(|p| (p.commands.clone(), jobs(p))).collect()
    }

    /// What it shows the engine: its config and destinations.
    fn replaced(&self, files: Vec<CodeFile>) -> Replaced {
        let destinations = self
            .config
            .destinations()
            .zip(&self.outlets)
            .map(|((name, _), o)| RunningDestination { name: name.to_string(), id: o.id, stats: o.stats.clone() })
            .collect();
        Replaced { config: self.config.clone(), destinations, files }
    }

    /// Its parts as they are, on no thread (`Kept::stop`).
    fn stopped(&self) -> Interface {
        Interface {
            config: self.config.clone(),
            wake: self.wake.clone(),
            health: self.health.clone(),
            stats: self.stats.clone(),
            lanes: self.lanes.clone(),
            outlets: self.outlets.clone(),
            intake: self.intake.clone(),
            decided: self.decided.clone(),
            threads: Vec::new(),
            removed: false,
            retiring: self.retiring.clone(),
            generation: self.generation,
            code: self.code.clone(),
        }
    }

    /// What the activity saver watches of it.
    pub(crate) fn watched(&self) -> Watched {
        Watched {
            stats: self.stats.clone(),
            destinations: self.outlets.iter().map(|o| (o.id, o.stats.clone())).collect(),
        }
    }

    /// The outlets for `config`. A destination it had (by name) keeps its id, counts and pause,
    /// and its allowance if its rate limit is the same; a new one gets its id and what's stored
    /// about it (`Activity::watch`, which from then on watches `config`'s destinations).
    pub(crate) async fn outlets_for(
        &self,
        shared: &Shared,
        config: &InterfaceConfig,
        activity: &Activity,
    ) -> Result<Vec<Outlet>> {
        let old: HashMap<&str, (&DestinationConfig, &Outlet)> =
            self.config.destinations().zip(&self.outlets).map(|((name, d), o)| (name, (d, o))).collect();
        let added: Vec<(String, String)> = config
            .destinations()
            .filter(|(name, _)| !old.contains_key(name))
            .map(|(name, _)| (config.name.clone(), name.to_string()))
            .collect();
        let mut ids: HashMap<&str, DestinationId> = HashMap::new();
        if !added.is_empty() {
            let new = shared.store.destination_ids(&added).await.context("registering the new destinations")?;
            ids.extend(added.iter().map(|(_, name)| name.as_str()).zip(new));
        }
        let mut kept: Vec<(DestinationId, Arc<DestinationStats>, Option<Arc<Limiter>>)> = Vec::new();
        for (name, d) in config.destinations() {
            kept.push(match old.get(name) {
                Some((was, o)) => {
                    let same_limit = config.rate_limit_for(d) == self.config.rate_limit_for(was);
                    (o.id, o.stats.clone(), if same_limit { o.limiter.clone() } else { None })
                }
                None => (ids[name], Arc::new(DestinationStats::new(name)), None),
            });
        }
        if self.config.destinations().map(|(n, _)| n).ne(config.destinations().map(|(n, _)| n)) {
            let destinations = kept.iter().map(|(id, stats, _)| (*id, stats.clone())).collect();
            activity.watch(Watched { stats: self.stats.clone(), destinations }).await?;
        }
        config
            .destinations()
            .zip(kept)
            .map(|((name, d), (id, stats, limiter))| {
                let pause = activity
                    .pause(id)
                    .with_context(|| format!("{}: no pause switch", config.label_destination(name)))?;
                shared.outlet(config, name, d, KeptDestination { id, stats, pause, limiter })
            })
            .collect()
    }
}

impl Kept {
    /// From the next call on, every interface's code reads `env` (its variables, secrets and
    /// `console:`); the one it replaces is returned (None: no worker threads, as in unit tests).
    pub(crate) fn swap_environment(&self, env: Arc<ductus_core::Environment>) -> Option<Arc<ductus_core::Environment>> {
        self.threads.spawn.as_ref().map(|s| s.services.swap_environment(env))
    }

    /// What a start built: `interfaces` with the threads their slots run on.
    pub(crate) fn new(shared: Shared, interfaces: impl IntoIterator<Item = Interface>, threads: Threads) -> Self {
        let interfaces = interfaces.into_iter().map(|i| (i.config.name.clone(), Arc::new(Mutex::new(i)))).collect();
        Self { interfaces: std::sync::RwLock::new(interfaces), shared, threads, generation: Default::default() }
    }

    /// A new generation for the code a deploy or reload is about to load (`Interface::generation`):
    /// one per deploy, never used before. In folder mode it's what makes a changed file a new
    /// module (its path is the same: `ductus_script::Isolate::set_generation`); a release's files
    /// have paths of their own, where it changes nothing but the URLs.
    pub(crate) fn next_generation(&self) -> u64 {
        self.generation.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1
    }

    /// The generation `name`'s code loaded under, if it runs here (what putting it back loads under).
    pub(crate) async fn generation_of(&self, name: &str) -> Option<u64> {
        Some(self.get(name)?.lock().await.generation)
    }

    /// What every interface's jobs and outlets are built with (an added interface's too).
    pub(crate) fn shared(&self) -> &Shared {
        &self.shared
    }

    fn get(&self, name: &str) -> Option<Arc<Mutex<Interface>>> {
        self.interfaces.read().unwrap_or_else(|e| e.into_inner()).get(name).cloned()
    }

    /// Whether `name` runs here.
    pub fn contains(&self, name: &str) -> bool {
        self.interfaces.read().unwrap_or_else(|e| e.into_inner()).contains_key(name)
    }

    /// Every worker thread's handle, to join at the engine's stop (a thread made after this is
    /// left to end by itself: the stop has begun, so it does at once).
    pub(crate) fn take_threads(&self) -> Vec<std::thread::JoinHandle<()>> {
        std::mem::take(&mut *self.threads.handles.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// Run `interface` from now on (added by a deploy: its parts built, its `threads` empty).
    /// A shared interface's slots go on one new pool thread while the pool has fewer threads than
    /// it may (the start made only as many as it needed), else each on the pool thread running the
    /// fewest slots; a dedicated interface's each on a new thread of its own (sharing its own
    /// pools). Its code loads there first; a respond-mode interface's first slot answers
    /// `requests`; its workers read first (`Interface::job`). A queued interface's intake is
    /// registered for routes to it once it runs. Answers the files its code loaded (its code
    /// version's).
    ///
    /// All or nothing: when a thread can't load its code, nothing of it runs (every thread it was
    /// given to is told to stop running it; a dedicated thread made for it then ends; a new pool
    /// thread stays, empty, for the next) and the error says why.
    pub(crate) async fn add(&self, mut interface: Interface, mut requests: Option<Requests>) -> Result<Vec<CodeFile>> {
        let name = interface.config.name.clone();
        if self.contains(&name) {
            bail!("interface `{name}` already runs here");
        }
        let spawn = self.threads.spawn.as_ref();
        let fresh: Arc<Waiting> = Arc::default();
        let jobs: Vec<Job> = (0..interface.config.slots())
            .map(|slot| interface.job(&self.shared, slot, requests.take(), &fresh))
            .collect();
        // Existing pool threads given jobs, and new threads to make (name, jobs, its own pools).
        let mut existing: Vec<(UnboundedSender<Command>, Vec<Job>)> = Vec::new();
        let mut new: Vec<(String, Vec<Job>, crate::services::Pools)> = Vec::new();
        let mut new_pool = None; // the new pool thread's commands, once registered
        if interface.config.isolation == Isolation::Dedicated {
            let spawn = spawn.with_context(|| format!("`{name}`: no worker threads can be made here"))?;
            let own = spawn.services.dedicated_pools(&interface.config);
            new.extend(jobs.into_iter().map(|job| (format!("{name}-w{}", job.slot), vec![job], own.clone())));
        } else {
            let mut pool = self.threads.pool.lock().unwrap_or_else(|e| e.into_inner());
            if spawn.is_some_and(|s| pool.len() < s.pool_threads) {
                let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
                pool.push(PoolThread { commands: commands.clone(), slots: jobs.len() });
                new_pool = Some((commands, receiver));
                new.push((format!("pool-{}", pool.len() - 1), jobs, Default::default()));
            } else {
                for job in jobs {
                    let thread = pool.iter_mut().min_by_key(|p| p.slots).context("the pool has no threads")?;
                    thread.slots += 1;
                    match existing.iter_mut().find(|(c, _)| c.same_channel(&thread.commands)) {
                        Some((_, jobs)) => jobs.push(job),
                        None => existing.push((thread.commands.clone(), vec![job])),
                    }
                }
            }
        }

        // New threads load their code as they start (as a start's do), reporting on `ready`.
        let mut placed: Vec<Placed> = Vec::new();
        let mut failed: Option<anyhow::Error> = None;
        let mut files = None;
        let (ready, answers) = std::sync::mpsc::channel();
        let mut waiting_for = 0;
        for (thread, jobs, own) in new {
            let (commands, receiver) = match new_pool.take() {
                Some(channel) => channel,
                None => tokio::sync::mpsc::unbounded_channel(),
            };
            let spawn = spawn.expect("new threads only when they can be made");
            let budget = spawn.budget(&commands);
            placed.push(Placed { commands, slots: jobs.iter().map(|j| j.slot).collect() });
            waiting_for += jobs.len();
            let services = crate::services::EngineServices::new(spawn.services.clone(), own);
            let stop = spawn.stop.clone();
            let store = self.shared.store.clone();
            match crate::worker::spawn_thread(
                thread,
                jobs,
                store,
                services,
                spawn.heap_mb,
                ready.clone(),
                stop,
                None,
                receiver,
                budget,
            ) {
                Ok(handle) => self.threads.handles.lock().unwrap_or_else(|e| e.into_inner()).push(handle),
                Err(e) => {
                    failed.get_or_insert(e);
                    break;
                }
            }
        }
        drop(ready);
        let reported = tokio::task::spawn_blocking(move || answers.iter().collect::<Vec<_>>())
            .await
            .context("waiting for the new worker threads to load the code")?;
        if failed.is_none() && reported.len() < waiting_for {
            failed =
                Some(anyhow!("`{name}`: a new worker thread stopped before loading its code (see the errors above)"));
        }
        for (_, result) in reported {
            match result {
                Ok(f) => {
                    files.get_or_insert(f);
                }
                Err(e) => {
                    failed.get_or_insert(e);
                }
            }
        }
        // Existing pool threads load it beside what they run (`Command::Replace` from nothing), one
        // after another; each is counted already (above), sent to or not.
        let mut counted: Vec<Placed> = Vec::new();
        for (commands, jobs) in existing {
            let slots = jobs.iter().map(|j| j.slot).collect();
            if failed.is_none() {
                match send(&commands, &name, jobs).await {
                    Ok(f) => {
                        files.get_or_insert(f);
                    }
                    Err(e) => failed = Some(e),
                }
            }
            counted.push(Placed { commands, slots });
        }
        if let Some(error) = failed {
            // Told to stop running it, wherever it may run (a thread that never got it: nothing).
            for p in placed.iter().chain(&counted) {
                if let Err(e) = send(&p.commands, &name, Vec::new()).await {
                    tracing::warn!(interface = %name, error = %format!("{e:#}"), "couldn't stop an added interface that failed to load on another thread");
                }
            }
            placed.extend(counted);
            self.counted_off(&placed);
            return Err(error);
        }
        placed.extend(counted);
        interface.threads = placed;
        let files = files.unwrap_or_default();
        interface.code = fingerprint(&files);
        if let Some(intake) = &interface.intake {
            self.shared.intakes.write().unwrap_or_else(|e| e.into_inner()).insert(name.clone(), intake.clone());
        }
        self.interfaces.write().unwrap_or_else(|e| e.into_inner()).insert(name, Arc::new(Mutex::new(interface)));
        Ok(files)
    }

    /// Stop running `name` (removed by a deploy): its workers end between messages as at the
    /// engine's stop (the message in hand finishes, what's done is saved), each thread it ran on
    /// told so (a dedicated thread then ends), and nothing of it is kept, its intake no longer
    /// found by routes. Close its gate and let what it was storing finish first. A change of it
    /// waiting for its lock finds it gone. Errors only when it isn't running here.
    pub(crate) async fn remove(&self, name: &str) -> Result<()> {
        let (stopped, _) = self.stop(name).await?;
        stopped.retiring.cancel();
        Ok(())
    }

    /// Stop running `name` as `remove` does, and answer its parts (no threads: ready for `add`
    /// with other settings, or the same again) and a respond-mode interface's requests waiting
    /// (handed back by its old responder, to give its next one: none is lost). Every worker of it
    /// has stopped when this returns, so the next ones never share a lane with them (a restart in
    /// place, 0.7.5.2.4).
    pub(crate) async fn stop(&self, name: &str) -> Result<(Interface, Option<Requests>)> {
        let interface = self
            .interfaces
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(name)
            .with_context(|| format!("interface `{name}` isn't running here"))?;
        let mut interface = interface.lock().await;
        interface.removed = true;
        let mut requests = None;
        for placed in &interface.threads {
            let (handed, handing) = oneshot::channel();
            match send_handing(&placed.commands, name, Vec::new(), Some(handed)).await {
                Ok(_) => {
                    if let Ok(r) = handing.await {
                        requests = Some(r);
                    }
                }
                Err(e) => {
                    tracing::warn!(interface = %name, error = %format!("{e:#}"), "couldn't tell a worker thread to stop running an interface (it ends with the engine)")
                }
            }
        }
        self.counted_off(&interface.threads);
        self.shared.intakes.write().unwrap_or_else(|e| e.into_inner()).remove(name);
        Ok((interface.stopped(), requests))
    }

    /// The pool threads among `placed` run their slots no more. A pool thread that couldn't be
    /// made (or stopped) leaves the pool.
    fn counted_off(&self, placed: &[Placed]) {
        let mut pool = self.threads.pool.lock().unwrap_or_else(|e| e.into_inner());
        for p in placed {
            if let Some(thread) = pool.iter_mut().find(|t| t.commands.same_channel(&p.commands)) {
                thread.slots = thread.slots.saturating_sub(p.slots.len());
            }
        }
        pool.retain(|t| !t.commands.is_closed());
    }

    /// Run `config`'s interface on its new code and settings from its next message (see
    /// `worker::Command::Replace`), on every thread its slots run on, its destinations added or
    /// removed as the module says. Its new workers save their decisions with the version id
    /// `decided` gives (they wait for it; the workers they replace keep the old one for the
    /// message in hand). Answers what it runs now.
    ///
    /// A thread that can't load the new code leaves the interface as it was: threads that already
    /// switched are given the old code back (the same files: V8's own modules, so nothing loads
    /// twice) before the error is returned. A destination change while its lanes are changing is
    /// refused with `NeedsHandover`, nothing changed. Its code loads under `generation`
    /// (`Interface::generation`).
    pub async fn replace(
        &self,
        config: InterfaceConfig,
        activity: &Activity,
        decided: crate::deployment::Decided,
        generation: u64,
    ) -> Result<Replaced> {
        let name = config.name.clone();
        let interface = self.get(&name).with_context(|| format!("interface `{name}` isn't running here"))?;
        let mut interface = interface.lock().await;
        if interface.removed {
            bail!("interface `{name}` was removed meanwhile");
        }
        let config = Arc::new(config);
        let retarget = interface.config.destinations().map(|(n, _)| n).ne(config.destinations().map(|(n, _)| n));
        if retarget && interface.lanes.is_changing() {
            return Err(NeedsHandover(format!(
                "`{name}`: its destinations changed while its lanes are changing (`ordering` changed since this engine \
                 started; each old lane's drain is followed per destination): a handover applies it"
            ))
            .into());
        }
        if retarget && interface.intake.is_none() {
            bail!("`{name}`: destinations added to an interface in respond mode (it has no queue)");
        }
        let outlets = match interface.outlets_for(&self.shared, &config, activity).await {
            Ok(outlets) => outlets,
            Err(e) => {
                if retarget {
                    let _ = activity.watch(interface.watched()).await; // nothing new to read: no query
                }
                return Err(e);
            }
        };
        let wake = match retarget {
            true => Arc::new(Wake::new(interface.wake.slots() as u32, outlets.len())),
            false => interface.wake.clone(),
        };
        let old = (
            std::mem::replace(&mut interface.config, config),
            std::mem::replace(&mut interface.outlets, outlets),
            std::mem::replace(&mut interface.wake, wake),
            std::mem::replace(&mut interface.decided, decided),
            std::mem::replace(&mut interface.generation, generation),
        );
        let mut switched = Vec::new();
        let mut files = Vec::new();
        let mut failed = None;
        for (commands, jobs) in interface.jobs(&self.shared) {
            match send(&commands, &name, jobs).await {
                Ok(f) => {
                    files = f;
                    switched.push(commands);
                }
                Err(e) => {
                    failed = Some(e);
                    break;
                }
            }
        }
        if let Some(error) = failed {
            (interface.config, interface.outlets, interface.wake, interface.decided, interface.generation) = old;
            self.put_back(&interface, &switched).await;
            if retarget && let Err(e) = activity.watch(interface.watched()).await {
                tracing::warn!(interface = %name, error = %format!("{e:#}"), "couldn't watch its old destinations again: their last activity isn't saved until the next start");
            }
            return Err(error);
        }
        interface.code = fingerprint(&files);
        if retarget {
            self.retarget(&interface).await?;
        }
        Ok(interface.replaced(files))
    }

    /// Give the threads in `switched` `interface`'s jobs again (its old config, put back).
    async fn put_back(&self, interface: &Interface, switched: &[UnboundedSender<Command>]) {
        let name = &interface.config.name;
        let back = interface.jobs(&self.shared);
        for (commands, jobs) in back.into_iter().filter(|(c, _)| switched.iter().any(|s| s.same_channel(c))) {
            if let Err(e) = send(&commands, name, jobs).await {
                // Its old code loaded before: only a runaway during the load gets here.
                tracing::error!(interface = %name, error = %format!("{e:#}"), "couldn't give a thread its old code back: it runs the new code there");
            }
        }
    }

    /// Queue `interface`'s messages for its destinations now, and match what's waiting to them
    /// (steps 3 of the module).
    async fn retarget(&self, interface: &Interface) -> Result<()> {
        let config = &interface.config;
        let Some(intake) = &interface.intake else { return Ok(()) };
        let ids: Vec<DestinationId> = interface.outlets.iter().map(|o| o.id).collect();
        intake.retarget(Targets { destinations: ids.clone(), wake: interface.wake.clone() }).await;
        let names: Vec<(String, String)> =
            config.destinations().map(|(n, _)| (config.name.clone(), n.to_string())).collect();
        let mut tries = 0;
        let reconciled = loop {
            match self.shared.store.reconcile(&names, &ids).await {
                Ok(r) => break r,
                // Its workers run and new messages go to the new destinations; what waited from
                // before is matched at the next start if not now.
                Err(e) if tries == 4 => {
                    interface.wake.everyone();
                    return Err(e.context(format!(
                        "`{}`: its new destinations run, but the messages already waiting weren't matched to them \
                         (a removed destination's stay pending, a new one has none of them): the next start does it",
                        config.name
                    )));
                }
                Err(e) => {
                    tries += 1;
                    tracing::warn!(interface = %config.name, error = %format!("{e:#}"), "matching waiting messages to its new destinations failed: trying again");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        };
        for (_, count) in &reconciled.added {
            tracing::info!(interface = %config.name, count, "queued messages waiting for its new destinations");
        }
        for (_, destination, count) in &reconciled.removed {
            tracing::warn!(interface = %config.name, %destination, count, "destination is no longer in interface.yaml: its pending deliveries were marked filtered");
        }
        // Its workers started before the messages above were theirs: they read again.
        interface.wake.everyone();
        Ok(())
    }

    /// Give the thread asking (`renewal`: its isolate holds more superseded code than its budget)
    /// a fresh isolate. V8 can't drop an isolate made before another on the same thread, so the
    /// fresh one is on a new thread, of the same name and services:
    /// 1. Every interface with slots on the old thread is locked, in name order (a deploy of one
    ///    waits for the renewal; deploys of the others go on).
    /// 2. The new thread loads each one's code for its slots there, beside everything running (no
    ///    one waits for it), and must find the files its version names (`Interface::code`): code
    ///    edited in a project folder and not reloaded is never loaded this way.
    /// 3. Then each in turn moves, between messages: stopped on the old thread (the message in
    ///    hand finishes, a respond-mode interface's waiting requests handed over), started on the
    ///    new, its lock let go. Only that interface pauses, for that long.
    /// 4. The old thread, commanded by no one and hosting nothing, ends: its isolate and every
    ///    version it held go with it.
    ///
    /// Until step 3 nothing has changed: a failure leaves everything on the old thread (the new one
    /// ends), and the thread asks again after its next deploy. Answers None when there was nothing
    /// to do (the thread ended or was renewed already).
    pub(crate) async fn renew(&self, renewal: Renewal) -> Result<Option<Renewed>> {
        let Some(old) = renewal.commands.upgrade() else { return Ok(None) };
        let spawn = self.threads.spawn.as_ref().context("no worker threads can be made here")?;
        let mut names: Vec<String> =
            self.interfaces.read().unwrap_or_else(|e| e.into_inner()).keys().cloned().collect();
        names.sort();
        let mut moving = Vec::new();
        for name in names {
            let Some(interface) = self.get(&name) else { continue };
            let interface = interface.lock_owned().await;
            if !interface.removed && interface.threads.iter().any(|p| p.commands.same_channel(&old)) {
                moving.push(interface);
            }
        }
        if moving.is_empty() {
            return Ok(None);
        }
        let started = std::time::Instant::now();
        let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
        let (ready, _) = std::sync::mpsc::channel(); // it starts with nothing to report
        let handle = crate::worker::spawn_thread(
            renewal.thread.clone(),
            Vec::new(),
            self.shared.store.clone(),
            renewal.services,
            spawn.heap_mb,
            ready,
            spawn.stop.clone(),
            None,
            receiver,
            spawn.budget(&commands),
        )?;
        self.threads.handles.lock().unwrap_or_else(|e| e.into_inner()).push(handle);
        let fresh: Arc<Waiting> = Arc::default();
        let slots_on_old = |interface: &Interface| -> Vec<u32> {
            interface.threads.iter().filter(|p| p.commands.same_channel(&old)).flat_map(|p| p.slots.clone()).collect()
        };
        for interface in &moving {
            let name = &interface.config.name;
            let jobs =
                slots_on_old(interface).into_iter().map(|s| interface.job(&self.shared, s, None, &fresh)).collect();
            let files = load_held(&commands, name, jobs).await?;
            if fingerprint(&files) != interface.code {
                bail!(
                    "`{name}`'s files on disk aren't the ones it runs (edited since it was loaded, not reloaded): it \
                     isn't loaded again from them this way"
                );
            }
        }
        let loading = started.elapsed();
        let mut longest = Duration::ZERO;
        let count = moving.len();
        for mut interface in moving {
            let name = interface.config.name.clone();
            let pausing = std::time::Instant::now();
            let (handed, handing) = oneshot::channel();
            if let Err(e) = send_handing(&old, &name, Vec::new(), Some(handed)).await {
                // Its workers there are gone with the thread: it starts on the new one all the same.
                tracing::warn!(interface = %name, error = %format!("{e:#}"), "couldn't stop it on the thread being renewed");
            }
            let requests = handing.await.ok();
            start_held(&commands, &name, requests).await.with_context(|| {
                format!("`{name}` stopped on the thread being renewed and couldn't start on the new one (a restart runs it again)")
            })?;
            for placed in interface.threads.iter_mut().filter(|p| p.commands.same_channel(&old)) {
                placed.commands = commands.clone();
            }
            longest = longest.max(pausing.elapsed());
        }
        let mut pool = self.threads.pool.lock().unwrap_or_else(|e| e.into_inner());
        for thread in pool.iter_mut().filter(|t| t.commands.same_channel(&old)) {
            thread.commands = commands.clone();
        }
        Ok(Some(Renewed { interfaces: count, loading, took: started.elapsed(), longest_pause: longest }))
    }

    /// Where threads ask to be renewed, and the engine's stop (once: `renew_beside`).
    fn renewals(&self) -> Option<(tokio::sync::mpsc::UnboundedReceiver<Renewal>, tokio_util::sync::CancellationToken)> {
        let stop = self.threads.spawn.as_ref()?.stop.clone();
        Some((self.threads.renewals.lock().unwrap_or_else(|e| e.into_inner()).take()?, stop))
    }
}

/// What `Kept::renew` did.
pub(crate) struct Renewed {
    /// How many interfaces moved to the new thread.
    pub interfaces: usize,
    /// Loading their code there (nothing paused meanwhile).
    pub loading: Duration,
    /// The whole renewal.
    pub took: Duration,
    /// The longest one interface paused while it moved.
    pub longest_pause: Duration,
}

/// Renew the worker threads that ask (`Kept::renew`), one at a time, beside everything else, until
/// the engine stops. Never on a timer: a thread asks only after a deploy replaced code on it.
pub(crate) fn renew_beside(engine: &Arc<crate::control::Engine>) {
    let Some((mut asked, stop)) = engine.kept.renewals() else { return };
    let engine = Arc::downgrade(engine);
    tokio::spawn(async move {
        loop {
            let renewal = tokio::select! {
                _ = stop.cancelled() => return,
                asked = asked.recv() => match asked {
                    Some(renewal) => renewal,
                    None => return,
                },
            };
            let Some(engine) = engine.upgrade() else { return };
            let (thread, superseded) = (renewal.thread.clone(), renewal.superseded);
            match engine.kept.renew(renewal).await {
                Ok(Some(r)) => {
                    // The old thread ends once let go: glibc keeps what its arena freed until asked.
                    crate::give_back_freed_memory();
                    tracing::info!(
                    %thread,
                    superseded_mb = format!("{:.1}", superseded as f64 / (1 << 20) as f64),
                    interfaces = r.interfaces,
                    loading = ?r.loading,
                    took = ?r.took,
                    longest_pause = ?r.longest_pause,
                    "renewed: its interfaces run in a fresh isolate on a new thread, and the old one's memory is freed (isolates.superseded_mb)"
                    )
                }
                Ok(None) => {}
                Err(e) => tracing::warn!(
                    %thread,
                    error = %format!("{e:#}"),
                    "couldn't give this thread a fresh isolate (isolates.superseded_mb): its interfaces run on as they were; it's tried again after its next deploy"
                ),
            }
        }
    });
}

/// Ask a renewal's new thread to load `jobs` for `interface` and hold them (`Command::Load`).
async fn load_held(commands: &UnboundedSender<Command>, interface: &str, jobs: Vec<Job>) -> Result<Vec<CodeFile>> {
    let (answer, answered) = oneshot::channel();
    commands
        .send(Command::Load { interface: interface.to_string(), jobs, answer })
        .map_err(|_| anyhow!("`{interface}`: the new worker thread has stopped"))?;
    answered.await.map_err(|_| anyhow!("`{interface}`: the new worker thread stopped before answering"))?
}

/// Ask a renewal's new thread to start what it holds for `interface` (`Command::Start`).
async fn start_held(commands: &UnboundedSender<Command>, interface: &str, requests: Option<Requests>) -> Result<()> {
    let (answer, answered) = oneshot::channel();
    commands
        .send(Command::Start { interface: interface.to_string(), requests, answer })
        .map_err(|_| anyhow!("`{interface}`: the new worker thread has stopped"))?;
    match answered.await {
        Ok(true) => Ok(()),
        Ok(false) => bail!("`{interface}`: the new worker thread held nothing for it"),
        Err(_) => bail!("`{interface}`: the new worker thread stopped before answering"),
    }
}

/// Ask one thread to run `jobs` as `interface` and wait for its answer.
async fn send(commands: &UnboundedSender<Command>, interface: &str, jobs: Vec<Job>) -> Result<Vec<CodeFile>> {
    send_handing(commands, interface, jobs, None).await
}

/// `send`, a respond-mode interface's requests handed to `handed` when no job takes them.
async fn send_handing(
    commands: &UnboundedSender<Command>,
    interface: &str,
    jobs: Vec<Job>,
    handed: Option<oneshot::Sender<Requests>>,
) -> Result<Vec<CodeFile>> {
    let (answer, answered) = oneshot::channel();
    commands
        .send(Command::Replace { interface: interface.to_string(), jobs, answer, handed })
        .map_err(|_| anyhow!("`{interface}`: its worker thread has stopped"))?;
    answered.await.map_err(|_| anyhow!("`{interface}`: its worker thread stopped before answering"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lanes::{Generation, Layout};
    use tokio::sync::mpsc;

    fn config(yaml: &str) -> InterfaceConfig {
        serde_yaml_ng::from_str(yaml).unwrap()
    }

    /// `adt`'s version id is `id`, saved.
    fn decided(id: i32) -> crate::deployment::Decided {
        let (saving, decided) = crate::deployment::Decided::channel();
        let by_interface = [("adt".to_string(), id)].into();
        saving.send_replace(Some(Arc::new(crate::deployment::Saved { ids: Default::default(), by_interface })));
        decided
    }

    const ONE: &str = "name: adt\nsource: { mllp: { port: 6661 } }\ndestinations:\n  lab: { file: { dir: lab }, rate_limit: { per_minute: 60 } }\n  ris: { file: { dir: ris }, rate_limit: { per_minute: 60 } }\n";

    /// A thread that answers each `Replace` with `answer(n)` (n: how many it got before), and
    /// tells the test which jobs each one carried (the first slot's).
    fn thread(
        answer: impl Fn(usize) -> Result<Vec<CodeFile>> + Send + 'static,
    ) -> (UnboundedSender<Command>, mpsc::UnboundedReceiver<Job>) {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let (seen, jobs) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut n = 0;
            while let Some(Command::Replace { mut jobs, answer: reply, .. }) = rx.recv().await {
                if !jobs.is_empty() {
                    let _ = seen.send(jobs.swap_remove(0));
                }
                let _ = reply.send(answer(n));
                n += 1;
            }
        });
        (tx, jobs)
    }

    /// A thread that answers each `Replace` with `answer(n)` and tells the test each one's
    /// interface and slots (none: stop running it here).
    fn recording(
        answer: impl Fn(usize) -> Result<Vec<CodeFile>> + Send + 'static,
    ) -> (UnboundedSender<Command>, mpsc::UnboundedReceiver<(String, Vec<u32>)>) {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let (seen, commands) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut n = 0;
            while let Some(Command::Replace { interface, jobs, answer: reply, .. }) = rx.recv().await {
                let _ = seen.send((interface, jobs.iter().map(|j| j.slot).collect()));
                let _ = reply.send(answer(n));
                n += 1;
            }
        });
        (tx, commands)
    }

    /// A shared interface (one slot: `workers` is a dedicated interface's).
    const TWO: &str = "name: two\nsource: { mllp: { port: 6662 } }\ndestinations:\n  lab: { file: { dir: lab } }\n";

    fn pool_slots(kept: &Kept) -> Vec<usize> {
        kept.threads.pool.lock().unwrap().iter().map(|p| p.slots).collect()
    }

    /// An added interface runs on the pool thread running the fewest slots (the first of equals),
    /// its intake is found by routes, and adding it again is refused. Removed: the thread it ran
    /// on is told to stop running it, the count and its intake go, and a change of it after is
    /// refused.
    #[tokio::test]
    async fn an_added_interface_runs_where_fewest_slots_run_and_a_removed_one_leaves_nothing() {
        let ((a, mut seen_a), (b, mut seen_b)) = (recording(|_| Ok(Vec::new())), recording(|_| Ok(Vec::new())));
        let store = Store::lazy("postgres://nobody@127.0.0.1:1/none").unwrap();
        let (kept, activity) = kept_with(ONE, vec![a], store, 1, Layout::single(1, "x"));
        kept.threads.pool.lock().unwrap().push(PoolThread { commands: b, slots: 0 });
        let (two, _) = parts(TWO, kept.shared(), 10, Layout::single(1, "x"));
        kept.add(two, None).await.unwrap();
        assert_eq!(seen_b.recv().await.unwrap(), ("two".to_string(), vec![0]), "where none ran");
        let (three, _) = parts(&TWO.replace("two", "three"), kept.shared(), 20, Layout::single(1, "x"));
        kept.add(three, None).await.unwrap();
        assert_eq!(seen_a.recv().await.unwrap(), ("three".to_string(), vec![0]), "on the first of equals");
        assert_eq!(pool_slots(&kept), [2, 1]);
        assert!(kept.contains("two") && kept.shared().intake("two").is_some());
        let (again, _) = parts(TWO, kept.shared(), 30, Layout::single(1, "x"));
        let error = kept.add(again, None).await.map(drop).unwrap_err();
        assert!(format!("{error:#}").contains("`two` already runs here"), "{error:#}");

        kept.remove("two").await.unwrap();
        assert_eq!(seen_b.recv().await.unwrap(), ("two".to_string(), vec![]), "told to stop running it");
        assert_eq!(pool_slots(&kept), [2, 0]);
        assert!(!kept.contains("two") && kept.shared().intake("two").is_none());
        let error = kept.replace(config(TWO), &activity, decided(2), 1).await.map(drop).unwrap_err();
        assert!(format!("{error:#}").contains("`two` isn't running here"), "{error:#}");
        assert!(kept.remove("two").await.is_err());
        assert!(kept.contains("adt") && kept.contains("three"), "the others run on");
        assert!(seen_a.try_recv().is_err(), "and their thread was told nothing");
    }

    /// A thread that can't load an added interface's code leaves nothing of it: the thread is
    /// told to stop running it, its count given back, its intake not registered.
    #[tokio::test]
    async fn an_added_interface_that_fails_to_load_leaves_nothing() {
        let (a, mut seen_a) =
            recording(|n| if n == 0 { Err(anyhow!("loading `two`'s code: SyntaxError")) } else { Ok(Vec::new()) });
        let store = Store::lazy("postgres://nobody@127.0.0.1:1/none").unwrap();
        let (kept, _) = kept_with(ONE, vec![a], store, 1, Layout::single(1, "x"));
        let (two, _) = parts(TWO, kept.shared(), 10, Layout::single(1, "x"));
        let error = kept.add(two, None).await.map(drop).unwrap_err();
        assert!(format!("{error:#}").contains("SyntaxError"), "{error:#}");
        assert_eq!(seen_a.recv().await.unwrap(), ("two".to_string(), vec![0]));
        assert_eq!(seen_a.recv().await.unwrap(), ("two".to_string(), vec![]), "told to stop running it");
        assert_eq!(pool_slots(&kept), [1], "its count given back");
        assert!(!kept.contains("two") && kept.shared().intake("two").is_none());
    }

    /// What jobs share, storing into `store`, with no interface yet.
    fn shared(store: Store) -> Shared {
        Shared {
            store,
            ownership: Arc::new(crate::ownership::Ownership::new(true)),
            waiting: Default::default(),
            certificates: Default::default(),
            resources: Default::default(),
            intakes: Default::default(),
            stopped: tokio::sync::mpsc::unbounded_channel().0,
        }
    }

    /// `yaml`'s interface's parts, its destinations numbered from `first_id` (watched by the
    /// activity returned), with `lanes`, on no thread yet; its slots as configured.
    fn parts(yaml: &str, shared: &Shared, first_id: DestinationId, lanes: Layout) -> (Interface, Activity) {
        let config = Arc::new(config(yaml));
        let ids: Vec<DestinationId> =
            (0..config.destinations().count() as DestinationId).map(|i| first_id + i).collect();
        let activity = Activity::idle(shared.store.clone(), ids.clone());
        let stats = Arc::new(InterfaceStats::new(&config.name));
        let wake = Arc::new(Wake::new(config.slots(), ids.len()));
        let lanes = Arc::new(Lanes::new(lanes, config.slots(), ids.len()));
        let tier = Arc::new(std::sync::atomic::AtomicI16::new(ductus_core::Retention::DEFAULT.days() as i16));
        let targets = Targets { destinations: ids.clone(), wake: wake.clone() };
        let settings = crate::ingest::Settings::new(config.clone(), lanes.clone(), tier, targets).unwrap();
        let intake = Intake::new(shared.store.clone(), Default::default(), stats.clone(), settings);
        let outlets = config
            .destinations()
            .zip(&ids)
            .map(|((name, d), &id)| {
                let kept = KeptDestination {
                    id,
                    stats: Arc::new(DestinationStats::new(name)),
                    pause: activity.pause(id).unwrap(),
                    limiter: None,
                };
                shared.outlet(&config, name, d, kept).unwrap()
            })
            .collect();
        let interface = Interface {
            wake,
            health: Default::default(),
            stats,
            lanes,
            outlets,
            intake: Some(Arc::new(intake)),
            decided: decided(1),
            threads: Vec::new(),
            removed: false,
            retiring: Default::default(),
            generation: 0,
            code: Vec::new(),
            config,
        };
        (interface, activity)
    }

    /// `yaml`'s interface on `threads`, slot 0 on each, storing into `store` (its destinations
    /// numbered from `first_id`), with `lanes`; and the activity its destinations are watched by.
    /// The pool is `threads`, each running that one slot.
    fn kept_with(
        yaml: &str,
        threads: Vec<UnboundedSender<Command>>,
        store: Store,
        first_id: DestinationId,
        lanes: Layout,
    ) -> (Kept, Activity) {
        let shared = shared(store);
        let (mut interface, activity) = parts(yaml, &shared, first_id, lanes);
        let name = interface.config.name.clone();
        shared.intakes.write().unwrap().insert(name, interface.intake.clone().unwrap());
        interface.threads =
            threads.iter().map(|commands| Placed { commands: commands.clone(), slots: vec![0] }).collect();
        let pool = threads.into_iter().map(|commands| PoolThread { commands, slots: 1 }).collect();
        let threads = Threads {
            pool: std::sync::Mutex::new(pool),
            spawn: None,
            handles: Default::default(),
            renewals: Default::default(),
        };
        (Kept::new(shared, [interface], threads), activity)
    }

    /// `adt` (ONE) on two threads, with a store it never reaches.
    fn kept(threads: [UnboundedSender<Command>; 2]) -> (Kept, Activity) {
        let store = Store::lazy("postgres://nobody@127.0.0.1:1/none").unwrap();
        kept_with(ONE, threads.into(), store, 1, Layout::single(1, "x"))
    }

    async fn running(kept: &Kept, name: &str) -> (Arc<InterfaceConfig>, Vec<Outlet>, Arc<Wake>) {
        let i = kept.get(name).unwrap();
        let i = i.lock().await;
        (i.config.clone(), i.outlets.clone(), i.wake.clone())
    }

    /// Every thread the interface runs on gets its new config, each destination keeping its id,
    /// counts and pause, and its allowance only while its rate limit is the same. The new workers
    /// read first (their `waiting` list is empty: the old ones may have left messages unread).
    #[tokio::test]
    async fn every_thread_gets_the_new_config_and_destinations_keep_their_parts() {
        let ((a, mut seen_a), (b, mut seen_b)) = (thread(|_| Ok(Vec::new())), thread(|_| Ok(Vec::new())));
        let (kept, activity) = kept([a, b]);
        let (_, before, wake) = running(&kept, "adt").await;
        let two = ONE.replace(
            "ris: { file: { dir: ris }, rate_limit: { per_minute: 60 } }",
            "ris: { file: { dir: ris2 }, rate_limit: { per_minute: 30 } }",
        );
        let replaced = kept.replace(config(&two), &activity, decided(2), 1).await.unwrap();
        for seen in [&mut seen_a, &mut seen_b] {
            let job = seen.recv().await.unwrap();
            assert!(job.config.destinations().any(|(_, d)| d.rate_limit.as_ref().unwrap().per_minute == 30));
            assert!(job.waiting.get().is_none(), "a replacement reads first");
            assert_eq!(job.decided.id("adt").await, Some(2), "its decisions are saved with the new version");
            assert!(Arc::ptr_eq(&job.wake, &wake), "same destinations: same wake-ups");
        }
        let (now, after, _) = running(&kept, "adt").await;
        assert!(Arc::ptr_eq(&now, &replaced.config));
        assert!(now.destinations().any(|(_, d)| d.rate_limit.as_ref().unwrap().per_minute == 30));
        for ((b, a), shown) in before.iter().zip(&after).zip(&replaced.destinations) {
            assert_eq!((b.id, a.id), (shown.id, shown.id));
            assert!(Arc::ptr_eq(&b.stats, &a.stats) && Arc::ptr_eq(&b.pause, &a.pause));
            assert!(Arc::ptr_eq(&a.stats, &shown.stats));
        }
        let same = |o: &Outlet| o.limiter.clone().unwrap();
        assert!(Arc::ptr_eq(&same(&before[0]), &same(&after[0])), "lab's limit didn't change: its allowance is kept");
        assert!(!Arc::ptr_eq(&same(&before[1]), &same(&after[1])), "ris's did: a new allowance");
    }

    /// A thread that can't load the new code leaves the interface as it was: a thread that had
    /// switched is given the old config back.
    #[tokio::test]
    async fn a_thread_that_fails_puts_the_others_back() {
        let (a, mut seen_a) = thread(|_| Ok(Vec::new()));
        let (b, _) = thread(|_| Err(anyhow!("loading `adt`'s new code (its running code goes on): SyntaxError")));
        let (kept, activity) = kept([a, b]);
        let (old, _, _) = running(&kept, "adt").await;
        let two = ONE.replace("dir: lab", "dir: lab2");
        let error = kept.replace(config(&two), &activity, decided(2), 1).await.map(drop).unwrap_err();
        assert!(format!("{error:#}").contains("SyntaxError"));
        assert!(!Arc::ptr_eq(&seen_a.recv().await.unwrap().config, &old), "the first thread switched");
        let back = seen_a.recv().await.unwrap();
        assert!(Arc::ptr_eq(&back.config, &old), "and was given the old config back");
        assert_eq!(back.decided.id("adt").await, Some(1), "and the old version");
        let (now, _, _) = running(&kept, "adt").await;
        assert!(Arc::ptr_eq(&now, &old), "the old config is what runs");
    }

    /// While its lanes are changing an interface's destinations can't change in place: refused
    /// as needing the handover, nothing sent. An interface that isn't running is refused too.
    #[tokio::test]
    async fn changed_destinations_while_lanes_change_need_the_handover() {
        let ((a, mut seen_a), (b, _)) = (thread(|_| Ok(Vec::new())), thread(|_| Ok(Vec::new())));
        let store = Store::lazy("postgres://nobody@127.0.0.1:1/none").unwrap();
        let generation = |first, lanes| Generation { first, lanes, key: Some(String::new()), mixed: false };
        let changing = Layout { generations: vec![generation(0, 2), generation(2, 1)] };
        let (kept, activity) = kept_with(ONE, vec![a, b], store, 1, changing);
        let error =
            kept.replace(config(&ONE.replace("ris:", "pacs:")), &activity, decided(2), 1).await.map(drop).unwrap_err();
        assert!(error.is::<NeedsHandover>(), "{error:#}");
        assert!(format!("{error:#}").contains("while its lanes are changing"), "{error:#}");
        assert!(seen_a.try_recv().is_err());
        // A change to a destination's settings still goes in place.
        kept.replace(config(&ONE.replace("dir: lab", "dir: lab2")), &activity, decided(2), 1).await.unwrap();
        let error =
            kept.replace(config(&ONE.replace("adt", "nope")), &activity, decided(2), 1).await.map(drop).unwrap_err();
        assert!(format!("{error:#}").contains("interface `nope` isn't running here"), "{error:#}");
    }

    /// A destination added and one removed while messages wait (needs the database): the threads
    /// get the new destinations with new wake-ups; what waited is queued for the new one and
    /// filtered for the removed one; the one that stays keeps its id and counts; messages stored
    /// from then on go to the new destinations.
    #[tokio::test]
    async fn destinations_added_and_removed_while_messages_wait() {
        let Ok(url) = std::env::var("DUCTUS_TEST_DATABASE_URL") else { return };
        let store = Store::connect(&url, 2).await.unwrap();
        let name = format!("in-place-{}", chrono::Utc::now().timestamp_micros());
        let yaml = ONE.replace("adt", &name);
        let names = |ds: &[&str]| ds.iter().map(|d| (name.clone(), d.to_string())).collect::<Vec<_>>();
        let ids = store.destination_ids(&names(&["lab", "ris"])).await.unwrap();
        assert_eq!(ids[1], ids[0] + 1, "registered together: consecutive ids");
        let (a, mut seen) = thread(|_| Ok(Vec::new()));
        let (kept, activity) = kept_with(&yaml, vec![a], store.clone(), ids[0], Layout::single(1, ""));
        let (_, before, old_wake) = running(&kept, &name).await;
        let intake = kept.shared.intake(&name).unwrap();
        for n in 0..2 {
            intake.accept(format!("MSH|^~\\&|A|B|C|D|20261113||ADT^A01|{n}|P|2.5\r").as_bytes()).await.unwrap();
        }
        let pending = async |ids: &[DestinationId]| {
            let counts = store.queue_counts().await.unwrap();
            ids.iter()
                .map(|id| counts.iter().find(|c| c.destination_id == *id).map_or(0, |c| c.pending))
                .collect::<Vec<_>>()
        };
        assert_eq!(pending(&ids).await, [2, 2]);

        let swapped = yaml.replace("ris: { file: { dir: ris }", "pacs: { file: { dir: pacs }");
        let replaced = kept.replace(config(&swapped), &activity, decided(2), 1).await.unwrap();
        let shown: Vec<&str> = replaced.destinations.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(shown, ["lab", "pacs"]);
        let (lab, pacs) = (&replaced.destinations[0], &replaced.destinations[1]);
        assert_eq!(lab.id, ids[0]);
        assert!(Arc::ptr_eq(&lab.stats, &before[0].stats), "lab stays: its counts go on");
        assert_ne!(pacs.id, ids[1]);
        let job = seen.recv().await.unwrap();
        assert!(!Arc::ptr_eq(&job.wake, &old_wake), "new destinations: new wake-ups");
        assert_eq!(job.outlets.iter().map(|o| o.id).collect::<Vec<_>>(), [lab.id, pacs.id]);
        assert!(activity.pause(pacs.id).is_some(), "the new one is watched");
        assert_eq!(pending(&[ids[0], ids[1], pacs.id]).await, [2, 0, 2], "ris's filtered, pacs's queued");

        intake.accept(b"MSH|^~\\&|A|B|C|D|20261113||ADT^A01|9|P|2.5\r").await.unwrap();
        assert_eq!(pending(&[ids[0], ids[1], pacs.id]).await, [3, 0, 3], "new messages go to the new destinations");
    }
}
