//! Deploys (0.7.4.4): `POST /deployments` replaces the release an engine in release mode runs with
//! another stored here, through the handover: receiving never stops, sending pauses while the
//! engines swap (well under a second at a thousand interfaces).
//!
//! In order, each step able to refuse with what to do: the engine runs releases (folder mode
//! refuses, saying how to switch); the release is stored and of this engine's project, and isn't
//! the one running; a reason is given (except in `dev`); the **"replaces" check** (`deploy`):
//! nothing it changes is someone else's work it would undo, unless `force` (recorded); the **gate**
//! (`ductus release check` against this environment, as `POST /releases/{id}/check`), or a passing
//! check it may reuse (the pre-run: same release, environment, database and Ductus version, within
//! `DUCTUS_DEPLOY_REUSE_CHECK`, nothing deployed here since); then the handover, asked from inside
//! the engine as `ductus handover` asks over its socket: to a new process under the package's
//! systemd unit, else in this process (Windows, containers, a plain `ductus run`). The engine being
//! replaced answers once the new one sends (or failed, and it goes on), with what the new engine
//! recorded.

use super::releases::{CHECKING, CheckInfo, check_info, find, run_check};
use super::{ApiError, Caller, Eng};
use crate::deploy;
use crate::deployment::Who;
use crate::release::{self, Manifest};
use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::Duration;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

/// How long the new engine may take to be ready (it reads the release, migrates the database and
/// loads every interface's code), as `ductus handover`'s default.
const READY_WITHIN: Duration = Duration::from_secs(5 * 60);
/// How long the answer waits for the new engine's record of the deployment (it lands beside its
/// start, tens of milliseconds after it sends).
const RECORDED_WITHIN: Duration = Duration::from_secs(3);

pub fn routes() -> OpenApiRouter<super::Shared> {
    OpenApiRouter::new().routes(routes!(deploy))
}

#[derive(Deserialize, ToSchema)]
pub struct DeployRequest {
    /// The release: its id, or its first 12 characters. Store it first (`POST /releases`).
    release: String,
    /// Why: required, except in an environment named `dev`. Recorded with the deployment, in its
    /// history and in the audit log.
    #[serde(default)]
    reason: Option<String>,
    /// Deploy over someone else's work the "replaces" check names: their change is undone. The
    /// interfaces and whose work go into the audit log (`release.force`) and the deployment's
    /// history.
    #[serde(default)]
    force: bool,
    /// Run every interface's tests in the gate, not only those of what the release changes here
    /// (a passing check is reused only when it ran them all too: never, then).
    #[serde(default)]
    all_tests: bool,
}

/// What a deploy did, or why it didn't.
#[derive(Serialize, ToSchema)]
pub struct DeployReport {
    /// The new release runs here.
    ok: bool,
    /// What happened, in a sentence.
    outcome: String,
    /// The release, in hex, and its first 12 characters.
    release: String,
    short: String,
    project: String,
    environment: String,
    /// Each interface (and "the project's own files") whose running version is someone else's work
    /// this release would undo, with why. A deploy is refused when any is named, unless forced.
    replaces: BTreeMap<String, String>,
    /// Deployed over the work `replaces` names.
    forced: bool,
    /// The gate: a check run now, or the passing check reused (`reused`).
    gate: Option<DeployGate>,
    /// What the handover said as it went.
    steps: Vec<String>,
    /// The deployment the new engine recorded and what it changed (absent if it wasn't recorded
    /// within seconds: `GET /engine` shows it then).
    deployment: Option<i32>,
    changes: Vec<DeployChange>,
}

#[derive(Serialize, ToSchema)]
pub struct DeployGate {
    /// A recent passing check reused instead of running the gate again (the pre-run).
    reused: bool,
    /// Why no check could be reused (when one ran).
    #[serde(skip_serializing_if = "Option::is_none")]
    why_not_reused: Option<String>,
    check: CheckInfo,
}

#[derive(Serialize, ToSchema)]
pub struct DeployChange {
    interface: String,
    /// The code version before (12 hex characters); null: new here.
    from: Option<String>,
    to: String,
    /// The "replaces" check's text, when deployed over someone else's work (`force`).
    warning: Option<String>,
}

