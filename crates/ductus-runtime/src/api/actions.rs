//! Acting on messages (operator): send a delivery again, or queue an edited copy; and the
//! quarantine across interfaces, where most of this starts. Every action is recorded with who did
//! it and what the delivery was before (`message_actions`; `GET /messages/{id}` shows them).
//!
//! **Resend** puts finished deliveries back to pending in place: the message keeps its id, so
//! its worker takes it next in its lane (ahead of what's waiting there). Without destinations
//! named, only quarantined deliveries are resent; naming a destination resends to it whatever its
//! delivery ended as (sent or filtered too: a replay). **Edit** queues a corrected copy (a new
//! message at the back of its lane) and marks the original's quarantined deliveries to the same
//! destinations replaced.

use super::{ApiError, ApiResult, Caller, Eng, Shared};
use crate::control::{Engine, Running};
use axum::Json;
use axum::extract::{Path, Query};
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use ductus_store::{Actor, DestinationId, Edited, Resend, Resent, Role, Status};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

pub fn routes() -> OpenApiRouter<Shared> {
    OpenApiRouter::new()
        .routes(routes!(resend))
        .routes(routes!(resend_many))
        .routes(routes!(edit))
        .routes(routes!(quarantine))
}

/// At most this many messages per bulk resend (a page of the quarantine screen).
const MAX_BULK: usize = 500;
const MAX_PAGE: i64 = 500;

impl Caller {
    fn actor<'a>(&'a self, reason: Option<&'a str>) -> Actor<'a> {
        Actor { token_id: Some(self.token.id), name: self.name(), reason: reason.filter(|r| !r.trim().is_empty()) }
    }
}

fn conflict(message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::CONFLICT, message)
}

/// The interface a stored message belongs to, if this engine runs it with a queue.
async fn owner(engine: &Engine, id: i64) -> Result<&Running, ApiError> {
    let (row, _) = engine.store.message(id).await?.ok_or_else(|| ApiError::not_found(format!("no message {id}")))?;
    engine
        .interface(&row.interface)
        .filter(|i| i.intake.is_some())
        .ok_or_else(|| conflict(format!("message {id} belongs to `{}`, which this engine doesn't run", row.interface)))
}

/// Destination names of `i` to their ids; unknown names are a 400 that lists the known ones.
fn destination_ids(engine: &Engine, i: &Running, names: &[String]) -> Result<Vec<DestinationId>, ApiError> {
    names
        .iter()
        .map(|n| engine.destination(&i.config.name, n).map(|(_, id)| id).map_err(ApiError::bad_request))
        .collect()
}

fn destination_name(engine: &Engine, id: DestinationId) -> Option<(&str, &str)> {
    engine.interfaces.iter().find_map(|i| {
        i.destinations.iter().find(|(_, d)| *d == id).map(|(name, _)| (i.config.name.as_str(), name.as_str()))
    })
}

/// Wake the workers of every lane that got a delivery back.
fn wake(engine: &Engine, resent: &[Resent]) {
    let lanes: HashSet<(&str, i32)> = resent.iter().map(|r| (r.interface.as_str(), r.partition_key)).collect();
    for (interface, lane) in lanes {
        if let Some(intake) = engine.interface(interface).and_then(|i| i.intake.as_ref()) {
            intake.wake(lane);
        }
    }
}

