//! The engine, its interfaces, drain mode and live updates.

use super::{ApiError, ApiResult, Caller, Eng, Shared};
use crate::control::{DrainState, Engine, Running, Snapshot};
use crate::stats::{self, DestinationStats, Histogram, InterfaceStats};
use axum::Json;
use axum::extract::Path;
use axum::response::sse::{Event, KeepAlive, Sse};
use chrono::{DateTime, Utc};
use ductus_core::about::given;
use ductus_core::config::{Destination, HttpMode, Isolation, Source};
use ductus_store::Role;
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

pub fn routes() -> OpenApiRouter<Shared> {
    OpenApiRouter::new()
        .routes(routes!(health))
        .routes(routes!(engine))
        .routes(routes!(interfaces))
        .routes(routes!(interface))
        .routes(routes!(drain_status, drain, resume))
        .routes(routes!(events))
}

#[derive(Serialize, ToSchema)]
pub struct Health {
    /// Always `ok` when the engine answers.
    status: &'static str,
    /// `accepting`, `draining` or `drained`: a load balancer can stop sending when it isn't
    /// `accepting`.
    drain: &'static str,
}

/// Is the engine up? Needs no token (for load balancers and monitoring).
#[utoipa::path(get, path = "/health", tag = "engine", security(()), responses((status = 200, body = Health)))]
async fn health(engine: Eng) -> Json<Health> {
    Json(Health { status: "ok", drain: drain_label(engine.drain_state()) })
}

#[derive(Serialize, ToSchema)]
pub struct DrainStatus {
    /// `accepting`, `draining` or `drained`.
    state: &'static str,
    /// When draining started, or when it finished.
    since: Option<DateTime<Utc>>,
    /// The engine exits once drained.
    stop_when_drained: bool,
    /// Deliveries still to go, over all interfaces.
    pending: i64,
    /// Interfaces still holding pending deliveries (a destination that's down, or a stopped
    /// interface, keeps a drain from finishing).
    waiting_for: Vec<String>,
}

fn drain_label(state: DrainState) -> &'static str {
    match state {
        DrainState::Accepting => "accepting",
        DrainState::Draining { .. } => "draining",
        DrainState::Drained { .. } => "drained",
    }
}

fn drain_report(engine: &Engine, snapshot: &Snapshot) -> DrainStatus {
    let state = engine.drain_state();
    let (since, stop) = match state {
        DrainState::Accepting => (None, false),
        DrainState::Draining { since, stop } | DrainState::Drained { since, stop } => (Some(since), stop),
    };
    let pending = |i: &Running| i.destinations.iter().map(|(_, id)| snapshot.queue(*id).pending).sum::<i64>();
    DrainStatus {
        state: drain_label(state),
        since,
        stop_when_drained: stop,
        pending: engine.interfaces.iter().map(pending).sum(),
        waiting_for: engine.interfaces.iter().filter(|i| pending(i) > 0).map(|i| i.config.name.clone()).collect(),
    }
}

#[derive(Serialize, ToSchema)]
pub struct EngineInfo {
    version: &'static str,
    environment: String,
    /// The server's name.
    host: String,
    started_at: DateTime<Utc>,
    uptime_seconds: i64,
    interfaces: usize,
    /// Interfaces stopped because their code ran away (their messages wait).
    stopped: usize,
    drain: DrainStatus,
    /// Calls from interface code to each database and API in resources.yaml (`fetch`: URLs
    /// called directly), since the engine started.
    resources: Vec<ResourceCalls>,
}

#[derive(Serialize, ToSchema)]
pub struct ResourceCalls {
    /// `db`, `api` or `fetch`.
    kind: String,
    name: String,
    calls: Timing,
    /// Calls that failed (network, SQL, timeout; an HTTP error status is an answer, not a failure).
    errors: u64,
}

