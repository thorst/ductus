//! Runs interfaces: receive → persist → ACK, then workers transform and deliver.

pub mod activity;
pub mod alerts;
pub mod allow;
pub mod api;
pub mod audit;
pub mod cert_show;
pub mod certificates;
pub mod changes;
pub mod code;
pub mod control;
pub mod deploy;
pub mod deployment;
mod destination;
pub mod doctor;
pub mod events;
mod files;
pub mod gate;
pub mod git;
pub mod handover;
pub mod health;
pub mod history;
mod http;
pub mod in_place;
mod ingest;
pub mod inspect;
pub mod keys;
pub mod lanes;
pub mod ldap;
mod limiter;
pub mod mssql;
pub mod ownership;
pub mod project;
pub mod release;
pub mod replay;
pub mod retention;
mod schedule;
mod services;
mod sftp;
mod split;
pub mod stats;
pub mod systemd;
pub mod tls;
pub mod try_message;
mod worker;

use anyhow::{Context, Result, bail};
use ductus_core::config::{Isolation, Source};
use ductus_core::{Environment, InterfaceConfig};
use ductus_store::Store;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;

/// Load every `interfaces/*/interface.yaml` under `root`.
pub fn load_interfaces(root: &Path, env: &Environment) -> Result<Vec<InterfaceConfig>> {
    let dir = root.join("interfaces");
    let mut configs = Vec::new();
    for entry in std::fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        if path.join("interface.yaml").is_file() {
            configs.push(InterfaceConfig::load_in(&path, env)?);
        }
    }
    configs.sort_by(|a, b| a.name.cmp(&b.name));
    let mut names: Vec<_> = configs.iter().map(|c| c.name.as_str()).collect();
    names.dedup();
    if names.len() != configs.len() {
        bail!("two interfaces share a name");
    }
    Ok(configs)
}

/// Engine-wide settings.
pub struct EngineOptions {
    /// Threads in the shared worker pool. Default: one per CPU core.
    pub pool_threads: usize,
    /// Maximum PostgreSQL connections.
    pub db_connections: u32,
    /// Environment whose variables and secrets fill in the config (e.g. "dev", "prod").
    pub env: String,
    /// Where the REST API listens (`host:port` or `off`), instead of `api.listen` in ductus.yaml.
    pub api_listen: Option<String>,
    /// How long a stop may take before the engine exits anyway (see `stop`).
    pub stop_timeout: Duration,
    /// Another way to ask for the stop a signal asks for: the Windows service manager's Stop
    /// (cancelled once; every engine in the process heeds it, a handover's new one too).
    pub stop_request: Option<tokio_util::sync::CancellationToken>,
    /// Called once when every interface and the API are up (the service reports Running then).
    pub on_started: Option<Box<dyn FnOnce() + Send>>,
    /// Take handovers (`ductus handover`, deploys, reloads): `ductus run` does, `ductus dev` not.
    pub control: bool,
    /// How to start an engine again in this process, with its settings read again (the CLI's
    /// `run` gives one): for a handover without a new process (`handover`) and for a start again
    /// after a new engine failed once the old one let go. None: no in-process handover.
    pub rebuild: Option<handover::Rebuild>,
    /// Take over from the running engine that started this one (`handover`).
    pub takeover: Option<Arc<handover::Takeover>>,
    /// `ductus dev --inspect`: interfaces a debugger may attach to (never `ductus run`).
    pub inspect: Option<Inspect>,
    /// Release mode (`ductus run --releases DIR`, 0.7.4.4): run the release deployed here, not
    /// the project folder `run` is given.
    pub releases: Option<deploy::Mode>,
}

/// Interfaces a debugger may attach to, served by `inspector`. Each gets a thread and isolate of
/// its own for the run (all its workers on it, if it's dedicated), so a pause or an endless loop
/// under the debugger holds only that interface: receiving, ACKs and every other interface go on.
pub struct Inspect {
    pub inspector: Arc<inspect::Inspector>,
    /// By name.
    pub interfaces: Vec<String>,
}

impl Inspect {
    /// Each named interface exists and has code to debug.
    fn check(&self, configs: &[InterfaceConfig]) -> Result<()> {
        for name in &self.interfaces {
            match configs.iter().find(|c| &c.name == name) {
                None => bail!(
                    "`--inspect {name}`: no such interface (there are: {})",
                    configs.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(", ")
                ),
                Some(c) if !c.has_code() => {
                    bail!(
                        "`--inspect {name}`: it has no code to debug (no `script:` in its interface.yaml, on it or its destinations)"
                    )
                }
                Some(_) => {}
            }
        }
        Ok(())
    }
}

impl Default for EngineOptions {
    fn default() -> Self {
        Self {
            pool_threads: std::thread::available_parallelism().map_or(4, |n| n.get()),
            db_connections: 32,
            env: "dev".into(),
            api_listen: None,
            stop_timeout: STOP_TIMEOUT,
            stop_request: None,
            on_started: None,
            control: false,
            rebuild: None,
            takeover: None,
            inspect: None,
            releases: None,
        }
    }
}

/// Start every interface under `root` and run until Ctrl-C or SIGTERM (or until drained, when a
/// drain asked to stop, or handed over to a new engine: `handover`); then stop cleanly (`stop`).
/// A handover in this process goes on with the new engine: this returns when the last engine
/// that ran here ended.
pub async fn run(root: &Path, database_url: &str, options: EngineOptions) -> Result<()> {
    handover::remember_executable();
    systemd::init();
    let (rebuild, stop) = (options.rebuild.clone(), options.stop_request.clone());
    let mut ended = run_one(root.to_path_buf(), database_url.to_string(), options).await;
    loop {
        ended = match ended {
            Ok(Ended::Stopped) => {
                // The last engine here ended: the ports close now, as a stop always closed them,
                // not when the process exits (the Windows service reports Stopped before that).
                systemd::close_all();
                return Ok(());
            }
            Ok(Ended::HandedOver(next)) => match next.task.await {
                Ok(ended @ Ok(_)) => ended,
                // It was ready but failed once this one let go (its interfaces untouched or
                // released again): a plain start in this process, on the same sockets (senders
                // wait). The deployment it would have recorded isn't, so a release-mode start
                // runs the release that ran before.
                failed => {
                    let why = match failed {
                        Ok(Err(e)) => format!("{e:#}"),
                        Err(e) => format!("it panicked: {e}"),
                        Ok(Ok(_)) => unreachable!(),
                    };
                    let what = if next.releases { "the release that ran before" } else { "the project as it is" };
                    tracing::error!(error = %why, "the new engine failed after the old one let go: starting {what} again in this process");
                    match rebuild.as_ref().map(|rebuild| rebuild()) {
                        Some(Ok(handover::Restart { root, database_url, mut options })) => {
                            // The service manager's stop reaches it too.
                            options.stop_request = stop.clone();
                            run_one(root, database_url, options).await
                        }
                        Some(Err(e)) => Err(e.context("reading the engine's settings again to start it again")),
                        None => Err(anyhow::anyhow!("{why}")),
                    }
                }
            },
            Err(e) => {
                // A failed or refused start: senders are refused while it's down, as without
                // systemd (nothing while a takeover from another process hasn't taken over: the
                // sockets are the old engine's).
                systemd::abandon();
                systemd::close_all();
                return Err(e);
            }
        };
    }
}

