//! The running engine as the REST API sees it and steers it: its interfaces and their health,
//! queue counts, drain mode, and pausing a destination.
//!
//! **Drain mode** stops taking new messages and keeps delivering what's stored, until nothing
//! is pending: for server moves and upgrades without a handover. MLLP
//! listeners close (senders see the engine as down and keep their messages), queued HTTP
//! answers 503 with `Retry-After`, file pickup and schedules pause. Respond-mode HTTP keeps
//! answering: it stores nothing. "Drained" means the gates are closed, nothing is being
//! stored, and no delivery is pending; with `stop`, the engine then exits.

use crate::activity::{Activity, Pause, Paused};
use crate::gate::Gate;
use crate::health::Health;
use anyhow::{Context as _, Result};
use chrono::{DateTime, Utc};
use ductus_core::impact::CodeUses;
use ductus_core::monitor::{self, Diagnosis, Facts, InterfaceState};
use ductus_core::{InterfaceConfig, Resources};
use ductus_store::{DestinationId, QueueCount, Store};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{OnceCell, watch};
use tokio_util::sync::CancellationToken;

/// How often the monitor counts queues while someone is watching (or draining).
const MONITOR_EVERY: Duration = Duration::from_secs(2);

/// The running interfaces at one moment, in engine order (`Engine::interfaces`).
pub type Interfaces = Arc<[Arc<Running>]>;

/// The certificates the engine connects to and presents (`Engine::certificates`), as a snapshot.
pub type Certificates = Arc<[crate::certificates::CertificateTarget]>;

/// One running interface. A deploy in place swaps it whole (`Engine::swap_interface`): whoever
/// reads it sees the old interface or the new one, never a mix of the two.
#[derive(Clone)]
pub struct Running {
    pub config: Arc<InterfaceConfig>,
    pub health: Arc<Health>,
    pub gate: Arc<Gate>,
    /// What it received and delivered since the engine started.
    pub stats: Arc<crate::stats::InterfaceStats>,
    /// Its destinations, in config order.
    pub destinations: Vec<RunningDestination>,
    /// Where its messages are stored and its workers woken (none in respond mode: no queue).
    pub intake: Option<Arc<crate::ingest::Intake>>,
    /// Its lanes: which generations are listed, and while they change, which lanes are held.
    pub lanes: Arc<crate::lanes::Lanes>,
}

/// What a restart in place changed before it failed (`Engine::put_back_restart`).
#[derive(Default)]
struct Undo {
    /// Its lanes decided again (maybe a generation written).
    lanes: bool,
    /// Its destinations watched as the new config has them.
    watched: bool,
    /// Its retention applied from the new config.
    retention: bool,
    /// Its intake's settings before the restart reset them.
    intake: Option<Arc<crate::ingest::Settings>>,
    /// Its new workers started.
    added: bool,
}