#[derive(Deserialize, ToSchema, Default)]
pub struct ResendRequest {
    /// Resend to these destinations, whatever their delivery ended as (sent and filtered too: a
    /// replay). Without it: every destination that quarantined the message.
    #[serde(default)]
    destinations: Option<Vec<String>>,
    /// Why (kept with the action).
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct ResentDelivery {
    destination: String,
    /// What the delivery was before: `sent`, `filtered` or `quarantined`.
    previous_status: &'static str,
    previous_attempts: i32,
    previous_error: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct ResentMessage {
    id: i64,
    interface: String,
    /// The deliveries now pending again.
    destinations: Vec<ResentDelivery>,
}

fn by_message(resent: Vec<Resent>) -> Vec<ResentMessage> {
    let mut messages: BTreeMap<i64, ResentMessage> = BTreeMap::new();
    for r in resent {
        messages
            .entry(r.message_id)
            .or_insert_with(|| ResentMessage { id: r.message_id, interface: r.interface.clone(), destinations: vec![] })
            .destinations
            .push(ResentDelivery {
                destination: r.destination,
                previous_status: r.previous_status.label(),
                previous_attempts: r.previous_attempts,
                previous_error: r.previous_error,
            });
    }
    messages.into_values().collect()
}

/// Send a message again: its quarantined deliveries, or those to the destinations named
/// (whatever they ended as: a replay). Each goes back to pending in place and is sent next in
/// its lane. Pending deliveries are left alone. Operator.
#[utoipa::path(
    post, path = "/messages/{id}/resend", tag = "messages",
    params(("id" = i64, Path, description = "Message id")),
    request_body(content = ResendRequest, content_type = "application/json"),
    responses(
        (status = 200, body = ResentMessage), (status = 400, body = super::ErrorBody), (status = 403, body = super::ErrorBody),
        (status = 404, body = super::ErrorBody), (status = 409, description = "Nothing to resend", body = super::ErrorBody)
    )
)]
async fn resend(
    engine: Eng,
    caller: Caller,
    Path(id): Path<i64>,
    body: Option<Json<ResendRequest>>,
) -> ApiResult<ResentMessage> {
    caller.require(Role::Operator)?;
    let request = body.map(|Json(b)| b).unwrap_or_default();
    let i = owner(&engine, id).await?;
    let (destinations, quarantined_only) = match &request.destinations {
        Some(names) if !names.is_empty() => (destination_ids(&engine, i, names)?, false),
        _ => (i.destinations.iter().map(|(_, id)| *id).collect(), true),
    };
    let r = Resend { messages: &[id], destinations: &destinations, quarantined_only };
    let resent = engine.store.resend(r, caller.actor(request.reason.as_deref())).await?;
    if resent.is_empty() {
        return Err(nothing_to_resend(&engine, id, quarantined_only).await);
    }
    wake(&engine, &resent);
    let names: Vec<&str> = resent.iter().map(|r| r.destination.as_str()).collect();
    tracing::warn!(id, interface = %i.config.name, destinations = %names.join(", "), by = caller.name(), reason = request.reason.as_deref().unwrap_or(""), "message resent");
    Ok(Json(by_message(resent).remove(0)))
}

/// Why a resend found nothing to do, from the message's deliveries.
async fn nothing_to_resend(engine: &Engine, id: i64, quarantined_only: bool) -> ApiError {
    let message = match engine.store.message(id).await {
        Ok(Some((row, _))) => row,
        Ok(None) => return ApiError::not_found(format!("no message {id}")),
        Err(e) => return e.into(),
    };
    if message.status == Status::Duplicate {
        return conflict(format!("message {id} is a duplicate: duplicates are never delivered"));
    }
    let deliveries = engine
        .store
        .deliveries(ductus_store::MessageRef { id, received_at: message.received_at })
        .await
        .unwrap_or_default();
    let states: Vec<String> = deliveries.iter().map(|d| format!("{} {}", d.destination, d.status.label())).collect();
    let hint = if quarantined_only {
        " (to send it again to a destination that has it, name the destination)"
    } else {
        " (a pending delivery is being sent or retried already)"
    };
    conflict(format!("nothing to resend for message {id}: {}{hint}", states.join(", ")))
}

#[derive(Deserialize, ToSchema)]
pub struct BulkResendRequest {
    /// Message ids (at most 500), from any interfaces.
    ids: Vec<i64>,
    /// Resend to the destination with this name in each message's interface, whatever its
    /// delivery ended as (a replay). Without it: every destination that quarantined each message.
    #[serde(default)]
    destination: Option<String>,
    /// Only deliveries still quarantined, even with `destination` named (what the quarantine
    /// screen sends: a delivery someone resolved meanwhile is skipped, not replayed). Default:
    /// true without `destination`, false with it.
    #[serde(default)]
    quarantined_only: Option<bool>,
    /// Why (kept with each action).
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct BulkResendResult {
    /// The messages sent again, by id.
    resent: Vec<ResentMessage>,
    /// Ids with nothing to resend: not found, not run by this engine, a duplicate, or no
    /// delivery that matched (pending already, or not quarantined).
    skipped: Vec<i64>,
}

/// Send many messages again at once (the quarantine screen's "resend selected"): as
/// `POST /messages/{id}/resend` for each, in one transaction. Operator.
#[utoipa::path(
    post, path = "/messages/resend", tag = "messages",
    request_body(content = BulkResendRequest, content_type = "application/json"),
    responses((status = 200, body = BulkResendResult), (status = 400, body = super::ErrorBody), (status = 403, body = super::ErrorBody))
)]
async fn resend_many(
    engine: Eng,
    caller: Caller,
    Json(request): Json<BulkResendRequest>,
) -> ApiResult<BulkResendResult> {
    caller.require(Role::Operator)?;
    if request.ids.is_empty() || request.ids.len() > MAX_BULK {
        return Err(ApiError::bad_request(format!("give 1 to {MAX_BULK} message ids (got {})", request.ids.len())));
    }
    let named = request.destination.as_deref().filter(|d| !d.is_empty());
    let destinations: Vec<DestinationId> = engine
        .interfaces
        .iter()
        .flat_map(|i| i.destinations.iter())
        .filter(|(name, _)| named.is_none_or(|n| n == name))
        .map(|(_, id)| *id)
        .collect();
    if let Some(name) = named
        && destinations.is_empty()
    {
        return Err(ApiError::bad_request(format!("no interface has a destination named `{name}`")));
    }
    let mut ids = request.ids.clone();
    ids.sort_unstable();
    ids.dedup();
    let quarantined_only = request.quarantined_only.unwrap_or(named.is_none());
    let r = Resend { messages: &ids, destinations: &destinations, quarantined_only };
    let resent = engine.store.resend(r, caller.actor(request.reason.as_deref())).await?;
    wake(&engine, &resent);
    let resent = by_message(resent);
    let done: HashSet<i64> = resent.iter().map(|m| m.id).collect();
    let skipped = ids.into_iter().filter(|id| !done.contains(id)).collect();
    tracing::warn!(
        messages = resent.len(),
        destination = named.unwrap_or("quarantined"),
        by = caller.name(),
        reason = request.reason.as_deref().unwrap_or(""),
        "messages resent"
    );
    Ok(Json(BulkResendResult { resent, skipped }))
}