/// The engine: version, environment, uptime, drain state.
#[utoipa::path(get, path = "/engine", tag = "engine", responses((status = 200, body = EngineInfo)))]
async fn engine(engine: Eng, _caller: Caller) -> ApiResult<EngineInfo> {
    let snapshot = engine.snapshot().await?;
    Ok(Json(EngineInfo {
        version: env!("CARGO_PKG_VERSION"),
        environment: engine.environment.clone(),
        host: crate::doctor::hostname(),
        started_at: engine.started_at,
        uptime_seconds: (Utc::now() - engine.started_at).num_seconds(),
        interfaces: engine.interfaces.len(),
        stopped: engine.interfaces.iter().filter(|i| i.health.stopped().is_some()).count(),
        drain: drain_report(&engine, &snapshot),
        resources: engine
            .calls
            .all()
            .map(|(kind, name, s)| ResourceCalls {
                kind: kind.into(),
                name: name.into(),
                calls: Timing::of(&s.calls),
                errors: stats::get(&s.errors),
            })
            .collect(),
    }))
}

#[derive(Serialize, ToSchema)]
pub struct InterfaceStatus {
    name: String,
    /// `running`, or `stopped` (its code ran away: its messages wait until it's fixed and the
    /// engine restarted).
    state: &'static str,
    stopped_reason: Option<String>,
    /// False while draining.
    accepting: bool,
    /// Where messages come from: `mllp 0.0.0.0:6661`, `http POST /orders on 0.0.0.0:8080 (queue)`.
    source: String,
    /// `shared` or `dedicated`.
    isolation: &'static str,
    /// Parallel workers (dedicated) or 1 (shared).
    workers: u32,
    /// Days its messages are kept, as configured (none in respond mode: it stores nothing).
    /// `GET /retention` has changes in progress.
    retention_days: Option<i16>,
    description: Option<String>,
    owner: Option<String>,
    /// Counts since the engine started (they start at zero with it).
    since_start: InterfaceCounts,
    destinations: Vec<DestinationStatus>,
}

#[derive(Serialize, ToSchema)]
pub struct InterfaceCounts {
    /// Stored and queued.
    received: u64,
    /// Stored as duplicates, not queued.
    duplicates: u64,
    /// Couldn't be stored (the sender was told to retry).
    store_failures: u64,
    /// Respond mode: requests answered, how long they took in the engine, and how many failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    requests: Option<Timing>,
    #[serde(skip_serializing_if = "Option::is_none")]
    request_errors: Option<u64>,
}

/// How long something took, over every time since the engine started (estimated from histogram
/// buckets, like Prometheus does: good to a bucket's width).
#[derive(Serialize, ToSchema)]
pub struct Timing {
    count: u64,
    mean_ms: Option<f64>,
    p50_ms: Option<f64>,
    p95_ms: Option<f64>,
}

impl Timing {
    fn of(h: &Histogram) -> Self {
        let ms = |d: Option<std::time::Duration>| d.map(|d| (d.as_secs_f64() * 1e4).round() / 10.0);
        Self { count: h.count(), mean_ms: ms(h.mean()), p50_ms: ms(h.quantile(0.5)), p95_ms: ms(h.quantile(0.95)) }
    }
}

#[derive(Serialize, ToSchema)]
pub struct DestinationCounts {
    sent: u64,
    filtered: u64,
    quarantined: u64,
    /// Temporary failures (each retry counts).
    retries: u64,
    /// From receipt to sent, waiting in the queue included.
    latency: Timing,
    /// Each successful send (the receiver's time).
    send: Timing,
    /// The destination's function.
    transform: Timing,
}

fn interface_counts(s: &InterfaceStats, responds: bool) -> InterfaceCounts {
    InterfaceCounts {
        received: stats::get(&s.received),
        duplicates: stats::get(&s.duplicates),
        store_failures: stats::get(&s.store_failures),
        requests: responds.then(|| Timing::of(&s.requests)),
        request_errors: responds.then(|| stats::get(&s.request_errors)),
    }
}

