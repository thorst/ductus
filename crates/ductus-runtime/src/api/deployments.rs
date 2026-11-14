//! Deploys (0.7.4.4): `POST /deployments` replaces the release an engine in release mode runs with
//! another stored here, through the handover: receiving never stops, sending pauses while the
//! engines swap (well under a second at a thousand interfaces).
//!
//! **In place (0.7.5):** when the release changes only interfaces' code, destinations and
//! destination settings (`changes::plan`, from the two manifests), it's applied without a handover
//! (`deploy::apply_in_place`): each changed interface's workers restart between two messages on the
//! new code, the others never pause, and a deployment `how = deploy` is recorded. Anything else
//! (a source, lanes, an interface added or removed, the project's own files) goes by the handover,
//! the answer saying which change needed it; `in_place_only` refuses instead.
//!
//! In order, each step able to refuse with what to do: the engine runs releases (folder mode
//! refuses, saying how to switch); the release is stored and of this engine's project, and isn't
//! the one running; a reason is given (except in `dev`); **this environment's rules** (0.7.4.4.3:
//! `DUCTUS_DEPLOY_REQUIRES`, the environments it must have been deployed to first, by the record it
//! carries and this database's deployments; `DUCTUS_DEPLOY_BRANCHES`, the branches it may be made
//! on), never skipped by `force`; the **"replaces" check** (`deploy`): nothing it changes is
//! someone else's work it would undo, unless `force` (recorded); the **gate** (`ductus release
//! check` against this environment, as `POST /releases/{id}/check`), or a passing check it may
//! reuse (the pre-run: same release, environment, database and Ductus version, within
//! `DUCTUS_DEPLOY_REUSE_CHECK`, nothing deployed here since); then the handover, asked from inside
//! the engine as `ductus handover` asks over its socket: to a new process under the package's
//! systemd unit, else in this process (Windows, containers, a plain `ductus run`). The engine being
//! replaced answers once the new one sends (or failed, and it goes on), with what the new engine
//! recorded.
//!
//! **A rollback** (`rollback`, 0.7.4.4.3) is a deploy of a release that ran in this environment
//! before (by default the last one before the current run, skipping any a rollback left): this
//! environment's rules don't hold it (it ran here), the "replaces" check names whose newer work it
//! undoes without refusing (undoing is the point), and the gate always runs.

use super::releases::{CHECKING, CheckInfo, check_info, find, run_check};
use super::{ApiError, Caller, Eng};
use crate::deploy;
use crate::deployment::Who;
use crate::release::{self, Manifest};
use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
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
    OpenApiRouter::new().routes(routes!(deploy, runs))
}

/// One run of a release in this environment: a deploy (or rollback), and the restarts that ran
/// the same release again.
#[derive(Serialize, ToSchema)]
pub struct RunInfo {
    /// The release, in hex, and its first 12 characters.
    release: String,
    short: String,
    /// Its first deployment in this run: when, by whom, why.
    deployment: i32,
    at: DateTime<Utc>,
    by: String,
    reason: Option<String>,
    /// The run began with a rollback.
    rollback: bool,
    /// It runs now (the newest run).
    running: bool,
    /// What `ductus deploy --rollback` (no release named) would go back to.
    rollback_target: bool,
}

/// The releases this engine's project ran in its environment, newest first, one entry per run:
/// what runs now, what ran before, and which one a rollback without a release named goes back
/// to. Empty in folder mode.
#[utoipa::path(
    get, path = "/deployments", operation_id = "list_deployments", tag = "releases",
    responses((status = 200, body = Vec<RunInfo>))
)]
async fn runs(engine: Eng, _caller: Caller) -> Result<Json<Vec<RunInfo>>, ApiError> {
    let Some(project) = engine.releases.as_ref().and_then(|r| r.project()) else { return Ok(Json(Vec::new())) };
    let runs = engine.store.release_runs(&engine.environment, &project).await?;
    let current = engine.releases.as_ref().and_then(|r| r.running()).map(|r| r.id.clone());
    let target = current.as_deref().and_then(|c| deploy::rollback_target(&runs, c)).map(|r| r.deployment);
    let last = runs.len().checked_sub(1);
    Ok(Json(
        runs.iter()
            .enumerate()
            .rev()
            .map(|(n, r)| {
                let hex = crate::code::hex(&r.release);
                RunInfo {
                    short: release::short(&hex).to_string(),
                    release: hex,
                    deployment: r.deployment,
                    at: r.at,
                    by: r.by.clone(),
                    reason: r.reason.clone(),
                    rollback: r.rollback,
                    running: Some(n) == last && current.as_deref() == Some(r.release.as_slice()),
                    rollback_target: target == Some(r.deployment),
                }
            })
            .collect(),
    ))
}

