//! Deploys (0.7.4.4): `POST /deployments` replaces the release an engine in release mode runs with
//! another stored here, through the handover: receiving never stops, sending pauses while the
//! engines swap (well under a second at a thousand interfaces).
//!
//! **In place (0.7.5):** when the release changes only interfaces' code, destinations, destination
//! settings and sources (`changes::plan`, from the two manifests), it's applied without a handover
//! (`deploy::apply_in_place`): each changed interface's workers restart between two messages on the
//! new code, the others never pause, and a deployment `how = deploy` is recorded; a changed source
//! moves, swaps or restarts alone (0.7.5.2.2), and interfaces added or removed start or stop alone
//! (0.7.5.2.1, `sources`). Anything else (how messages are stored, lanes, the project's own files, a
//! port moving between interfaces) goes by the handover, the answer saying which change needed it;
//! `in_place_only` refuses instead.
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

use super::releases::{CheckInfo, TURN_WITHIN, check_info, find, keep, run_check, turn};
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
/// start, tens of milliseconds after it sends; the answer goes as soon as it's there). 3 s was
/// once too short on a busy database (a test run with three engines), and without the record the
/// release file isn't stamped with where it went: a deploy already takes seconds, so wait longer.
const RECORDED_WITHIN: Duration = Duration::from_secs(15);

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
    /// Only these interfaces of the release (0.7.5.3.2, `ductus deploy R --interface A`): the
    /// engine composes a release of its own from what runs with their folders and the code they
    /// load taken from `release` (an interface it lacks is removed, one only it has is added),
    /// stores it, and deploys that; everything else stays as it runs, the project's own files
    /// too. Refused (409) when that would change another interface (a shared file it needs
    /// differs, and others run it), naming them. Not with `rollback`.
    #[serde(default)]
    interfaces: Vec<String>,
}