fn destination_counts(s: &DestinationStats) -> DestinationCounts {
    DestinationCounts {
        sent: stats::get(&s.sent),
        filtered: stats::get(&s.filtered),
        quarantined: stats::get(&s.quarantined),
        retries: stats::get(&s.retries),
        latency: Timing::of(&s.latency),
        send: Timing::of(&s.send),
        transform: Timing::of(&s.transform),
    }
}

#[derive(Serialize, ToSchema)]
pub struct DestinationStatus {
    name: String,
    /// Where it sends: `mllp lab.local:5000`, `http api billing /charges`, `file out`.
    connector: String,
    /// Set when it isn't the interface's owner.
    owner: Option<String>,
    /// Waiting to be delivered.
    pending: i64,
    /// Needing a person.
    quarantined: i64,
    /// Counts and timings since the engine started.
    since_start: DestinationCounts,
}

#[derive(Serialize, ToSchema)]
pub struct SystemInfo {
    application: Option<String>,
    vendor: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct InterfaceDetail {
    #[serde(flatten)]
    status: InterfaceStatus,
    /// The sending system and where its connections come from.
    from: SystemInfo,
    from_hosts: Vec<String>,
    /// The receiving system (destinations may name their own).
    to: SystemInfo,
    /// Longest the code may run without pausing.
    timeout_ms: u64,
    format: String,
    folder: String,
}

fn source_label(source: &Source) -> String {
    match source {
        Source::Mllp { host, port } => format!("mllp {host}:{port}"),
        Source::Http { host, port, path, method, mode } => {
            let mode = if *mode == HttpMode::Respond { "respond" } else { "queue" };
            format!("http {} {path} on {host}:{port} ({mode})", method.to_ascii_uppercase())
        }
        Source::File(f) => format!("file {}/{}", f.dir.trim_end_matches('/'), f.pattern),
        Source::Sftp(f) => {
            format!("sftp {}:{}/{}", f.server.as_deref().unwrap_or("?"), f.dir.trim_end_matches('/'), f.pattern)
        }
        Source::Schedule(s) => match (&s.every, &s.cron) {
            (Some(every), _) => format!("schedule every {}", humanize(*every)),
            (_, Some(cron)) => format!("schedule cron {cron}"),
            _ => "schedule".into(),
        },
        Source::Internal => "internal (routed from other interfaces)".into(),
    }
}

fn humanize(d: std::time::Duration) -> String {
    let s = d.as_secs();
    match s {
        0 => format!("{}ms", d.as_millis()),
        s if s % 3600 == 0 => format!("{}h", s / 3600),
        s if s % 60 == 0 => format!("{}m", s / 60),
        s => format!("{s}s"),
    }
}

fn connector_label(d: &Destination) -> String {
    match d {
        Destination::Mllp { host, port, .. } => format!("mllp {host}:{port}"),
        Destination::File { dir } => format!("file {}", dir.display()),
        Destination::Sftp { server, dir } => format!("sftp {server}:{dir}"),
        Destination::Http { url: Some(url), method, .. } => {
            format!("http {} {}", method.to_ascii_uppercase(), strip_query(url))
        }
        Destination::Http { api, path, method, .. } => format!(
            "http {} api {}{}",
            method.to_ascii_uppercase(),
            api.as_deref().unwrap_or("?"),
            path.as_deref().map(|p| format!(" {p}")).unwrap_or_default()
        ),
        Destination::Interface(target) => format!("interface {target}"),
    }
}

/// URLs may carry credentials in the query or user info: never show them.
fn strip_query(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(mut u) => {
            u.set_query(None);
            let _ = u.set_password(None);
            u.to_string()
        }
        Err(_) => url.split('?').next().unwrap_or(url).to_string(),
    }
}