#[derive(Deserialize, ToSchema)]
pub struct DeployRequest {
    /// The release: its id, or its first 12 characters. Store it first (`POST /releases`).
    /// Required, except for a rollback (default: the last release run here before the current
    /// one, skipping any a rollback left).
    #[serde(default)]
    release: Option<String>,
    /// Why: required, except in an environment named `dev`. Recorded with the deployment, in its
    /// history and in the audit log.
    #[serde(default)]
    reason: Option<String>,
    /// Deploy over someone else's work the "replaces" check names: their change is undone. The
    /// interfaces and whose work go into the audit log (`release.force`) and the deployment's
    /// history. Doesn't skip this environment's rules (`DUCTUS_DEPLOY_REQUIRES`,
    /// `DUCTUS_DEPLOY_BRANCHES`).
    #[serde(default)]
    force: bool,
    /// Run every interface's tests in the gate, not only those of what the release changes here
    /// (a passing check is reused only when it ran them all too: never, then).
    #[serde(default)]
    all_tests: bool,
    /// Roll back: go back to a release that ran in this environment before. Not held to this
    /// environment's rules; the "replaces" check names whose work it undoes without refusing; the
    /// gate runs. Audited as `release.rollback`.
    #[serde(default)]
    rollback: bool,
    /// Only in place: refuse (409, before the gate runs) when the release changes something only
    /// the handover applies (`plan.handover_because` says what), and (500) when applying in place
    /// fails, instead of taking the handover. For sites that schedule handovers.
    #[serde(default)]
    in_place_only: bool,
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
    /// The branches it was made on, as its files said (the same files made on two branches are
    /// one release); empty: a detached HEAD without `--branch`, or made without git.
    branches: Vec<String>,
    /// A rollback: to a release that ran here before.
    rollback: bool,
    /// What this environment's rules said (`DUCTUS_DEPLOY_REQUIRES`, `DUCTUS_DEPLOY_BRANCHES`):
    /// each reason a deploy is refused, or (a rollback) that they don't hold it.
    rules: Vec<String>,
    /// Each interface (and "the project's own files") whose running version is someone else's work
    /// this release would undo, with why. A deploy is refused when any is named, unless forced; a
    /// rollback names them and goes on.
    replaces: BTreeMap<String, String>,
    /// Deployed over the work `replaces` names.
    forced: bool,
    /// The gate: a check run now, or the passing check reused (`reused`).
    gate: Option<DeployGate>,
    /// What it changes here, by kind, and whether that applies in place; absent when refused
    /// before it was worked out, or on the first deploy here (nothing to compare: the handover).
    plan: Option<PlanInfo>,
    /// How it was applied: `in_place` (only the changed interfaces restarted their workers,
    /// between two messages; the others never paused) or `handover` (a new engine took over:
    /// every interface's sending paused for a moment); absent when refused.
    applied: Option<&'static str>,
    /// What applying it said as it went: each interface applied in place, or the handover's steps.
    steps: Vec<String>,
    /// The deployment recorded and what it changed (absent if the new engine's wasn't recorded
    /// within seconds: `GET /engine` shows it then).
    deployment: Option<i32>,
    changes: Vec<DeployChange>,
    /// The record of this deployment, as a release file carries it (`ductus deploy` writes it into
    /// the file it deployed; `GET /releases/{id}/file` includes it): what an environment that
    /// requires this one first reads.
    place: Option<PlaceInfo>,
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

/// What a release changes here, by kind (`changes::plan`).
#[derive(Serialize, ToSchema)]
pub struct PlanInfo {
    /// Every interface whose code version changes, is added or is removed.
    changes: Vec<PlannedChange>,
    /// Why it needs the handover, a line per reason (the project's own files, then each
    /// interface); empty: it applies in place.
    handover_because: Vec<String>,
}

#[derive(Serialize, ToSchema)]
pub struct PlannedChange {
    interface: String,
    /// What changed, in words: `code`, `destinations.lab`, `destination lab added`, `retry`,
    /// `source`, `added`, `removed`, ...
    what: Vec<String>,
    /// It can be applied in place (its workers restarted between messages).
    in_place: bool,
    /// Why only the handover applies it; absent when it goes in place.
    #[serde(skip_serializing_if = "Option::is_none")]
    handover_because: Option<String>,
}

impl PlanInfo {
    fn of(plan: &crate::changes::Plan) -> Self {
        use crate::changes::Apply;
        Self {
            changes: plan
                .changes
                .iter()
                .map(|c| PlannedChange {
                    interface: c.interface.clone(),
                    what: c.what.clone(),
                    in_place: c.apply == Apply::InPlace,
                    handover_because: match &c.apply {
                        Apply::InPlace => None,
                        Apply::Handover(why) => Some(why.clone()),
                    },
                })
                .collect(),
            handover_because: plan.handover_because(),
        }
    }
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

/// Where a release was deployed: one entry of a release file's record (`deployed`).
#[derive(Serialize, Deserialize, ToSchema)]
pub struct PlaceInfo {
    pub environment: String,
    /// The engine's server.
    pub host: String,
    /// The deployment's id on that engine's database.
    pub deployment: i32,
    pub at: DateTime<Utc>,
    pub by: String,
    /// The Ductus version that ran it.
    pub ductus: String,
    /// Recorded on this engine's database; false: a release file said so when it was stored here.
    #[serde(default)]
    pub here: bool,
}

impl PlaceInfo {
    pub(super) fn of(p: &ductus_store::Place) -> Self {
        Self {
            environment: p.environment.clone(),
            host: p.host.clone(),
            deployment: p.deployment,
            at: p.at,
            by: p.by.clone(),
            ductus: p.ductus_version.clone(),
            here: p.here,
        }
    }
}

/// Deploy a stored release to this engine (release mode): this environment's rules, the "replaces"
/// check, what it changes here (`plan`), the gate (or a recent passing check reused), then **in
/// place** when only interfaces' code, destinations and destination settings change (those
/// interfaces' workers restart between two messages on the new code; the others never pause), else
/// the handover to an engine running it (`plan.handover_because` says why); or roll back
/// (`rollback`) to a release that ran here before, the same way. Answers once it runs (in place:
/// milliseconds; a handover: when the new engine sends), or when it couldn't (this one goes on as
/// it was), plus the gate's time when it runs. 200: deployed (`applied` says how). 409: refused
/// before anything ran (folder mode, another deploy or check running, the release already runs,
/// this environment's rules, its "replaces" check names someone's work, a rollback to a release
/// that never ran here, `in_place_only` and it needs the handover), the report says why. 422: the
/// gate found problems. 500: it couldn't be applied (this engine goes on as it was). Needs the
/// `deploy` permission. One deploy or check at a time per engine.
#[utoipa::path(
    post, path = "/deployments", operation_id = "deploy_release", tag = "releases",
    request_body = DeployRequest,
    responses(
        (status = 200, body = DeployReport, description = "Deployed: the release runs here (`applied`: in place or by the handover)"),
        (status = 409, body = DeployReport, description = "Refused before anything ran: `outcome` says why"),
        (status = 422, body = DeployReport, description = "The gate found problems: `gate.check.report`"),
        (status = 500, body = DeployReport, description = "It couldn't be applied: this engine goes on as it was"),
        (status = 400, body = super::ErrorBody, description = "No reason given, no release named, or no such release"),
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
    let what = if asked.rollback { "rollback" } else { "deploy" };
    let reason = asked.reason.as_deref().map(str::trim).filter(|r| !r.is_empty()).map(str::to_string);
    if reason.is_none() && engine.environment != "dev" {
        let flag = if asked.rollback { "ductus deploy --rollback -m \"...\"" } else { "ductus deploy -m \"...\"" };
        return Err(ApiError::bad_request(format!(
            "say why (`reason`, `{flag}`): every {what} to `{}` is recorded with its reason",
            engine.environment
        )));
    }
    if asked.force && reason.is_none() {
        return Err(ApiError::bad_request("a forced deploy needs a reason (`reason`)"));
    }
    if asked.force && asked.rollback {
        return Err(ApiError::bad_request(
            "a rollback isn't refused over someone's work (it names it), so `force` means nothing there: leave it out",
        ));
    }

    // Which release: the one named, or (a rollback) the last one run here before the current run.
    let runs = match (asked.rollback, releases.project()) {
        (true, Some(project)) => engine.store.release_runs(&engine.environment, &project).await?,
        _ => Vec::new(),
    };
    let current = releases.running().map(|r| r.id.clone());
    let id = match (&asked.release, asked.rollback) {
        (Some(named), _) => find(&engine, named).await?,
        (None, false) => return Err(ApiError::bad_request("name the release to deploy (`release`)")),
        (None, true) => {
            let Some(current) = &current else {
                return Err(ApiError::new(StatusCode::CONFLICT, "nothing is deployed here yet: nothing to roll back"));
            };
            match deploy::rollback_target(&runs, current) {
                Some(run) => run.release.clone(),
                None => {
                    return Err(ApiError::new(
                        StatusCode::CONFLICT,
                        format!(
                            "no earlier release ran in `{}` to roll back to (a release a rollback left is skipped: \
                             deploy it by its id to go back to it)",
                            engine.environment
                        ),
                    ));
                }
            }
        }
    };
    let (manifest, ancestry) = engine
        .store
        .release_manifest(&id)
        .await?
        .ok_or_else(|| ApiError::not_found("that release is no longer stored"))?;
    let manifest: Manifest =
        serde_json::from_value(manifest).map_err(|e| anyhow::anyhow!("reading the release's manifest: {e}"))?;
    let hex = crate::code::hex(&id);
    let branches = engine.store.release_branches(&id).await?;
    let mut report = DeployReport {
        ok: false,
        outcome: String::new(),
        short: release::short(&hex).to_string(),
        release: hex.clone(),
        project: manifest.project.clone(),
        environment: engine.environment.clone(),
        branches: branches.clone(),
        rollback: asked.rollback,
        rules: Vec::new(),
        replaces: BTreeMap::new(),
        forced: false,
        gate: None,
        plan: None,
        applied: None,
        steps: Vec::new(),
        deployment: None,
        changes: Vec::new(),
        place: None,
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
    if current.as_deref() == Some(id.as_slice()) {
        let outcome = format!("release {} already runs here: nothing to do", report.short);
        return refuse(report, StatusCode::CONFLICT, outcome);
    }
    if asked.rollback && !runs.iter().any(|r| r.release == id) {
        let outcome = format!(
            "release {} never ran in `{}`, so it isn't a rollback: deploy it (`ductus deploy {}`), held to this \
             environment's rules",
            report.short, engine.environment, report.short
        );
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
    let action = if asked.rollback { "release.rollback" } else { "release.deploy" };
    let from = current.as_ref().map(|c| release::short(&crate::code::hex(c)).to_string());
    let short = report.short.clone();
    let audit = |action: &'static str, mut detail: serde_json::Value| {
        let (engine, by) = (engine.clone(), caller.audit(action));
        let short = short.clone();
        if action == "release.rollback" {
            detail["from"] = from.clone().into();
        }
        async move {
            let row = by.subject(&short).detail(detail);
            if let Err(e) = engine.store.record(&row).await {
                tracing::error!(error = %format!("{e:#}"), action, "couldn't write the audit log");
            }
        }
    };

    // This environment's rules: what it requires first, and from which branches. Never `force`d.
    let has_rules = !releases.mode.requires.is_empty() || !releases.mode.branches.is_empty();
    if asked.rollback {
        if has_rules {
            report.rules.push(format!(
                "a rollback isn't held to this environment's rules (DUCTUS_DEPLOY_REQUIRES, DUCTUS_DEPLOY_BRANCHES): \
                 release {} ran here before",
                report.short
            ));
        }
    } else if has_rules {
        let places = engine.store.release_places(&id).await?;
        report.rules = deploy::rules(&releases.mode, &manifest, &branches, &places);
        if !report.rules.is_empty() {
            let outcome = "refused by this environment's rules (`rules` says why)".to_string();
            let detail = serde_json::json!({
                "environment": engine.environment, "ok": false, "outcome": outcome, "rules": report.rules
            });
            audit(action, detail).await;
            return refuse(report, StatusCode::CONFLICT, outcome);
        }
    }

    // The "replaces" check: what this release changes over someone else's work.
    let who = Who { how: "handover", by: caller.name().to_string(), reason: reason.clone(), rollback: asked.rollback };
    let source = deploy::Source::Release {
        commit: manifest.git.as_ref().map(|g| g.commit.clone()),
        ancestry: std::sync::Arc::new(ancestry.clone()),
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
    if !report.replaces.is_empty() && !asked.force && !asked.rollback {
        let outcome = format!(
            "refused: it would undo someone else's work ({}): merge their commit and release again, or deploy with \
             `force` (`ductus deploy --force`) and a reason to undo it on purpose",
            report.replaces.keys().cloned().collect::<Vec<_>>().join(", ")
        );
        let detail = serde_json::json!({ "environment": engine.environment, "ok": false, "outcome": outcome });
        audit(action, detail).await;
        return refuse(report, StatusCode::CONFLICT, outcome);
    }
    report.forced = asked.force && !report.replaces.is_empty();

    // What it changes here, by kind: in place when only code, destinations and their settings
    // change (those interfaces' workers restart between messages, nothing else pauses), else by
    // the handover. Before the gate, so `in_place_only` refuses without running it.
    let (folder, _) = deploy::unpacked(&engine.store, &releases.mode, &id).await?;
    let running = releases.running();
    let plan = match &running {
        Some(r) => {
            let (dir, r, next, folder) = (releases.mode.dir.clone(), r.clone(), manifest.clone(), folder.clone());
            let plan = tokio::task::spawn_blocking(move || deploy::plan_over(&dir, &r, &next, &folder))
                .await
                .map_err(anyhow::Error::from)??;
            Some(plan)
        }
        None => None,
    };
    report.plan = plan.as_ref().map(PlanInfo::of);
    let handover_because = match &plan {
        Some(p) => p.handover_because(),
        None => vec!["nothing ran here before: the first release starts by the handover".to_string()],
    };
    if asked.in_place_only && !handover_because.is_empty() {
        let outcome = format!(
            "refused: only the handover applies it ({}), and `in_place_only` was asked: deploy it without that flag \
             when a handover suits",
            handover_because.join("; ")
        );
        let detail = serde_json::json!({ "environment": engine.environment, "ok": false, "outcome": outcome });
        audit(action, detail).await;
        return refuse(report, StatusCode::CONFLICT, outcome);
    }

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
        audit(action, detail).await;
        return refuse(report, StatusCode::UNPROCESSABLE_ENTITY, outcome);
    }
    if report.forced {
        let detail =
            serde_json::json!({ "environment": engine.environment, "reason": reason, "replaces": report.replaces });
        audit("release.force", detail).await;
    }

    // In place when it can: only the changed interfaces take their new code, between two messages.
    let deployer = Who::deploy(caller.name(), reason.clone(), asked.rollback);
    let warnings: std::collections::HashMap<String, String> =
        report.replaces.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    if let Some(plan) = plan.as_ref().filter(|_| handover_because.is_empty()) {
        let next = std::sync::Arc::new(deploy::Running { id: id.clone(), manifest: manifest.clone(), ancestry });
        match deploy::apply_in_place(&engine, plan, next, &folder, &deployer, &warnings).await {
            Ok(done) => {
                report.ok = true;
                report.applied = Some("in_place");
                report.steps = done.steps;
                let n = plan.changes.len();
                report.outcome = match n {
                    0 => "applied in place: no interface's code or settings changed".to_string(),
                    1 => {
                        "applied in place: 1 interface took its new code between two messages; the others never paused"
                            .to_string()
                    }
                    n => format!(
                        "applied in place: {n} interfaces took their new code between two messages; the others never paused"
                    ),
                };
                if let Some(d) = &done.deployment {
                    report.deployment = Some(d.id);
                    report.changes = d
                        .interfaces
                        .values()
                        .filter(|i| i.changed)
                        .map(|i| DeployChange {
                            interface: i.interface.clone(),
                            from: i.previous.as_deref().map(crate::code::short),
                            to: crate::code::short(&i.version),
                            warning: warnings.get(&i.interface).cloned(),
                        })
                        .collect();
                    report.place = engine
                        .store
                        .release_places(&id)
                        .await?
                        .iter()
                        .find(|p| p.here && p.environment == engine.environment)
                        .map(PlaceInfo::of);
                }
                let mut detail = serde_json::json!({
                    "environment": engine.environment,
                    "ok": true,
                    "applied": "in_place",
                    "reason": reason,
                    "forced": report.forced,
                    "check": report.gate.as_ref().map(|g| g.check.id),
                    "reused_check": report.gate.as_ref().is_some_and(|g| g.reused),
                    "outcome": report.outcome,
                });
                if asked.rollback {
                    detail["undoes"] = serde_json::to_value(&report.replaces).unwrap_or_default();
                }
                audit(action, detail).await;
                return Ok((StatusCode::OK, Json(report)).into_response());
            }
            Err(e) if asked.in_place_only => {
                let outcome = format!(
                    "couldn't apply it in place ({e:#}): every interface runs what it ran, and `in_place_only` was \
                     asked, so no handover was tried"
                );
                let detail = serde_json::json!({ "environment": engine.environment, "ok": false, "outcome": outcome });
                audit(action, detail).await;
                return refuse(report, StatusCode::INTERNAL_SERVER_ERROR, outcome);
            }
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "a deploy couldn't be applied in place: every interface was put back, and the handover applies it");
                report.steps.push(format!("couldn't apply it in place ({e:#}): put back, applying it by the handover"));
            }
        }
    } else {
        report.steps.extend(handover_because.iter().map(|why| format!("needs the handover: {why}")));
    }