/// One of a running interface's destinations: its name, its id in the store and its counts. A
/// destination added by a deploy in place starts at zero; one that stays keeps its counts.
#[derive(Clone)]
pub struct RunningDestination {
    pub name: String,
    pub id: DestinationId,
    pub stats: Arc<crate::stats::DestinationStats>,
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
    /// Every interface's state and then its destinations', in engine order (`Engine::states`):
    /// live updates are sent when one changes, even if no count did (a destination turning
    /// stalled, an interface going silent). Filled in by the monitor.
    pub states: Vec<&'static str>,
    /// Deliveries finished since the engine started, per destination (`Engine::finished`): an
    /// update is sent when any finished, even if every count is back where it was (a resent
    /// message quarantined again, a replay sent, between two counts). Filled in by the monitor.
    pub finished: Vec<u64>,
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
    /// The company-defined documentation fields (docs-schema.yaml; none if it is missing or broken).
    pub docs: ductus_core::docs::Schema,
    /// Every running interface, in engine order: read with `interfaces()`, swapped by a deploy in
    /// place.
    interfaces: std::sync::RwLock<Interfaces>,
    /// Each interface's parts and threads, to change it while it runs (a deploy in place).
    pub kept: crate::in_place::Kept,
    pub retention: Arc<crate::retention::Retention>,
    /// Calls to each database and API in resources.yaml (`fetch` for URLs called directly).
    pub calls: Arc<crate::stats::ResourceCalls>,
    /// When each interface was first watched, and every destination's pause switch.
    pub activity: Activity,
    drain: watch::Sender<DrainState>,
    monitor: watch::Sender<Arc<Snapshot>>,
    /// What each interface's code uses, worked out on first request (it needs the TypeScript
    /// analysis, which takes a moment). A deploy in place puts an empty cell here, so the next
    /// request works it out from the new code; nothing is analysed until someone asks.
    code: std::sync::Mutex<Arc<OnceCell<Arc<CodeUses>>>>,
    /// Cancelled when the engine should stop (drained with `stop`).
    pub shutdown: CancellationToken,
    /// How people sign in (`auth:` in ductus.yaml).
    pub auth: ductus_core::settings::AuthSettings,
    /// The audit log's settings (`audit:` in ductus.yaml).
    pub audit: ductus_core::settings::AuditSettings,
    /// Names this run in the audit log (`engine.start` and `engine.stop` carry it), so a start
    /// with no stop shows up as a crash.
    pub instance: String,
    /// Alerts (`alerts:` in ductus.yaml), if configured.
    pub alerts: Option<Arc<crate::alerts::Alerts>>,
    /// The engine's own connection checks (`doctor:` in ductus.yaml).
    pub doctor: ductus_core::settings::DoctorSettings,
    /// The certificates the engine connects to and those it presents: worked out from the
    /// project at start and again after a deploy in place. Read with `certificates()`.
    certificates: std::sync::RwLock<Certificates>,
    /// Held while `certificates` is worked out again, so two deploys in place finishing together
    /// can't leave the earlier one's list.
    certificates_again: std::sync::Mutex<()>,
    /// The directory people sign in with (`auth.ldap`), if configured.
    pub directory: Option<crate::ldap::Directory>,
    /// Teams and lists that own things (`owners:` in ductus.yaml).
    pub teams: BTreeMap<String, ductus_core::alerts::OwnerContact>,
    /// Whether the engine holds its interfaces now (`ownership`).
    pub ownership: Arc<crate::ownership::Ownership>,
    /// Its lock and claims: a deploy in place claims the interfaces it adds and lets go of those
    /// it removes.
    pub(crate) owner: Arc<crate::ownership::Owner>,
    /// Every `certificates:` entry, loaded (what connections trust and present).
    pub tls: Arc<crate::tls::Certificates>,
    /// Where the REST API listens, if it does.
    pub api: Option<ApiListener>,
    /// This server's name (as the audit log and `engines` name it): our certificates' rows.
    pub host: String,
    /// How this engine took over from the one before it (`systemctl reload`); unset after a
    /// plain start.
    pub handed_over: std::sync::OnceLock<crate::handover::HandedOver>,
    /// What this engine runs, as recorded (`deployment()`): by its start, then by each deploy in
    /// place; None until the start's is, or when that couldn't be.
    deployment: std::sync::RwLock<Option<Arc<crate::deployment::Deployment>>>,
    /// Held while a deployment is recorded: the start's first (taken before the engine serves
    /// anything), then each deploy in place in turn, each recording the set the one before it left
    /// (two at once would each put back the other's interface).
    pub(crate) recording: Arc<tokio::sync::Mutex<()>>,
    /// Release mode (`ductus run --releases`): where releases are unpacked, the project, the
    /// release it runs and how long a passing check may be reused. None: folder mode.
    pub releases: Option<crate::deploy::Releases>,
    /// Asks this engine for a handover from inside (a deploy); unset when it takes no handovers.
    pub asker: std::sync::OnceLock<crate::handover::Asker>,
    /// Cancelled once this engine's run ended, however it ended: what it started that outlives a
    /// step (its API server, hourly jobs, alert rounds) ends with it, so an engine started again
    /// in the same process (an in-process handover) leaves nothing of it behind.
    pub ended: CancellationToken,
    /// Every interface's source: a deploy in place starts an added one's and stops a removed
    /// one's, alone.
    pub sources: crate::sources::Sources,
    /// The project's own files as this engine runs them (`ProjectFiles`): the start's, then each
    /// reload's in folder mode, which compares the folder with them. Read with `project_files()`.
    project_files: std::sync::RwLock<Arc<ductus_core::project_files::ProjectFiles>>,
    /// What it was started with outside the project (`crate::started_with`): a reload in this
    /// process hands over when its settings read again differ.
    pub(crate) started_with: Vec<(&'static str, String)>,
}

/// Where the engine's REST API (and web UI) listens, and its `tls` (none: plain HTTP).
#[derive(Debug, Clone)]
pub struct ApiListener {
    pub host: String,
    pub port: u16,
    pub tls: Option<ductus_core::tls::ListenTls>,
    /// Who may connect (none: anyone who can reach it).
    pub allow: Option<ductus_core::allow::Allow>,
}

/// What the engine is made of, from startup.
pub struct Parts {
    pub environment: String,
    pub root: PathBuf,
    pub database_url: String,
    pub store: Store,
    pub resources: Resources,
    pub docs: ductus_core::docs::Schema,
    pub interfaces: Vec<Running>,
    pub kept: crate::in_place::Kept,
    pub retention: Arc<crate::retention::Retention>,
    pub calls: Arc<crate::stats::ResourceCalls>,
    pub activity: Activity,
    pub auth: ductus_core::settings::AuthSettings,
    pub audit: ductus_core::settings::AuditSettings,
    pub instance: String,
    pub alerts: Option<Arc<crate::alerts::Alerts>>,
    pub doctor: ductus_core::settings::DoctorSettings,
    pub directory: Option<crate::ldap::Directory>,
    pub teams: BTreeMap<String, ductus_core::alerts::OwnerContact>,
    pub ownership: Arc<crate::ownership::Ownership>,
    pub owner: Arc<crate::ownership::Owner>,
    pub tls: Arc<crate::tls::Certificates>,
    pub api: Option<ApiListener>,
    pub host: String,
    pub releases: Option<crate::deploy::Releases>,
    pub ended: CancellationToken,
    pub sources: crate::sources::Sources,
    pub project_files: ductus_core::project_files::ProjectFiles,
    pub started_with: Vec<(&'static str, String)>,
}

impl Engine {
    pub fn new(parts: Parts) -> Arc<Self> {
        let Parts {
            environment,
            root,
            database_url,
            store,
            resources,
            docs,
            interfaces,
            kept,
            retention,
            calls,
            activity,
            auth,
            audit,
            instance,
            alerts,
            doctor,
            directory,
            teams,
            ownership,
            owner,
            tls,
            api,
            host,
            releases,
            ended,
            sources,
            project_files,
            started_with,
        } = parts;
        let certificates = {
            let configs: Vec<&InterfaceConfig> = interfaces.iter().map(|i| &*i.config).collect();
            let alerts = alerts.as_ref().map(|a| &a.settings);
            certificate_targets(&configs, &resources, &database_url, alerts, api.as_ref(), &host)
        };
        let engine = Arc::new(Self {
            started_at: Utc::now(),
            environment,
            root,
            database_url,
            store,
            resources,
            docs,
            interfaces: std::sync::RwLock::new(interfaces.into_iter().map(Arc::new).collect()),
            kept,
            retention,
            calls,
            activity,
            drain: watch::Sender::new(DrainState::Accepting),
            monitor: watch::Sender::new(Arc::default()),
            code: Default::default(),
            shutdown: CancellationToken::new(),
            auth,
            audit,
            instance,
            alerts,
            doctor,
            certificates: std::sync::RwLock::new(certificates),
            certificates_again: std::sync::Mutex::new(()),
            directory,
            teams,
            ownership,
            owner,
            tls,
            api,
            host,
            handed_over: std::sync::OnceLock::new(),
            deployment: Default::default(),
            recording: Default::default(),
            releases,
            asker: std::sync::OnceLock::new(),
            ended,
            sources,
            project_files: std::sync::RwLock::new(Arc::new(project_files)),
            started_with,
        });
        tokio::spawn(engine.ended.clone().run_until_cancelled_owned(monitor(Arc::downgrade(&engine))));
        engine
    }