fn interface_status(engine: &Engine, i: &Running, snapshot: &Snapshot) -> InterfaceStatus {
    let c = &i.config;
    let owner = given(&c.about.owner);
    let stopped = i.health.stopped();
    InterfaceStatus {
        name: c.name.clone(),
        state: if stopped.is_some() { "stopped" } else { "running" },
        stopped_reason: stopped.map(str::to_string),
        accepting: i.gate.is_open(),
        source: source_label(&c.source),
        isolation: if c.isolation == Isolation::Dedicated { "dedicated" } else { "shared" },
        workers: c.slots(),
        retention_days: engine.retention.get(&c.name).map(|m| m.configured),
        description: given(&c.about.description).map(Into::into),
        owner: owner.map(Into::into),
        since_start: interface_counts(&i.stats, c.responds()),
        destinations: c
            .destinations()
            .zip(&i.destinations)
            .zip(&i.stats.destinations)
            .map(|(((name, d), (_, id)), stats)| {
                let queue = snapshot.queue(*id);
                DestinationStatus {
                    name: name.into(),
                    connector: connector_label(&d.connector),
                    owner: given(&d.owner)
                        .filter(|o| owner.is_none_or(|i| !ductus_core::owners::same(o, i)))
                        .map(Into::into),
                    pending: queue.pending,
                    quarantined: queue.quarantined,
                    since_start: destination_counts(stats),
                }
            })
            .collect(),
    }
}

/// Every interface with its state and each destination's queue.
#[utoipa::path(get, path = "/interfaces", tag = "engine", responses((status = 200, body = Vec<InterfaceStatus>)))]
async fn interfaces(engine: Eng, _caller: Caller) -> ApiResult<Vec<InterfaceStatus>> {
    let snapshot = engine.snapshot().await?;
    Ok(Json(engine.interfaces.iter().map(|i| interface_status(&engine, i, &snapshot)).collect()))
}

/// One interface: its state, queues and what it connects.
#[utoipa::path(
    get, path = "/interfaces/{name}", tag = "engine",
    params(("name" = String, Path, description = "Interface name")),
    responses((status = 200, body = InterfaceDetail), (status = 404, body = super::ErrorBody))
)]
async fn interface(engine: Eng, _caller: Caller, Path(name): Path<String>) -> ApiResult<InterfaceDetail> {
    let i = engine.interface(&name).ok_or_else(|| ApiError::not_found(format!("no interface named `{name}`")))?;
    let snapshot = engine.snapshot().await?;
    let about = &i.config.about;
    let system = |application: &Option<String>, vendor: &Option<String>| SystemInfo {
        application: given(application).map(Into::into),
        vendor: given(vendor).map(Into::into),
    };
    Ok(Json(InterfaceDetail {
        status: interface_status(&engine, i, &snapshot),
        from: system(&about.from.application, &about.from.vendor),
        from_hosts: about.from.hosts.clone(),
        to: system(&about.to.application, &about.to.vendor),
        timeout_ms: i.config.timeout.as_millis() as u64,
        format: i.config.format.name().to_string(),
        folder: i.config.dir.display().to_string(),
    }))
}

/// Drain mode: accepting, draining or drained, and what's still pending.
#[utoipa::path(get, path = "/drain", tag = "drain", responses((status = 200, body = DrainStatus)))]
async fn drain_status(engine: Eng, _caller: Caller) -> ApiResult<DrainStatus> {
    let snapshot = engine.snapshot().await?;
    Ok(Json(drain_report(&engine, &snapshot)))
}

#[derive(Deserialize, ToSchema, Default)]
pub struct DrainRequest {
    /// Exit the engine once drained (for an upgrade or a server move).
    #[serde(default)]
    stop: bool,
}

