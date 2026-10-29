//! The engine, its interfaces, drain mode and live updates.

use super::{ApiError, ApiResult, Caller, Eng, Shared};
use crate::activity::Paused;
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
        .routes(routes!(pause, unpause))
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
    /// Paused destinations (`interface/destination`): they keep their messages and don't hold
    /// the drain up.
    paused: Vec<String>,
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
    let pending = |i: &Running| {
        let live = i.destinations.iter().filter(|(_, id)| engine.paused(*id).is_none());
        live.map(|(_, id)| snapshot.queue(*id).pending).sum::<i64>()
    };
    let paused = engine.interfaces.iter().flat_map(|i| {
        let paused = i.destinations.iter().filter(|(_, id)| engine.paused(*id).is_some());
        paused.map(|(name, _)| format!("{}/{name}", i.config.name))
    });
    DrainStatus {
        state: drain_label(state),
        since,
        stop_when_drained: stop,
        pending: engine.interfaces.iter().map(pending).sum(),
        waiting_for: engine.interfaces.iter().filter(|i| pending(i) > 0).map(|i| i.config.name.clone()).collect(),
        paused: paused.collect(),
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
    /// False while the engine has lost the database connection that holds its interfaces:
    /// sending, file pickup and schedules wait until it is back (receiving goes on).
    holds_interfaces: bool,
    /// Other engines running on this database, oldest first, and the interfaces each runs.
    /// An interface runs on one engine at a time.
    others: Vec<OtherEngine>,
    /// How this engine took over from the one before it (`systemctl reload`: an upgrade or a
    /// project deploy); none after a plain start.
    handover: Option<Handover>,
}

#[derive(Serialize, ToSchema)]
pub struct Handover {
    /// The version it took over from.
    from_version: String,
    from_pid: u32,
    /// When it held the interfaces.
    at: DateTime<Utc>,
    /// From the old engine's stop until this one sent (receiving never stopped).
    sending_paused_ms: u64,
}

#[derive(Serialize, ToSchema)]
pub struct OtherEngine {
    host: String,
    pid: i32,
    version: String,
    environment: String,
    started_at: DateTime<Utc>,
    /// The interfaces it runs (respond-mode HTTP interfaces aren't claimed, so not listed).
    interfaces: Vec<String>,
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
    let others = engine.store.other_engines(&engine.instance).await?;
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
        holds_interfaces: engine.ownership.is_held(),
        others: others
            .into_iter()
            .map(|e| OtherEngine {
                host: e.host,
                pid: e.pid,
                version: e.version,
                environment: e.environment,
                started_at: e.started_at,
                interfaces: e.interfaces,
            })
            .collect(),
        handover: engine.handed_over.get().map(|h| Handover {
            from_version: h.from.version.clone(),
            from_pid: h.from.pid,
            at: h.at,
            sending_paused_ms: h.sending_paused.as_millis() as u64,
        }),
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
    /// What it should have received (`monitor:` in interface.yaml), checked now.
    health: InterfaceHealth,
    /// Counts since the engine started (they start at zero with it).
    since_start: InterfaceCounts,
    destinations: Vec<DestinationStatus>,
}

#[derive(Serialize, ToSchema)]
pub struct InterfaceHealth {
    /// `ok`, `silent` (nothing within `active_within`), `missed` (nothing by `active_by`) or
    /// `stopped` (best to worst).
    state: &'static str,
    /// In operations' words: "silent: nothing received for 47 min".
    summary: String,
    /// When something last arrived (kept across restarts).
    last_received: Option<DateTime<Utc>>,
}

#[derive(Serialize, ToSchema)]
pub struct DestinationHealth {
    /// `ok`, `behind` (past its `behind` limit), `error` (has quarantined messages), `stalled`
    /// (messages wait, nothing finished for a minute) or `paused` (best to worst).
    state: &'static str,
    /// In operations' words: "behind 1,240 (oldest 12 min), catching up ~6 min; burst: 400/min
    /// received (normal 80)".
    summary: String,
    /// Which `behind` limit is past: `messages` or `age`.
    behind: Option<&'static str>,
    /// Seconds the oldest waiting message has waited.
    oldest_waiting_s: Option<u64>,
    /// Seconds to catch up at the last few minutes' rates (only while catching up).
    catch_up_s: Option<u64>,
    /// Why a backlog grows: `burst` (more arriving than normal for this hour of the week),
    /// `slow_receiver` (each send slower than normal).
    causes: Vec<&'static str>,
    /// When it last delivered (kept across restarts).
    last_sent: Option<DateTime<Utc>>,
    /// The last failure (a retry or a quarantine; kept across restarts).
    last_error: Option<LastError>,
    /// The last few minutes (none in the engine's first half minute).
    recent: Option<Rates>,
    /// Normal for this hour of the week, learned from the hours watched (none until then).
    normal: Rates,
}

