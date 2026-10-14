//! The audit log: who did what, to what, from where. Reading it needs the `audit` permission
//! (admins have it); anyone signed in can read their own activity.

use super::{ApiError, Caller, Eng, Shared};
use axum::Json;
use axum::extract::Query;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use ductus_store::{AuditFilter, AuditRow};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

pub fn routes() -> OpenApiRouter<Shared> {
    OpenApiRouter::new().routes(routes!(list)).routes(routes!(my_activity))
}

const MAX_PAGE: i64 = 500;
/// An export is one request of at most this many rows; the next continues with `after`.
const MAX_EXPORT: i64 = 100_000;

#[derive(Serialize, ToSchema)]
pub struct AuditEntry {
    pub id: i64,
    pub at: DateTime<Utc>,
    /// `user`, `token`, `system` (the engine, or someone at the server's command line) or
    /// `anonymous` (a sign-in as a name nobody has).
    pub actor_kind: String,
    /// The user's or token's name when it happened.
    pub actor: String,
    /// `session` (the web UI), `token`, `cli` or `engine`; none for entries from before the log.
    pub via: Option<String>,
    /// The address the request came from (a proxy's, if there is one).
    pub ip: Option<String>,
    /// `message.payload.read`, `message.dismiss`, `user.create`, ...
    pub action: String,
    /// What it was done to: a username, a token, `interface/destination`.
    pub subject: Option<String>,
    pub interface: Option<String>,
    pub message: Option<i64>,
    /// `ok`, `denied` or `failed`.
    pub outcome: String,
    /// More about it, by action (never a password, token or payload).
    #[schema(value_type = Option<Object>)]
    pub detail: Option<serde_json::Value>,
}

impl From<AuditRow> for AuditEntry {
    fn from(r: AuditRow) -> Self {
        Self {
            id: r.id,
            at: r.at,
            actor_kind: r.actor_kind,
            actor: r.actor_name,
            via: r.via,
            ip: r.ip,
            action: r.action,
            subject: r.subject,
            interface: r.interface,
            message: r.message_id,
            outcome: r.outcome,
            detail: r.detail.and_then(|d| serde_json::from_str(&d).ok()),
        }
    }
}

#[derive(Serialize, ToSchema)]
pub struct AuditPage {
    /// Newest first.
    pub entries: Vec<AuditEntry>,
    /// Pass as `before` for the next (older) page; absent on the last page.
    pub next_before: Option<i64>,
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct AuditQuery {
    /// A user's or token's name (a user's tokens count as theirs).
    actor: Option<String>,
    /// An action (`message.dismiss`) or a group of them (`message`, `user`, `session`).
    action: Option<String>,
    interface: Option<String>,
    /// A message id.
    message: Option<i64>,
    /// From this time (inclusive).
    from: Option<DateTime<Utc>>,
    /// Until this time (exclusive).
    to: Option<DateTime<Utc>>,
    /// Entries older than this id (the previous page's `next_before`).
    before: Option<i64>,
    /// Entries newer than this id, oldest first (what an export follows).
    after: Option<i64>,
    /// `json` (default: a page, newest first) or `ndjson` (one entry per line, oldest first, up to
    /// `limit`, default 10,000, at most 100,000: continue with `after` the last id). An export is
    /// itself logged.
    format: Option<String>,
    /// Page size, at most 500 (default 100).
    limit: Option<i64>,
}

/// The audit log, filtered. Needs the `audit` permission.
#[utoipa::path(
    get, path = "/audit", tag = "audit", operation_id = "list_audit",
    params(AuditQuery),
    responses(
        (status = 200, body = AuditPage, description = "With `format=ndjson`: `application/x-ndjson`, one entry (as in `entries`) per line"),
        (status = 400, body = super::ErrorBody), (status = 403, body = super::ErrorBody)
    )
)]
async fn list(engine: Eng, caller: Caller, Query(q): Query<AuditQuery>) -> Result<Response, ApiError> {
    caller.require_permission("audit")?;
    fn non_empty(s: &Option<String>) -> Option<&str> {
        s.as_deref().map(str::trim).filter(|s| !s.is_empty())
    }
    let export = match q.format.as_deref().map(str::trim) {
        None | Some("") | Some("json") => false,
        Some("ndjson") => true,
        Some(other) => return Err(ApiError::bad_request(format!("unknown format `{other}` (json or ndjson)"))),
    };
    let limit =
        if export { q.limit.unwrap_or(10_000).clamp(1, MAX_EXPORT) } else { q.limit.unwrap_or(100).clamp(1, MAX_PAGE) };
    let filter = AuditFilter {
        actor: non_empty(&q.actor),
        action: non_empty(&q.action),
        interface: non_empty(&q.interface),
        message_id: q.message,
        from: q.from,
        to: q.to,
        before: if export { None } else { q.before },
        // An export is oldest first from the start; a page is newest first.
        after: if export { Some(q.after.unwrap_or(0)) } else { None },
        limit,
        ..AuditFilter::default()
    };
    let rows = engine.store.audit_log(&filter).await?;
    if export {
        super::record_first(
            &engine,
            caller.audit("audit.export").detail(serde_json::json!({ "rows": rows.len(), "after": q.after })),
        )
        .await?;
        let mut body = String::new();
        for entry in rows.into_iter().map(AuditEntry::from) {
            body.push_str(&serde_json::to_string(&entry).map_err(|e| anyhow::anyhow!(e))?);
            body.push('\n');
        }
        return Ok((StatusCode::OK, [(header::CONTENT_TYPE, "application/x-ndjson")], body).into_response());
    }
    let next_before = (rows.len() as i64 == limit).then(|| rows.last().map(|r| r.id)).flatten();
    Ok(Json(AuditPage { entries: rows.into_iter().map(Into::into).collect(), next_before }).into_response())
}

/// What you did yourself (and what your tokens did): the same entries, only yours. Anyone signed
/// in with a username and password.
#[utoipa::path(
    get, path = "/me/activity", tag = "me", operation_id = "my_activity",
    params(("before" = Option<i64>, Query, description = "Entries older than this id"), ("limit" = Option<i64>, Query, description = "At most 500 (default 50)")),
    responses((status = 200, body = AuditPage), (status = 403, body = super::ErrorBody))
)]
async fn my_activity(engine: Eng, caller: Caller, Query(q): Query<MyQuery>) -> Result<Json<AuditPage>, ApiError> {
    let id = caller.user_id()?;
    let limit = q.limit.unwrap_or(50).clamp(1, MAX_PAGE);
    let filter = AuditFilter { user_id: Some(id), before: q.before, limit, ..AuditFilter::default() };
    let rows = engine.store.audit_log(&filter).await?;
    let next_before = (rows.len() as i64 == limit).then(|| rows.last().map(|r| r.id)).flatten();
    Ok(Json(AuditPage { entries: rows.into_iter().map(Into::into).collect(), next_before }))
}

#[derive(Deserialize)]
struct MyQuery {
    before: Option<i64>,
    limit: Option<i64>,
}