/// After an in-process handover, once the engine taken over from has ended: give the memory it
/// freed back to the system. glibc keeps what a thread's arena freed for that arena, and the
/// ended engine's threads are gone, so without this the process grew a few MB at every handover
/// (measured at 1,001 interfaces: 94 → 221 MB over ten; with it: see `bench-handover.sh`
/// IN_PROCESS=1). Once, beside the engine, off the async threads; nothing elsewhere (other
/// allocators return memory themselves).
fn give_back_freed_memory() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    tokio::spawn(async {
        tokio::time::sleep(Duration::from_secs(3)).await;
        // SAFETY: malloc_trim has no preconditions; it only returns free pages to the system.
        let _ = tokio::task::spawn_blocking(|| unsafe { libc::malloc_trim(0) }).await;
    });
}

/// How an engine's run ended.
pub(crate) enum Ended {
    /// It stopped (a signal, the service manager, a drain that stops, a handover to a new
    /// process): the process ends.
    Stopped,
    /// A new engine in this process took over: the run goes on with it.
    HandedOver(handover::Next),
}

/// One engine's run, from its start to its end. A takeover that fails tells the engine it took
/// over from why; in this process, what it bound anew closes then (the sockets it shares with
/// the running engine stay).
///
/// Boxed with its `Send` said: an engine's run may start the next one's (`handover`), and the
/// compiler can't prove that cycle `Send` by itself.
fn run_one(
    root: PathBuf,
    database_url: String,
    options: EngineOptions,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Ended>> + Send>> {
    Box::pin(async move {
        let takeover = options.takeover.clone();
        let result = run_engine(&root, &database_url, options).await;
        if let (Err(e), Some(t)) = (&result, &takeover) {
            // The old engine says why (and goes on, if it hadn't let go yet).
            t.failed(&format!("{e:#}")).await;
            if t.in_process() {
                systemd::abandon_takeover();
            }
        }
        result
    })
}

async fn run_engine(root: &Path, database_url: &str, mut options: EngineOptions) -> Result<Ended> {
    // Everything this engine started that outlives a step (listeners, schedules, file pickup,
    // hourly jobs, worker threads, the API) ends with it, however it ends: in a process that goes
    // on (an in-process handover, a start again) nothing of an engine that ended may linger.
    let ended = tokio_util::sync::CancellationToken::new();
    let _end = ended.clone().drop_guard();
    let mut phases = Phases::new();
    // Release mode: the release to run comes from the database (a deploy's handover names it),
    // unpacked into its folder the first time; that folder is the project from here on. The
    // database is connected (and migrated) first for it, once.
    let (root, releases, connected) = match &options.releases {
        None => (root.to_path_buf(), None, None),
        Some(mode) => {
            let store = Store::connect(database_url, options.db_connections).await?;
            let asked = match &options.takeover {
                Some(t) => t.predecessor().await?.asked.release,
                None => None,
            };
            let (root, releases) = deploy::resolve(&store, &options.env, mode, asked.as_deref()).await?;
            phases.mark("release mode: release found and unpacked");
            (root, Some(releases), Some(store))
        }
    };
    let root = root.as_path();
    let running_release = releases.as_ref().and_then(|r| r.running.clone());
    let env = Environment::load(root, &options.env)?;
    // Said in the start's line: which of the code's `console` lines this environment logs.
    let console = env.console().as_str();
    let configs = load_interfaces(root, &env)?;
    phases.mark("interface.yaml files read");
    // Loaded now so a missing variable or secret stops startup; used by interface code (v0.4).
    let resources = ductus_core::Resources::load(root, &env)?;
    let settings = ductus_core::settings::Settings::load(root)?;
    let alert_settings = settings.alerts(&env)?;
    let heap_mb = settings.isolates.heap_mb;
    let api_address = settings.api.address(options.api_listen.as_deref())?;
    // Release mode before the first deploy runs none, waiting for one.
    let waiting_for_a_release = releases.as_ref().is_some_and(|r| r.running.is_none());
    if configs.is_empty() && !waiting_for_a_release {
        bail!("no interfaces found under {}", root.join("interfaces").display());
    }
    // Started by a service manager (the deployment's `how`): the Windows service, or systemd (which
    // gives every unit's processes `INVOCATION_ID`).
    let service = options.stop_request.is_some() || std::env::var_os("INVOCATION_ID").is_some();
    let inspected = options.inspect.take();
    if let Some(inspect) = &inspected {
        inspect.check(&configs)?;
    }
    let conflicts = ductus_core::config::check_endpoints(&configs);
    if !conflicts.is_empty() {
        bail!("port conflicts:\n  {}", conflicts.join("\n  "));
    }
    let routes = ductus_core::config::check_routes(&configs);
    if !routes.is_empty() {
        bail!("routes between interfaces:\n  {}", routes.join("\n  "));
    }
    for name in ductus_core::config::unrouted(&configs) {
        tracing::warn!(interface = %name, "has `source: internal` but no interface routes to it: it will get nothing");
    }
    if let Some((_, port)) = &api_address
        && let Some(l) = ductus_core::ports::listeners(&configs).into_iter().find(|l| l.port == *port)
    {
        bail!("port {port}: the REST API (`api.listen` in ductus.yaml) and interface `{}` both want it", l.interface);
    }
    let missing = resources.missing_references(&configs);
    if !missing.is_empty() {
        bail!("{}", missing.join("\n"));
    }
    let mut tls_problems =
        ductus_core::tls::check(&ductus_core::tls::listeners(&configs, settings.api.tls.as_ref()), &resources);
    let outbound = ductus_core::tls::outbound(&configs, &resources, alert_settings.as_ref());
    tls_problems.extend(ductus_core::tls::check_outbound(&outbound, &resources));
    if !tls_problems.is_empty() {
        bail!("{}", tls_problems.join("\n"));
    }
    // Every certificate, before the database: a broken or expired one stops the start here.
    let certificates = Arc::new(tls::Certificates::load(&resources.certificates)?);
    let server_tls = |t: &Option<ductus_core::tls::ListenTls>, alpn: &[&[u8]]| {
        t.as_ref().map(|t| certificates.server(t, alpn)).transpose()
    };
    const HTTP1: &[&[u8]] = &[b"http/1.1"];
    if let Some((host, port)) = &api_address
        && settings.api.tls.is_none()
        && !ductus_core::settings::loopback(host)
    {
        tracing::warn!(
            %host,
            port,
            "the REST API listens on the network without TLS: passwords and tokens cross it readable, and browsers \
             won't keep the session cookie; add `tls: {{ certificate: <name> }}` to `api` in ductus.yaml"
        );
    }
    let directory = ldap::Directory::new(settings.auth.ldap.as_ref(), &resources, &certificates)?;
    for name in resources.insecure_ldap() {
        tracing::warn!(
            directory = name,
            "plain ldap:// (`insecure: true`): passwords cross the network readable; use ldaps:// or starttls"
        );
    }
    for (name, mode) in resources.unverified_postgres() {
        tracing::warn!(
            database = name,
            tls = mode.label(),
            "its certificate isn't checked: whoever answers at its address is believed (and sees every query); set `tls: verify-full` (with `ca:` for a private CA)"
        );
    }
    for name in resources.unverified_sqlserver() {
        tracing::warn!(
            database = name,
            tls = "trust_server_certificate",
            "its certificate isn't checked: whoever answers at its address is believed (and sees every query); remove `trust_server_certificate` (with `ca:` for a private CA)"
        );
    }
    for name in resources.ntlm_sqlserver() {
        tracing::warn!(
            database = name,
            auth = "windows",
            "a named Windows account logs in from Linux with NTLM (never Kerberos): Microsoft is retiring NTLM and \
             some domains block it; a SQL login, or the engine on Windows, avoids it"
        );
    }
    let alerts = match alert_settings {
        Some(a) => {
            let wrong: Vec<_> = configs.iter().flat_map(|c| a.check_interface(&c.name, &c.alert)).collect();
            if !wrong.is_empty() {
                bail!("{}", wrong.join("\n"));
            }
            for warning in a.warnings() {
                tracing::warn!("{warning}");
            }
            Some(Arc::new(alerts::Alerts::new(a, settings.owners.clone(), &options.env, certificates.clone())?))
        }
        None => None,
    };
    let sftp_server = |name: &str| resources.sftp.get(name).cloned();
    // Host names in `allow:` lists, looked up while the database connects (needed before
    // anything listens); nothing at all when no list names a host.
    let allow_lists = configs.iter().filter_map(|c| c.source.allow()).chain(settings.api.allow.iter());
    let names = allow::Names::new(allow::names_in(allow_lists), allow::system_resolver());
    let resolving = (!names.is_empty()).then(|| {
        let names = names.clone();
        tokio::spawn(async move { names.resolve_all().await })
    });
    let admission = |listener: &str, host: &str, port: u16, list: Option<&ductus_core::allow::Allow>| {
        list.map(|a| allow::Admission::new(listener, host, port, a, names.clone())).transpose()
    };
    phases.mark("project loaded and checked");
    let store = match connected {
        Some(store) => store,
        None => Store::connect(database_url, options.db_connections).await?,
    };
    phases.mark("database connected and migrated");
    log_database_security(&store, database_url).await;
    // Before anything is changed for an interface (its lanes, deliveries reconciled, retention
    // applied, workers started): it must be this engine's. A takeover only checks that now, and
    // claims once the engine it takes over from let go.
    let instance = audit::new_instance();
    let me = ownership::Me { instance: instance.clone(), host: audit::host_name(), environment: options.env.clone() };
    let queued: Vec<String> = configs.iter().filter(|c| !c.responds()).map(|c| c.name.clone()).collect();
    let wanted: Vec<_> = configs.iter().filter_map(lanes::Wanted::of).collect();
    let takeover = options.takeover.clone();
    let owner = Arc::new(match &takeover {
        None => ownership::Owner::claim(&store, me, queued.clone()).await?,
        Some(t) => {
            let from = t.predecessor().await?;
            tracing::info!(pid = from.pid, version = %from.version, "taking over from the running engine: getting ready beside it");
            ownership::Owner::prepare(&store, me, queued.clone(), &from.instance).await?
        }
    });
    phases.mark(if takeover.is_some() { "ready to take the interfaces" } else { "interfaces claimed" });
    // Each interface's lanes, a generation added where `ordering` changed: after the claim, and in
    // a takeover before it receives (what it stores goes to the new lanes, which only it reads; a
    // takeover that fails leaves them listed, for the next start to read).
    let mut layouts = lanes::start(&store, &wanted).await?;
    phases.mark("lanes decided");
    // The company's documentation fields: a broken docs-schema.yaml is for `ductus validate` to report,
    // not a reason to stop the engine. The snapshot is for SQL reports, so it never blocks startup either.
    let docs = ductus_core::docs::Schema::load(root).unwrap_or_else(|e| {
        tracing::warn!(error = format!("{e:#}"), "ignoring the documentation fields: run `ductus validate`");
        Default::default()
    });
    let destinations: Vec<(String, String)> =
        configs.iter().flat_map(|c| c.destinations().map(|(name, _)| (c.name.clone(), name.to_string()))).collect();
    let all_ids = store.destination_ids(&destinations).await?;
    // Saved with the deployment (its history names it), beside the start.
    let snapshot: Vec<ductus_store::DocsRow> = ductus_core::docs::snapshot(&configs, &resources)
        .into_iter()
        .map(|(kind, name, docs)| ductus_store::DocsRow { kind, name, docs: docs.to_string() })
        .collect();
    let charge = Charge { destinations, ids: all_ids.clone(), queued };
    // Each interface's retention, before anything is stored: intake needs the tier.
    let configured: Vec<retention::Configured> = configs
        .iter()
        .filter(|c| !c.responds())
        .map(|c| retention::Configured {
            name: c.name.clone(),
            days: c.retention.unwrap_or(settings.retention).days() as ductus_store::Tier,
            purge_quarantined: c.quarantined.unwrap_or(settings.quarantined) == ductus_core::Quarantined::Purge,
        })
        .collect();
    // What the audit log's hourly job needs: how long each interface keeps its messages.
    let retention_days: Vec<(String, i32)> = configured.iter().map(|c| (c.name.clone(), i32::from(c.days))).collect();
    let retention =
        retention::Retention::new(store.clone(), configured, ended.clone()).await.context("reading retention")?;
    // Which interfaces have messages waiting, known once they're this engine's (`take_charge`).
    let waiting = Arc::new(worker::Waiting::new());
    // Each interface's code version id, saved once computed (below): workers record it with every
    // decision, and wait for it before saving their first.
    let (saving, decided) = deployment::Decided::channel();
    // Who started it and why: a takeover's is who asked the engine it takes over from.
    let who = match &takeover {
        Some(t) => deployment::Who::handover(&t.predecessor().await?.asked),
        None => deployment::Who::start(service),
    };
    if takeover.is_none() {
        let _ = waiting.set(take_charge(&store, &charge, &mut phases).await?);
    }
    let mut ids = all_ids.into_iter();
    let (stopped_tx, stopped_rx) = tokio::sync::mpsc::unbounded_channel();
    // Keep dedupe keys a little longer than the longest window that uses them.
    let dedupe_retention = configs.iter().filter_map(|c| c.dedupe.as_ref()).map(|d| d.window).max().unwrap_or_default()
        + Duration::from_secs(3600);

    // Plan threads: shared interfaces spread round-robin over the pool; each dedicated slot
    // gets a thread (and isolate) of its own.
    let pool_threads = options.pool_threads.max(1);
    let mut pool: Vec<Vec<worker::Job>> = (0..pool_threads).map(|_| Vec::new()).collect();
    let mut dedicated: Vec<(String, worker::Job)> = Vec::new();
    // Under a debugger: one thread per interface, all its workers on it.
    let mut debugged: std::collections::BTreeMap<String, Vec<worker::Job>> = Default::default();
    let mut interfaces = Vec::new();
    let mut http_endpoints = Vec::new();
    // One admission per HTTP port (its interfaces share one list: `check_endpoints`).
    let mut http_admissions: std::collections::HashMap<(String, u16), tls::Admission> = Default::default();
    let mut shared_count = 0;
    // Every queued interface's intake first: routes need their target's.
    let mut intakes = std::collections::HashMap::new();
    let mut parts = Vec::with_capacity(configs.len());
    for config in configs {
        let config = Arc::new(config);
        let wake = Arc::new(Wake::new(config.slots(), config.destinations().count()));
        let destination_ids: Vec<_> = ids.by_ref().take(config.destinations().count()).collect();
        let gate = Arc::new(gate::Gate::default());
        let stats =
            Arc::new(stats::InterfaceStats::new(&config.name, config.destinations().map(|(name, _)| name.to_string())));
        // Respond mode has no queue: one lane, never read.
        let layout = layouts.remove(&config.name).unwrap_or_else(|| lanes::Layout::single(1, ""));
        let lanes = Arc::new(lanes::Lanes::new(layout, config.slots(), config.destinations().count()));
        lanes::retire_when_settled(store.clone(), config.name.clone(), lanes.clone(), ended.clone());
        if !config.responds() {
            let intake = ingest::Intake::new(
                config.clone(),
                store.clone(),
                wake.clone(),
                gate.clone(),
                destination_ids.clone(),
                retention.tier(&config.name),
                stats.clone(),
                lanes.clone(),
            )?;
            intakes.insert(config.name.clone(), Arc::new(intake));
        }
        parts.push((config, wake, destination_ids, gate, stats, lanes));
    }
    // Last activity and pauses, before any worker starts (a paused destination sends nothing).
    let watched = parts
        .iter()
        .filter(|(config, ..)| !config.responds())
        .map(|(_, _, ids, _, stats, _)| activity::Watched { stats: stats.clone(), destinations: ids.clone() })
        .collect();
    // The engine is stopping: workers finish what's in hand, save it and end (see `stop`).
    let stopping = tokio_util::sync::CancellationToken::new();
    // A start that fails from here on stops the worker threads it started (a stop cancels it too).
    let _stop_workers = stopping.clone().drop_guard();
    let activity =
        activity::Activity::start(store.clone(), watched, stopping.clone()).await.context("loading last activity")?;
    phases.mark("last activity loaded");
    // What every interface's jobs and outlets are built with, kept for a deploy in place.
    let shared = in_place::Shared {
        ownership: owner.ownership.clone(),
        waiting: waiting.clone(),
        decided: decided.clone(),
        certificates: certificates.clone(),
        resources: resources.clone(),
        intakes,
    };
    let intakes = &shared.intakes;
    let mut kept: std::collections::HashMap<String, in_place::Interface> = Default::default();
    for (config, wake, destination_ids, gate, stats, lanes) in parts {
        let health = Arc::new(health::Health::reporting(&config.name, stopped_tx.clone()));
        let outlets: Vec<worker::Outlet> = config
            .destinations()
            .zip(&destination_ids)
            .zip(&stats.destinations)
            .map(|(((name, d), &id), destination_stats)| {
                let kept = in_place::KeptDestination {
                    id,
                    stats: destination_stats.clone(),
                    pause: activity.pauses[&id].clone(),
                    limiter: None,
                };
                shared.outlet(&config, name, d, kept)
            })
            .collect::<Result<_>>()?;
        // Respond-mode HTTP interfaces get requests over a channel instead of a queue.
        let mut requests = None;
        if let Source::Http { host, port, path, method, tls, allow, .. } = &config.source {
            let route = if config.responds() {
                let (calls, rx) = tokio::sync::mpsc::channel(1024);
                requests = Some(rx);
                http::Route::Respond { interface: config.name.clone(), calls }
            } else {
                http::Route::Queue(intakes[&config.name].clone())
            };
            http_endpoints.push(http::Endpoint {
                host: host.clone(),
                port: *port,
                method: method.clone(),
                path: path.clone(),
                route,
                tls: server_tls(tls, HTTP1)?,
                admission: match http_admissions.get(&(host.clone(), *port)) {
                    Some(a) => a.clone(),
                    None => {
                        let protocol = if tls.is_some() { "HTTPS" } else { "HTTP" };
                        let a = admission(&format!("{protocol} {host}:{port}"), host, *port, allow.as_ref())?;
                        http_admissions.insert((host.clone(), *port), a.clone());
                        a
                    }
                },
            });
        }
        let parts = in_place::Interface {
            config: config.clone(),
            wake: wake.clone(),
            health: health.clone(),
            stats: stats.clone(),
            lanes: lanes.clone(),
            outlets,
            threads: Vec::new(), // once they're spawned (below)
        };
        for slot in 0..config.slots() {
            let job = parts.job(&shared, slot, requests.take());
            if inspected.as_ref().is_some_and(|i| i.interfaces.contains(&config.name)) {
                debugged.entry(config.name.clone()).or_default().push(job);
                continue;
            }
            match config.isolation {
                Isolation::Shared => {
                    pool[shared_count % pool_threads].push(job);
                    shared_count += 1;
                }
                Isolation::Dedicated => dedicated.push((format!("{}-w{slot}", config.name), job)),
            }
        }
        kept.insert(config.name.clone(), parts);
        interfaces.push((config, wake, destination_ids, health, gate, stats, lanes));
    }

    let services = Arc::new(services::SharedServices::new(env, &resources, &certificates)?);
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let mut jobs = 0;
    let mut threads = Vec::new();
    // Each interface's threads and its slots on each, to ask them for changes while the engine
    // runs (a deploy in place).
    let mut commands_for = |thread_jobs: &[worker::Job]| {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        for job in thread_jobs {
            let threads = &mut kept.get_mut(&job.config.name).expect("every job's interface is kept").threads;
            match threads.last_mut() {
                Some(placed) if placed.commands.same_channel(&tx) => placed.slots.push(job.slot),
                _ => threads.push(in_place::Placed { commands: tx.clone(), slots: vec![job.slot] }),
            }
        }
        rx
    };
    for (i, thread_jobs) in pool.into_iter().enumerate().filter(|(_, j)| !j.is_empty()) {
        jobs += thread_jobs.len();
        let engine = services::EngineServices::new(services.clone(), Default::default());
        let name = format!("pool-{i}");
        let ready = ready_tx.clone();
        let stop = stopping.clone();
        let commands = commands_for(&thread_jobs);
        let thread =
            worker::spawn_thread(name, thread_jobs, store.clone(), engine, heap_mb, ready, stop, None, commands);
        threads.push(thread?);
    }
    // A dedicated interface's own pools are shared by all its worker threads.
    let mut own_pools = std::collections::HashMap::new();
    for (name, job) in dedicated {
        jobs += 1;
        let own =
            own_pools.entry(job.config.name.clone()).or_insert_with(|| services.dedicated_pools(&job.config)).clone();
        let engine = services::EngineServices::new(services.clone(), own);
        let ready = ready_tx.clone();
        let stop = stopping.clone();
        let jobs = vec![job];
        let commands = commands_for(&jobs);
        threads.push(worker::spawn_thread(name, jobs, store.clone(), engine, heap_mb, ready, stop, None, commands)?);
    }
    let inspector = inspected.map(|i| i.inspector);
    let (debugged_jobs, debugged_interfaces) = (debugged.values().map(Vec::len).sum::<usize>(), debugged.len());
    for (name, interface_jobs) in debugged {
        jobs += interface_jobs.len();
        let config = &interface_jobs[0].config;
        let own = match config.isolation {
            Isolation::Dedicated => services.dedicated_pools(config),
            Isolation::Shared => Default::default(),
        };
        let engine = services::EngineServices::new(services.clone(), own);
        let (ready, stop) = (ready_tx.clone(), stopping.clone());
        let thread = format!("{name}-debug");
        let commands = commands_for(&interface_jobs);
        threads.push(worker::spawn_thread(
            thread,
            interface_jobs,
            store.clone(),
            engine,
            heap_mb,
            ready,
            stop,
            inspector.clone(),
            commands,
        )?);
    }
    drop(ready_tx);
    // Fail fast if any interface's code doesn't load, before accepting any messages. Waited for
    // off the async threads: another engine in this process (a handover's) runs on them meanwhile.
    let answers = tokio::task::spawn_blocking(move || ready_rx.iter().take(jobs).collect::<Vec<_>>())
        .await
        .context("waiting for the interface workers to load their code")?;
    let mut failed = Vec::new();
    let mut reported = 0;
    // The files each interface's code loaded: its code version's (`code`).
    let mut loaded = std::collections::HashMap::new();
    for (name, result) in answers {
        reported += 1;
        match result {
            Ok(files) => {
                loaded.entry(name).or_insert(files);
            }
            Err(e) => failed.push(format!("interface `{name}`: {e:#}")),
        }
    }
    // A thread that died before loading its interfaces (a panic, logged above) reports nothing:
    // the channel closes with fewer answers than jobs.
    if reported < jobs {
        failed.push(format!(
            "{} of {jobs} interface workers didn't start (their threads stopped; see the errors above)",
            jobs - reported
        ));
    }
    if !failed.is_empty() {
        failed.dedup();
        bail!("{}", failed.join("\n"));
    }
    phases.mark("interface code loaded, workers running");
    // What each interface runs (every delivery records it): computed, saved (the workers wait for
    // the ids before saving a decision, not before deciding) and checked (folder mode: whether a
    // change undoes someone else's) beside the start, never in its way; recorded as a deployment
    // once the interfaces are this engine's (below, also beside the start). Nothing the start does
    // waits for them, except a takeover's `ready`, for the check's warnings (at most 2 s).
    let (computed, code) = tokio::sync::watch::channel(None::<Option<Arc<Vec<code::Version>>>>);
    let (warning, warned) = deployment::warnings_channel();
    {
        let (root, configs) = (root.to_path_buf(), interfaces.iter().map(|(c, ..)| c.clone()).collect::<Vec<_>>());
        let (store, who) = (store.clone(), who.clone());
        let source = match &running_release {
            Some(r) => r.source(),
            None => deploy::Source::Folder(root.clone()),
        };
        tokio::spawn(async move {
            let started = std::time::Instant::now();
            let reading = root.clone();
            let versions = match tokio::task::spawn_blocking(move || deployment::versions(&reading, &configs, &loaded))
                .await
            {
                Ok(Ok(code)) => Some(Arc::new(code)),
                Ok(Err(e)) => {
                    tracing::error!(error = %format!("{e:#}"), "couldn't compute the interfaces' code versions: this start's deployment isn't recorded");
                    None
                }
                Err(e) => {
                    tracing::error!(error = %e, "computing the code versions failed: this start's deployment isn't recorded");
                    None
                }
            };
            tracing::debug!(took = ?started.elapsed(), "code versions computed beside the start");
            computed.send_replace(Some(versions.clone()));
            match &versions {
                Some(v) => drop(tokio::spawn(deployment::check(store.clone(), source, v.clone(), who, warning))),
                None => drop(warning.send_replace(Some(Default::default()))), // nothing to warn about
            }
            deployment::save(store, versions, saving).await;
        });
    }
    if let Some(resolving) = resolving {
        resolving.await.context("looking up the host names in `allow:` lists")?;
        phases.mark("allow: host names looked up");
    }
    names.start(stopping.clone());

    // Sockets systemd kept for addresses no longer configured would hold their ports.
    let listening = ductus_core::ports::listeners(interfaces.iter().map(|(c, ..)| c.as_ref()));
    systemd::keep_only(
        listening.iter().map(|l| (l.host.as_str(), l.port)).chain(api_address.iter().map(|(h, p)| (h.as_str(), *p))),
    );
    for (config, ..) in &interfaces {
        let intake = || intakes[&config.name].clone();
        match &config.source {
            Source::Mllp { host, port, tls, allow } => {
                let label = format!("interface `{}` (MLLP)", config.name);
                let admission = admission(&label, host, *port, allow.as_ref())?;
                ingest::spawn_mllp(intake(), server_tls(tls, &[])?, admission, ended.clone()).await?
            }
            Source::File(f) | Source::Sftp(f) => {
                let server = f.server.as_deref().and_then(sftp_server);
                let watcher = files::FileWatcher::new(intake(), server, owner.ownership.clone())?;
                tokio::spawn(ended.clone().run_until_cancelled_owned(watcher.run()));
            }
            Source::Schedule(s) => {
                let timing = ductus_core::schedule::Timing::new(s)?;
                tokio::spawn(ended.clone().run_until_cancelled_owned(schedule::run(
                    intake(),
                    timing,
                    owner.ownership.clone(),
                )));
            }
            Source::Http { .. } => {} // served below, with the other HTTP interfaces on its port
            Source::Internal => {}    // other interfaces' workers store its messages (routes)
        }
    }
    // HTTP sources stop with the workers, or first in a handover (`receiving`).
    let receiving = stopping.child_token();
    let http_servers = http::serve(http_endpoints, receiving.clone()).await?;
    let certificate_changes = certificates.watch(stopping.clone());
    phases.mark("listening");
    let running: Vec<control::Running> = interfaces
        .iter()
        .map(|(config, _, ids, health, gate, stats, lanes)| control::Running {
            config: config.clone(),
            health: health.clone(),
            gate: gate.clone(),
            stats: stats.clone(),
            destinations: config.destinations().map(|(name, _)| name.to_string()).zip(ids.iter().copied()).collect(),
            intake: intakes.get(&config.name).cloned(),
            lanes: lanes.clone(),
        })
        .collect();
    let engine = control::Engine::new(control::Parts {
        environment: options.env.clone(),
        root: root.to_path_buf(),
        database_url: database_url.to_string(),
        store: store.clone(),
        resources: resources.clone(),
        docs,
        interfaces: running,
        kept: in_place::Kept::new(shared, kept.into_values()),
        retention,
        calls: services.calls(),
        activity,
        auth: settings.auth.clone(),
        audit: settings.audit,
        instance: instance.clone(),
        alerts,
        doctor: settings.doctor,
        directory,
        teams: settings.owners.clone(),
        ownership: owner.ownership.clone(),
        tls: certificates.clone(),
        api: api_address.clone().map(|(host, port)| control::ApiListener {
            host,
            port,
            tls: settings.api.tls.clone(),
            allow: settings.api.allow.clone(),
        }),
        host: audit::host_name(),
        releases: releases.clone(),
        ended: ended.clone(),
    });
    // The audit log: this start (and how the last run on this host ended), interfaces the runaway
    // guard stops, and the hourly trimming. A log that can't be written is not a reason to refuse
    // to start (the engine may be all that is left to move messages), but it is an error. Then
    // alerts, the engine's own certificate checks and the directory's refresh. All once the
    // interfaces are this engine's: a takeover's only after it took over (two engines judging
    // one interface's alerts would tell twice). Beside the start, not in its way (since 0.7.4.2:
    // its `engine.start` row lands just after `started`); a takeover's `engine.handover` row after
    // `engine.start`, as before.
    let mut background = Some(move |engine: Arc<control::Engine>, handed: Option<handover::HandedOver>| {
        tokio::spawn(async move {
            let started = std::time::Instant::now();
            audit::start(&engine, stopped_rx, retention_days).await;
            if let Some(handed) = &handed {
                audit::handed_over(&engine, handed).await;
            }
            alerts::start(&engine).await;
            certificates::start(&engine, certificate_changes).await;
            ldap::start(&engine);
            tracing::debug!(took = ?started.elapsed(), "audit log and alerts started beside the start");
        });
    });
    // Recorded beside the start, not in its way (tens of milliseconds at a thousand interfaces, the
    // first time far more): a task that ends once it's written. The API shows it from then on.
    let record = |engine: &Arc<control::Engine>| {
        let engine = engine.clone();
        let docs = snapshot.clone();
        let (mut code, decided, warned, root, who) =
            (code.clone(), decided.clone(), warned.clone(), root.to_path_buf(), who.clone());
        let release = running_release.clone();
        tokio::spawn(async move {
            // Once they're computed (beside the start).
            let versions = code.wait_for(Option::is_some).await.ok().and_then(|v| v.clone().flatten());
            let Some(versions) = versions else {
                // No versions, no deployment: the documentation is saved all the same.
                deployment::docs_alone(&engine.store, &docs).await;
                return;
            };
            let recording = deployment::Recording { versions, decided, warned, root, docs, release };
            let host = audit::host_name();
            let recorded = deployment::record(&engine, &host, &who, recording).await;
            if let Some(d) = recorded {
                let _ = engine.deployment.set(d);
            }
        });
    };
    if takeover.is_none()
        && let Some(start) = background.take()
    {
        start(engine.clone(), None);
        settle_in(engine.retention.clone(), &ended);
        record(&engine);
    }
    let mut api_server = None;
    if let Some((host, port)) = &api_address {
        let admission = admission("the REST API", host, *port, settings.api.allow.as_ref())?;
        api_server =
            Some(api::serve(engine.clone(), host, *port, server_tls(&settings.api.tls, HTTP1)?, admission).await?);
    }
    phases.mark("API listening");
    let status = match interfaces.len() {
        1 => "running 1 interface".to_string(),
        n => format!("running {n} interfaces"),
    };
    // A takeover: ready, receiving beside the engine it takes over from. Once that one stopped and
    // let go, take the interfaces as a start does, and send.
    if let Some(t) = &takeover {
        // What the changes replace, told to whoever asked for the handover: read while the old
        // engine still sends (milliseconds); not waited for past a moment.
        let warnings = tokio::time::timeout(Duration::from_secs(2), deployment::warnings(&warned)).await;
        let told: Vec<String> = warnings
            .map(|w| w.iter().map(|(interface, warning)| format!("⚠ {interface}: {warning}")).collect())
            .unwrap_or_default();
        t.ready(phases.total(), told).await?;
        tracing::info!("ready: receiving beside the engine being taken over, which stops now");
        let stop_took = t.released().await;
        let released = std::time::Instant::now();
        systemd::promote();
        phases.mark("the old engine let go");
        owner.claim_now(&store).await?;
        phases.mark("interfaces claimed");
        let _ = waiting.set(take_charge(&store, &charge, &mut phases).await?);
        engine.activity.refresh_pauses(&store).await.context("reading the pauses")?;
        owner.hold();
        let sending_paused = stop_took.unwrap_or_default() + released.elapsed();
        phases.mark("sending");
        settle_in(engine.retention.clone(), &ended);
        let handed = handover::HandedOver {
            from: t.predecessor().await?,
            at: chrono::Utc::now(),
            sending_paused,
            in_process: t.in_process(),
        };
        tracing::info!(
            from_pid = handed.from.pid,
            from_version = %handed.from.version,
            sending_paused = ?sending_paused,
            "took over: receiving never stopped"
        );
        if let Some(start) = background.take() {
            start(engine.clone(), Some(handed.clone()));
        }
        record(&engine);
        let _ = engine.handed_over.set(handed);
        t.running(sending_paused).await;
        if t.in_process() {
            give_back_freed_memory();
        }
    }
    // Messages left pending with nothing pending for them (after a crash, or reconcile's
    // changes): workers don't wait for their overall status, so it's set after the start.
    let settling = store.clone();
    let busy: Vec<String> = waiting.get().map(|w| w.iter().cloned().collect()).unwrap_or_default();
    tokio::spawn(async move {
        if busy.is_empty() {
            return;
        }
        match settling.settle_finished(&busy).await {
            Ok(0) => {}
            Ok(n) => tracing::info!(messages = n, "set the overall status of messages whose deliveries were all done"),
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "could not settle finished messages (the next start tries again)")
            }
        }
    });
    // One line for the whole start (demos wait for `started interfaces=`, so it comes first).
    let count = |kind: fn(&Source) -> bool| interfaces.iter().filter(|(c, ..)| kind(&c.source)).count();
    tracing::info!(
        interfaces = interfaces.len(),
        mllp = count(|s| matches!(s, Source::Mllp { .. })),
        http = count(|s| matches!(s, Source::Http { .. })),
        files = count(|s| matches!(s, Source::File(_) | Source::Sftp(_))),
        schedules = count(|s| matches!(s, Source::Schedule(_))),
        internal = count(|s| matches!(s, Source::Internal)),
        shared = shared_count,
        pool_threads,
        dedicated_threads = jobs - shared_count - debugged_jobs,
        debugged = debugged_interfaces,
        console = %console,
        took = ?phases.total(),
        "{}",
        events::STARTED
    );
    systemd::ready(&status);
    if let Some(started) = options.on_started.take() {
        started();
    }

    let maintenance = store.clone();
    tokio::spawn(ended.clone().run_until_cancelled_owned(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            if let Err(e) = maintenance.ensure_partitions(3).await {
                // Usually a long query holding the tables (it gives up rather than queue ACKs
                // behind it). Partitions exist days ahead, so the next hour's try is in time.
                tracing::warn!(error = %format!("{e:#}"), "partition maintenance didn't run; trying again in an hour");
            }
            if let Err(e) = maintenance.purge_dedupe_keys(dedupe_retention).await {
                tracing::error!(error = %e, "dedupe key cleanup failed");
            }
        }
    }));

    let requested = async {
        match &options.stop_request {
            Some(request) => request.cancelled().await,
            None => std::future::pending().await,
        }
    };
    // Unpacked releases no longer needed, beside the start.
    if let (Some(r), Some(running)) = (&releases, &running_release) {
        let (dir, keep) = (r.mode.dir.clone(), running.hex());
        tokio::task::spawn_blocking(move || deploy::tidy(&dir, &keep));
    }
    owner.watch(store.clone(), stopping.clone());
    let mut control = handover::Control::listen(options.control, options.rebuild.clone(), options.stop_request.clone());
    // A deploy (`POST /deployments`) asks for its handover here, as `ductus handover` does.
    if let Some(asker) = control.asker() {
        let _ = engine.asker.set(asker);
    }
    let mut successor = None;
    let why = tokio::select! {
        signal = stop_signal() => signal,
        _ = engine.shutdown.cancelled() => "drained and asked to stop",
        _ = requested => "asked by the service manager",
        _ = owner.taken_over() => "another engine took over its interfaces",
        next = control.next(&engine) => {
            successor = Some(next);
            "handed over to a new engine"
        }
    };
    let limit = options.stop_timeout;
    tracing::info!(why, limit = ?limit, "stopping: taking no new messages, finishing the ones in hand");
    if successor.is_some() {
        // The new engine listens on the same sockets: they stay open, and systemd isn't told the
        // service stops. HTTP sources first, gates still open: a request in flight is answered
        // here, the next one there (not a 503 from a closed gate).
        systemd::handing_over();
        receiving.cancel();
        let until = tokio::time::Instant::now() + Duration::from_secs(5);
        for server in http_servers {
            let _ = tokio::time::timeout_at(until, server).await;
        }
    } else {
        // Before the gates close: listeners closing now stay in systemd's store for the next start.
        systemd::stopping();
    }
    let started = std::time::Instant::now();
    // A call paused at a breakpoint would hold its thread until someone pressed "continue".
    if let Some(inspector) = &inspector {
        inspector.close();
    }
    let stop = stop(&engine, &stopping, threads);
    let mut forced = false;
    tokio::select! {
        finished = tokio::time::timeout(limit, stop) => match finished {
            Ok(()) => tracing::info!(took = ?started.elapsed(), "{}", events::STOPPED),
            Err(_) => {
                forced = true;
                tracing::warn!(
                    "{} {limit:?}; stopping anyway: the messages in hand stay pending and are processed again at the next start (a destination may get one again)",
                    events::STOP_TIMED_OUT
                )
            }
        },
        signal = stop_signal() => {
            forced = true;
            tracing::warn!(signal, "{}", events::STOP_CUT_SHORT)
        }
    }
    let stop_took = started.elapsed();
    owner.release(&store).await;
    // Before the new engine takes over: its start reads how this run ended.
    let why = match &successor {
        Some(s) if s.in_process() => format!("handed over to a new engine in this process (ductus {})", s.version()),
        Some(s) => format!("handed over to pid {} (ductus {})", s.pid(), s.version()),
        None => why.to_string(),
    };
    audit::stopped(&engine, &why, forced).await;
    let mut next = None;
    if let Some(successor) = successor {
        next = successor.released(stop_took).await;
    }
    // Answer the API requests in flight (a drain's own, say; a deploy's). Live-update streams
    // never end by themselves, hence the limit: the server ends with the engine (`ended`).
    if let Some(server) = api_server {
        let _ = tokio::time::timeout(Duration::from_secs(5), server).await;
    }
    match (owner.taken_why(), next) {
        (Some(why), _) => Err(ownership::Refused(why).into()),
        (None, Some(next)) => Ok(Ended::HandedOver(next)),
        (None, None) => Ok(Ended::Stopped),
    }
}