#[derive(Serialize, ToSchema)]
pub struct LastError {
    at: DateTime<Utc>,
    error: String,
}

#[derive(Serialize, ToSchema)]
pub struct Rates {
    /// Messages arriving per minute.
    received_per_min: Option<f64>,
    /// Messages finished per minute (sent, filtered or quarantined).
    #[serde(skip_serializing_if = "Option::is_none")]
    done_per_min: Option<f64>,
    /// A successful send's mean time.
    send_ms: Option<f64>,
}

fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

fn destination_health(engine: &Engine, i: &Running, n: usize, snapshot: &Snapshot) -> DestinationHealth {
    use ductus_core::monitor::{Cause, Trend, Tripped};
    let now = Utc::now();
    let d = engine.diagnose(i, n, snapshot, now);
    let id = i.destinations[n].1;
    let stats = &i.stats.destinations[n];
    let (recent, normal_inflow, normal_send_ms) = engine.activity.trend(id, now);
    DestinationHealth {
        state: d.state.label(),
        summary: d.summary,
        behind: d.tripped.map(|t| match t {
            Tripped::Messages(_) => "messages",
            Tripped::Age(_) => "age",
        }),
        oldest_waiting_s: snapshot.oldest_age(id).map(|a| a.as_secs()),
        catch_up_s: match d.trend {
            Some(Trend::CatchingUp { eta }) => Some(eta.as_secs()),
            _ => None,
        },
        causes: d
            .causes
            .iter()
            .map(|c| match c {
                Cause::Burst { .. } => "burst",
                Cause::SlowReceiver { .. } => "slow_receiver",
            })
            .collect(),
        last_sent: stats.last_sent.get(),
        last_error: stats.last_error().map(|(at, error)| LastError { at, error }),
        recent: recent.map(|r| Rates {
            received_per_min: Some(round1(r.received as f64 / r.minutes)),
            done_per_min: Some(round1(r.done as f64 / r.minutes)),
            send_ms: r.send_ms.map(round1),
        }),
        normal: Rates {
            received_per_min: normal_inflow.map(round1),
            done_per_min: None,
            send_ms: normal_send_ms.map(round1),
        },
    }
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
    /// Set while it's paused on purpose (nothing sent or retried until resumed).
    paused: Option<PauseInfo>,
    /// Its state and why: behind, stalled, errors, and how it compares with normal.
    health: DestinationHealth,
    /// Counts and timings since the engine started.
    since_start: DestinationCounts,
}

#[derive(Serialize, ToSchema)]
pub struct PauseInfo {
    at: DateTime<Utc>,
    /// The token that paused it.
    by: String,
    /// Why, once someone says (it can be added after the pause).
    reason: Option<String>,
    reason_by: Option<String>,
    reason_at: Option<DateTime<Utc>>,
}

impl From<Paused> for PauseInfo {
    fn from(p: Paused) -> Self {
        Self { at: p.at, by: p.by, reason: p.reason, reason_by: p.reason_by, reason_at: p.reason_at }
    }
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
    /// The source's TLS, when it listens with it.
    source_tls: Option<SourceTls>,
    /// Who may connect to the source, when it has an `allow:` list (none: anyone).
    source_allow: Option<SourceAllow>,
}

/// A listener's `allow:` list and how it's doing.
#[derive(Serialize, ToSchema)]
pub struct SourceAllow {
    /// The list as written: addresses, subnets and host names.
    entries: Vec<String>,
    /// Each host name in it with what it resolves to now.
    names: Vec<AllowedName>,
    /// Connections refused since the engine started (0 when this engine doesn't hold the listener).
    refused: u64,
    /// The last refused connection's address and time.
    last_refused: Option<String>,
    last_refused_at: Option<DateTime<Utc>>,
}

#[derive(Serialize, ToSchema)]
pub struct AllowedName {
    name: String,
    /// The addresses let in for it: the last lookup that found some (kept when a later one fails).
    addresses: Vec<String>,
    /// When it was last looked up.
    checked_at: Option<DateTime<Utc>>,
    /// Why the last lookup failed, if it did.
    error: Option<String>,
}

impl SourceAllow {
    /// A listener's list, with what the running listener on its port knows (names and refusals).
    pub fn of(allow: &ductus_core::allow::Allow, source: &Source) -> Self {
        let port = match source {
            Source::Mllp { port, .. } | Source::Http { port, .. } => Some(*port),
            _ => None,
        };
        Self::on_port(allow, port)
    }

