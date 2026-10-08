//! Runs interfaces: receive → persist → ACK, then workers transform and deliver.

pub mod api;
pub mod control;
mod destination;
pub mod doctor;
mod files;
pub mod gate;
pub mod health;
mod http;
mod ingest;
pub mod keys;
mod limiter;
mod mssql;
pub mod project;
mod schedule;
mod services;
mod sftp;
mod split;
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
}

impl Default for EngineOptions {
    fn default() -> Self {
        Self {
            pool_threads: std::thread::available_parallelism().map_or(4, |n| n.get()),
            db_connections: 32,
            env: "dev".into(),
            api_listen: None,
        }
    }
}

/// Start every interface under `root` and run until Ctrl-C (or until drained, when a drain
/// asked to stop).
pub async fn run(root: &Path, database_url: &str, options: EngineOptions) -> Result<()> {
    let env = Environment::load(root, &options.env)?;
    let configs = load_interfaces(root, &env)?;
    // Loaded now so a missing variable or secret stops startup; used by interface code (v0.4).
    let resources = ductus_core::Resources::load(root, &env)?;
    let settings = ductus_core::settings::Settings::load(root)?;
    let heap_mb = settings.isolates.heap_mb;
    let api_address = settings.api.address(options.api_listen.as_deref())?;
    if configs.is_empty() {
        bail!("no interfaces found under {}", root.join("interfaces").display());
    }
    let conflicts = ductus_core::config::check_endpoints(&configs);
    if !conflicts.is_empty() {
        bail!("port conflicts:\n  {}", conflicts.join("\n  "));
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
    let sftp_server = |name: &str| resources.sftp.get(name).cloned();
    let store = Store::connect(database_url, options.db_connections).await?;
    // Before any worker starts: stored deliveries must match the destinations configured now.
    let destinations: Vec<(String, String)> =
        configs.iter().flat_map(|c| c.destinations().map(|(name, _)| (c.name.clone(), name.to_string()))).collect();
    let changes = store.reconcile(&destinations).await.context("matching stored deliveries to the destinations")?;
    let mut ids = store.destination_ids(&destinations).await?.into_iter();
    for (interface, count) in &changes.added {
        tracing::info!(%interface, count, "queued messages in flight for destinations they had no delivery for (new destination, or stored by an older version)");
    }
    for (interface, destination, count) in &changes.removed {
        tracing::warn!(%interface, %destination, count, "destination is no longer in interface.yaml: its pending deliveries were marked filtered");
    }
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
    for config in configs {
        let config = Arc::new(config);
        let wake = Arc::new(Wake::new(config.slots(), config.destinations().count()));
        let destination_ids: Vec<_> = ids.by_ref().take(config.destinations().count()).collect();
        let health = Arc::new(health::Health::default());
        let gate = Arc::new(gate::Gate::default());
        let outlets: Vec<worker::Outlet> = config
            .destinations()
            .zip(&destination_ids)
            .map(|((_, d), &id)| worker::Outlet {
                id,
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
            })
            .collect();
        // Respond-mode HTTP interfaces get requests over a channel instead of a queue.
        let mut requests = None;
        if let Source::Http { host, port, path, method, .. } = &config.source {
            let route = if config.responds() {
                let (calls, rx) = tokio::sync::mpsc::channel(1024);
                requests = Some(rx);
                http::Route::Respond { interface: config.name.clone(), calls }
            } else {
                http::Route::Queue(Arc::new(ingest::Intake::new(
                    config.clone(),
                    store.clone(),
                    wake.clone(),
                    gate.clone(),
                    destination_ids.clone(),
                )?))
            };
            http_endpoints.push(http::Endpoint {
                host: host.clone(),
                port: *port,
                method: method.clone(),
                path: path.clone(),
                route,
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
            };
            match config.isolation {
                Isolation::Shared => {
                    pool[shared_count % pool_threads].push(job);
                    shared_count += 1;
                }
                Isolation::Dedicated => dedicated.push((format!("{}-w{slot}", config.name), job)),
            }
        }
        interfaces.push((config, wake, destination_ids, health, gate));
    }

    let services = Arc::new(services::SharedServices::new(env, &resources));
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let mut jobs = 0;
    for (i, thread_jobs) in pool.into_iter().enumerate().filter(|(_, j)| !j.is_empty()) {
        jobs += thread_jobs.len();
        let engine = services::EngineServices::new(services.clone(), Default::default());
        worker::spawn_thread(format!("pool-{i}"), thread_jobs, store.clone(), engine, heap_mb, ready_tx.clone())?;
    }
    // A dedicated interface's own pools are shared by all its worker threads.
    let mut own_pools = std::collections::HashMap::new();
    for (name, job) in dedicated {
        jobs += 1;
        let own =
            own_pools.entry(job.config.name.clone()).or_insert_with(|| services.dedicated_pools(&job.config)).clone();
        let engine = services::EngineServices::new(services.clone(), own);
        worker::spawn_thread(name, vec![job], store.clone(), engine, heap_mb, ready_tx.clone())?;
    }
    drop(ready_tx);
    // Fail fast if any interface's code doesn't load, before accepting any messages.
    let mut failed = Vec::new();
    for (name, result) in ready_rx.iter().take(jobs) {
        if let Err(e) = result {
            failed.push(format!("interface `{name}`: {e:#}"));
        }
    }
    if !failed.is_empty() {
        failed.dedup();
        bail!("{}", failed.join("\n"));
    }

    for (config, wake, destination_ids, _, gate) in &interfaces {
        let intake = || {
            ingest::Intake::new(config.clone(), store.clone(), wake.clone(), gate.clone(), destination_ids.clone())
                .map(Arc::new)
        };
        match &config.source {
            Source::Mllp { .. } => ingest::spawn_mllp(intake()?).await?,
            Source::File(f) | Source::Sftp(f) => {
                let watcher = files::FileWatcher::new(intake()?, f.server.as_deref().and_then(sftp_server))?;
                tokio::spawn(watcher.run());
            }
            Source::Schedule(s) => {
                tokio::spawn(schedule::run(intake()?, ductus_core::schedule::Timing::new(s)?));
            }
            Source::Http { .. } => {} // served below, with the other HTTP interfaces on its port
        }
    }
    http::serve(http_endpoints).await?;
    let running: Vec<control::Running> = interfaces
        .iter()
        .map(|(config, _, ids, health, gate)| control::Running {
            config: config.clone(),
            health: health.clone(),
            gate: gate.clone(),
            destinations: config.destinations().map(|(name, _)| name.to_string()).zip(ids.iter().copied()).collect(),
        })
        .collect();
    let engine = control::Engine::new(
        options.env.clone(),
        root.to_path_buf(),
        database_url.to_string(),
        store.clone(),
        resources.clone(),
        running,
    );
    if let Some((host, port)) = &api_address {
        api::serve(engine.clone(), host, *port).await?;
    }
    tracing::info!(
        interfaces = interfaces.len(),
        shared = shared_count,
        pool_threads,
        dedicated_threads = jobs - shared_count,
        "started"
    );

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

    tokio::select! {
        signal = tokio::signal::ctrl_c() => signal?,
        _ = engine.shutdown.cancelled() => tracing::info!("drained and asked to stop"),
    }
    tracing::info!("shutting down");
    Ok(())
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