/// How long each step of a start took, logged at debug level as it goes (`RUST_LOG=ductus_runtime=debug`),
/// so a slow start says where its time went.
struct Phases {
    started: std::time::Instant,
    last: std::time::Instant,
}

impl Phases {
    fn new() -> Self {
        let now = std::time::Instant::now();
        Self { started: now, last: now }
    }

    fn mark(&mut self, phase: &'static str) {
        let now = std::time::Instant::now();
        tracing::debug!(took = ?(now - self.last), "start: {phase}");
        self.last = now;
    }

    fn total(&self) -> Duration {
        self.started.elapsed()
    }
}

/// How long a stop may take by default: a transform or send under way finishes first, and
/// both have their own limits (a transform's `timeout`, 5 s by default).
pub const STOP_TIMEOUT: Duration = Duration::from_secs(30);

/// How the connection to the engine's database is protected, as its server reports it: `TLS
/// 1.3, verified (sslmode=verify-full)`. A database on another machine whose certificate isn't
/// checked is a warning (libpq's default `prefer` believes whoever answers at its address).
pub async fn database_security(store: &Store, database_url: &str) -> Result<ductus_core::pgtls::Security> {
    use ductus_core::pgtls::{Security, Setting, UrlTls};
    let url = UrlTls::read(database_url);
    let mode = url.as_ref().map(|u| u.mode).unwrap_or_default();
    let host = url.map(|u| u.host).unwrap_or_default();
    let version = store.tls_version().await.context("reading pg_stat_ssl")?;
    Ok(Security::new(mode, version.as_deref(), &host, Setting::Url))
}