    // The handover: a new engine running the release takes over from this one.
    report.applied = Some("handover");
    let request = crate::handover::Request {
        timeout_secs: READY_WITHIN.as_secs(),
        environment: None,
        asked: crate::handover::Asked {
            by: Some(who.by.clone()),
            reason: reason.clone(),
            release: Some(hex.clone()),
            rollback: asked.rollback,
        },
    };
    let started = Utc::now();
    let mut steps = std::mem::take(&mut report.steps);
    let (ok, text) = asker.ask(request, |line| steps.push(line.to_string())).await?;
    report.steps = steps;
    report.ok = ok;
    let mut detail = serde_json::json!({
        "environment": engine.environment,
        "ok": ok,
        "applied": "handover",
        "reason": reason,
        "forced": report.forced,
        "check": report.gate.as_ref().map(|g| g.check.id),
        "reused_check": report.gate.as_ref().is_some_and(|g| g.reused),
        "outcome": text,
    });
    if asked.rollback {
        detail["undoes"] = serde_json::to_value(&report.replaces).unwrap_or_default();
    }
    audit(action, detail).await;
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
            // The record a release file carries: this environment's first deployment of it here.
            report.place = engine
                .store
                .release_places(&id)
                .await?
                .iter()
                .find(|p| p.here && p.environment == engine.environment)
                .map(PlaceInfo::of);
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
