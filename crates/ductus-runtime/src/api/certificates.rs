//! Certificates the engine connects to (as its checks last saw them), and the checks themselves
//! (the engine's scheduled ones and those someone asked for, the last 30).

use super::{ApiError, ApiResult, Caller, Eng, Shared};
use axum::Json;
use axum::extract::Path;
use chrono::{DateTime, Utc};
use ductus_core::alerts::{self, CertificateSettings};
use ductus_store::{DoctorRunRow, Role};
use serde::Serialize;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

pub fn routes() -> OpenApiRouter<Shared> {
    OpenApiRouter::new().routes(routes!(list)).routes(routes!(runs)).routes(routes!(run))
}

#[derive(Serialize, ToSchema)]
pub struct CertificateInfo {
    /// `empi` (resources.yaml), `alerts.smtp`, `alerts.<channel>`, `alerts.heartbeat`, or
    /// `<interface>/<destination>`.
    pub resource: String,
    pub host: String,
    pub port: u16,
    /// `ok`, `expiring` (within the first of `alerts.certificates.warn_days`), `expired`, or
    /// `unknown` (never read: not checked yet, or never reachable).
    pub state: String,
    /// Whole days left (UTC dates), counted to the earliest expiry in the chain; negative once
    /// expired.
    pub days_left: Option<i64>,
    /// The step of the countdown it is within (a warn day; 0 once expired).
    pub stage: Option<i32>,
    pub subject: Option<String>,
    pub issuer: Option<String>,
    /// SHA-256 of the server certificate, hex.
    pub fingerprint: Option<String>,
    pub not_before: Option<DateTime<Utc>>,
    /// The server certificate's own expiry.
    pub not_after: Option<DateTime<Utc>>,
    /// The earliest expiry in its chain (what counts).
    pub chain_not_after: Option<DateTime<Utc>>,
    /// Whose that is, when it isn't the server certificate's (an intermediate).
    pub chain_earliest: Option<String>,
    /// This server trusts it.
    pub trusted: Option<bool>,
    /// Why the last check failed, if it did.
    pub error: Option<String>,
    /// The last check.
    pub checked_at: Option<DateTime<Utc>>,
    /// When the certificate shown was read (older than `checked_at` when the last check couldn't).
    pub read_at: Option<DateTime<Utc>>,
    /// Its alert (`/alerts/{id}`), when alerts are set up.
    pub alert: Option<i32>,
}

#[derive(Serialize, ToSchema)]
pub struct DoctorRun {
    pub id: i64,
    pub at: DateTime<Utc>,
    /// `certificates` or `full`.
    pub mode: String,
    /// `schedule`, or who asked.
    pub started_by: String,
    pub seconds: f64,
    pub checks: i32,
    pub failed: i32,
}

impl From<DoctorRunRow> for DoctorRun {
    fn from(r: DoctorRunRow) -> Self {
        Self {
            id: r.id,
            at: r.at,
            mode: r.mode,
            started_by: r.started_by,
            seconds: r.seconds,
            checks: r.checks,
            failed: r.failed,
        }
    }
}

#[derive(Serialize, ToSchema)]
pub struct CertificatesOverview {
    /// Earliest expiry first; never read ones last.
    pub certificates: Vec<CertificateInfo>,
    /// The steps of the countdown (`alerts.certificates.warn_days`).
    pub warn_days: Vec<u32>,
    /// How often the engine checks by itself, in seconds; none when `doctor.schedule: off`.
    pub schedule_seconds: Option<u64>,
    /// What those checks cover: `certificates` or `full`.
    pub mode: String,
    /// The last check the engine ran by itself.
    pub last_scheduled: Option<DoctorRun>,
    /// The last check of any kind.
    pub last_run: Option<DoctorRun>,
    /// A rule names `certificate` (alerts are sent about them).
    pub alerting: bool,
}

