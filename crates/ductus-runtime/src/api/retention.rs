//! Retention: how long each interface keeps its messages, changes in progress, shorter values
//! waiting for a confirmation, expired partitions that had to stay, and the held area.

use super::{ApiError, ApiResult, Caller, Eng};
use crate::retention::Retention;
use axum::Json;
use axum::extract::Path;
use chrono::{DateTime, Utc};
use ductus_store::{RetentionState, Role};
use serde::Serialize;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

pub fn routes() -> OpenApiRouter<super::Shared> {
    OpenApiRouter::new().routes(routes!(retention)).routes(routes!(confirm))
}

#[derive(Serialize, ToSchema)]
pub struct RetentionReport {
    interfaces: Vec<InterfaceRetention>,
    /// When expired partitions were last dropped (hourly).
    last_drop_at: Option<DateTime<Utc>>,
    /// Expired partitions kept at the last drop, and why.
    kept: Vec<KeptPartition>,
    /// The held area as of the last drop: messages past their retention that someone still
    /// needs (pending for a destination, or quarantined and kept until resolved).
    held: Held,
}

#[derive(Serialize, ToSchema, Default)]
pub struct Held {
    /// Messages in it, finished ones included (they go when their month drops).
    messages: i64,
    pending: i64,
    quarantined: i64,
    /// Disk it uses, indexes included.
    bytes: i64,
    /// By month: a month is dropped once nothing in it is unfinished.
    months: Vec<HeldMonth>,
    /// Interfaces with unfinished messages in it.
    interfaces: Vec<HeldInterface>,
}

#[derive(Serialize, ToSchema)]
pub struct HeldMonth {
    /// The month, `2026-09`.
    month: String,
    partition: String,
    messages: i64,
    pending: i64,
    quarantined: i64,
    bytes: i64,
}

#[derive(Serialize, ToSchema)]
pub struct HeldInterface {
    name: String,
    pending: i64,
    quarantined: i64,
}

impl From<ductus_store::HeldArea> for Held {
    fn from(area: ductus_store::HeldArea) -> Self {
        let sum = |f: fn(&ductus_store::HeldMonth) -> i64| area.months.iter().map(f).sum();
        Held {
            messages: sum(|m| m.messages),
            pending: sum(|m| m.pending),
            quarantined: sum(|m| m.quarantined),
            bytes: sum(|m| m.bytes),
            months: area
                .months
                .iter()
                .map(|m| {
                    // `messages_held_202609` (the store only lists names of that shape).
                    let (year, month) =
                        m.partition.rsplit('_').next().unwrap_or_default().split_at_checked(4).unwrap_or_default();
                    HeldMonth {
                        month: format!("{year}-{month}"),
                        partition: m.partition.clone(),
                        messages: m.messages,
                        pending: m.pending,
                        quarantined: m.quarantined,
                        bytes: m.bytes,
                    }
                })
                .collect(),
            interfaces: area
                .interfaces
                .into_iter()
                .map(|i| HeldInterface { name: i.interface, pending: i.pending, quarantined: i.quarantined })
                .collect(),
        }
    }
}

#[derive(Serialize, ToSchema)]
pub struct InterfaceRetention {
    name: String,
    /// What interface.yaml (or ductus.yaml) says, in days.
    configured: i16,
    /// The retention in force: its new messages are kept this many days.
    days: i16,
    /// A change in progress (its older messages are being moved, or deleted when past the new
    /// retention), if any.
    change: Option<Change>,
    /// A configured value not applied yet: a change is running, or it's shorter and needs a
    /// confirmation.
    waiting: Option<i16>,
    /// The waiting value deletes messages: confirm it (`POST /retention/{name}/confirm`).
    needs_confirmation: bool,
    /// How many messages the waiting value would delete now, when it needs a confirmation.
    would_delete: Option<i64>,
}

#[derive(Serialize, ToSchema)]
pub struct Change {
    from: i16,
    to: i16,
    started_at: DateTime<Utc>,
    /// Messages still to move or delete (an estimate).
    rows_left: Option<i64>,
}

#[derive(Serialize, ToSchema)]
pub struct KeptPartition {
    partition: String,
    /// Its retention tier, in days.
    tier: i16,
    pending: i64,
    quarantined: i64,
    /// Interfaces that now keep their messages longer, with messages still in it.
    kept_for: Vec<String>,
}

async fn describe(retention: &Retention, state: RetentionState) -> anyhow::Result<InterfaceRetention> {
    let would_delete = if state.needs_confirmation() { retention.would_delete(&state).await? } else { None };
    let configured = retention.get(&state.interface).map(|m| m.configured).unwrap_or(state.days);
    Ok(InterfaceRetention {
        configured,
        days: state.days,
        change: state.changing_from.map(|from| Change {
            from,
            to: state.days,
            started_at: state.changed_at,
            rows_left: state.rows_left,
        }),
        waiting: state.waiting,
        needs_confirmation: state.needs_confirmation(),
        would_delete,
        name: state.interface,
    })
}

/// Every interface's retention, expired partitions that had to stay, and the held area.
#[utoipa::path(get, path = "/retention", tag = "retention", responses((status = 200, body = RetentionReport)))]
async fn retention(engine: Eng, _caller: Caller) -> ApiResult<RetentionReport> {
    let r = &engine.retention;
    let mut interfaces = Vec::new();
    for state in r.states().await? {
        interfaces.push(describe(r, state).await?);
    }
    let last = r.last_drop();
    Ok(Json(RetentionReport {
        interfaces,
        last_drop_at: last.as_ref().map(|l| l.at),
        kept: last
            .as_ref()
            .map(|l| l.kept.clone())
            .unwrap_or_default()
            .into_iter()
            .map(|k| KeptPartition {
                partition: k.partition,
                tier: k.tier,
                pending: k.pending,
                quarantined: k.quarantined,
                kept_for: k.kept_for,
            })
            .collect(),
        held: last.map(|l| l.held.into()).unwrap_or_default(),
    }))
}

/// Confirm an interface's shorter retention, which deletes its messages past it. It's applied
/// at once (or when the running change finishes). Operator.
#[utoipa::path(
    post, path = "/retention/{name}/confirm", tag = "retention",
    params(("name" = String, Path, description = "Interface name")),
    responses((status = 200, body = InterfaceRetention), (status = 404, body = super::ErrorBody), (status = 409, body = super::ErrorBody))
)]
async fn confirm(engine: Eng, caller: Caller, Path(name): Path<String>) -> ApiResult<InterfaceRetention> {
    caller.require(Role::Operator)?;
    let r = &engine.retention;
    if r.get(&name).is_none() {
        return Err(ApiError::not_found(format!("no interface named `{name}` stores messages here")));
    }
    // Confirming a shorter retention approves deleting messages: recorded before it is done.
    super::record_first(&engine, caller.audit("retention.confirm").interface(&name)).await?;
    match r.confirm(&name, caller.name()).await? {
        Some(state) => Ok(Json(describe(r, state).await?)),
        None => Err(ApiError::new(
            axum::http::StatusCode::CONFLICT,
            format!("`{name}` has no shorter retention waiting for a confirmation"),
        )),
    }
}
