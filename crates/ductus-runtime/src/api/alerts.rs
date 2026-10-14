//! Alerts: what is open, what happened, and acknowledging or silencing one; the channels they go
//! to, and a test notification. Reading is for anyone signed in; acting is for operators (the
//! configuration itself is ductus.yaml, so there is nothing for admins only).

use super::{ApiError, ApiResult, Caller, Eng, Shared};
use axum::Json;
use axum::extract::Path;
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use ductus_core::alerts::OWNER;
use ductus_store::{AlertEventRow, AlertRow, Role};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

pub fn routes() -> OpenApiRouter<Shared> {
    OpenApiRouter::new()
        .routes(routes!(list))
        .routes(routes!(channels))
        .routes(routes!(test_channel))
        .routes(routes!(one))
        .routes(routes!(acknowledge))
        .routes(routes!(silence, unsilence))
}

/// The longest silence: past it, people forget why.
const MAX_SILENCE: std::time::Duration = std::time::Duration::from_secs(30 * 86_400);

#[derive(Serialize, ToSchema)]
pub struct AlertInfo {
    pub id: i32,
    /// `iface:<interface>` or `dest:<interface>/<destination>`.
    pub key: String,
    pub interface: String,
    pub destination: Option<String>,
    /// `ok`, or what is wrong: `error`, `stalled`, `behind` (a destination), `stopped`, `silent`,
    /// `missed` (an interface).
    pub state: String,
    /// `warning` or `critical` while open.
    pub severity: Option<String>,
    /// Bad since.
    pub since: Option<DateTime<Utc>>,
    /// Fine again since then, but not for `alerts.settle` yet (it may still reopen).
    pub recovering_since: Option<DateTime<Utc>>,
    /// The health view's words, as of the last check.
    pub summary: Option<String>,
    pub acknowledged_by: Option<String>,
    pub acknowledged_at: Option<DateTime<Utc>>,
    /// Set while silenced: nothing is sent until then.
    pub silenced_until: Option<DateTime<Utc>>,
    pub silenced_by: Option<String>,
    pub silence_reason: Option<String>,
    /// Notifications sent since it fired.
    pub notifications: i32,
    /// When people were last told (or will be, once quiet hours end).
    pub last_notified_at: Option<DateTime<Utc>>,
    /// Its interface says `alert: { mute: true }`: nothing is ever sent about it.
    pub muted: bool,
}

impl AlertInfo {
    fn new(r: AlertRow, muted: bool) -> Self {
        let silenced = r.is_silenced(Utc::now());
        Self {
            id: r.id,
            key: r.key,
            interface: r.interface,
            destination: r.destination,
            state: r.state,
            severity: r.severity,
            since: r.since,
            recovering_since: r.ok_since,
            summary: r.summary,
            acknowledged_by: r.ack_by,
            acknowledged_at: r.ack_at,
            silenced_until: r.silenced_until.filter(|_| silenced),
            silenced_by: r.silenced_by.filter(|_| silenced),
            silence_reason: r.silence_reason.filter(|_| silenced),
            notifications: r.notify_count,
            last_notified_at: r.last_notified_at,
            muted,
        }
    }
}

#[derive(Serialize, ToSchema)]
pub struct AlertEvent {
    pub id: i64,
    pub at: DateTime<Utc>,
    pub alert: i32,
    pub key: String,
    /// `fired`, `escalated`, `reminded`, `recovered`, `acknowledged`, `silenced`, `unsilenced`.
    pub kind: String,
    /// The state then (for `recovered`: what it recovered from).
    pub state: Option<String>,
    pub severity: Option<String>,
    pub summary: Option<String>,
    /// Who acknowledged or silenced it.
    pub by: Option<String>,
    /// Its notifications: sent, still to send (waiting for quiet hours to end, or being retried),
    /// and given up on.
    pub sent: i64,
    pub waiting: i64,
    pub failed: i64,
}

impl From<AlertEventRow> for AlertEvent {
    fn from(e: AlertEventRow) -> Self {
        Self {
            id: e.id,
            at: e.at,
            alert: e.alert_id,
            key: e.key,
            kind: e.kind,
            state: e.state,
            severity: e.severity,
            summary: e.summary,
            by: e.by_name,
            sent: e.sent,
            waiting: e.waiting,
            failed: e.failed,
        }
    }
}

#[derive(Serialize, ToSchema)]
pub struct AlertsOverview {
    /// `alerts:` is in ductus.yaml. Without it nothing is judged or sent.
    pub configured: bool,
    /// Every interface and destination, open alerts first (critical, then oldest).
    pub alerts: Vec<AlertInfo>,
    /// The latest events, newest first.
    pub recent: Vec<AlertEvent>,
}

