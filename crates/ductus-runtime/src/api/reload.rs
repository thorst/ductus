//! `POST /engine/reload` (0.7.5.3.1): an engine in folder mode applies its project folder as it
//! is now, in place where it can (`crate::reload`), else by the handover, as `ductus reload` asks
//! over the control socket. The way to reload where there's no control socket (Windows, a plain
//! `ductus run`), and from another machine.

use super::deployments::PlanInfo;
use super::{ApiError, Caller, Eng};
use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::Duration;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

/// How long a reload that takes the handover gives the new engine to be ready, as `ductus
/// handover`'s default.
const READY_WITHIN: Duration = Duration::from_secs(5 * 60);

pub fn routes() -> OpenApiRouter<super::Shared> {
    OpenApiRouter::new().routes(routes!(reload))
}

/// What to reload with.
#[derive(Deserialize, ToSchema, Default)]
#[serde(deny_unknown_fields)]
pub struct ReloadRequest {
    /// Why: recorded with the deployment, beside who asked. Optional (a reload records what
    /// changed whoever asks; `systemctl reload` gives none).
    #[serde(default)]
    pub reason: Option<String>,
    /// Refuse what only the handover applies (ductus.yaml, resources.yaml, docs-schema.yaml, the
    /// environment's secrets file changed) instead of taking it. Default: false.
    #[serde(default)]
    pub in_place_only: bool,
}

/// How a reload went: what `ductus reload --json` prints and `POST /engine/reload` answers.
#[derive(Serialize, ToSchema)]
pub struct ReloadReport {
    /// It runs the folder as it is now (applied, or nothing to apply).
    pub ok: bool,
    /// `in_place`, `nothing` (the folder is what runs), `handover` (a new engine runs it) or
    /// `refused` (nothing changed: `outcome` says why).
    pub applied: String,
    /// What it did, or why it didn't, in words.
    pub outcome: String,
    /// What the folder changes here, by kind; absent when the folder didn't load.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<PlanInfo>,
    /// What it did, a line per interface (in place).
    pub steps: Vec<String>,
    /// The deployment it recorded (in place: `how` = `reload`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deployment: Option<i32>,
    /// Each interface it changed: its code version before and after (12 hex characters).
    pub changes: Vec<ReloadChange>,
    /// Interfaces changed over someone else's work (the "replaces" check: folder mode warns), by
    /// interface, with why.
    pub warnings: BTreeMap<String, String>,
    /// How long it took, in milliseconds (the handover's: until the new engine sent).
    pub took_ms: u64,
}

/// One interface a reload changed.
#[derive(Serialize, ToSchema)]
pub struct ReloadChange {
    pub interface: String,
    /// Its version before; null: new here.
    pub from: Option<String>,
    pub to: String,
}

/// A reload's report (`ReloadReport`) as JSON: what the control socket's answer carries.
pub(crate) fn report(r: &crate::reload::Reloaded) -> serde_json::Value {
    let changes = r
        .deployment
        .iter()
        .flat_map(|d| d.interfaces.values())
        .filter(|i| i.changed)
        .map(|i| ReloadChange {
            interface: i.interface.clone(),
            from: i.previous.as_deref().map(crate::code::short),
            to: crate::code::short(&i.version),
        })
        .collect();
    let report = ReloadReport {
        ok: r.ok,
        applied: r.how.as_str().to_string(),
        outcome: r.outcome.clone(),
        plan: r.plan.as_ref().map(PlanInfo::of),
        steps: r.steps.clone(),
        deployment: r.deployment.as_ref().map(|d| d.id),
        changes,
        warnings: r.warnings.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        took_ms: r.took.as_millis() as u64,
    };
    serde_json::to_value(report).unwrap_or_default()
}

/// Apply the project folder as it is now (folder mode): only the interfaces whose files or
/// filled-in settings changed take the change (between two messages, or restarting alone; added
/// ones start, removed ones stop), the others never pause. What only a start reads (ductus.yaml,
/// resources.yaml, docs-schema.yaml, the environment's secrets file; a new binary) goes by the
/// handover, unless `in_place_only`. Recorded as a deployment (`how` = `reload`). In release mode
/// it's a handover to the release that runs. 200: it runs the folder (`applied`: `in_place`,
/// `nothing`, `handover`). 409: refused, nothing changed (the folder doesn't load, a start would
/// refuse it, `in_place_only` and it needs the handover, another reload or handover under way).
/// 500: a handover that failed (this engine goes on as it was). Needs the `deploy` permission.
#[utoipa::path(
    post, path = "/engine/reload", operation_id = "reload_engine", tag = "engine",
    request_body = ReloadRequest,
    responses(
        (status = 200, body = ReloadReport, description = "It runs the folder as it is now: `applied` says how"),
        (status = 409, body = ReloadReport, description = "Refused, nothing changed: `outcome` says why"),
        (status = 500, body = ReloadReport, description = "The handover failed: this engine goes on as it was"),
        (status = 403, body = super::ErrorBody)
    )
)]
async fn reload(engine: Eng, caller: Caller, Json(asked): Json<ReloadRequest>) -> Result<Response, ApiError> {
    caller.require_permission("deploy")?;
    let Some(asker) = engine.asker.get().cloned() else {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "this engine takes no reloads (`ductus dev` applies the folder by itself): restart it instead",
        ));
    };
    let reason = asked.reason.map(|r| r.trim().to_string()).filter(|r| !r.is_empty());
    let request = crate::handover::Request {
        timeout_secs: READY_WITHIN.as_secs(),
        asked: crate::handover::Asked { by: Some(caller.name().to_string()), reason, ..Default::default() },
        in_place: true,
        in_place_only: asked.in_place_only,
        ..Default::default()
    };
    let started = std::time::Instant::now();
    let mut said = Vec::new();
    let done = asker.ask_for(request, |line| said.push(line.to_string())).await?;
    let (status, report) = match done {
        Err(why) => (StatusCode::CONFLICT, refused(why, started)),
        // In place, nothing to apply, or refused: the report says.
        Ok(crate::handover::Done { ok, report: Some(report), .. }) => {
            let status = if ok { StatusCode::OK } else { StatusCode::CONFLICT };
            (status, report)
        }
        // The handover took it: its answer is a handover's.
        Ok(crate::handover::Done { ok, text, report: None }) => {
            let mut report = handed_over(ok, text, said, started);
            if !ok {
                report["applied"] = "refused".into();
            }
            (if ok { StatusCode::OK } else { StatusCode::INTERNAL_SERVER_ERROR }, report)
        }
    };
    // The engine wrote the audit log's row (`engine.reload`, or the new engine's `engine.handover`).
    Ok((status, Json(report)).into_response())
}

/// A report for a reload refused before anything ran.
fn refused(why: String, started: std::time::Instant) -> serde_json::Value {
    serde_json::json!({
        "ok": false, "applied": "refused", "outcome": why, "steps": [], "changes": [], "warnings": {},
        "took_ms": started.elapsed().as_millis() as u64,
    })
}

/// A report for a reload the handover took: what it said as it went, and how it ended.
fn handed_over(ok: bool, text: String, said: Vec<String>, started: std::time::Instant) -> serde_json::Value {
    serde_json::json!({
        "ok": ok, "applied": "handover", "outcome": text, "steps": said, "changes": [], "warnings": {},
        "took_ms": started.elapsed().as_millis() as u64,
    })
}