async fn log_database_security(store: &Store, database_url: &str) {
    match database_security(store, database_url).await {
        Ok(s) if s.warns() => tracing::warn!(
            database = %s,
            "the engine's database connection isn't verified: whoever answers at its address is believed; add sslmode=verify-full to DUCTUS_DATABASE_URL (sslrootcert=/path/ca.pem for a private CA)"
        ),
        Ok(s) => tracing::info!(database = %s, "the engine's database"),
        Err(e) => tracing::debug!(error = %format!("{e:#}"), "couldn't read how the database connection is protected"),
    }
}

/// What `take_charge` works on.
struct Charge {
    /// Every destination as `(interface, destination)`, with its id.
    destinations: Vec<(String, String)>,
    ids: Vec<ductus_store::DestinationId>,
    /// The queued interfaces.
    queued: Vec<String>,
}

/// What a start changes for its interfaces once they're its own (a takeover's: once the engine
/// it takes over from let go), before any worker sends: stored deliveries matched to the
/// destinations configured now. Returns the interfaces with messages waiting.
async fn take_charge(store: &Store, charge: &Charge, phases: &mut Phases) -> Result<std::collections::HashSet<String>> {
    // Stored deliveries must match the destinations configured now.
    // Interfaces with nothing waiting have nothing to reconcile, and their workers nothing to read
    // yet (they wait for the first message's wake-up, or the idle poll).
    let waiting = store.with_waiting(&charge.queued).await?;
    let (busy, busy_ids): (Vec<(String, String)>, Vec<_>) = charge
        .destinations
        .iter()
        .cloned()
        .zip(charge.ids.iter().copied())
        .filter(|((i, _), _)| waiting.contains(i))
        .unzip();
    let changes = if busy.is_empty() {
        ductus_store::Reconciled::default()
    } else {
        store.reconcile(&busy, &busy_ids).await.context("matching stored deliveries to the destinations")?
    };
    for (interface, count) in &changes.added {
        tracing::info!(%interface, count, "queued messages in flight for destinations they had no delivery for (new destination, or stored by an older version)");
    }
    for (interface, destination, count) in &changes.removed {
        tracing::warn!(%interface, %destination, count, "destination is no longer in interface.yaml: its pending deliveries were marked filtered");
    }
    phases.mark("deliveries reconciled");
    Ok(waiting.into_iter().collect())
}