    pub fn on_port(allow: &ductus_core::allow::Allow, port: Option<u16>) -> Self {
        let running = port.and_then(crate::allow::on_port);
        let names = match &running {
            Some(a) => a
                .names()
                .into_iter()
                .map(|(name, r)| AllowedName {
                    name,
                    addresses: r.addresses.iter().map(|a| a.to_string()).collect(),
                    checked_at: r.checked_at,
                    error: r.error,
                })
                .collect(),
            None => Vec::new(),
        };
        let (refused, last) = running.map(|a| a.refused()).unwrap_or_default();
        Self {
            entries: allow.0.iter().map(|e| e.trim().to_string()).collect(),
            names,
            refused,
            last_refused: last.map(|(ip, _)| ip.to_string()),
            last_refused_at: last.map(|(_, at)| at),
        }
    }
}

/// TLS on an interface's listener.
#[derive(Serialize, ToSchema)]
pub struct SourceTls {
    /// `MLLP over TLS` or `HTTPS`.
    protocol: &'static str,
    /// Where: `0.0.0.0:6661`, `POST /orders on 0.0.0.0:8443 (queue)`.
    address: String,
    /// The `certificates:` pair it presents.
    certificate: String,
    /// Senders need a certificate this CA (a `certificates:` entry) issued; none: any sender.
    clients_ca: Option<String>,
    /// And one of these subjects (none: any the CA issued).
    subjects: Vec<String>,
}

fn source_tls(source: &Source) -> Option<SourceTls> {
    let (protocol, address, tls) = match source {
        Source::Mllp { host, port, tls: Some(t), .. } => ("MLLP over TLS", format!("{host}:{port}"), t),
        Source::Http { host, port, path, method, mode, tls: Some(t), .. } => {
            let mode = if *mode == HttpMode::Respond { "respond" } else { "queue" };
            ("HTTPS", format!("{} {path} on {host}:{port} ({mode})", method.to_ascii_uppercase()), t)
        }
        _ => return None,
    };
    Some(SourceTls {
        protocol,
        address,
        certificate: tls.certificate.clone(),
        clients_ca: tls.clients.as_ref().map(|c| c.ca.clone()),
        subjects: tls.clients.as_ref().map(|c| c.subjects.clone()).unwrap_or_default(),
    })
}

/// ` over TLS`, and who may send: ` over TLS, clients need a certificate from hospital-ca`.
fn tls_label(tls: &Option<ductus_core::tls::ListenTls>) -> String {
    match tls {
        None => String::new(),
        Some(t) => match &t.clients {
            None => " over TLS".into(),
            Some(c) => format!(" over TLS, clients need a certificate from {}", c.ca),
        },
    }
}