/// Deploy a stored release to this engine (release mode): the "replaces" check, the gate (or a
/// recent passing check reused), then the handover to an engine running it. Answers when the new
/// engine sends, or when it couldn't (this one goes on): seconds, plus the gate's time when it
/// runs. 200: deployed. 409: refused before anything ran (folder mode, another deploy or check
/// running, the release already runs, its "replaces" check names someone's work), the report
/// says why. 422: the gate found problems. 500: the new engine couldn't take over (this one goes
/// on). Needs the `deploy` permission. One deploy or check at a time per engine.
#[utoipa::path(
    post, path = "/deployments", operation_id = "deploy_release", tag = "releases",
    request_body = DeployRequest,
    responses(
        (status = 200, body = DeployReport, description = "Deployed: the release runs here"),
        (status = 409, body = DeployReport, description = "Refused before anything ran: `outcome` says why"),
        (status = 422, body = DeployReport, description = "The gate found problems: `gate.check.report`"),
        (status = 500, body = DeployReport, description = "The new engine couldn't take over: this one goes on"),
        (status = 400, body = super::ErrorBody, description = "No reason given, or no such release"),
        (status = 403, body = super::ErrorBody)
    )
)]
async fn deploy(engine: Eng, caller: Caller, Json(asked): Json<DeployRequest>) -> Result<Response, ApiError> {
    caller.require_permission("deploy")?;
    let Some(releases) = &engine.releases else {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            format!(
                "this engine runs the project folder {} (folder mode), not releases: start it with DUCTUS_RELEASES=<folder> \
                 (`ductus run --releases`) instead of DUCTUS_PROJECT to deploy releases to it; in folder mode, update the \
                 folder and hand over (`ductus handover`)",
                engine.root.display()
            ),
        ));
    };
    let reason = asked.reason.as_deref().map(str::trim).filter(|r| !r.is_empty()).map(str::to_string);
    if reason.is_none() && engine.environment != "dev" {
        return Err(ApiError::bad_request(format!(
            "say why (`reason`, `ductus deploy -m \"...\"`): every deploy to `{}` is recorded with its reason",
            engine.environment
        )));
    }
    if asked.force && reason.is_none() {
        return Err(ApiError::bad_request("a forced deploy needs a reason (`reason`)"));
    }
    let id = find(&engine, &asked.release).await?;
    let (manifest, ancestry) = engine
        .store
        .release_manifest(&id)
        .await?
        .ok_or_else(|| ApiError::not_found("that release is no longer stored"))?;
    let manifest: Manifest =
        serde_json::from_value(manifest).map_err(|e| anyhow::anyhow!("reading the release's manifest: {e}"))?;
    let hex = crate::code::hex(&id);
    let mut report = DeployReport {
        ok: false,
        outcome: String::new(),
        short: release::short(&hex).to_string(),
        release: hex.clone(),
        project: manifest.project.clone(),
        environment: engine.environment.clone(),
        replaces: BTreeMap::new(),
        forced: false,
        gate: None,
        steps: Vec::new(),
        deployment: None,
        changes: Vec::new(),
    };
    let refuse = |mut report: DeployReport, status: StatusCode, outcome: String| {
        report.outcome = outcome;
        Ok((status, Json(report)).into_response())
    };
    if let Some(project) = releases.project()
        && project != manifest.project
    {
        let outcome = format!(
            "release {} is of project `{}`, and this engine runs `{project}`: deploy it to that project's engine",
            report.short, manifest.project
        );
        return refuse(report, StatusCode::CONFLICT, outcome);
    }
    if releases.running.as_ref().is_some_and(|r| r.id == id) {
        let outcome = format!("release {} already runs here: nothing to do", report.short);
        return refuse(report, StatusCode::CONFLICT, outcome);
    }
    let Some(asker) = engine.asker.get().cloned() else {
        let outcome = "this engine takes no handovers, so it can't apply a deploy (`ductus dev`): run the release \
                       with `ductus run --releases`"
            .to_string();
        return refuse(report, StatusCode::CONFLICT, outcome);
    };
    let Ok(_one_at_a_time) = CHECKING.try_lock() else {
        let outcome = "another deploy or release check is running on this engine: try again when it ends".to_string();
        return refuse(report, StatusCode::CONFLICT, outcome);
    };
    let short = report.short.clone();
    let audit = |action: &'static str, detail: serde_json::Value| {
        let (engine, by) = (engine.clone(), caller.audit(action));
        let short = short.clone();
        async move {
            let row = by.subject(&short).detail(detail);
            if let Err(e) = engine.store.record(&row).await {
                tracing::error!(error = %format!("{e:#}"), action, "couldn't write the audit log");
            }
        }
    };

    // The "replaces" check: what this release changes over someone else's work.
    let who = Who { how: "handover", by: caller.name().to_string(), reason: reason.clone() };
    let source = deploy::Source::Release {
        commit: manifest.git.as_ref().map(|g| g.commit.clone()),
        ancestry: std::sync::Arc::new(ancestry),
        over: manifest.over.as_deref().and_then(deploy::unhex),
    };
    let versions: Vec<(String, Vec<u8>)> =
        manifest.interfaces.iter().filter_map(|(name, i)| Some((name.clone(), deploy::unhex(&i.version)?))).collect();
    let pairs: Vec<(&str, &[u8])> = versions.iter().map(|(n, v)| (n.as_str(), v.as_slice())).collect();
    report.replaces = deploy::replaced(&engine.store, &source, &pairs, &who).await?;
    if let Some(version) = deploy::unhex(&manifest.project_version.version)
        && let Some(why) =
            deploy::project_replaced(&engine.store, &engine.environment, &manifest.project, &version, &source, &who)
                .await?
    {
        report.replaces.insert(deploy::PROJECT.to_string(), why);
    }
    if !report.replaces.is_empty() && !asked.force {
        let outcome = format!(
            "refused: it would undo someone else's work ({}): merge their commit and release again, or deploy with \
             `force` (`ductus deploy --force`) and a reason to undo it on purpose",
            report.replaces.keys().cloned().collect::<Vec<_>>().join(", ")
        );
        let detail = serde_json::json!({ "environment": engine.environment, "ok": false, "outcome": outcome });
        audit("release.deploy", detail).await;
        return refuse(report, StatusCode::CONFLICT, outcome);
    }
    report.forced = !report.replaces.is_empty();

    // The gate, or a recent passing check of the same release here (the pre-run).
    let reuse = releases.mode.reuse_check;
    let reusable = if asked.all_tests || reuse.is_zero() {
        None
    } else {
        // Never from before this engine started: its start is a deployment too, recorded beside
        // the start a moment after (a deploy right after a start would otherwise slip in before it).
        let window = Utc::now() - chrono::Duration::from_std(reuse).unwrap_or(chrono::TimeDelta::MAX);
        let since = window.max(engine.started_at);
        engine.store.reusable_check(&id, &engine.environment, env!("CARGO_PKG_VERSION"), since).await?
    };
    let gate = match reusable {
        Some(check) => DeployGate { reused: true, why_not_reused: None, check: check_info(check) },
        None => {
            let why = if asked.all_tests {
                "every test was asked for (`all_tests`)".to_string()
            } else if reuse.is_zero() {
                "reusing checks is off here (DUCTUS_DEPLOY_REUSE_CHECK=0)".to_string()
            } else {
                format!(
                    "no passing check of it here by Ductus {} in the last {}, with nothing deployed to `{}` since",
                    env!("CARGO_PKG_VERSION"),
                    span(reuse),
                    engine.environment
                )
            };
            DeployGate {
                reused: false,
                why_not_reused: Some(why),
                check: run_check(&engine, &caller, &id, asked.all_tests).await?,
            }
        }
    };
    let passed = gate.check.ok;
    report.gate = Some(gate);
    if !passed {
        let outcome = "refused: the gate found problems with this release here (`gate.check.report`)".to_string();
        let detail = serde_json::json!({ "environment": engine.environment, "ok": false, "outcome": outcome });
        audit("release.deploy", detail).await;
        return refuse(report, StatusCode::UNPROCESSABLE_ENTITY, outcome);
    }
    if report.forced {
        let detail =
            serde_json::json!({ "environment": engine.environment, "reason": reason, "replaces": report.replaces });
        audit("release.force", detail).await;
    }

    // The handover: a new engine running the release takes over from this one.
    let request = crate::handover::Request {
        timeout_secs: READY_WITHIN.as_secs(),
        environment: None,
        asked: crate::handover::Asked { by: Some(who.by.clone()), reason: reason.clone(), release: Some(hex.clone()) },
    };
    let started = Utc::now();
    let mut steps = Vec::new();
    let (ok, text) = asker.ask(request, |line| steps.push(line.to_string())).await?;
    report.steps = steps;
    report.ok = ok;
    let detail = serde_json::json!({
        "environment": engine.environment,
        "ok": ok,
        "reason": reason,
        "forced": report.forced,
        "check": report.gate.as_ref().map(|g| g.check.id),
        "reused_check": report.gate.as_ref().is_some_and(|g| g.reused),
        "outcome": text,
    });
    audit("release.deploy", detail).await;
    if !ok {
        return refuse(report, StatusCode::INTERNAL_SERVER_ERROR, text);
    }
    // What the new engine recorded (beside its start, just after it sends).
    let until = tokio::time::Instant::now() + RECORDED_WITHIN;
    loop {
        if let Some((deployment, changes)) =
            engine.store.deployment_of_release(&engine.environment, &id, started).await?
        {
            report.deployment = Some(deployment);
            report.changes = changes
                .into_iter()
                .map(|c| DeployChange {
                    interface: c.interface,
                    from: c.from.map(|f| crate::code::short(&f)),
                    to: crate::code::short(&c.to),
                    warning: c.warning,
                })
                .collect();
            break;
        }
        if tokio::time::Instant::now() >= until {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    report.outcome = text;
    Ok((StatusCode::OK, Json(report)).into_response())
}

/// "1 h", "15 min", "90 s".
fn span(d: Duration) -> String {
    match d.as_secs() {
        s if s >= 3600 && s % 3600 == 0 => format!("{} h", s / 3600),
        s if s >= 60 && s % 60 == 0 => format!("{} min", s / 60),
        s => format!("{s} s"),
    }
}