/// An alert of this engine's interfaces (the database may hold others': another engine's, or
/// interfaces no longer configured).
pub(super) fn ours(engine: &Shared, row: &AlertRow) -> bool {
    engine.interface(&row.interface).is_some_and(|i| match &row.destination {
        Some(d) => i.destinations.iter().any(|(name, _)| name == d),
        None => true,
    })
}

fn muted(engine: &Shared, interface: &str) -> bool {
    engine.interface(interface).is_some_and(|i| i.config.alert.mute)
}

fn info(engine: &Shared, row: AlertRow) -> AlertInfo {
    let muted = muted(engine, &row.interface);
    AlertInfo::new(row, muted)
}

/// Alerts: open ones first, and what happened lately.
#[utoipa::path(get, path = "/alerts", tag = "alerts", operation_id = "list_alerts", responses((status = 200, body = AlertsOverview)))]
async fn list(engine: Eng, _caller: Caller) -> ApiResult<AlertsOverview> {
    let configured = engine.alerts.is_some();
    let (alerts, recent) = if configured {
        let rows = engine.store.alerts().await?;
        let events = engine.store.alert_events(None, 200).await?;
        let rows: Vec<_> = rows.into_iter().filter(|r| ours(&engine, r)).collect();
        let ids: std::collections::HashSet<i32> = rows.iter().map(|r| r.id).collect();
        let events = events.into_iter().filter(|e| ids.contains(&e.alert_id)).take(50);
        (rows.into_iter().map(|r| info(&engine, r)).collect(), events.map(Into::into).collect())
    } else {
        (Vec::new(), Vec::new())
    };
    Ok(Json(AlertsOverview { configured, alerts, recent }))
}

#[derive(Serialize, ToSchema)]
pub struct AlertDetail {
    pub alert: AlertInfo,
    /// Newest first (the last 100).
    pub events: Vec<AlertEvent>,
}

/// One alert and its history.
#[utoipa::path(
    get, path = "/alerts/{id}", tag = "alerts", operation_id = "get_alert",
    params(("id" = i32, Path, description = "Alert id")),
    responses((status = 200, body = AlertDetail), (status = 404, body = super::ErrorBody))
)]
async fn one(engine: Eng, _caller: Caller, Path(id): Path<i32>) -> ApiResult<AlertDetail> {
    let row = engine.store.alert(id).await?.ok_or_else(|| ApiError::not_found(format!("no alert {id}")))?;
    let events = engine.store.alert_events(Some(id), 100).await?;
    Ok(Json(AlertDetail { alert: info(&engine, row), events: events.into_iter().map(Into::into).collect() }))
}

fn configured(engine: &Shared) -> Result<&std::sync::Arc<crate::alerts::Alerts>, ApiError> {
    engine.alerts.as_ref().ok_or_else(|| ApiError::not_found("alerts aren't set up (`alerts:` in ductus.yaml)"))
}

/// Acknowledge an open alert: someone is on it. No reminders until it gets worse or recovers.
/// Operator.
#[utoipa::path(
    post, path = "/alerts/{id}/ack", tag = "alerts",
    params(("id" = i32, Path, description = "Alert id")),
    responses((status = 200, body = AlertInfo), (status = 403, body = super::ErrorBody), (status = 404, body = super::ErrorBody), (status = 409, body = super::ErrorBody))
)]
async fn acknowledge(engine: Eng, caller: Caller, Path(id): Path<i32>) -> ApiResult<AlertInfo> {
    caller.require(Role::Operator)?;
    configured(&engine)?;
    let row = engine.store.alert(id).await?.ok_or_else(|| ApiError::not_found(format!("no alert {id}")))?;
    if !row.is_open() {
        return Err(ApiError::new(StatusCode::CONFLICT, format!("{} is fine: nothing to acknowledge", row.key)));
    }
    let audit = caller.audit("alert.ack").detail(serde_json::json!({ "state": row.state, "severity": row.severity }));
    let row = engine
        .store
        .acknowledge_alert(id, caller.name(), &audit)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::CONFLICT, "it recovered meanwhile: nothing to acknowledge"))?;
    tracing::info!(alert = %row.key, by = caller.name(), "alert acknowledged");
    Ok(Json(info(&engine, row)))
}

#[derive(Deserialize, ToSchema)]
pub struct SilenceRequest {
    /// How long: `30m`, `4h`, `2d` (at most 30 days).
    #[serde(rename = "for")]
    pub duration: String,
    /// Why (planned work, a known outage): shown to everyone who sees the alert.
    pub reason: Option<String>,
}