/// The rest of what a start changes for its interfaces, which nothing waits for: the configured
/// retention applied, beside the start (since 0.7.4.2's follow-up; a takeover always sent first).
/// Until then new messages go to the tier stored, as the engine before stored them; a change runs
/// beside sending anyway. A database that refuses is no reason to stop an engine that's moving
/// messages: it says so and tries again a minute later (not sooner: a change that just began is
/// recorded in the audit log only within its first minute, so a retry never records it twice),
/// and the retention tasks (changes, drops) start once it's applied. The documentation snapshot
/// is saved with the deployment, beside the start too.
fn settle_in(retention: Arc<retention::Retention>, ended: &tokio_util::sync::CancellationToken) {
    tokio::spawn(ended.clone().run_until_cancelled_owned(async move {
        let started = std::time::Instant::now();
        loop {
            match retention.apply().await {
                Ok(()) => break,
                Err(e) => {
                    tracing::error!(error = %format!("{e:#}"), "couldn't apply the interfaces' retention (new messages keep their stored tier, nothing expired is dropped meanwhile): trying again in a minute");
                    tokio::time::sleep(Duration::from_secs(60)).await;
                }
            }
        }
        tracing::debug!(took = ?started.elapsed(), "retention applied beside the start");
    }));
}

/// Stop in order, so nothing is left half done: close every interface's gate (listeners close,
/// queued HTTP answers 503, file pickup and schedules wait) and wait for messages being stored,
/// so each gets its ACK; then the workers finish the message in hand and save their statuses;
/// then last activity is saved (the API answers what's in flight after: `run_engine`). What's
/// still pending stays in the queue for the next start, as in a drain.
async fn stop(
    engine: &control::Engine,
    stopping: &tokio_util::sync::CancellationToken,
    threads: Vec<std::thread::JoinHandle<()>>,
) {
    for i in &engine.interfaces {
        i.gate.set_open(false);
    }
    while engine.interfaces.iter().any(|i| i.gate.in_flight() > 0) {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    stopping.cancel();
    engine.shutdown.cancel(); // the API takes no new requests
    let _ = tokio::task::spawn_blocking(move || {
        for thread in threads {
            let _ = thread.join();
        }
    })
    .await;
    engine.activity.saved().await;
}

/// The operating system asking the engine to stop: Ctrl-C, or SIGTERM on Unix (systemd,
/// Docker and Kubernetes stop services with it). Its name, for the log.
pub async fn stop_signal() -> &'static str {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut terminate) => tokio::select! {
                _ = tokio::signal::ctrl_c() => "Ctrl-C",
                _ = terminate.recv() => "SIGTERM",
            },
            Err(e) => {
                tracing::warn!(error = %e, "can't listen for SIGTERM: only Ctrl-C stops the engine cleanly");
                let _ = tokio::signal::ctrl_c().await;
                "Ctrl-C"
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        "Ctrl-C"
    }
}