    /// Every running interface now, in engine order. A snapshot: a deploy in place that swaps one
    /// meanwhile doesn't change it, so a reader sees each interface whole.
    pub fn interfaces(&self) -> Interfaces {
        self.interfaces.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The project's own files as this engine runs them (folder mode: the start's, then the last
    /// reload's).
    pub fn project_files(&self) -> Arc<ductus_core::project_files::ProjectFiles> {
        self.project_files.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The project's own files a reload applied: what the next one compares with.
    pub(crate) fn now_runs_files(&self, files: ductus_core::project_files::ProjectFiles) {
        *self.project_files.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(files);
    }

    /// What this engine runs, as last recorded.
    pub fn deployment(&self) -> Option<Arc<crate::deployment::Deployment>> {
        self.deployment.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// What it runs from now on, recorded (hold `recording` while recording it).
    pub(crate) fn now_runs(&self, deployment: crate::deployment::Deployment) {
        *self.deployment.write().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(deployment));
    }

    pub fn interface(&self, name: &str) -> Option<Arc<Running>> {
        self.interfaces().iter().find(|i| i.config.name == name).cloned()
    }

    /// Run `config`'s interface on its new code and settings (`in_place::Kept::replace`: its
    /// destinations added or removed too), and show it so from then on: the API and metrics see
    /// its new destinations, and what its code uses is worked out again when next asked (what
    /// reads the whole engine, certificates and alerts, follows once per deploy:
    /// `shown_after_replacing`). Its new code version (`root`: the project it was loaded from,
    /// the new release's folder) is saved once applied, and its new workers save their decisions
    /// with it (they wait for it). Answers the version, for the deploy's record
    /// (`deployment::record_in_place`). `in_place::NeedsHandover` when only the handover can
    /// apply it; any other `Err` leaves it running as it was. Its code loads under `generation`
    /// (`in_place::Interface::generation`: a deploy's new one, or its own to put it back).
    pub(crate) async fn replace_interface(
        self: &Arc<Self>,
        config: InterfaceConfig,
        root: &std::path::Path,
        generation: u64,
    ) -> Result<crate::deployment::Applied> {
        let name = config.name.clone();
        let running = self.interface(&name).ok_or_else(|| anyhow::anyhow!("interface `{name}` isn't running here"))?;
        // Destinations added or removed while its lanes change (an `ordering` change not settled
        // yet): each old lane's drain is followed per destination, so it restarts alone (its lanes
        // built again for the new destinations).
        let retargets = running.config.destinations().map(|(n, _)| n).ne(config.destinations().map(|(n, _)| n));
        if retargets && running.lanes.is_changing() {
            return self.restart_interface(config, root, generation).await;
        }
        let (saving, decided) = crate::deployment::Decided::channel();
        // Its source first (`sources::Sources::change`: nothing to do when it's the same), so a
        // failure (its new port in use) leaves everything as it was; what it receives meanwhile
        // waits for the workers like any message.
        let intake = || running.intake.clone();
        let changing = self.sources.change(&running.config, &config, intake());
        changing.await.with_context(|| format!("`{name}`: changing its source"))?;
        let replaced = match self.kept.replace(config.clone(), &self.activity, decided.clone(), generation).await {
            Ok(replaced) => replaced,
            Err(e) => {
                if let Err(again) = self.sources.change(&config, &running.config, intake()).await {
                    tracing::error!(interface = %name, error = %format!("{again:#}"), "couldn't put its source back after its new code didn't load");
                }
                return Err(e);
            }
        };
        let (version, id) = self.saved_version(&replaced.config, replaced.files, root, saving, &decided).await;
        self.swap_interface(Running {
            config: replaced.config,
            destinations: replaced.destinations,
            ..(*running).clone()
        });
        Ok(crate::deployment::Applied { interface: name, version, id })
    }

    /// The code version an interface runs from now on (the files its code loaded, V8's so exactly
    /// what runs, and its interface.yaml read again from `root`), saved; its new workers wait for
    /// the id (`decided`) before saving a decision. None when it couldn't be worked out (said).
    async fn saved_version(
        &self,
        config: &Arc<InterfaceConfig>,
        files: Vec<ductus_script::CodeFile>,
        root: &std::path::Path,
        saving: crate::deployment::Saving,
        decided: &crate::deployment::Decided,
    ) -> (Option<crate::code::Version>, Option<i32>) {
        let (name, config, root) = (config.name.clone(), config.clone(), root.to_path_buf());
        let version = tokio::task::spawn_blocking(move || {
            let root = anyhow::Context::with_context(ductus_script::loaded_path(&root), || {
                format!("finding {}", root.display())
            })?;
            crate::code::Version::of(&root, &config, &files)
        })
        .await
        .map_err(anyhow::Error::from)
        .and_then(|v| v);
        let version = match version {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::error!(interface = %name, error = %format!("{e:#}"), "couldn't work out the code version a deploy brought: its deliveries are saved without it, and the deploy isn't recorded");
                None
            }
        };
        crate::deployment::save(self.store.clone(), version.clone().map(|v| Arc::new(vec![v])), saving).await;
        (version, decided.id(&name).await)
    }

    /// Run `config`'s interface from now on (a deploy in place added it), as a start would: a
    /// queued one claimed first (a `Refused` error naming the engine that runs it, nothing done),
    /// its lanes decided, its destinations registered, its configured retention applied, what
    /// waits for it from before matched to its destinations now (as a start matches them), what's
    /// stored about it loaded (last activity, pauses), its code loaded and its workers started
    /// (`in_place::Kept::add`), its source started (`sources::Sources::add`: its listener, file
    /// pickup or schedule; an HTTP route on a server of its own, respond mode too), its version
    /// saved, and shown. Its gate stays closed while the engine drains. Answers its version, for
    /// the deploy's record. A respond-mode interface stores nothing: it is neither claimed nor
    /// managed (as at a start).
    ///
    /// All or nothing: an error leaves it unclaimed, unwatched and with no workers or source (its
    /// lanes and destinations stay registered: a start would register them the same way). Its
    /// code loads under `generation` (`in_place::Interface::generation`).
    pub(crate) async fn add_interface(
        self: &Arc<Self>,
        config: InterfaceConfig,
        root: &std::path::Path,
        generation: u64,
    ) -> Result<crate::deployment::Applied> {
        let name = config.name.clone();
        if self.interface(&name).is_some() || self.kept.contains(&name) {
            anyhow::bail!("interface `{name}` already runs here");
        }
        if !config.responds() {
            self.owner.add(&self.store, std::slice::from_ref(&name)).await?;
        }
        match self.add_claimed(Arc::new(config), root, generation).await {
            Ok(applied) => Ok(applied),
            Err(e) => {
                self.let_go(&name).await;
                Err(e)
            }
        }
    }

    /// `add_interface` once it is this engine's.
    async fn add_claimed(
        self: &Arc<Self>,
        config: Arc<InterfaceConfig>,
        root: &std::path::Path,
        generation: u64,
    ) -> Result<crate::deployment::Applied> {
        use anyhow::Context;
        let (name, store, shared) = (config.name.clone(), &self.store, self.kept.shared());
        let mut layouts = match crate::lanes::Wanted::of(&config) {
            Some(wanted) => crate::lanes::start(store, &[wanted]).await?,
            None => HashMap::new(),
        };
        let layout = layouts.remove(&name).unwrap_or_else(|| crate::lanes::Layout::single(1, ""));
        let names: Vec<(String, String)> = config.destinations().map(|(d, _)| (name.clone(), d.to_string())).collect();
        let ids = match names.is_empty() {
            true => Vec::new(),
            false => store.destination_ids(&names).await.context("registering its destinations")?,
        };
        let wake = Arc::new(crate::Wake::new(config.slots(), ids.len()));
        let gate = Arc::new(Gate::default());
        let stats = Arc::new(crate::stats::InterfaceStats::new(&name));
        let destinations: Vec<RunningDestination> = config
            .destinations()
            .zip(&ids)
            .map(|((d, _), &id)| RunningDestination {
                name: d.to_string(),
                id,
                stats: Arc::new(crate::stats::DestinationStats::new(d)),
            })
            .collect();
        let lanes = Arc::new(crate::lanes::Lanes::new(layout, config.slots(), ids.len()));
        let retiring = self.ended.child_token();
        crate::lanes::retire_when_settled(store.clone(), name.clone(), lanes.clone(), retiring.clone());
        // Respond mode: no queue, so no intake, retention or stored activity (as at a start); its
        // requests come over a channel to its first worker.
        let queued = !config.responds();
        let (calls, requests) = match queued {
            true => (None, None),
            false => {
                let (calls, requests) = tokio::sync::mpsc::channel(1024);
                (Some(calls), Some(requests))
            }
        };
        let intake = match queued {
            true => {
                let tier = self.retention.add(self.retention.configured(&config)).await?;
                let targets = crate::ingest::Targets { destinations: ids.clone(), wake: wake.clone() };
                let settings = crate::ingest::Settings::new(config.clone(), lanes.clone(), tier, targets)?;
                Some(Arc::new(crate::ingest::Intake::new(store.clone(), gate.clone(), stats.clone(), settings)))
            }
            false => None,
        };
        if queued && !names.is_empty() {
            let reconciled =
                store.reconcile(&names, &ids).await.context("matching what waits for it to its destinations")?;
            for (_, count) in &reconciled.added {
                tracing::info!(interface = %name, count, "queued messages waiting for destinations they had no delivery for");
            }
            for (_, destination, count) in &reconciled.removed {
                tracing::warn!(interface = %name, %destination, count, "destination is no longer in interface.yaml: its pending deliveries were marked filtered");
            }
        }
        if queued {
            let watched = crate::activity::Watched {
                stats: stats.clone(),
                destinations: destinations.iter().map(|d| (d.id, d.stats.clone())).collect(),
            };
            self.activity.watch(watched).await.context("loading its last activity")?;
        }
        let outlets = config
            .destinations()
            .zip(&destinations)
            .map(|((d_name, d), running)| {
                let pause = self.activity.pause(running.id).context("a destination's pause switch")?;
                let kept = crate::in_place::KeptDestination {
                    id: running.id,
                    stats: running.stats.clone(),
                    pause,
                    limiter: None,
                };
                shared.outlet(&config, d_name, d, kept)
            })
            .collect::<Result<Vec<_>>>()?;
        let health = Arc::new(Health::reporting(&name, shared.stopped.clone()));
        let (saving, decided) = crate::deployment::Decided::channel();
        let interface = crate::in_place::Interface {
            config: config.clone(),
            wake,
            health: health.clone(),
            stats: stats.clone(),
            lanes: lanes.clone(),
            outlets,
            intake: intake.clone(),
            decided: decided.clone(),
            threads: Vec::new(),
            removed: false,
            retiring,
            generation,
            code: Vec::new(), // once loaded (`Kept::add`)
        };
        let files = self.kept.add(interface, requests).await?;
        // Closed before it receives anything while the engine drains (an MLLP listener then
        // waits for the gate to open).
        if !matches!(self.drain_state(), DrainState::Accepting) {
            gate.set_open(false);
        }
        // Its workers run: now its source (as a start listens once the code loaded).
        if let Err(e) = self.sources.add(&config, intake.clone(), calls).await {
            if let Err(e) = self.kept.remove(&name).await {
                tracing::error!(interface = %name, error = %format!("{e:#}"), "couldn't stop the workers of an interface whose source didn't start");
            }
            return Err(e.context(format!("`{name}`: starting its source")));
        }
        let (version, id) = self.saved_version(&config, files, root, saving, &decided).await;
        let running = Running { config, health, gate, stats, destinations, intake, lanes };
        self.add_running(running);
        Ok(crate::deployment::Applied { interface: name, version, id })
    }

    /// Stop running interface `name` (a deploy in place removed it): its gate closed and what it
    /// was storing finished, its source stopped (`sources::Sources::stop`: its port given back,
    /// an HTTP server's requests in flight answered, respond mode's too), its workers stopped
    /// between messages (the message in hand finishes and is saved), then it is no longer shown,
    /// watched or managed (retention), and its claim is given back, so another engine may run it.
    /// What waits for it stays stored, for whichever engine runs it next. Its alerts stay as they
    /// were, no longer judged (as after a handover that leaves it out). Errors only when it isn't
    /// running here.
    pub(crate) async fn remove_interface(self: &Arc<Self>, name: &str) -> Result<()> {
        let running = self.interface(name).ok_or_else(|| anyhow::anyhow!("interface `{name}` isn't running here"))?;
        running.gate.set_open(false);
        while running.gate.in_flight() > 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        self.sources.stop(name).await;
        self.kept.remove(name).await?;
        self.remove_running(name);
        self.let_go(name).await;
        Ok(())
    }

    /// Run `config`'s interface on new settings that only a start sets up (0.7.5.2.4: `format`,
    /// `ordering`, `isolation`, `workers`, `dedicated_pools`, `durability`, `dedupe`, `retention`,
    /// `quarantined`, queue ↔ respond mode) by restarting it alone, as a start would start it, with
    /// its claim, gate, counts and source kept: senders never notice (its intake goes on storing and
    /// acknowledging; what arrives meanwhile waits for its new workers, a respond-mode request too).
    ///
    /// In order: claimed if it becomes queued (refused, nothing done, when another engine runs it);
    /// its source changed as `replace_interface` changes it; every worker of it stopped between
    /// messages (`Kept::stop`: no lane ever has two owners, whatever its new slot count); its lanes
    /// decided as at a start (`lanes::start`: a changed `ordering` adds a generation, old lanes
    /// read first); its destinations kept, added or removed (`outlets_for`); its retention applied
    /// as at a start; its intake storing with the new settings (`Intake::reset`, once every store
    /// with the old ones finished) and what waits matched to its destinations; its new workers
    /// started where a start would place them (`Kept::add`: pool or dedicated threads, its own
    /// pools); an HTTP route changing mode answered the new way; shown and its version saved.
    /// Becoming respond mode, its claim is given back and what its queue holds stays stored (as at
    /// a remove).
    ///
    /// All or nothing: a failure puts everything back as it was (its lanes taken back where
    /// nothing was stored in them, its intake's old settings, its old workers on the code they
    /// ran, under their old generation) and errors. Its new code loads under `generation`.
    pub(crate) async fn restart_interface(
        self: &Arc<Self>,
        config: InterfaceConfig,
        root: &std::path::Path,
        generation: u64,
    ) -> Result<crate::deployment::Applied> {
        let name = config.name.clone();
        let running = self.interface(&name).ok_or_else(|| anyhow::anyhow!("interface `{name}` isn't running here"))?;
        let config = Arc::new(config);
        let (was_queued, queued) = (!running.config.responds(), !config.responds());
        let claims = queued && !was_queued;
        if claims {
            self.owner.add(&self.store, std::slice::from_ref(&name)).await?;
        }
        let unclaim = || async {
            if claims && let Err(e) = self.owner.remove(&self.store, std::slice::from_ref(&name)).await {
                tracing::warn!(interface = %name, error = %format!("{e:#}"), "couldn't give back the claim a failed restart made");
            }
        };
        // Its source first, as for a replace (an HTTP route changing mode: once its workers run).
        let mode_changes = was_queued != queued;
        if !mode_changes && let Err(e) = self.sources.change(&running.config, &config, running.intake.clone()).await {
            return Err(e.context(format!("`{name}`: changing its source")));
        }
        let source_back = || async {
            if !mode_changes && let Err(e) = self.sources.change(&config, &running.config, running.intake.clone()).await
            {
                tracing::error!(interface = %name, error = %format!("{e:#}"), "couldn't put its source back after a failed restart");
            }
        };
        let (old, requests) = match self.kept.stop(&name).await {
            Ok(stopped) => stopped,
            Err(e) => {
                source_back().await;
                unclaim().await;
                return Err(e);
            }
        };
        let mut undo = Undo::default();
        let stopped = std::time::Instant::now();
        match self.restarted(&running, &old, config.clone(), root, generation, &mut undo).await {
            Ok((applied, calls)) => {
                tracing::info!(
                    interface = %name,
                    workers = config.slots(),
                    isolation = ?config.isolation,
                    lanes = self.interface(&name).map_or(0, |r| r.lanes.layout.newest().lanes),
                    sending_paused = ?stopped.elapsed(),
                    "restarted alone on its new settings: its senders never noticed"
                );
                old.retiring.cancel(); // its old lanes' watch: the new lanes have their own
                match (requests, calls) {
                    // Respond mode still: what waited for the old responder goes to the new one.
                    (Some(mut waiting), Some(calls)) => {
                        tokio::spawn(async move {
                            while let Some(call) = waiting.recv().await {
                                if calls.send(call).await.is_err() {
                                    break;
                                }
                            }
                        });
                    }
                    (Some(waiting), None) if !waiting.is_empty() => {
                        tracing::warn!(interface = %name, count = waiting.len(), "requests waiting when it changed to queue mode weren't answered: their callers get an error and may send again")
                    }
                    _ => {}
                }
                if was_queued && !queued {
                    self.let_go(&name).await; // what its queue holds stays stored, unclaimed
                }
                Ok(applied)
            }
            Err(e) => {
                self.put_back_restart(&running, old, requests, undo).await;
                source_back().await;
                unclaim().await;
                Err(e)
            }
        }
    }

    /// `restart_interface` once its old workers stopped (`old`: its parts as they ran), noting in
    /// `undo` what to put back. Answers its version and, in respond mode, where its new workers
    /// take requests.
    async fn restarted(
        self: &Arc<Self>,
        running: &Running,
        old: &crate::in_place::Interface,
        config: Arc<InterfaceConfig>,
        root: &std::path::Path,
        generation: u64,
        undo: &mut Undo,
    ) -> Result<(crate::deployment::Applied, Option<tokio::sync::mpsc::Sender<crate::http::HttpCall>>)> {
        use anyhow::Context;
        let (name, store, shared) = (config.name.clone(), &self.store, self.kept.shared());
        let queued = !config.responds();
        // Its lanes, as a start decides them (nothing written when `ordering` is the same).
        let layout = match crate::lanes::Wanted::of(&config).filter(|_| queued) {
            Some(wanted) => {
                undo.lanes = true;
                crate::lanes::start(store, &[wanted]).await?.remove(&name)
            }
            None => None,
        };
        let layout = layout.unwrap_or_else(|| crate::lanes::Layout::single(1, ""));
        // Its destinations: those it keeps keep their ids, counts and pauses; watched as they
        // change (becoming queued: all new, so watched from now on).
        let outlets = old.outlets_for(shared, &config, &self.activity).await?;
        undo.watched = true;
        let destinations: Vec<RunningDestination> = config
            .destinations()
            .zip(&outlets)
            .map(|((d, _), o)| RunningDestination { name: d.to_string(), id: o.id, stats: o.stats.clone() })
            .collect();
        let ids: Vec<DestinationId> = outlets.iter().map(|o| o.id).collect();
        let wake = Arc::new(crate::Wake::new(config.slots(), ids.len()));
        let lanes = Arc::new(crate::lanes::Lanes::new(layout, config.slots(), ids.len()));
        // Its intake stores with the new settings; one made when it becomes queued.
        let intake = match queued {
            false => None,
            true => {
                undo.retention = true;
                let tier = self.retention.add(self.retention.configured(&config)).await?;
                let targets = crate::ingest::Targets { destinations: ids.clone(), wake: wake.clone() };
                let settings = Arc::new(crate::ingest::Settings::new(config.clone(), lanes.clone(), tier, targets)?);
                match &running.intake {
                    Some(intake) => {
                        undo.intake = Some(intake.reset(settings).await);
                        Some(intake.clone())
                    }
                    None => {
                        let settings = Arc::into_inner(settings).expect("not shared yet");
                        let made = crate::ingest::Intake::new(
                            store.clone(),
                            running.gate.clone(),
                            running.stats.clone(),
                            settings,
                        );
                        Some(Arc::new(made))
                    }
                }
            }
        };
        let names: Vec<(String, String)> = config.destinations().map(|(d, _)| (name.clone(), d.to_string())).collect();
        if queued && !names.is_empty() {
            let reconciled =
                store.reconcile(&names, &ids).await.context("matching what waits for it to its destinations")?;
            for (_, destination, count) in &reconciled.removed {
                tracing::warn!(interface = %name, %destination, count, "destination is no longer in interface.yaml: its pending deliveries were marked filtered");
            }
        }
        let (calls, requests) = match queued {
            true => (None, None),
            false => {
                let (calls, requests) = tokio::sync::mpsc::channel(1024);
                (Some(calls), Some(requests))
            }
        };
        let (saving, decided) = crate::deployment::Decided::channel();
        let retiring = self.ended.child_token();
        let interface = crate::in_place::Interface {
            config: config.clone(),
            wake,
            health: old.health.clone(),
            stats: old.stats.clone(),
            lanes: lanes.clone(),
            outlets,
            intake: intake.clone(),
            decided: decided.clone(),
            threads: Vec::new(),
            removed: false,
            retiring: retiring.clone(),
            generation,
            code: Vec::new(), // once loaded (`Kept::add`)
        };
        let files = self.kept.add(interface, requests).await?;
        undo.added = true;
        // An HTTP route answered the new way once its workers run (respond mode: by them, from
        // now on; the old responder's waiting requests follow, `restart_interface`).
        let http = matches!(config.source, ductus_core::config::Source::Http { .. });
        let route = match (&calls, &intake) {
            (Some(calls), _) => Some(crate::http::Route::Respond { interface: name.clone(), calls: calls.clone() }),
            (None, Some(intake)) if old.config.responds() => Some(crate::http::Route::Queue(intake.clone())),
            _ => None,
        };
        if let Some(route) = route.filter(|_| http) {
            self.sources
                .reroute(&config, route)
                .await
                .with_context(|| format!("`{name}`: rerouting its HTTP source"))?;
        }
        crate::lanes::retire_when_settled(store.clone(), name.clone(), lanes.clone(), retiring);
        let (version, id) = self.saved_version(&config, files, root, saving, &decided).await;
        self.swap_interface(Running { config, destinations, intake, lanes, ..running.clone() });
        Ok((crate::deployment::Applied { interface: name, version, id }, calls))
    }

    /// Put an interface a restart failed for back as it ran (`undo` says what changed): its
    /// lanes taken back where nothing was stored in them, its retention and intake settings as
    /// before, its old destinations watched, its new workers stopped and its old ones started
    /// again on the code they ran (the same files: nothing loads twice on a thread that had them).
    async fn put_back_restart(
        self: &Arc<Self>,
        running: &Running,
        old: crate::in_place::Interface,
        requests: Option<crate::worker::Requests>,
        undo: Undo,
    ) {
        let name = old.config.name.clone();
        let warn = |what: &str, e: anyhow::Error| {
            tracing::error!(interface = %name, error = %format!("{e:#}"), "after a failed restart, couldn't {what}");
        };
        if undo.added
            && let Err(e) = self.kept.remove(&name).await
        {
            warn("stop its new workers", e);
        }
        if let (Some(intake), Some(settings)) = (&running.intake, undo.intake) {
            intake.reset(settings).await;
        }
        if undo.retention {
            match old.config.responds() {
                true => self.retention.remove(&name),
                false => {
                    if let Err(e) = self.retention.add(self.retention.configured(&old.config)).await {
                        warn("apply its retention as before", e);
                    }
                }
            }
        }
        if undo.lanes
            && let Some(told) = crate::lanes::take_back(&self.store, &[(name.clone(), &*old.lanes)]).await
        {
            tracing::warn!(interface = %name, "{told}");
        }
        if undo.watched {
            let result = match old.config.responds() {
                true => self.activity.unwatch(&name).await,
                false => self.activity.watch(old.watched()).await,
            };
            if let Err(e) = result {
                warn("watch its destinations as before", e);
            }
        }
        if let Err(e) = self.kept.add(old, requests).await {
            warn("start it again as it ran: it runs nothing until the next start", e);
        }
    }

    /// Interface `name` is no longer this engine's: not watched, not managed, its claim given
    /// back. A failure is said, not returned: the rest still goes.
    async fn let_go(&self, name: &str) {
        self.retention.remove(name);
        if let Err(e) = self.activity.unwatch(name).await {
            tracing::warn!(interface = %name, error = %format!("{e:#}"), "couldn't save its last activity");
        }
        if let Err(e) = self.owner.remove(&self.store, &[name.to_string()]).await {
            tracing::warn!(interface = %name, error = %format!("{e:#}"), "couldn't give back its claim: another engine may run it once this one stops");
        }
    }

    /// After a deploy in place applied (or put back) the interfaces `names`: what reads the whole
    /// engine sees them. Once per deploy, not per interface: working the certificates out again
    /// reads every interface (3–10 ms at a thousand), and fifty of those in a row, each blocking
    /// a thread of the runtime that receives and saves for every interface, held up interfaces
    /// the deploy never touched (0.7.5.1's measurement: a send latency of 400 ms). So it runs off
    /// that runtime, once. Then live updates, the alert rows of their new destinations and
    /// certificates (judging updates only rows that exist; the others are there, untouched), and
    /// a certificate connected to for the first time checked beside the deploy (unless
    /// `doctor.schedule: off`), so its expiry shows without waiting for the next round.
    pub(crate) async fn shown_after_replacing(self: &Arc<Self>, names: &[String]) {
        let engine = self.clone();
        let new = tokio::task::spawn_blocking(move || engine.certificates_again()).await.unwrap_or(false);
        self.monitor.send_modify(|_| {}); // live updates show the new destinations at once
        if self.alerts.is_some() {
            let running: Vec<Arc<Running>> = names.iter().filter_map(|n| self.interface(n)).collect();
            let keys = crate::alerts::alert_keys(&running, &self.certificates());
            if let Err(e) = self.store.ensure_alerts(&keys).await {
                tracing::warn!(interfaces = names.join(", "), error = %format!("{e:#}"), "couldn't make the alerts of a destination or certificate a deploy added; the next start does");
            }
        }
        if new && self.doctor.schedule.is_some() {
            crate::certificates::check_beside(self, names.to_vec());
        }
    }

    /// Release mode: remove the unpacked releases nothing here runs from any more (`deploy::tidy`
    /// keeping `deploy::in_use` and the release it runs), beside the caller. At the start, and
    /// after each deploy in place.
    pub(crate) fn tidy_releases(&self) {
        let Some(releases) = &self.releases else { return };
        let interfaces = self.interfaces();
        let mut keep = crate::deploy::in_use(&releases.mode.dir, interfaces.iter().map(|i| i.config.dir.as_path()));
        keep.extend(releases.running().map(|r| r.hex()));
        let dir = releases.mode.dir.clone();
        tokio::task::spawn_blocking(move || crate::deploy::tidy(&dir, &keep));
    }

    /// Put `running` in place of the running interface of its name (a deploy in place). What its
    /// code uses is forgotten with it: a request that took its snapshot just before may see the
    /// new analysis (names of databases and APIs only), never one older than what it shows.
    fn swap_interface(&self, running: Running) {
        let mut interfaces = self.interfaces.write().unwrap_or_else(|e| e.into_inner());
        let running = Arc::new(running);
        *interfaces = interfaces
            .iter()
            .map(|i| if i.config.name == running.config.name { running.clone() } else { i.clone() })
            .collect();
        *self.code.lock().unwrap_or_else(|e| e.into_inner()) = Default::default();
    }

    /// Show `running` from now on (a deploy in place added it), after those running already.
    pub(crate) fn add_running(&self, running: Running) {
        let mut interfaces = self.interfaces.write().unwrap_or_else(|e| e.into_inner());
        let mut next: Vec<Arc<Running>> =
            interfaces.iter().filter(|i| i.config.name != running.config.name).cloned().collect();
        next.push(Arc::new(running));
        *interfaces = next.into();
        *self.code.lock().unwrap_or_else(|e| e.into_inner()) = Default::default();
    }

    /// Show interface `name` no more (a deploy in place removed it). A reader holding a snapshot
    /// from before still sees it whole. Answers what was shown, if it was.
    pub(crate) fn remove_running(&self, name: &str) -> Option<Arc<Running>> {
        let mut interfaces = self.interfaces.write().unwrap_or_else(|e| e.into_inner());
        let removed = interfaces.iter().find(|i| i.config.name == name).cloned()?;
        *interfaces = interfaces.iter().filter(|i| i.config.name != name).cloned().collect();
        *self.code.lock().unwrap_or_else(|e| e.into_inner()) = Default::default();
        Some(removed)
    }

    /// The certificates the engine connects to and those it presents, now.
    pub fn certificates(&self) -> Certificates {
        self.certificates.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Work out the certificates again from the running interfaces (after a deploy in place);
    /// true when one is connected to that wasn't before.
    fn certificates_again(&self) -> bool {
        let _one_at_a_time = self.certificates_again.lock().unwrap_or_else(|e| e.into_inner());
        let started = std::time::Instant::now();
        let interfaces = self.interfaces();
        let configs: Vec<&InterfaceConfig> = interfaces.iter().map(|i| &*i.config).collect();
        let alerts = self.alerts.as_ref().map(|a| &a.settings);
        let new =
            certificate_targets(&configs, &self.resources, &self.database_url, alerts, self.api.as_ref(), &self.host);
        let before = self.certificates();
        let added =
            new.iter().any(|t| t.side == crate::certificates::Side::Remote && !before.iter().any(|b| b.key == t.key));
        *self.certificates.write().unwrap_or_else(|e| e.into_inner()) = new;
        tracing::debug!(interfaces = interfaces.len(), took = ?started.elapsed(), "deploy: certificates worked out again");
        added
    }

    fn destination_ids(&self) -> Vec<DestinationId> {
        self.interfaces().iter().flat_map(|i| i.destinations.iter().map(|d| d.id)).collect()
    }

    /// A destination's pause, if it's paused.
    pub fn paused(&self, destination: DestinationId) -> Option<Paused> {
        self.activity.pause(destination).and_then(|p| p.get())
    }

    /// An interface's destination by name: `Err` says what's wrong in plain words (no such
    /// interface or destination).
    pub fn destination(&self, interface: &str, destination: &str) -> Result<(Arc<Running>, DestinationId), String> {
        let i = self.interface(interface).ok_or_else(|| format!("no interface named `{interface}`"))?;
        match i.destinations.iter().find(|d| d.name == destination) {
            Some(d) => Ok((i.clone(), d.id)),
            None if i.destinations.is_empty() => Err(format!("`{interface}` has no destinations (respond mode)")),
            None => {
                let names: Vec<&str> = i.destinations.iter().map(|d| d.name.as_str()).collect();
                Err(format!("`{interface}` has no destination `{destination}` (it has {})", names.join(", ")))
            }
        }
    }

    fn pause_switch(&self, destination: DestinationId) -> Result<Arc<Pause>> {
        self.activity.pause(destination).ok_or_else(|| anyhow::anyhow!("destination {destination} isn't watched"))
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
        let mine: std::collections::HashSet<DestinationId> = self.destination_ids().into_iter().collect();
        let counts = self.store.queue_counts().await?;
        let queues: HashMap<_, _> =
            counts.into_iter().filter(|c| mine.contains(&c.destination_id)).map(|c| (c.destination_id, c)).collect();
        let waiting: Vec<DestinationId> = queues.values().filter(|q| q.pending > 0).map(|q| q.destination_id).collect();
        let oldest = self.store.oldest_pending(&waiting).await?;
        let oldest = oldest.into_iter().map(|o| (o.destination_id, o.received_at)).collect();
        Ok(Snapshot { at: Utc::now(), queues, oldest, states: Vec::new(), finished: Vec::new() })
    }

    /// Deliveries each destination finished since the engine started (sent, filtered or
    /// quarantined), from the workers' counters: no query.
    pub fn finished(&self) -> Vec<u64> {
        self.interfaces().iter().flat_map(|i| i.destinations.iter().map(|d| d.stats.done())).collect()
    }

    /// The health state of every interface and destination (see `Snapshot::states`).
    pub fn states(&self, snapshot: &Snapshot) -> Vec<&'static str> {
        let now = Utc::now();
        self.interfaces()
            .iter()
            .flat_map(|i| {
                let own = std::iter::once(self.interface_state(i, now).0.label());
                own.chain((0..i.destinations.len()).map(move |n| self.diagnose(i, n, snapshot, now).state.label()))
            })
            .collect()
    }

    /// An interface's own state: what it should have received (`monitor:`), whether its keys read
    /// empty, or stopped.
    pub fn interface_state(&self, i: &Running, now: DateTime<Utc>) -> (InterfaceState, String) {
        let watched_since = self.activity.watched_since(&i.config.name).unwrap_or(self.started_at);
        let stopped = i.health.stopped();
        let keys = i.intake.as_ref().map(|intake| intake.keys_seen()).unwrap_or_default();
        let last = i.stats.last_received.get();
        monitor::interface_state(&i.config.monitor, last, watched_since, stopped, &keys, now)
    }

    /// A destination's state and why (`n`: its place in `i.destinations`).
    pub fn diagnose(&self, i: &Running, n: usize, snapshot: &Snapshot, now: DateTime<Utc>) -> Diagnosis {
        let (Some(d), Some((_, config))) = (i.destinations.get(n), i.config.destinations().nth(n)) else {
            return monitor::diagnose(&Facts::default(), now);
        };
        let (id, stats) = (d.id, &d.stats);
        let queue = snapshot.queue(id);
        let (recent, normal_inflow, normal_send_ms) = self.activity.trend(id, now);
        let last_error = stats.last_error();
        let paused = self.paused(id).map(|p| {
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
            oldest: snapshot.oldest_age(id),
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
        for i in self.interfaces().iter() {
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
        for i in self.interfaces().iter() {
            i.gate.set_open(true);
        }
    }

    /// Pending deliveries of this engine's interfaces, if nothing can add more: every gate
    /// closed and nothing being stored. `None` while a store is still in flight.
    async fn left_to_deliver(&self) -> Result<Option<i64>> {
        if self.interfaces().iter().any(|i| i.gate.is_open() || i.gate.in_flight() > 0) {
            return Ok(None);
        }
        let snapshot = self.snapshot().await?;
        // A paused destination keeps its messages on purpose: it doesn't hold the drain up.
        let ids = self.destination_ids().into_iter();
        Ok(Some(ids.filter(|d| self.paused(*d).is_none()).map(|d| snapshot.queue(d).pending).sum()))
    }

    /// What each interface's code uses (`db`/`api` names), worked out once (and again after a
    /// deploy in place).
    pub async fn code_uses(&self) -> Result<Arc<CodeUses>> {
        let cell = self.code.lock().unwrap_or_else(|e| e.into_inner()).clone();
        cell.get_or_try_init(|| async {
            let root = self.root.clone();
            let configs: Vec<InterfaceConfig> = self.interfaces().iter().map(|i| (*i.config).clone()).collect();
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
                let _ = tx.send(result.map(Arc::new));
            })?;
            rx.await?
        })
        .await
        .cloned()
    }
}

/// The certificates the engine connects to (destinations', resources', its own database's when it
/// verifies it, alerts') and the pairs it presents.
pub(crate) fn certificate_targets(
    configs: &[&InterfaceConfig],
    resources: &Resources,
    database_url: &str,
    alerts: Option<&ductus_core::alerts::AlertSettings>,
    api: Option<&ApiListener>,
    host: &str,
) -> Certificates {
    let mut all = crate::certificates::targets(configs, resources, database_url, alerts);
    all.extend(crate::certificates::ours(configs, resources, alerts, api, host));
    all.into()
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
                Ok(mut snapshot) => {
                    snapshot.states = e.states(&snapshot);
                    snapshot.finished = e.finished();
                    e.monitor.send_if_modified(|s| {
                        // Nothing changed: keep the old snapshot, so watchers only hear of changes.
                        let changed = s.queues != snapshot.queues
                            || s.states != snapshot.states
                            || s.finished != snapshot.finished;
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
