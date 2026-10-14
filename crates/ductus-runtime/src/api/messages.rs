//! The message browser: an interface's messages page by page, and one message with its
//! payload and what each destination did with it.

use super::{ApiError, ApiResult, Caller, Eng, Shared};
use axum::Json;
use axum::extract::{Path, Query};
use chrono::{DateTime, Utc};
use ductus_store::{Browse, MessageRef, MessageRow, Status};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

pub fn routes() -> OpenApiRouter<Shared> {
    OpenApiRouter::new().routes(routes!(list)).routes(routes!(message))
}

const MAX_PAGE: i64 = 500;

#[derive(Serialize, ToSchema)]
pub struct MessageSummary {
    id: i64,
    received_at: DateTime<Utc>,
    interface: String,
    /// `pending`, `sent`, `filtered`, `quarantined` or `duplicate`: the overall status (pending
    /// until every destination is done).
    status: &'static str,
    ordering_key: Option<String>,
    /// MSH-10 for HL7.
    control_id: Option<String>,
    /// Payload size in bytes.
    size: i32,
    /// Why it was quarantined or filtered (per destination when there are several).
    last_error: Option<String>,
    updated_at: DateTime<Utc>,
    /// Routed from another interface: the id of the message there (`GET /messages/{origin}`).
    origin: Option<i64>,
}

impl From<MessageRow> for MessageSummary {
    fn from(r: MessageRow) -> Self {
        Self {
            id: r.id,
            received_at: r.received_at,
            interface: r.interface,
            status: r.status.label(),
            ordering_key: r.ordering_key,
            control_id: r.control_id,
            size: r.size,
            last_error: r.last_error,
            updated_at: r.updated_at,
            origin: r.origin_id,
        }
    }
}

#[derive(Serialize, ToSchema)]
pub struct MessagePage {
    /// Newest first.
    messages: Vec<MessageSummary>,
    /// Pass as `before` for the next (older) page; absent on the last page.
    next_before: Option<i64>,
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListQuery {
    /// Only messages with this overall status.
    status: Option<String>,
    /// Only messages older than this id (the previous page's `next_before`).
    before: Option<i64>,
    /// Page size, at most 500 (default 50).
    limit: Option<i64>,
}

/// An interface's messages, newest first, a page at a time.
#[utoipa::path(
    get, path = "/interfaces/{name}/messages", tag = "messages",
    params(("name" = String, Path, description = "Interface name"), ListQuery),
    responses((status = 200, body = MessagePage), (status = 400, body = super::ErrorBody), (status = 404, body = super::ErrorBody))
)]
async fn list(
    engine: Eng,
    _caller: Caller,
    Path(name): Path<String>,
    Query(q): Query<ListQuery>,
) -> ApiResult<MessagePage> {
    if engine.interface(&name).is_none() {
        return Err(ApiError::not_found(format!("no interface named `{name}`")));
    }
    let status = match q.status.as_deref() {
        None | Some("") => None,
        Some(s) => Some(Status::parse(s).ok_or_else(|| {
            let known: Vec<_> = Status::ALL.iter().map(|s| s.label()).collect();
            ApiError::bad_request(format!("unknown status `{s}` (one of {})", known.join(", ")))
        })?),
    };
    let limit = q.limit.unwrap_or(50).clamp(1, MAX_PAGE);
    let rows = engine.store.browse(&Browse { interface: &name, status, before: q.before, limit }).await?;
    let next_before = (rows.len() as i64 == limit).then(|| rows.last().map(|r| r.id)).flatten();
    Ok(Json(MessagePage { messages: rows.into_iter().map(Into::into).collect(), next_before }))
}