/// Silence an alert: nothing is sent about it (fired, worse, reminders, recovery) until the time
/// is up. It still shows. Works before anything fires (planned work). A silence is not a pause:
/// messages still wait. Operator.
#[utoipa::path(
    post, path = "/alerts/{id}/silence", tag = "alerts",
    params(("id" = i32, Path, description = "Alert id")),
    request_body(content = SilenceRequest, content_type = "application/json"),
    responses((status = 200, body = AlertInfo), (status = 400, body = super::ErrorBody), (status = 403, body = super::ErrorBody), (status = 404, body = super::ErrorBody))
)]
async fn silence(
    engine: Eng,
    caller: Caller,
    Path(id): Path<i32>,
    Json(body): Json<SilenceRequest>,
) -> ApiResult<AlertInfo> {
    caller.require(Role::Operator)?;
    configured(&engine)?;
    let duration =
        ductus_core::config::parse_duration(&body.duration).map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
    if duration.is_zero() || duration > MAX_SILENCE {
        return Err(ApiError::bad_request("silence for more than nothing and at most 30d"));
    }
    let until = Utc::now() + chrono::Duration::from_std(duration).map_err(|e| anyhow::anyhow!(e))?;
    let reason = body.reason.as_deref().map(str::trim).filter(|r| !r.is_empty());
    let audit = caller.audit("alert.silence");
    let row = engine
        .store
        .silence_alert(id, Some(until), caller.name(), reason, &audit)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("no alert {id}")))?;
    tracing::info!(alert = %row.key, by = caller.name(), %until, reason = reason.unwrap_or(""), "alert silenced");
    Ok(Json(info(&engine, row)))
}

/// End a silence early. Operator.
#[utoipa::path(
    delete, path = "/alerts/{id}/silence", tag = "alerts",
    params(("id" = i32, Path, description = "Alert id")),
    responses((status = 200, body = AlertInfo), (status = 403, body = super::ErrorBody), (status = 404, body = super::ErrorBody))
)]
async fn unsilence(engine: Eng, caller: Caller, Path(id): Path<i32>) -> ApiResult<AlertInfo> {
    caller.require(Role::Operator)?;
    configured(&engine)?;
    let audit = caller.audit("alert.unsilence");
    let row = engine
        .store
        .silence_alert(id, None, caller.name(), None, &audit)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("no alert {id}")))?;
    Ok(Json(info(&engine, row)))
}