#[derive(Deserialize, ToSchema)]
pub struct EditRequest {
    /// The corrected message, in full (HL7 segments separated by `\r`).
    payload: String,
    /// Where the copy goes. Without it: the destinations that quarantined the original.
    #[serde(default)]
    destinations: Option<Vec<String>>,
    /// Why (kept with the action).
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct EditResult {
    /// The copy's id (a new message, queued at the back of its lane).
    id: i64,
    /// The message it corrects.
    original: i64,
    interface: String,
    /// Where the copy goes.
    destinations: Vec<String>,
    /// Destinations whose quarantined delivery of the original is now filtered ("replaced by
    /// edited copy N").
    replaced: Vec<String>,
}

/// Queue a corrected copy of a message. The original stays as it was received; its quarantined
/// deliveries to the copy's destinations are marked replaced. The copy is a new message (new
/// id, so a new `meta.id`), never deduplicated, with the original's `origin`. Refused for a
/// destination where the original is still pending (it may be being sent right now). Operator.
#[utoipa::path(
    post, path = "/messages/{id}/edit", tag = "messages",
    params(("id" = i64, Path, description = "The message to correct")),
    request_body(content = EditRequest, content_type = "application/json"),
    responses(
        (status = 201, body = EditResult), (status = 400, body = super::ErrorBody), (status = 403, body = super::ErrorBody),
        (status = 404, body = super::ErrorBody), (status = 409, description = "Still pending for a destination", body = super::ErrorBody)
    )
)]
async fn edit(
    engine: Eng,
    caller: Caller,
    Path(id): Path<i64>,
    Json(request): Json<EditRequest>,
) -> Result<(StatusCode, Json<EditResult>), ApiError> {
    caller.require(Role::Operator)?;
    if request.payload.trim().is_empty() {
        return Err(ApiError::bad_request("the edited message is empty"));
    }
    let i = owner(&engine, id).await?;
    let names = match request.destinations.filter(|d| !d.is_empty()) {
        Some(names) => names,
        None => {
            let (row, _) =
                engine.store.message(id).await?.ok_or_else(|| ApiError::not_found(format!("no message {id}")))?;
            let reference = ductus_store::MessageRef { id, received_at: row.received_at };
            let deliveries = engine.store.deliveries(reference).await?;
            let quarantined: Vec<String> = deliveries
                .into_iter()
                .filter(|d| d.status == Status::Quarantined && i.destinations.iter().any(|(n, _)| *n == d.destination))
                .map(|d| d.destination)
                .collect();
            if quarantined.is_empty() {
                return Err(ApiError::bad_request(format!(
                    "message {id} has no quarantined deliveries: name the destinations the copy should go to"
                )));
            }
            quarantined
        }
    };
    let destinations = destination_ids(&engine, i, &names)?;
    let intake = i.intake.as_ref().expect("owner() checked");
    let by = caller.actor(request.reason.as_deref());
    let name = |d: &DestinationId| destination_name(&engine, *d).map(|(_, n)| n.to_string()).unwrap_or_default();
    match intake.copy_edited(id, request.payload.as_bytes(), &destinations, by).await? {
        Edited::NotFound => Err(ApiError::not_found(format!("no message {id}"))),
        Edited::StillPending(pending) => {
            let pending: Vec<String> = pending.iter().map(name).collect();
            Err(conflict(format!(
                "message {id} is still pending for {}: it may be being sent or retried right now; wait until it's quarantined or sent (pausing the destination doesn't stop a send under way)",
                pending.join(", ")
            )))
        }
        Edited::Copied { copy, replaced } => {
            let replaced: Vec<String> = replaced.iter().map(name).collect();
            tracing::warn!(id, copy = copy.id, interface = %i.config.name, destinations = %names.join(", "), by = caller.name(), reason = request.reason.as_deref().unwrap_or(""), "edited copy queued");
            let result = EditResult {
                id: copy.id,
                original: id,
                interface: i.config.name.clone(),
                destinations: names,
                replaced,
            };
            Ok((StatusCode::CREATED, Json(result)))
        }
    }
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct QuarantineQuery {
    /// Only this interface's.
    interface: Option<String>,
    /// Only this destination's (with `interface`, that interface's; without, any interface's
    /// destination of that name).
    destination: Option<String>,
    /// The previous page's `next_before`.
    before: Option<String>,
    /// Page size, at most 500 (default 100).
    limit: Option<i64>,
}

#[derive(Serialize, ToSchema)]
pub struct QuarantinedDelivery {
    /// The message's id.
    id: i64,
    received_at: DateTime<Utc>,
    interface: String,
    destination: String,
    attempts: i32,
    /// Why.
    last_error: Option<String>,
    quarantined_at: DateTime<Utc>,
    ordering_key: Option<String>,
    /// MSH-10 for HL7.
    control_id: Option<String>,
    /// Payload size in bytes.
    size: i32,
}

#[derive(Serialize, ToSchema)]
pub struct QuarantinePage {
    /// Newest message first; a message quarantined by two destinations is listed twice.
    deliveries: Vec<QuarantinedDelivery>,
    /// Pass as `before` for the next page; absent on the last page.
    next_before: Option<String>,
}

/// Quarantined deliveries across this engine's interfaces (or one interface or destination),
/// newest message first: what needs a person. Counts per destination are in `GET /interfaces`.
#[utoipa::path(
    get, path = "/quarantine", tag = "messages",
    params(QuarantineQuery),
    responses((status = 200, body = QuarantinePage), (status = 400, body = super::ErrorBody), (status = 404, body = super::ErrorBody))
)]
async fn quarantine(engine: Eng, _caller: Caller, Query(q): Query<QuarantineQuery>) -> ApiResult<QuarantinePage> {
    let interface = q.interface.as_deref().filter(|s| !s.is_empty());
    let destination = q.destination.as_deref().filter(|s| !s.is_empty());
    if let Some(name) = interface
        && engine.interface(name).is_none()
    {
        return Err(ApiError::not_found(format!("no interface named `{name}`")));
    }
    let destinations: Vec<DestinationId> = engine
        .interfaces
        .iter()
        .filter(|i| interface.is_none_or(|n| n == i.config.name))
        .flat_map(|i| i.destinations.iter())
        .filter(|(name, _)| destination.is_none_or(|d| d == name))
        .map(|(_, id)| *id)
        .collect();
    if let Some(name) = destination
        && destinations.is_empty()
    {
        return Err(ApiError::not_found(format!("no destination named `{name}`")));
    }
    let before = match q.before.as_deref().filter(|s| !s.is_empty()) {
        None => None,
        Some(cursor) => Some(parse_cursor(cursor).ok_or_else(|| {
            ApiError::bad_request(format!("`before` must be a page's `next_before` (got `{cursor}`)"))
        })?),
    };
    let limit = q.limit.unwrap_or(100).clamp(1, MAX_PAGE);
    let rows = engine.store.quarantined(&destinations, before, limit).await?;
    let next_before = (rows.len() as i64 == limit)
        .then(|| rows.last().map(|r| format!("{}.{}", r.message_id, r.destination_id)))
        .flatten();
    let deliveries = rows
        .into_iter()
        .map(|r| QuarantinedDelivery {
            id: r.message_id,
            received_at: r.received_at,
            destination: destination_name(&engine, r.destination_id).map(|(_, n)| n.to_string()).unwrap_or_default(),
            interface: r.interface,
            attempts: r.attempts,
            last_error: r.last_error,
            quarantined_at: r.quarantined_at,
            ordering_key: r.ordering_key,
            control_id: r.control_id,
            size: r.size,
        })
        .collect();
    Ok(Json(QuarantinePage { deliveries, next_before }))
}

/// `next_before`: `<message id>.<destination id>`.
fn parse_cursor(cursor: &str) -> Option<(i64, DestinationId)> {
    let (message, destination) = cursor.split_once('.')?;
    Some((message.parse().ok()?, destination.parse().ok()?))
}

#[cfg(test)]
mod tests {
    #[test]
    fn cursors_round_trip_and_reject_junk() {
        assert_eq!(super::parse_cursor("1234.7"), Some((1234, 7)));
        assert_eq!(super::parse_cursor("1234"), None);
        assert_eq!(super::parse_cursor("a.b"), None);
    }
}