/// Every certificate the engine connects to, as its last check saw it, and when the engine
/// checks next by itself. Read even when this server doesn't trust them.
#[utoipa::path(
    get, path = "/certificates", tag = "reports", operation_id = "list_certificates",
    responses((status = 200, body = CertificatesOverview))
)]
async fn list(engine: Eng, _caller: Caller) -> ApiResult<CertificatesOverview> {
    let warn_days = engine
        .alerts
        .as_ref()
        .map(|a| a.settings.certificates.warn_days.clone())
        .unwrap_or_else(|| CertificateSettings::default().warn_days);
    let seen = engine.store.certificates().await?;
    let alert_ids: std::collections::HashMap<String, i32> = if engine.alerts.is_some() {
        engine.store.alerts().await?.into_iter().filter(|r| r.key.starts_with("cert:")).map(|r| (r.key, r.id)).collect()
    } else {
        Default::default()
    };
    let now = Utc::now();
    let mut certificates: Vec<CertificateInfo> = engine
        .certificates
        .iter()
        .map(|t| {
            let row = seen.iter().find(|c| c.resource == t.resource && c.host == t.host && c.port == i32::from(t.port));
            let ends = row.and_then(|r| r.chain_min_not_after);
            let stage = ends.and_then(|e| alerts::certificate_stage(e, now, &warn_days));
            let state = match (ends, stage) {
                (None, _) => "unknown",
                (Some(_), Some(0)) => "expired",
                (Some(_), Some(_)) => "expiring",
                (Some(_), None) => "ok",
            };
            CertificateInfo {
                resource: t.resource.clone(),
                host: t.host.clone(),
                port: t.port,
                state: state.into(),
                days_left: ends.map(|e| alerts::days_left(e, now)),
                stage,
                subject: row.and_then(|r| r.subject.clone()),
                issuer: row.and_then(|r| r.issuer.clone()),
                fingerprint: row.and_then(|r| r.fingerprint.clone()),
                not_before: row.and_then(|r| r.not_before),
                not_after: row.and_then(|r| r.not_after),
                chain_not_after: ends,
                chain_earliest: row.and_then(|r| r.chain_min_subject.clone()),
                trusted: row.and_then(|r| r.trusted),
                error: row.and_then(|r| r.error.clone()),
                checked_at: row.map(|r| r.checked_at),
                read_at: row.and_then(|r| r.read_at),
                alert: alert_ids.get(&t.key).copied(),
            }
        })
        .collect();
    certificates.sort_by_key(|c| (c.chain_not_after.is_none(), c.chain_not_after, c.resource.clone()));
    let last_scheduled = engine.store.doctor_runs(true, 1).await?.into_iter().next().map(Into::into);
    let last_run = engine.store.doctor_runs(false, 1).await?.into_iter().next().map(Into::into);
    let alerting =
        engine.alerts.as_ref().is_some_and(|a| a.settings.rule_for(alerts::Condition::Certificate).is_some());
    Ok(Json(CertificatesOverview {
        certificates,
        warn_days,
        schedule_seconds: engine.doctor.schedule.map(|s| s.as_secs()),
        mode: engine.doctor.mode.label().into(),
        last_scheduled,
        last_run,
        alerting,
    }))
}

/// The connection checks kept (the last 30), newest first. Operator.
#[utoipa::path(
    get, path = "/doctor/runs", tag = "reports", operation_id = "list_doctor_runs",
    responses((status = 200, body = Vec<DoctorRun>), (status = 403, body = super::ErrorBody))
)]
async fn runs(engine: Eng, caller: Caller) -> ApiResult<Vec<DoctorRun>> {
    caller.require(Role::Operator)?;
    let runs = engine.store.doctor_runs(false, ductus_store::DOCTOR_RUNS_KEPT).await?;
    Ok(Json(runs.into_iter().map(Into::into).collect()))
}

/// One kept connection check's report, as `POST /doctor` returned it. Operator.
#[utoipa::path(
    get, path = "/doctor/runs/{id}", tag = "reports", operation_id = "get_doctor_run",
    params(("id" = i64, Path, description = "The run's id")),
    responses((status = 200, body = crate::doctor::Report), (status = 403, body = super::ErrorBody), (status = 404, body = super::ErrorBody))
)]
async fn run(engine: Eng, caller: Caller, Path(id): Path<i64>) -> Result<Json<serde_json::Value>, ApiError> {
    caller.require(Role::Operator)?;
    let report = engine.store.doctor_report(id).await?;
    report.map(Json).ok_or_else(|| ApiError::not_found(format!("no connection check {id} (the last 30 are kept)")))
}