/// Start draining: stop taking new messages (MLLP listeners close, queued HTTP answers 503,
/// file pickup and schedules pause) and keep delivering what's stored. Operator.
#[utoipa::path(
    post, path = "/drain", tag = "drain", request_body(content = DrainRequest, content_type = "application/json"),
    responses((status = 200, body = DrainStatus), (status = 403, body = super::ErrorBody))
)]
async fn drain(engine: Eng, caller: Caller, body: Option<Json<DrainRequest>>) -> ApiResult<DrainStatus> {
    caller.require(Role::Operator)?;
    let request = body.map(|Json(b)| b).unwrap_or_default();
    tracing::warn!(token = caller.name(), stop = request.stop, "drain requested through the API");
    engine.drain(request.stop);
    let snapshot = engine.snapshot().await?;
    Ok(Json(drain_report(&engine, &snapshot)))
}

/// Stop draining: take messages again. Operator.
#[utoipa::path(
    delete, path = "/drain", tag = "drain",
    responses((status = 200, body = DrainStatus), (status = 403, body = super::ErrorBody))
)]
async fn resume(engine: Eng, caller: Caller) -> ApiResult<DrainStatus> {
    caller.require(Role::Operator)?;
    tracing::info!(token = caller.name(), "resume requested through the API");
    engine.resume();
    let snapshot = engine.snapshot().await?;
    Ok(Json(drain_report(&engine, &snapshot)))
}

#[derive(Serialize, ToSchema)]
pub struct LiveUpdate {
    at: DateTime<Utc>,
    drain: DrainStatus,
    interfaces: Vec<LiveInterface>,
}

#[derive(Serialize, ToSchema)]
pub struct LiveInterface {
    name: String,
    state: &'static str,
    accepting: bool,
    destinations: Vec<LiveDestination>,
}

#[derive(Serialize, ToSchema)]
pub struct LiveDestination {
    name: String,
    pending: i64,
    quarantined: i64,
}

fn live_update(engine: &Engine, snapshot: &Snapshot) -> LiveUpdate {
    LiveUpdate {
        at: snapshot.at,
        drain: drain_report(engine, snapshot),
        interfaces: engine
            .interfaces
            .iter()
            .map(|i| LiveInterface {
                name: i.config.name.clone(),
                state: if i.health.stopped().is_some() { "stopped" } else { "running" },
                accepting: i.gate.is_open(),
                destinations: i
                    .destinations
                    .iter()
                    .map(|(name, id)| {
                        let q = snapshot.queue(*id);
                        LiveDestination { name: name.clone(), pending: q.pending, quarantined: q.quarantined }
                    })
                    .collect(),
            })
            .collect(),
    }
}

/// Live updates (Server-Sent Events): an `update` event with every interface's queues and the
/// drain state, sent at once and then whenever something changes (checked every 2 s).
#[utoipa::path(
    get, path = "/events", tag = "engine",
    responses((status = 200, description = "`text/event-stream` of `update` events", body = LiveUpdate, content_type = "text/event-stream"))
)]
async fn events(engine: Eng, _caller: Caller) -> Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>> {
    let engine: Shared = engine.0;
    let queues = engine.watch_queues();
    let drain = engine.watch_drain();
    let stream = futures_util::stream::unfold(
        (engine, queues, drain, true),
        |(engine, mut queues, mut drain, first)| async move {
            if !first {
                tokio::select! {
                    changed = queues.changed() => changed.ok()?,
                    changed = drain.changed() => changed.ok()?,
                }
            }
            let mut snapshot = queues.borrow_and_update().clone();
            drain.borrow_and_update();
            if first {
                // The monitor may not have counted yet: start from fresh counts.
                if let Ok(fresh) = engine.snapshot().await {
                    snapshot = std::sync::Arc::new(fresh);
                }
            }
            let update = live_update(&engine, &snapshot);
            let event = Event::default().event("update").json_data(&update).unwrap_or_default();
            Some((Ok(event), (engine, queues, drain, false)))
        },
    );
    Sse::new(stream).keep_alive(KeepAlive::default())
}
