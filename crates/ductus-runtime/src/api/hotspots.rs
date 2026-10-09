//! Hotspots: per destination with a backlog, the lanes it waits in, what's first in each, and
//! which worker a failing message holds up (`ductus_core::hotspots`).

use super::{ApiError, ApiResult, Caller, Eng, Shared};
use axum::Json;
use axum::extract::Query;
use chrono::{DateTime, Utc};
use ductus_core::hotspots::{self, Head, Kind, Lane, Layout};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

pub fn routes() -> OpenApiRouter<Shared> {
    OpenApiRouter::new().routes(routes!(hotspots))
}

/// Lanes listed per destination by default, and at most.
const LANES: usize = 10;
const MAX_LANES: usize = 1000;

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct HotspotQuery {
    /// Only this interface's destinations.
    interface: Option<String>,
    /// Lanes to list per destination, most waiting first (default 10, at most 1000). Lanes
    /// whose first message is failing, and lanes no worker reads, are always listed.
    lanes: Option<usize>,
}

#[derive(Serialize, ToSchema)]
pub struct DestinationHotspots {
    interface: String,
    destination: String,
    paused: bool,
    /// Lanes the interface has (`ordering.partitions`; 1 without an ordering key).
    lanes_total: u32,
    /// Workers each destination has (1 in the shared pool).
    workers_total: u32,
    waiting: u64,
    lanes_waiting: usize,
    /// In operations' words: "1,240 waiting in 3 of 64 lanes; worker 2 blocked by message ...".
    summary: String,
    findings: Vec<FindingInfo>,
    /// Workers with something waiting.
    workers: Vec<WorkerInfo>,
    /// The lanes with the most waiting, plus any blocked or unread one.
    lanes: Vec<LaneInfo>,
}

#[derive(Serialize, ToSchema)]
pub struct FindingInfo {
    /// `blocked` (a message keeps failing; its worker's lanes wait), `all_blocked` (every worker
    /// is retrying: the receiver looks down), `uneven` (one worker has most of the backlog),
    /// `hot_lane` (one lane has most of it), `unowned` (a lane no worker reads).
    kind: &'static str,
    worker: Option<u32>,
    lane: Option<i32>,
    /// Its share of the backlog (0–1), for `uneven` and `hot_lane`.
    share: Option<f64>,
    text: String,
}

#[derive(Serialize, ToSchema)]
pub struct WorkerInfo {
    worker: u32,
    lanes_waiting: usize,
    waiting: u64,
    /// The lane whose first message it keeps retrying.
    blocked_by: Option<i32>,
}

#[derive(Serialize, ToSchema)]
pub struct LaneInfo {
    lane: i32,
    /// None: no worker reads it (`ordering.partitions` was lowered while it had messages).
    worker: Option<u32>,
    waiting: u64,
    head: HeadInfo,
}

/// The first message waiting in a lane: the next one its worker handles.
#[derive(Serialize, ToSchema)]
pub struct HeadInfo {
    /// The message (`GET /messages/{id}`).
    id: i64,
    ordering_key: Option<String>,
    received_at: DateTime<Utc>,
    waiting_s: u64,
    /// Failed attempts: above zero, its worker is retrying it and its lanes wait.
    attempts: i32,
    last_error: Option<String>,
    last_attempt_at: Option<DateTime<Utc>>,
}

fn finding(f: hotspots::Finding) -> FindingInfo {
    let (kind, worker, lane, share) = match f.kind {
        Kind::Blocked { worker, lane, .. } => ("blocked", Some(worker), Some(lane), None),
        Kind::AllBlocked => ("all_blocked", None, None, None),
        Kind::Uneven { worker, share } => ("uneven", Some(worker), None, Some(share)),
        Kind::HotLane { lane, share } => ("hot_lane", None, Some(lane), Some(share)),
        Kind::Unowned { lane, .. } => ("unowned", None, Some(lane), None),
    };
    FindingInfo { kind, worker, lane, share: share.map(|s| (s * 1000.0).round() / 1000.0), text: f.text }
}

/// Where each destination's backlog sits: the lanes messages wait in (the ordering key's
/// hash), the first message of each, and which worker a failing message holds up. Only
/// destinations with messages waiting are listed.
#[utoipa::path(
    get, path = "/hotspots", tag = "engine", params(HotspotQuery),
    responses((status = 200, body = Vec<DestinationHotspots>), (status = 404, body = super::ErrorBody))
)]
async fn hotspots(engine: Eng, _caller: Caller, Query(q): Query<HotspotQuery>) -> ApiResult<Vec<DestinationHotspots>> {
    if let Some(name) = &q.interface
        && engine.interface(name).is_none()
    {
        return Err(ApiError::not_found(format!("no interface named `{name}`")));
    }
    let listed = q.lanes.unwrap_or(LANES).min(MAX_LANES);
    let snapshot = engine.snapshot().await?;
    let now = Utc::now();
    let mut out = Vec::new();
    for i in engine.interfaces.iter().filter(|i| q.interface.as_ref().is_none_or(|n| *n == i.config.name)) {
        let layout = Layout { lanes: i.config.partition_count(), workers: i.config.slots() };
        for (n, (name, id)) in i.destinations.iter().enumerate() {
            if snapshot.queue(*id).pending == 0 {
                continue;
            }
            let lanes: Vec<Lane> = engine
                .store
                .lanes(*id)
                .await?
                .into_iter()
                .map(|r| Lane {
                    lane: r.lane,
                    waiting: r.waiting.max(0) as u64,
                    head: Head {
                        id: r.head_id,
                        key: r.ordering_key,
                        received_at: r.received_at,
                        attempts: r.attempts,
                        last_error: r.last_error,
                        last_attempt: (r.attempts > 0).then_some(r.updated_at),
                    },
                })
                .collect();
            let paused = engine.paused(*id).is_some();
            let last_sent = i.stats.destinations.get(n).and_then(|s| s.last_sent.get());
            let h = hotspots::analyze(&lanes, layout, paused, last_sent, now);
            let blocked: Vec<i32> = h.workers.iter().filter_map(|w| w.blocked_by).collect();
            // Most waiting first (as stored), then anything that must be seen.
            let shown = lanes
                .iter()
                .enumerate()
                .filter(|(n, l)| *n < listed || blocked.contains(&l.lane) || layout.worker_of(l.lane).is_none());
            out.push(DestinationHotspots {
                interface: i.config.name.clone(),
                destination: name.clone(),
                paused,
                lanes_total: layout.lanes,
                workers_total: layout.workers,
                waiting: h.waiting,
                lanes_waiting: h.lanes_waiting,
                summary: h.summary,
                findings: h.findings.into_iter().map(finding).collect(),
                workers: h
                    .workers
                    .iter()
                    .map(|w| WorkerInfo {
                        worker: w.worker,
                        lanes_waiting: w.lanes_waiting,
                        waiting: w.waiting,
                        blocked_by: w.blocked_by,
                    })
                    .collect(),
                lanes: shown
                    .map(|(_, l)| LaneInfo {
                        lane: l.lane,
                        worker: layout.worker_of(l.lane),
                        waiting: l.waiting,
                        head: HeadInfo {
                            id: l.head.id,
                            ordering_key: l.head.key.clone(),
                            received_at: l.head.received_at,
                            waiting_s: (now - l.head.received_at).num_seconds().max(0) as u64,
                            attempts: l.head.attempts,
                            last_error: l.head.last_error.clone(),
                            last_attempt_at: l.head.last_attempt,
                        },
                    })
                    .collect(),
            });
        }
    }
    Ok(Json(out))
}