/// What a deploy did, or why it didn't.
#[derive(Serialize, ToSchema)]
pub struct DeployReport {
    /// The new release runs here.
    ok: bool,
    /// What happened, in a sentence.
    outcome: String,
    /// How long it waited for the deploys and checks sent before it to this engine (one at a
    /// time, in turn); 0 when none ran.
    waited_ms: u64,
    /// The release, in hex, and its first 12 characters: the one composed when `interfaces` was
    /// asked (`composed` says what of).
    release: String,
    short: String,
    /// A deploy of some interfaces only (`interfaces`): what was taken from which release.
    #[serde(skip_serializing_if = "Option::is_none")]
    composed: Option<ComposedInfo>,
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

/// A release composed for a deploy of some interfaces (`DeployRequest::interfaces`).
#[derive(Serialize, ToSchema)]
pub struct ComposedInfo {
    /// The release they were taken from, in hex, and its first 12 characters.
    from: String,
    from_short: String,
    /// The release that ran when it was composed (every other interface, and the project's own
    /// files, are its), first 12 characters.
    over_short: String,
    /// Each interface taken, and what it does: `changed`, `added`, `removed` or `unchanged` (its
    /// version there is the one running).
    interfaces: BTreeMap<String, String>,
    /// The project's own files the release has that differ from what runs: kept as they run
    /// (deploy the release whole to bring them).
    project_differs: Vec<String>,
    /// The composition is that release itself (it differs from what runs only in those
    /// interfaces), so no release of its own was made.
    is_from: bool,
    /// Stored now (false: composed and stored before, the same files).
    stored: bool,
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
    /// Why it needs the handover, a line per reason (the project's own files read at a start:
    /// ductus.yaml, resources.yaml, docs-schema.yaml, a secrets file); empty: it applies in place
    /// (every change to an interface does, some restarting it alone: `changes[].restarts`).
    handover_because: Vec<String>,
    /// What the project's own files change in place, a line each: the engine's environment file's
    /// variables (`environments/prod.yaml: MLLP_PORT "6661" → "6666"`) and `console:`, another
    /// environment's file changing nothing here.
    project: Vec<String>,
}

#[derive(Serialize, ToSchema)]
pub struct PlannedChange {
    interface: String,
    /// What changed, in words: `code`, `destinations.lab`, `destination lab added`, `retry`,
    /// `source`, `source (MLLP_PORT)` (a variable's new value moved it), `code reads EMPI_URL`
    /// (reloaded for a variable its code names), `added`, `removed`, ...
    what: Vec<String>,
    /// The environment variables whose new values reach it, with their values before and after;
    /// absent when none do.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    variables: Vec<crate::changes::VariableChange>,
    /// Why it restarts alone (what a start sets up for it changes: `format`, `ordering`,
    /// `workers`...; its workers all stop, then start on the new settings, its senders never
    /// notice); absent when its workers take the change between two messages.
    #[serde(skip_serializing_if = "Option::is_none")]
    restarts: Option<String>,
}

impl PlanInfo {
    pub(crate) fn of(plan: &crate::changes::Plan) -> Self {
        use crate::changes::Apply;
        Self {
            changes: plan
                .changes
                .iter()
                .map(|c| PlannedChange {
                    interface: c.interface.clone(),
                    what: c.what.clone(),
                    variables: c.reached.as_ref().map(|r| r.variables.clone()).unwrap_or_default(),
                    restarts: match &c.apply {
                        Apply::InPlace => None,
                        Apply::Restart(why) => Some(why.clone()),
                    },
                })
                .collect(),
            handover_because: plan.handover_because(),
            project: plan.project_in_place.clone(),
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
    /// Only these interfaces of it went there (a release composed from it: `ductus deploy R
    /// --interface A`); absent: the whole release.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interfaces: Vec<String>,
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
            interfaces: p.interfaces.clone(),
        }
    }
}

/// Deploy a stored release to this engine (release mode): this environment's rules, the "replaces"
/// check, what it changes here (`plan`), the gate (or a recent passing check reused), then **in
/// place** when only interfaces' code, destinations, destination settings and sources change, or
/// interfaces are added or removed (those interfaces' workers and sources restart, move, start or
/// stop between two messages; the others never pause), else
/// the handover to an engine running it (`plan.handover_because` says why); or roll back
/// (`rollback`) to a release that ran here before, the same way. With `interfaces`, only those
/// interfaces' work from the release: a release composed from what runs with theirs is stored and
/// deployed (`composed` says what it took). Answers once it runs (in place: milliseconds; a
/// handover: when the new engine sends), or when it couldn't (this one goes on as it was), plus
/// the gate's time when it runs. **One deploy or check at a time per engine, in turn**: one sent
/// while another runs waits (`waited_ms`), up to 5 minutes, then reads what runs. 200: deployed
/// (`applied` says how). 409: refused before anything ran (folder mode, 5 minutes waited for the
/// deploys before it, the release already runs, this environment's rules, its "replaces" check
/// names someone's work, a rollback to a release that never ran here, `in_place_only` and it needs
/// the handover, `interfaces` that would change another interface or that nothing runs under yet),
/// the report says why. 422: the gate found problems. 500: it couldn't be applied (this engine
/// goes on as it was). Needs the `deploy` permission.
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
    let mut only: Vec<String> =
        asked.interfaces.iter().map(|i| i.trim().to_string()).filter(|i| !i.is_empty()).collect();
    only.sort();
    only.dedup();
    if !only.is_empty() && asked.rollback {
        return Err(ApiError::bad_request(
            "a rollback goes back to a whole release that ran here: leave `interfaces` out (to bring back some \
             interfaces only, deploy the old release by its id with `interfaces`)",
        ));
    }

