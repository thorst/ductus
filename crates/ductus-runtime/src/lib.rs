//! Runs interfaces: receive → persist → ACK, then workers transform and deliver.

pub mod activity;
pub mod alerts;
pub mod api;
pub mod audit;
pub mod certificates;
pub mod control;
mod destination;
pub mod doctor;
mod files;
pub mod gate;
pub mod health;
mod http;
mod ingest;
pub mod keys;
pub mod ldap;
mod limiter;
mod mssql;
pub mod ownership;
pub mod project;
pub mod retention;
mod schedule;
mod services;
mod sftp;
mod split;
pub mod stats;
pub mod tls;
mod worker;

use anyhow::{Context, Result, bail};
use ductus_core::config::{Destination, Isolation, Source};
use ductus_core::{Environment, InterfaceConfig};
use ductus_store::Store;
use std::path::Path;
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
    /// Another way to ask for the stop a signal asks for: the Windows service manager's Stop.
    pub stop_request: Option<std::sync::Arc<tokio::sync::Notify>>,
    /// Called once when every interface and the API are up (the service reports Running then).
    pub on_started: Option<Box<dyn FnOnce() + Send>>,
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
        }
    }
}

/// Start every interface under `root` and run until Ctrl-C or SIGTERM (or until drained, when a
/// drain asked to stop); then stop cleanly (`stop`).
pub async fn run(root: &Path, database_url: &str, mut options: EngineOptions) -> Result<()> {
    let mut phases = Phases::new();
    let env = Environment::load(root, &options.env)?;
    let configs = load_interfaces(root, &env)?;
    phases.mark("interface.yaml files read");
    // Loaded now so a missing variable or secret stops startup; used by interface code (v0.4).
    let resources = ductus_core::Resources::load(root, &env)?;
    let settings = ductus_core::settings::Settings::load(root)?;
    let alert_settings = settings.alerts(&env)?;
    let heap_mb = settings.isolates.heap_mb;
    let api_address = settings.api.address(options.api_listen.as_deref())?;
    if configs.is_empty() {
        bail!("no interfaces found under {}", root.join("interfaces").display());
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
    let alerts = match alert_settings {
        Some(a) => {
            let wrong: Vec<String> = configs.iter().flat_map(|c| a.check_interface(&c.name, &c.alert)).collect();
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
    phases.mark("project loaded and checked");
    let store = Store::connect(database_url, options.db_connections).await?;
    phases.mark("database connected and migrated");
    // Before anything is changed for an interface (deliveries reconciled, retention applied,
    // workers started): it must be this engine's, and its lanes must still reach what waits.
    let instance = audit::new_instance();
    let me = ownership::Me { instance: instance.clone(), host: audit::host_name(), environment: options.env.clone() };
    let queued = configs.iter().filter(|c| !c.responds()).map(|c| c.name.clone()).collect();
    let owner = Arc::new(ownership::Owner::claim(&store, me, queued).await?);
    let lanes: Vec<_> = configs.iter().filter_map(ownership::Lanes::of).collect();
    owner.guard_lanes(&store, &lanes).await?;
    phases.mark("interfaces claimed, lanes checked");
    // The company's documentation fields: a broken docs-schema.yaml is for `ductus validate` to report,
    // not a reason to stop the engine. The snapshot is for SQL reports, so it never blocks startup either.
    let docs = ductus_core::docs::Schema::load(root).unwrap_or_else(|e| {
        tracing::warn!(error = format!("{e:#}"), "ignoring the documentation fields: run `ductus validate`");
        Default::default()
    });
    let snapshot: Vec<_> = ductus_core::docs::snapshot(&configs, &resources)
        .into_iter()
        .map(|(kind, name, docs)| ductus_store::DocsRow { kind, name, docs: docs.to_string() })
        .collect();
    if let Err(e) = store.save_docs_snapshot(&snapshot).await {
        tracing::warn!(error = format!("{e:#}"), "could not save the documentation snapshot");
    }
    // Before any worker starts: stored deliveries must match the destinations configured now.
    let destinations: Vec<(String, String)> =
        configs.iter().flat_map(|c| c.destinations().map(|(name, _)| (c.name.clone(), name.to_string()))).collect();
    let all_ids = store.destination_ids(&destinations).await?;
    // Interfaces with nothing waiting have nothing to reconcile, and their workers nothing to read
    // yet (they wait for the first message's wake-up, or the idle poll).
    let queued: Vec<String> = configs.iter().filter(|c| !c.responds()).map(|c| c.name.clone()).collect();
    let waiting = store.with_waiting(&queued).await?;
    let (busy, busy_ids): (Vec<(String, String)>, Vec<_>) =
        destinations.iter().cloned().zip(all_ids.iter().copied()).filter(|((i, _), _)| waiting.contains(i)).unzip();
    let changes = if busy.is_empty() {
        ductus_store::Reconciled::default()
    } else {
        store.reconcile(&busy, &busy_ids).await.context("matching stored deliveries to the destinations")?
    };
    let mut ids = all_ids.into_iter();
    for (interface, count) in &changes.added {
        tracing::info!(%interface, count, "queued messages in flight for destinations they had no delivery for (new destination, or stored by an older version)");
    }
    for (interface, destination, count) in &changes.removed {
        tracing::warn!(%interface, %destination, count, "destination is no longer in interface.yaml: its pending deliveries were marked filtered");
    }
    phases.mark("deliveries reconciled");
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
    let retention = retention::Retention::start(store.clone(), configured).await.context("applying retention")?;
    phases.mark("retention applied");
    let (stopped_tx, stopped_rx) = tokio::sync::mpsc::unbounded_channel();
    // Keep dedupe keys a little longer than the longest window that uses them.
    let dedupe_retention = configs.iter().filter_map(|c| c.dedupe.as_ref()).map(|d| d.window).max().unwrap_or_default()
        + Duration::from_secs(3600);

    // Plan threads: shared interfaces spread round-robin over the pool; each dedicated slot
    // gets a thread (and isolate) of its own.
    let pool_threads = options.pool_threads.max(1);
    let mut pool: Vec<Vec<worker::Job>> = (0..pool_threads).map(|_| Vec::new()).collect();
    let mut dedicated: Vec<(String, worker::Job)> = Vec::new();
    let mut interfaces = Vec::new();
    let mut http_endpoints = Vec::new();
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
        if !config.responds() {
            let intake = ingest::Intake::new(
                config.clone(),
                store.clone(),
                wake.clone(),
                gate.clone(),
                destination_ids.clone(),
                retention.tier(&config.name),
                stats.clone(),
            )?;
            intakes.insert(config.name.clone(), Arc::new(intake));
        }
        parts.push((config, wake, destination_ids, gate, stats));
    }
    // Last activity and pauses, before any worker starts (a paused destination sends nothing).
    let watched = parts
        .iter()
        .filter(|(config, ..)| !config.responds())
        .map(|(_, _, ids, _, stats)| activity::Watched { stats: stats.clone(), destinations: ids.clone() })
        .collect();
    // The engine is stopping: workers finish what's in hand, save it and end (see `stop`).
    let stopping = tokio_util::sync::CancellationToken::new();
    let activity =
        activity::Activity::start(store.clone(), watched, stopping.clone()).await.context("loading last activity")?;
    phases.mark("last activity loaded");
    for (config, wake, destination_ids, gate, stats) in parts {
        let health = Arc::new(health::Health::reporting(&config.name, stopped_tx.clone()));
        let outlets: Vec<worker::Outlet> = config
            .destinations()
            .zip(&destination_ids)
            .zip(&stats.destinations)
            .map(|(((name, d), &id), destination_stats)| {
                // Built here, once: the names were checked above, and destinations naming the
                // same CA and certificate share their settings.
                let tls = match &d.connector {
                    Destination::Mllp { tls: Some(t), .. } => Some(certificates.client(t)),
                    Destination::Http { api: Some(api), .. } => {
                        resources.apis.get(api).and_then(|a| certificates.named(&a.tls()).transpose())
                    }
                    Destination::Http { ca, certificate, .. } => {
                        certificates.named(&ductus_core::tls::ConnectTls::new(ca, certificate)).transpose()
                    }
                    _ => None,
                };
                let tls = tls.transpose().with_context(|| format!("{}: TLS", config.label_destination(name)))?;
                anyhow::Ok(worker::Outlet {
                    id,
                    tls,
                    stats: destination_stats.clone(),
                    pause: activity.pauses[&id].clone(),
                    limiter: config
                        .rate_limit_for(d)
                        .map(|r| Arc::new(limiter::Limiter::new(r.clone(), std::time::Instant::now()))),
                    server: match &d.connector {
                        Destination::Sftp { server, .. } => sftp_server(server),
                        _ => None,
                    },
                    api: match &d.connector {
                        Destination::Http { api: Some(api), .. } => resources.apis.get(api).cloned(),
                        _ => None,
                    },
                    route: d.route().map(|target| intakes[target].clone()), // checked by check_routes
                })
            })
            .collect::<Result<_>>()?;
        // Respond-mode HTTP interfaces get requests over a channel instead of a queue.
        let mut requests = None;
        if let Source::Http { host, port, path, method, tls, .. } = &config.source {
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
            });
        }
        for slot in 0..config.slots() {
            let job = worker::Job {
                config: config.clone(),
                slot,
                wake: wake.clone(),
                requests: requests.take(),
                health: health.clone(),
                outlets: outlets.clone(),
                stats: stats.clone(),
                ownership: owner.ownership.clone(),
                nothing_waiting: !waiting.contains(&config.name),
            };
            match config.isolation {
                Isolation::Shared => {
                    pool[shared_count % pool_threads].push(job);
                    shared_count += 1;
                }
                Isolation::Dedicated => dedicated.push((format!("{}-w{slot}", config.name), job)),
            }
        }
        interfaces.push((config, wake, destination_ids, health, gate, stats));
    }

    let services = Arc::new(services::SharedServices::new(env, &resources, &certificates)?);
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let mut jobs = 0;
    let mut threads = Vec::new();
    for (i, thread_jobs) in pool.into_iter().enumerate().filter(|(_, j)| !j.is_empty()) {
        jobs += thread_jobs.len();
        let engine = services::EngineServices::new(services.clone(), Default::default());
        let name = format!("pool-{i}");
        let ready = ready_tx.clone();
        threads.push(worker::spawn_thread(name, thread_jobs, store.clone(), engine, heap_mb, ready, stopping.clone())?);
    }
    // A dedicated interface's own pools are shared by all its worker threads.
    let mut own_pools = std::collections::HashMap::new();
    for (name, job) in dedicated {
        jobs += 1;
        let own =
            own_pools.entry(job.config.name.clone()).or_insert_with(|| services.dedicated_pools(&job.config)).clone();
        let engine = services::EngineServices::new(services.clone(), own);
        let ready = ready_tx.clone();
        threads.push(worker::spawn_thread(name, vec![job], store.clone(), engine, heap_mb, ready, stopping.clone())?);
    }
    drop(ready_tx);
    // Fail fast if any interface's code doesn't load, before accepting any messages.
    let mut failed = Vec::new();
    let mut reported = 0;
    for (name, result) in ready_rx.iter().take(jobs) {
        reported += 1;
        if let Err(e) = result {
            failed.push(format!("interface `{name}`: {e:#}"));
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

    for (config, ..) in &interfaces {
        let intake = || intakes[&config.name].clone();
        match &config.source {
            Source::Mllp { tls, .. } => ingest::spawn_mllp(intake(), server_tls(tls, &[])?).await?,
            Source::File(f) | Source::Sftp(f) => {
                let server = f.server.as_deref().and_then(sftp_server);
                let watcher = files::FileWatcher::new(intake(), server, owner.ownership.clone())?;
                tokio::spawn(watcher.run());
            }
            Source::Schedule(s) => {
                let timing = ductus_core::schedule::Timing::new(s)?;
                tokio::spawn(schedule::run(intake(), timing, owner.ownership.clone()));
            }
            Source::Http { .. } => {} // served below, with the other HTTP interfaces on its port
            Source::Internal => {}    // other interfaces' workers store its messages (routes)
        }
    }
    http::serve(http_endpoints, stopping.clone()).await?;
    certificates.watch(stopping.clone());
    phases.mark("listening");
    let running: Vec<control::Running> = interfaces
        .iter()
        .map(|(config, _, ids, health, gate, stats)| control::Running {
            config: config.clone(),
            health: health.clone(),
            gate: gate.clone(),
            stats: stats.clone(),
            destinations: config.destinations().map(|(name, _)| name.to_string()).zip(ids.iter().copied()).collect(),
            intake: intakes.get(&config.name).cloned(),
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
    });
    // The audit log: this start (and how the last run on this host ended), interfaces the runaway
    // guard stops, and the hourly trimming. A log that can't be written is not a reason to refuse
    // to start (the engine may be all that is left to move messages), but it is an error.
    audit::start(&engine, stopped_rx, retention_days).await;
    alerts::start(&engine).await;
    certificates::start(&engine);
    ldap::start(&engine);
    phases.mark("audit log and alerts started");
    let mut api_server = None;
    if let Some((host, port)) = &api_address {
        api_server = Some(api::serve(engine.clone(), host, *port, server_tls(&settings.api.tls, HTTP1)?).await?);
    }
    phases.mark("API listening");
    // Messages left pending with nothing pending for them (after a crash, or reconcile's
    // changes): workers don't wait for their overall status, so it's set after the start.
    let settling = store.clone();
    let busy: Vec<String> = waiting.into_iter().collect();
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
        dedicated_threads = jobs - shared_count,
        took = ?phases.total(),
        "started"
    );
    if let Some(started) = options.on_started.take() {
        started();
    }

    let maintenance = store.clone();
    tokio::spawn(async move {
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
    });

    let requested = async {
        match &options.stop_request {
            Some(request) => request.notified().await,
            None => std::future::pending().await,
        }
    };
    owner.watch(store.clone(), stopping.clone());
    let why = tokio::select! {
        signal = stop_signal() => signal,
        _ = engine.shutdown.cancelled() => "drained and asked to stop",
        _ = requested => "asked by the service manager",
        _ = owner.taken_over() => "another engine took over its interfaces",
    };
    let limit = options.stop_timeout;
    tracing::info!(why, limit = ?limit, "stopping: taking no new messages, finishing the ones in hand");
    let started = std::time::Instant::now();
    let stop = stop(&engine, &stopping, threads, api_server);
    let mut forced = false;
    tokio::select! {
        finished = tokio::time::timeout(limit, stop) => match finished {
            Ok(()) => tracing::info!(took = ?started.elapsed(), "stopped"),
            Err(_) => {
                forced = true;
                tracing::warn!(
                    "still finishing after {limit:?}; stopping anyway: the messages in hand stay pending and are processed again at the next start (a destination may get one again)"
                )
            }
        },
        signal = stop_signal() => {
            forced = true;
            tracing::warn!(signal, "asked again: stopping now; the messages in hand are processed again at the next start")
        }
    }
    owner.release(&store).await;
    audit::stopped(&engine, why, forced).await;
    match owner.taken_why() {
        Some(why) => Err(ownership::Refused(why).into()),
        None => Ok(()),
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

/// Stop in order, so nothing is left half done: close every interface's gate (listeners close,
/// queued HTTP answers 503, file pickup and schedules wait) and wait for messages being stored,
/// so each gets its ACK; then the workers finish the message in hand and save their statuses;
/// then last activity is saved and the API answers what's in flight. What's still pending stays
/// in the queue for the next start, as in a drain.
async fn stop(
    engine: &control::Engine,
    stopping: &tokio_util::sync::CancellationToken,
    threads: Vec<std::thread::JoinHandle<()>>,
    api_server: Option<tokio::task::JoinHandle<()>>,
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
    // Answer the requests in flight (a drain's own, say). Live-update streams never end by
    // themselves, hence the limit.
    if let Some(server) = api_server {
        let _ = tokio::time::timeout(Duration::from_secs(5), server).await;
    }
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

    /// Wake the owners of a lane (same assignment as `InterfaceConfig::partitions_for_worker`).
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