#[derive(Serialize, ToSchema)]
pub struct DeliveryInfo {
    destination: String,
    /// `pending`, `sent`, `filtered` or `quarantined`.
    status: &'static str,
    attempts: i32,
    last_error: Option<String>,
    /// What was sent, for destinations with `save_output: true`.
    output: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct MessageDetail {
    #[serde(flatten)]
    message: MessageSummary,
    /// The message as received (bytes that aren't UTF-8 are replaced). Absent when the caller
    /// doesn't have the `payloads` permission (`payload_hidden` is then true), and so are the
    /// deliveries' `output`s.
    payload: Option<String>,
    /// The payload (and saved outputs) were left out: the caller may not read them.
    payload_hidden: bool,
    deliveries: Vec<DeliveryInfo>,
    /// What people did to it (resends, edits), oldest first.
    actions: Vec<ActionInfo>,
    /// An edited copy: the message it corrects.
    copy_of: Option<i64>,
}

#[derive(Serialize, ToSchema)]
pub struct ActionInfo {
    at: DateTime<Utc>,
    /// `resend` (the delivery went back to pending), `edit` (a corrected copy was queued) or
    /// `dismiss` (a quarantined delivery was closed without sending).
    action: String,
    /// The user or token that did it.
    by: String,
    reason: Option<String>,
    destination: String,
    /// The delivery before: `sent`, `filtered` or `quarantined` (none: there was none).
    previous_status: Option<&'static str>,
    previous_attempts: Option<i32>,
    previous_error: Option<String>,
    /// `edit`: the copy's id.
    copy: Option<i64>,
}

/// One message: what was received, and what each destination did with it.
#[utoipa::path(
    get, path = "/messages/{id}", tag = "messages",
    params(("id" = i64, Path, description = "Message id")),
    responses((status = 200, body = MessageDetail), (status = 404, body = super::ErrorBody))
)]
async fn message(engine: Eng, caller: Caller, Path(id): Path<i64>) -> ApiResult<MessageDetail> {
    let (row, payload) =
        engine.store.message(id).await?.ok_or_else(|| ApiError::not_found(format!("no message {id}")))?;
    // Payloads hold patient data: only callers with the `payloads` permission get them, and the
    // read is in the audit log before they are returned: if it can't be written, nothing is
    // returned (an unlogged read of patient data is worse than a refused one).
    let may_read = caller.who.can("payloads");
    if may_read && engine.audit.payload_reads {
        let audit = caller.audit("message.payload.read").interface(&row.interface).message(id);
        engine.store.record_payload_read(&audit, engine.audit.payload_read_window).await.map_err(|e| {
            tracing::error!(id, error = %format!("{e:#}"), "audit log write failed: refusing to return a payload");
            ApiError::new(
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "the audit log can't be written, so the payload is not shown: check the database, then try again",
            )
        })?;
    }
    let deliveries = engine
        .store
        .deliveries(MessageRef { id: row.id, received_at: row.received_at })
        .await?
        .into_iter()
        .map(|d| DeliveryInfo {
            destination: d.destination,
            status: d.status.label(),
            attempts: d.attempts,
            last_error: d.last_error,
            output: d.output.filter(|_| may_read).map(|o| String::from_utf8_lossy(&o).into_owned()),
        })
        .collect();
    let (made_by, done_to): (Vec<_>, Vec<_>) =
        engine.store.actions(id).await?.into_iter().partition(|a| a.copy_id == Some(id));
    let actions = done_to
        .into_iter()
        .map(|a| ActionInfo {
            at: a.at,
            action: a.action,
            by: a.actor,
            reason: a.reason,
            destination: a.destination,
            previous_status: a.previous_status.and_then(Status::from_i16).map(Status::label),
            previous_attempts: a.previous_attempts,
            previous_error: a.previous_error,
            copy: a.copy_id,
        })
        .collect();
    let copy_of = made_by.first().map(|a| a.message_id);
    let payload = may_read.then(|| String::from_utf8_lossy(&payload).into_owned());
    Ok(Json(MessageDetail { message: row.into(), payload, payload_hidden: !may_read, deliveries, actions, copy_of }))
}