fn source_label(source: &Source) -> String {
    match source {
        Source::Mllp { host, port, tls, .. } => format!("mllp {host}:{port}{}", tls_label(tls)),
        Source::Http { host, port, path, method, mode, tls, .. } => {
            let mode = if *mode == HttpMode::Respond { "respond" } else { "queue" };
            let scheme = if tls.is_some() { "https" } else { "http" };
            format!("{scheme} {} {path} on {host}:{port} ({mode}){}", method.to_ascii_uppercase(), tls_label(tls))
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
        Destination::Mllp { host, port, tls, .. } => match tls {
            Some(tls) => format!("mllp {host}:{port} over TLS{}", tls.describe()),
            None => format!("mllp {host}:{port}"),
        },
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
        health: {
            let (state, summary) = engine.interface_state(i, Utc::now());
            InterfaceHealth { state: state.label(), summary, last_received: i.stats.last_received.get() }
        },
        since_start: interface_counts(&i.stats, c.responds()),
        destinations: c
            .destinations()
            .zip(&i.destinations)
            .zip(&i.stats.destinations)
            .enumerate()
            .map(|(n, (((name, d), (_, id)), stats))| {
                let queue = snapshot.queue(*id);
                DestinationStatus {
                    name: name.into(),
                    connector: connector_label(&d.connector),
                    owner: given(&d.owner)
                        .filter(|o| owner.is_none_or(|i| !ductus_core::owners::same(o, i)))
                        .map(Into::into),
                    pending: queue.pending,
                    quarantined: queue.quarantined,
                    paused: engine.paused(*id).map(Into::into),
                    health: destination_health(engine, i, n, snapshot),
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
        source_tls: source_tls(&i.config.source),
        source_allow: i.config.source.allow().map(|a| SourceAllow::of(a, &i.config.source)),
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
    super::record_first(&engine, caller.audit("engine.drain").detail(serde_json::json!({ "stop": request.stop })))
        .await?;
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
    super::record_first(&engine, caller.audit("engine.resume")).await?;
    tracing::info!(token = caller.name(), "resume requested through the API");
    engine.resume();
    let snapshot = engine.snapshot().await?;
    Ok(Json(drain_report(&engine, &snapshot)))
}

#[derive(Deserialize, ToSchema, Default)]
pub struct PauseRequest {
    /// Why (optional: pause first, explain after; pausing again with a reason adds it).
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct PauseStatus {
    interface: String,
    destination: String,
    /// The pause (on `DELETE`: the pause that was ended; none if it wasn't paused).
    paused: Option<PauseInfo>,
    /// Messages waiting for it.
    pending: i64,
}

/// Pause a destination at once: nothing is sent or retried until it's resumed (a send under way
/// finishes). Messages keep queueing. No reason needed; send one now or later (pausing again
/// with a reason adds it). Survives restarts. Operator.
#[utoipa::path(
    post, path = "/interfaces/{name}/destinations/{destination}/pause", tag = "engine",
    params(("name" = String, Path, description = "Interface name"), ("destination" = String, Path, description = "Destination name (`default` for one given with `destination:`)")),
    request_body(content = PauseRequest, content_type = "application/json"),
    responses((status = 200, body = PauseStatus), (status = 403, body = super::ErrorBody), (status = 404, body = super::ErrorBody))
)]
async fn pause(
    engine: Eng,
    caller: Caller,
    Path((name, destination)): Path<(String, String)>,
    body: Option<Json<PauseRequest>>,
) -> ApiResult<PauseStatus> {
    caller.require(Role::Operator)?;
    let (_, id) = engine.destination(&name, &destination).map_err(ApiError::not_found)?;
    let reason = body.and_then(|Json(b)| b.reason).filter(|r| !r.trim().is_empty());
    let subject = format!("{name}/{destination}");
    super::record_first(
        &engine,
        caller
            .audit("destination.pause")
            .subject(&subject)
            .interface(&name)
            .detail(serde_json::json!({ "reason": reason })),
    )
    .await?;
    let paused = engine.pause(id, caller.name(), reason.as_deref()).await?;
    tracing::warn!(interface = %name, %destination, by = caller.name(), reason = reason.as_deref().unwrap_or("none yet"), "destination paused");
    let pending = engine.snapshot().await?.queue(id).pending;
    Ok(Json(PauseStatus { interface: name, destination, paused: Some(paused.into()), pending }))
}

/// Resume a paused destination: it sends its backlog in order. Operator.
#[utoipa::path(
    delete, path = "/interfaces/{name}/destinations/{destination}/pause", tag = "engine",
    params(("name" = String, Path, description = "Interface name"), ("destination" = String, Path, description = "Destination name")),
    responses((status = 200, body = PauseStatus), (status = 403, body = super::ErrorBody), (status = 404, body = super::ErrorBody))
)]
async fn unpause(
    engine: Eng,
    caller: Caller,
    Path((name, destination)): Path<(String, String)>,
) -> ApiResult<PauseStatus> {
    caller.require(Role::Operator)?;
    let (_, id) = engine.destination(&name, &destination).map_err(ApiError::not_found)?;
    // Counted before the pause ends: a receiver that is up takes the backlog at once, and counting
    // afterwards could report "0 waiting" for a backlog of thousands.
    let pending = engine.snapshot().await?.queue(id).pending;
    let subject = format!("{name}/{destination}");
    super::record_first(&engine, caller.audit("destination.resume").subject(&subject).interface(&name)).await?;
    let ended = engine.unpause(id).await?;
    if ended.is_some() {
        tracing::warn!(interface = %name, %destination, by = caller.name(), "destination resumed");
    }
    Ok(Json(PauseStatus { interface: name, destination, paused: ended.map(Into::into), pending }))
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
    /// What it receives, as in `GET /interfaces`.
    health: LiveHealth,
    destinations: Vec<LiveDestination>,
}

#[derive(Serialize, ToSchema)]
pub struct LiveDestination {
    name: String,
    pending: i64,
    quarantined: i64,
    paused: bool,
    /// As in `GET /interfaces` (an update is sent when a state changes, not as a summary's
    /// minutes tick on).
    health: LiveHealth,
}

#[derive(Serialize, ToSchema)]
pub struct LiveHealth {
    state: &'static str,
    summary: String,
}

fn live_update(engine: &Engine, snapshot: &Snapshot) -> LiveUpdate {
    let now = Utc::now();
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
                health: {
                    let (state, summary) = engine.interface_state(i, now);
                    LiveHealth { state: state.label(), summary }
                },
                destinations: i
                    .destinations
                    .iter()
                    .enumerate()
                    .map(|(n, (name, id))| {
                        let q = snapshot.queue(*id);
                        let d = engine.diagnose(i, n, snapshot, now);
                        LiveDestination {
                            name: name.clone(),
                            pending: q.pending,
                            quarantined: q.quarantined,
                            paused: engine.paused(*id).is_some(),
                            health: LiveHealth { state: d.state.label(), summary: d.summary },
                        }
                    })
                    .collect(),
            })
            .collect(),
    }
}

/// Live updates (Server-Sent Events): an `update` event with every interface's queues, health and
/// the drain state, sent at once and then whenever a count or a health state changes or a delivery
/// finishes (checked every 2 s).
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