/// Wake-up signals, one per (slot, destination). Ingest wakes every destination's worker in the
/// slot that owns a new message's lane; `notify_one` keeps a permit while a worker is busy, so
/// a wake-up is never lost.
pub(crate) struct Wake {
    destinations: usize,
    notify: Box<[Notify]>,
}

impl Wake {
    fn new(slots: u32, destinations: usize) -> Self {
        let destinations = destinations.max(1); // respond mode has none, and no queue
        Self { destinations, notify: (0..slots as usize * destinations).map(|_| Notify::new()).collect() }
    }

    pub(crate) fn slots(&self) -> usize {
        self.notify.len() / self.destinations
    }

    /// Wake the owners of a lane (same assignment as `lanes::Layout::lanes_of_slot`).
    pub(crate) fn lane(&self, partition: i32) {
        let slot = partition as usize % self.slots();
        self.notify[slot * self.destinations..][..self.destinations].iter().for_each(Notify::notify_one);
    }

    pub(crate) fn get(&self, slot: usize, destination: usize) -> &Notify {
        &self.notify[slot * self.destinations + destination]
    }
}

/// Current local time in HL7 TS format.
pub(crate) fn hl7_now() -> String {
    chrono::Local::now().format("%Y%m%d%H%M%S").to_string()
}