#[derive(Serialize, ToSchema)]
pub struct SmtpInfo {
    pub host: String,
    pub port: u16,
    /// `starttls`, `tls` or `none`.
    pub tls: String,
    pub from: String,
    pub username: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct ChannelInfo {
    pub name: String,
    pub email: Vec<String>,
    /// Scheme and host only: the rest of a webhook URL is often its secret.
    pub webhook: Option<String>,
    /// `json`, `slack` or `teams`.
    pub webhook_format: Option<String>,
    /// Notifications in the last 7 days: sent, still to send, given up on.
    pub sent: i64,
    pub waiting: i64,
    pub failed: i64,
    pub last_sent: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct OwnerReach {
    pub owner: String,
    /// Where alerts for this owner go; empty: nowhere (no user with that name and an email, not
    /// in `owners:`, not an address).
    pub addresses: Vec<String>,
}

#[derive(Serialize, ToSchema)]
pub struct ChannelsOverview {
    pub configured: bool,
    pub smtp: Option<SmtpInfo>,
    pub channels: Vec<ChannelInfo>,
    /// Owners named in interface.yaml, when a rule sends to `owner`.
    pub owners: Vec<OwnerReach>,
    /// Sent to owners in the last 7 days (each owner address is a channel of its own).
    pub owner_notifications: Option<ChannelCounts>,
    /// `"22:00-06:00"` and which severities wait, if set.
    pub quiet: Option<String>,
    /// Reminders: `4h`, or none.
    pub repeat: Option<String>,
    /// The heartbeat URL (scheme and host) and how often.
    pub heartbeat: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct ChannelCounts {
    pub sent: i64,
    pub waiting: i64,
    pub failed: i64,
    pub last_error: Option<String>,
}

/// Where alerts go, and how sending has gone lately. Secrets are never shown. Operator.
#[utoipa::path(
    get, path = "/alerts/channels", tag = "alerts", operation_id = "alert_channels",
    responses((status = 200, body = ChannelsOverview), (status = 403, body = super::ErrorBody))
)]
async fn channels(engine: Eng, caller: Caller) -> ApiResult<ChannelsOverview> {
    caller.require(Role::Operator)?;
    let Some(a) = &engine.alerts else {
        return Ok(Json(ChannelsOverview {
            configured: false,
            smtp: None,
            channels: Vec::new(),
            owners: Vec::new(),
            owner_notifications: None,
            quiet: None,
            repeat: None,
            heartbeat: None,
        }));
    };
    let counts = engine.store.outbox_counts(7).await?;
    let count = |name: &str| {
        let rows: Vec<_> =
            counts.iter().filter(|c| c.channel == name || c.channel.starts_with(&format!("{name}/"))).collect();
        let last_error = rows.iter().find_map(|c| c.last_error.clone());
        let last_sent = rows.iter().filter_map(|c| c.last_sent).max();
        (
            rows.iter().map(|c| c.sent).sum(),
            rows.iter().map(|c| c.waiting).sum(),
            rows.iter().map(|c| c.failed).sum(),
            last_sent,
            last_error,
        )
    };
    let channels = a
        .settings
        .channels
        .iter()
        .map(|(name, c)| {
            let (sent, waiting, failed, last_sent, last_error) = count(name);
            ChannelInfo {
                name: name.clone(),
                email: c.email.clone(),
                webhook: c.webhook.as_ref().map(|w| w.masked_url()),
                webhook_format: c.webhook.as_ref().map(|w| w.format.label().to_string()),
                sent,
                waiting,
                failed,
                last_sent,
                last_error,
            }
        })
        .collect();
    let to_owner = a.settings.rules.iter().any(|r| r.to.iter().any(|t| t == OWNER))
        || engine.interfaces.iter().any(|i| i.config.alert.to.iter().any(|t| t == OWNER));
    let mut owners = Vec::new();
    let mut owner_notifications = None;
    if to_owner {
        let users = engine.store.users().await?;
        let people: Vec<_> = users
            .iter()
            .filter(|u| u.disabled_at.is_none())
            .map(|u| ductus_core::alerts::Person {
                username: &u.username,
                display_name: Some(u.display_name.as_str()).filter(|s| !s.is_empty()),
                email: Some(u.email.as_str()).filter(|s| !s.is_empty()),
            })
            .collect();
        let configs = engine.interfaces.iter().map(|i| i.config.as_ref());
        for (owner, _) in ductus_core::owners::by_owner(ductus_core::owners::owned(configs)) {
            if owner.is_empty() {
                continue;
            }
            let addresses = ductus_core::alerts::owner_addresses(&owner, &people, &a.owners).unwrap_or_default();
            owners.push(OwnerReach { owner, addresses });
        }
        let rows: Vec<_> = counts.iter().filter(|c| c.channel.starts_with("email:")).collect();
        owner_notifications = Some(ChannelCounts {
            sent: rows.iter().map(|c| c.sent).sum(),
            waiting: rows.iter().map(|c| c.waiting).sum(),
            failed: rows.iter().map(|c| c.failed).sum(),
            last_error: rows.iter().find_map(|c| c.last_error.clone()),
        });
    }
    let quiet = a.settings.quiet.as_ref().map(|q| {
        let severities: Vec<&str> = q.severities.iter().map(|s| s.label()).collect();
        format!("{} ({} wait)", q.between, severities.join(", "))
    });
    let human = |d: std::time::Duration| ductus_core::monitor::human(d);
    Ok(Json(ChannelsOverview {
        configured: true,
        smtp: a.settings.smtp.as_ref().map(|s| SmtpInfo {
            host: s.host.clone(),
            port: s.port(),
            tls: format!("{:?}", s.tls).to_lowercase(),
            from: s.from.clone(),
            username: s.username.clone(),
        }),
        channels,
        owners,
        owner_notifications,
        quiet,
        repeat: (!a.settings.repeat.is_zero()).then(|| human(a.settings.repeat)),
        heartbeat: a.settings.heartbeat.as_ref().map(|h| {
            let host = url::Url::parse(&h.url).ok().and_then(|u| Some(format!("{}://{}/…", u.scheme(), u.host_str()?)));
            format!("{} every {}", host.unwrap_or_default(), human(h.every))
        }),
    }))
}

#[derive(Serialize, ToSchema)]
pub struct TestResult {
    pub channel: String,
    pub sent: bool,
}

/// Send a test notification to a channel now (it says it is a test). Answers 502 with the reason
/// when it can't be sent. Operator; logged.
#[utoipa::path(
    post, path = "/alerts/channels/{name}/test", tag = "alerts",
    params(("name" = String, Path, description = "Channel name")),
    responses((status = 200, body = TestResult), (status = 403, body = super::ErrorBody), (status = 404, body = super::ErrorBody), (status = 502, body = super::ErrorBody))
)]
async fn test_channel(engine: Eng, caller: Caller, Path(name): Path<String>) -> ApiResult<TestResult> {
    caller.require(Role::Operator)?;
    let a = configured(&engine)?.clone();
    if !a.settings.channels.contains_key(&name) {
        return Err(ApiError::not_found(format!("no channel named `{name}`")));
    }
    super::record_first(&engine, caller.audit("alert.test").subject(&name)).await?;
    match a.test(&name).await {
        Ok(()) => Ok(Json(TestResult { channel: name, sent: true })),
        Err(e) => Err(ApiError::new(StatusCode::BAD_GATEWAY, format!("{e:#}"))),
    }
}