    // One deploy or check at a time on this engine, in the order they were sent (Q4): one sent
    // while another runs waits here, then reads what runs once its turn comes (so a composed deploy
    // composes over the one before it).
    let holder = match &asked.release {
        Some(r) if asked.rollback => format!("a rollback to {r} by {}", caller.name()),
        Some(r) => format!("a deploy of {r} by {}", caller.name()),
        None => format!("a rollback by {}", caller.name()),
    };
    let one_at_a_time = turn(holder, TURN_WITHIN).await.map_err(|why| ApiError::new(StatusCode::CONFLICT, why))?;
    let waited = one_at_a_time.waited;

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
        waited_ms: waited.as_millis() as u64,
        short: release::short(&hex).to_string(),
        release: hex.clone(),
        composed: None,
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
    // Some interfaces only: a release of its own, composed from what runs with theirs from the
    // release named (held to this environment's rules as that release's, below).
    let named = id.clone();
    let named_manifest = manifest.clone();
    let (id, manifest, ancestry) = if only.is_empty() {
        (id, manifest, ancestry)
    } else {
        match composed(&engine, releases, &caller, (&named, &manifest, &ancestry), &only, reason.clone()).await? {
            Ok((id, manifest, ancestry, info)) => {
                let hex = crate::code::hex(&id);
                report.short = release::short(&hex).to_string();
                report.release = hex;
                report.composed = Some(info);
                (id, manifest, ancestry)
            }
            Err((status, outcome)) => return refuse(report, status, outcome),
        }
    };
    let hex = report.release.clone();
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
    let action = if asked.rollback { "release.rollback" } else { "release.deploy" };
    let from = current.as_ref().map(|c| release::short(&crate::code::hex(c)).to_string());
    let short = report.short.clone();
    let composed_detail =
        report.composed.as_ref().map(|c| serde_json::json!({ "from": c.from_short, "interfaces": c.interfaces }));
    let audit = |action: &'static str, mut detail: serde_json::Value| {
        let (engine, by) = (engine.clone(), caller.audit(action));
        let short = short.clone();
        if action == "release.rollback" {
            detail["from"] = from.clone().into();
        }
        if let Some(composed) = &composed_detail {
            detail["composed"] = composed.clone();
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
        // A composed deploy is held to them as the release its interfaces come from.
        let places = engine.store.release_places(&named).await?;
        report.rules = deploy::rules(&releases.mode, &named_manifest, &branches, &places, &only);
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
    let who = Who {
        how: "handover",
        by: caller.name().to_string(),
        reason: reason.clone(),
        rollback: asked.rollback,
        variables: Vec::new(),
    };
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
            let environment = engine.environment.clone();
            let started = std::time::Instant::now();
            let plan = tokio::task::spawn_blocking(move || deploy::plan_over(&dir, &r, &next, &folder, &environment))
                .await
                .map_err(anyhow::Error::from)??;
            tracing::debug!(changes = plan.changes.len(), took = ?started.elapsed(), "deploy: what it changes here worked out");
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
    let reached = plan.as_ref().map(crate::changes::Plan::reached).unwrap_or_default();
    let deployer = Who::deploy(caller.name(), reason.clone(), asked.rollback, reached.clone());
    let warnings: std::collections::HashMap<String, String> =
        report.replaces.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    if let Some(plan) = plan.as_ref().filter(|_| handover_because.is_empty()) {
        let next = std::sync::Arc::new(deploy::Running { id: id.clone(), manifest: manifest.clone(), ancestry });
        let env = std::sync::Arc::new(ductus_core::Environment::load(&folder, &engine.environment)?);
        match deploy::apply_in_place(&engine, plan, &manifest, Some(next), &folder, env, &deployer, &warnings).await {
            Ok(done) => {
                report.ok = true;
                report.applied = Some("in_place");
                report.steps = done.steps;
                report.outcome = in_place_outcome(&plan.changes);
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
                    report.place = place_here(&engine, &named, &only, d.id).await?;
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
            // What a start would refuse too (another engine runs an added interface, or the set it
            // would leave running doesn't pass the start's checks): no handover is tried.
            Err(e) if e.downcast_ref::<crate::ownership::Refused>().is_some() => {
                let outcome = format!("refused: {e:#}\nNothing changed: every interface runs what it ran.");
                let detail = serde_json::json!({ "environment": engine.environment, "ok": false, "outcome": outcome });
                audit(action, detail).await;
                return refuse(report, StatusCode::CONFLICT, outcome);
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
            variables: reached,
        },
        ..Default::default()
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
            report.place = place_here(&engine, &named, &only, deployment).await?;
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

/// The record a release file carries of this deploy: this environment's first deployment of the
/// release here, or (a composed deploy, `only`) of a release composed from it with at least those
/// interfaces, this deployment's when it's one of them. What `ductus deploy` writes into the file
/// it deployed, so the next environment that requires this one reads it.
async fn place_here(
    engine: &Eng,
    release: &[u8],
    only: &[String],
    deployment: i32,
) -> Result<Option<PlaceInfo>, ApiError> {
    let places = engine.store.release_places(release).await?;
    let here = places.iter().filter(|p| p.here && p.environment == engine.environment);
    let found = match only {
        [] => here.filter(|p| p.interfaces.is_empty()).min_by_key(|p| p.deployment),
        _ => {
            let covers: Vec<_> = here.filter(|p| only.iter().all(|i| p.interfaces.contains(i))).collect();
            covers.iter().find(|p| p.deployment == deployment).or(covers.first()).copied()
        }
    };
    Ok(found.map(PlaceInfo::of))
}

/// The release a deploy of some interfaces runs (`DeployRequest::interfaces`): what runs here
/// with those interfaces taken from `named` (`compose`), stored as a release of its own, or
/// `named` itself when it differs from what runs only in them. Its id, manifest and ancestry (the
/// named release's: where the interfaces taken came from), and what it took. Err: the refusal
/// (status and outcome).
async fn composed(
    engine: &Eng,
    releases: &deploy::Releases,
    caller: &Caller,
    (named, named_manifest, named_ancestry): (&[u8], &Manifest, &[u8]),
    only: &[String],
    reason: Option<String>,
) -> Result<Result<(Vec<u8>, Manifest, Vec<u8>, ComposedInfo), (StatusCode, String)>, ApiError> {
    let short = release::short(&named_manifest.id).to_string();
    let Some(running) = releases.running() else {
        return Ok(Err((
            StatusCode::CONFLICT,
            format!(
                "nothing runs here yet, so there's nothing to take interfaces over: deploy release {short} whole first \
                 (`ductus deploy {short}`)"
            ),
        )));
    };
    let files = |id: Vec<u8>| async move {
        let contents =
            engine.store.release_contents(&id).await?.ok_or_else(|| {
                ApiError::not_found(format!("release {} is no longer stored", crate::code::short(&id)))
            })?;
        Ok::<_, ApiError>(
            contents.files.into_iter().map(|(path, bytes, _)| release::File::new(path, bytes)).collect::<Vec<_>>(),
        )
    };
    let started = std::time::Instant::now();
    let (over_files, from_files) = (files(running.id.clone()).await?, files(named.to_vec()).await?);
    let read = started.elapsed();
    let made = release::Made {
        at: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        by: caller.name().to_string(),
        host: engine.host.clone(),
        reason,
        binary: env!("CARGO_PKG_VERSION").to_string(),
        platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
    };
    let (over_manifest, from_manifest, asked) = (running.manifest.clone(), named_manifest.clone(), only.to_vec());
    let composition = tokio::task::spawn_blocking(move || {
        let over = crate::compose::Side { manifest: &over_manifest, files: &over_files };
        let from = crate::compose::Side { manifest: &from_manifest, files: &from_files };
        crate::compose::compose(over, from, &asked, made)
    })
    .await
    .map_err(anyhow::Error::from)?;
    let c = match composition {
        Ok(c) => c,
        Err(why) => return Ok(Err((StatusCode::CONFLICT, format!("refused: {why}")))),
    };
    let mut info = ComposedInfo {
        from: named_manifest.id.clone(),
        from_short: short.clone(),
        over_short: release::short(&running.manifest.id).to_string(),
        interfaces: c.taken.iter().map(|(n, what)| (n.clone(), what.to_string())).collect(),
        project_differs: c.project_differs.clone(),
        is_from: c.manifest.id == named_manifest.id,
        stored: false,
    };
    if c.manifest.id == running.manifest.id {
        let names = only.iter().map(|n| format!("`{n}`")).collect::<Vec<_>>().join(", ");
        return Ok(Err((
            StatusCode::CONFLICT,
            format!("{names} in release {short} is what runs here already: nothing to do"),
        )));
    }
    if info.is_from {
        return Ok(Ok((named.to_vec(), named_manifest.clone(), named_ancestry.to_vec(), info)));
    }
    let audit = caller.audit("release.store");
    let stored = keep(engine, caller.name(), &c.manifest, named_ancestry, &c.files, &audit).await?;
    info.stored = stored.new;
    tracing::debug!(
        interfaces = ?only,
        files = c.files.len(),
        read = ?read,
        took = ?started.elapsed(),
        "deploy: a release composed and stored"
    );
    let id = deploy::unhex(&c.manifest.id).ok_or_else(|| anyhow::anyhow!("a composed release's id isn't hex"))?;
    // As stored: composed before (the same files), or made by `ductus release` with them, its
    // manifest is the one kept.
    let (manifest, ancestry) =
        engine.store.release_manifest(&id).await?.ok_or_else(|| anyhow::anyhow!("the release composed is gone"))?;
    let manifest: Manifest = serde_json::from_value(manifest)
        .map_err(|e| anyhow::anyhow!("reading the composed release's manifest: {e}"))?;
    Ok(Ok((id, manifest, ancestry, info)))
}

/// "1 h", "15 min", "90 s".
fn span(d: Duration) -> String {
    match d.as_secs() {
        s if s >= 3600 && s % 3600 == 0 => format!("{} h", s / 3600),
        s if s >= 60 && s % 60 == 0 => format!("{} min", s / 60),
        s => format!("{s} s"),
    }
}

/// What a deploy applied in place did, by kind: interfaces changed (their new code between two
/// messages), sources changed (a moved port opens before the old one closes, a pickup restarts:
/// not "between two messages"), restarted alone (what a start sets up changed: 0.7.5.2.4), added
/// (started) and removed (stopped).
pub(crate) fn in_place_outcome(changes: &[crate::changes::Change]) -> String {
    use crate::changes::Apply;
    let added = changes.iter().filter(|c| c.is_added()).count();
    let removed = changes.iter().filter(|c| c.is_removed()).count();
    let restarts = |c: &&crate::changes::Change| matches!(c.apply, Apply::Restart(_));
    let restarted = changes.iter().filter(restarts).count();
    // `source`, or `source (MLLP_PORT)` when a variable moved it.
    let source = |w: &String| w == "source" || w.starts_with("source (");
    let sources = changes
        .iter()
        .filter(|c| !c.is_added() && !c.is_removed() && !restarts(c) && c.what.iter().any(source))
        .count();
    let changed = changes.len() - added - removed - sources - restarted;
    let mut parts: Vec<String> = Vec::new();
    match changed {
        0 => {}
        1 => parts.push("1 interface took its new code between two messages".into()),
        n => parts.push(format!("{n} interfaces took their new code between two messages")),
    }
    match (sources, parts.is_empty()) {
        (0, _) => {}
        (1, true) => parts.push("1 interface's source changed".into()),
        (n, true) => parts.push(format!("{n} interfaces' sources changed")),
        (1, false) => parts.push("1 changed its source".into()),
        (n, false) => parts.push(format!("{n} changed their sources")),
    }
    // The first part names what's counted; the ones after it don't need to.
    let noun = |n: usize, first: bool| match (n, first) {
        (1, true) => "1 interface was".to_string(),
        (n, true) => format!("{n} interfaces were"),
        (1, false) => "1 was".to_string(),
        (n, false) => format!("{n} were"),
    };
    if restarted > 0 {
        parts.push(format!("{} restarted alone on new settings", noun(restarted, parts.is_empty())));
    }
    if added > 0 {
        parts.push(format!("{} added and started", noun(added, parts.is_empty())));
    }
    if removed > 0 {
        parts.push(format!("{} removed and stopped", noun(removed, parts.is_empty())));
    }
    match parts.is_empty() {
        true => "applied in place: no interface's code or settings changed".to_string(),
        false => format!("applied in place: {}; the others never paused", parts.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::in_place_outcome;
    use crate::changes::{Apply, Change};

    /// The answer says what changed, what started and what stopped, in that order.
    #[test]
    fn a_deploy_in_place_says_what_it_changed_added_and_removed() {
        let change = |what: &str| Change {
            interface: "x".into(),
            what: vec![what.to_string()],
            apply: Apply::InPlace,
            reached: None,
        };
        let said = |c: &[Change]| in_place_outcome(c);
        assert_eq!(said(&[]), "applied in place: no interface's code or settings changed");
        assert_eq!(
            said(&[change("source (MLLP_PORT)")]),
            "applied in place: 1 interface's source changed; the others never paused"
        );
        assert_eq!(
            said(&[change("code")]),
            "applied in place: 1 interface took its new code between two messages; the others never paused"
        );
        assert_eq!(
            said(&[change("source")]),
            "applied in place: 1 interface's source changed; the others never paused"
        );
        assert_eq!(
            said(&[change("code"), change("source"), change("source")]),
            "applied in place: 1 interface took its new code between two messages, 2 changed their sources; the others never paused"
        );
        assert_eq!(
            said(&[change("added")]),
            "applied in place: 1 interface was added and started; the others never paused"
        );
        assert_eq!(
            said(&[change("code"), change("removed")]),
            "applied in place: 1 interface took its new code between two messages, 1 was removed and stopped; the others never paused"
        );
        assert_eq!(
            said(&[change("added"), change("added"), change("removed"), change("removed")]),
            "applied in place: 2 interfaces were added and started, 2 were removed and stopped; the others never paused"
        );
        let restart = |what: &str| Change { apply: Apply::Restart("`workers` changed".into()), ..change(what) };
        assert_eq!(
            said(&[restart("workers")]),
            "applied in place: 1 interface was restarted alone on new settings; the others never paused"
        );
        assert_eq!(
            said(&[change("code"), restart("source"), restart("format")]),
            "applied in place: 1 interface took its new code between two messages, 2 were restarted alone on new settings; the others never paused"
        );
    }
}
