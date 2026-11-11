//! Releases stored on this engine's database (0.7.4.3.3): CI or a person uploads one made by
//! `ductus release` (`POST /releases`), anyone signed in lists and reads them, and someone with the
//! `deploy` permission checks one against this engine's environment (`POST /releases/{id}/check`):
//! the gate `ductus release check` runs, here, with this engine's settings, secrets and database.
//!
//! A release is read with `release::read_bytes`, which recomputes its id from the files: what's
//! stored is what the id says, whatever the manifest claims. The check rebuilds the archive from
//! what's stored, checks its id again, and runs this binary's `ductus release check --json` on it
//! as a child process at a lower CPU priority (the engine's own threads go first), one check at a
//! time per engine.

use super::{ApiError, ApiResult, Caller, Eng, Shared};
use crate::release::{self, Manifest};
use axum::Json;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

/// The largest release `POST /releases` takes, as sent (gzipped). A project of 1,001 interfaces
/// with tests and fixtures is ~110 KB plus its manifest and history (20 KB per 1,000 commits).
const MAX_UPLOAD: usize = 256 << 20;
/// A check that runs longer than this is stopped (the gate at 1,001 interfaces: seconds).
const CHECK_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const MAX_PAGE: i64 = 500;

/// One check at a time per engine: each one spreads `validate` and the tests over every CPU. A
/// deploy holds it too, from its gate to its end (`api::deployments`).
pub(super) static CHECKING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub fn routes() -> OpenApiRouter<Shared> {
    OpenApiRouter::new()
        .routes(routes!(store, list))
        .routes(routes!(one))
        .routes(routes!(check))
        .layer(DefaultBodyLimit::max(MAX_UPLOAD))
}

/// A release made by `ductus release`: the `.tar.gz` file's bytes, as they are.
#[derive(ToSchema)]
#[schema(value_type = String, format = Binary)]
#[allow(dead_code)]
pub struct ReleaseArchive(Vec<u8>);

#[derive(Serialize, ToSchema)]
pub struct ReleaseInfo {
    /// The release id (SHA-256 over its files), in hex.
    id: String,
    /// Its first 12 characters, as `ductus release` shows it.
    short: String,
    /// The project's folder name.
    project: String,
    /// When and by whom it was made (`ductus release`), and why (`-m`).
    made_at: Option<DateTime<Utc>>,
    made_by: String,
    reason: Option<String>,
    /// The git commit it was made from (none: made without git).
    commit: Option<String>,
    interfaces: i32,
    files: i32,
    /// Its files' bytes, unpacked.
    bytes: i64,
    /// When it was stored here, and by whom.
    stored_at: DateTime<Utc>,
    stored_by: String,
    /// In a list: the last check against each environment (without its report). In a release's
    /// own page: every check, newest first, with their reports.
    checks: Vec<CheckInfo>,
}

#[derive(Serialize, ToSchema)]
pub struct CheckInfo {
    pub(super) id: i32,
    at: DateTime<Utc>,
    /// The environment checked against, and the engine and server that ran it.
    environment: String,
    instance: String,
    host: String,
    by: String,
    /// It passed the gate: no problem found.
    pub(super) ok: bool,
    problems: i32,
    seconds: f32,
    /// `ductus release check --json`'s document (without the manifest: it's the release's); null in lists.
    #[schema(value_type = Object)]
    report: serde_json::Value,
}

#[derive(Serialize, ToSchema)]
pub struct ReleaseDetail {
    #[serde(flatten)]
    release: ReleaseInfo,
    /// The release's manifest (`release.json`): every interface's code version and files, the
    /// project version, the tests' result when it was made, who made it and why.
    #[schema(value_type = Object)]
    manifest: serde_json::Value,
}

#[derive(Serialize, ToSchema)]
pub struct Stored {
    /// False: it was stored before (by `stored_by`, at `stored_at`); nothing changed.
    new: bool,
    #[serde(flatten)]
    release: ReleaseInfo,
    /// Folders and files this release added to the database (the rest were there already).
    new_folders: usize,
    new_files: usize,
}

fn info(r: ductus_store::ReleaseRow) -> ReleaseInfo {
    let id = crate::code::hex(&r.id);
    ReleaseInfo {
        short: release::short(&id).to_string(),
        id,
        project: r.project,
        made_at: r.made_at,
        made_by: r.made_by,
        reason: r.reason,
        commit: r.git_commit,
        interfaces: r.interfaces,
        files: r.files,
        bytes: r.bytes,
        stored_at: r.stored_at,
        stored_by: r.stored_by,
        checks: r.checks.into_iter().map(check_info).collect(),
    }
}

pub(super) fn check_info(c: ductus_store::CheckRow) -> CheckInfo {
    CheckInfo {
        id: c.id,
        at: c.at,
        environment: c.environment,
        instance: c.instance,
        host: c.host,
        by: c.by,
        ok: c.ok,
        problems: c.problems,
        seconds: c.seconds,
        report: c.report,
    }
}

/// Store a release made by `ductus release` (the `.tar.gz` file as the body). Its id is
/// recomputed from its files (a damaged or changed file is refused); one stored already is said
/// so (200) and nothing changes; a new one is 201. Refused when it carries a private key. Needs
/// the `deploy` permission.
#[utoipa::path(
    post, path = "/releases", operation_id = "store_release", tag = "releases",
    request_body(content = ReleaseArchive, content_type = "application/gzip", description = "The release file (`ductus release` wrote it)"),
    responses(
        (status = 201, body = Stored, description = "Stored"),
        (status = 200, body = Stored, description = "Stored before: nothing changed"),
        (status = 400, body = super::ErrorBody, description = "Not a release, damaged, or carrying a private key"),
        (status = 403, body = super::ErrorBody)
    )
)]
async fn store(engine: Eng, caller: Caller, body: Bytes) -> Result<Response, ApiError> {
    caller.require_permission("deploy")?;
    let bundle = tokio::task::spawn_blocking(move || release::read_bytes(&body, "the release sent"))
        .await
        .map_err(|e| anyhow::anyhow!("reading the release: {e}"))?
        .map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
    // `ductus release` refuses these; a release made another way is held to the same rule.
    let keys: Vec<&str> = bundle.files.iter().filter(|f| release::private_key(f)).map(|f| f.path.as_str()).collect();
    if !keys.is_empty() {
        return Err(ApiError::bad_request(format!(
            "the release carries a private key, which would be stored in the clear: {}; keep keys out of the project and name them in resources.yaml by a path outside it",
            keys.join(", ")
        )));
    }
    let m = &bundle.manifest;
    let id = unhex(&m.id).ok_or_else(|| ApiError::bad_request("the release's id isn't hex"))?;
    let hashes: Vec<[u8; 32]> = bundle.files.iter().map(release::File::hash).collect();
    let files: Vec<ductus_store::ReleaseFile> = bundle
        .files
        .iter()
        .zip(&hashes)
        .map(|(f, hash)| ductus_store::ReleaseFile { path: &f.path, hash, bytes: &f.bytes, binary: f.binary })
        .collect();
    let manifest = serde_json::to_value(m).map_err(anyhow::Error::from)?;
    let by = caller.name().to_string();
    let new = ductus_store::NewRelease {
        id: &id,
        project: &m.project,
        manifest: &manifest,
        made_at: DateTime::parse_from_rfc3339(&m.made.at).ok().map(|t| t.with_timezone(&Utc)),
        made_by: &m.made.by,
        reason: m.made.reason.as_deref(),
        git_commit: m.git.as_ref().map(|g| g.commit.as_str()),
        interfaces: m.interfaces.len(),
        ancestry: &bundle.ancestry,
        files: &files,
        by: &by,
    };
    let stored = engine.store.store_release(&new, &caller.audit("release.store")).await?;
    let row = engine.store.release_summary(&id).await?.ok_or_else(|| anyhow::anyhow!("the release stored is gone"))?;
    let status = if stored.new { StatusCode::CREATED } else { StatusCode::OK };
    let answer =
        Stored { new: stored.new, release: info(row), new_folders: stored.new_folders, new_files: stored.new_files };
    Ok((status, Json(answer)).into_response())
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ReleasesQuery {
    /// Releases stored before this time (the previous page's last `stored_at`).
    before: Option<DateTime<Utc>>,
    /// How many, newest first (default 50, at most 500).
    limit: Option<i64>,
}

/// The releases stored on this database, newest first, each with its last check per environment.
#[utoipa::path(
    get, path = "/releases", operation_id = "list_releases", tag = "releases", params(ReleasesQuery),
    responses((status = 200, body = Vec<ReleaseInfo>))
)]
async fn list(engine: Eng, _caller: Caller, Query(q): Query<ReleasesQuery>) -> ApiResult<Vec<ReleaseInfo>> {
    let limit = q.limit.unwrap_or(50).clamp(1, MAX_PAGE);
    let rows = engine.store.releases(q.before, limit).await?;
    Ok(Json(rows.into_iter().map(info).collect()))
}

/// The release whose id starts with `id` (12 characters are enough): 404 when none, 400 when
/// several do.
pub(super) async fn find(engine: &Shared, id: &str) -> Result<Vec<u8>, ApiError> {
    match engine.store.find_release(id).await {
        Ok(Some(found)) => Ok(found),
        Ok(None) => Err(ApiError::not_found(format!(
            "no release `{id}` is stored here: store it first (POST /releases with the file `ductus release` wrote)"
        ))),
        Err(e) => Err(ApiError::bad_request(format!("{e:#}"))),
    }
}

/// One release: what it is, its manifest, and every check of it (newest first, with reports).
#[utoipa::path(
    get, path = "/releases/{id}", operation_id = "get_release", tag = "releases",
    params(("id" = String, Path, description = "The release id, or its first 12 characters (at least 4)")),
    responses((status = 200, body = ReleaseDetail), (status = 404, body = super::ErrorBody), (status = 400, body = super::ErrorBody))
)]
async fn one(engine: Eng, _caller: Caller, Path(id): Path<String>) -> ApiResult<ReleaseDetail> {
    let id = find(&engine, &id).await?;
    let (row, manifest) =
        engine.store.release(&id).await?.ok_or_else(|| ApiError::not_found("that release is no longer stored"))?;
    Ok(Json(ReleaseDetail { release: info(row), manifest }))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct CheckQuery {
    /// Run every interface's tests, not only those of what the release changes here.
    #[serde(default)]
    all_tests: bool,
}

/// Check a stored release against this engine's environment: `ductus release check` runs here,
/// with this engine's settings, secrets and database (read-only), as a child process at a lower
/// CPU priority. `validate`, then the tests of the interfaces the release changes here. Nothing
/// is changed but the check's record (and an audit row). One check at a time per engine (409
/// while one runs). Answers when it ends (seconds at a thousand interfaces); `ok` says whether it
/// passed. Needs the `deploy` permission: the release's code runs with this environment's secrets.
#[utoipa::path(
    post, path = "/releases/{id}/check", operation_id = "check_release", tag = "releases",
    params(("id" = String, Path, description = "The release id, or its first 12 characters"), CheckQuery),
    responses(
        (status = 200, body = CheckInfo, description = "The check ran: `ok` says whether it passed"),
        (status = 404, body = super::ErrorBody),
        (status = 409, body = super::ErrorBody, description = "Another check is running on this engine"),
        (status = 403, body = super::ErrorBody)
    )
)]
async fn check(
    engine: Eng,
    caller: Caller,
    Path(id): Path<String>,
    Query(q): Query<CheckQuery>,
) -> ApiResult<CheckInfo> {
    caller.require_permission("deploy")?;
    let id = find(&engine, &id).await?;
    let Ok(_one_at_a_time) = CHECKING.try_lock() else {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "another release check or deploy is running on this engine: try again when it ends",
        ));
    };
    Ok(Json(run_check(&engine, &caller, &id, q.all_tests).await?))
}

/// Run the gate on stored release `id` against this engine's environment and record the check
/// (with its `release.check` audit row). The caller holds `CHECKING`.
pub(super) async fn run_check(
    engine: &Shared,
    caller: &Caller,
    id: &[u8],
    all_tests: bool,
) -> Result<CheckInfo, ApiError> {
    let id = id.to_vec();
    let contents = engine
        .store
        .release_contents(&id)
        .await?
        .ok_or_else(|| ApiError::not_found("that release is no longer stored"))?;
    let started = Instant::now();
    let report = gate(engine, &id, contents, all_tests).await?;
    // Hundredths, as `ductus release check --json` says them.
    let seconds = (started.elapsed().as_secs_f32() * 100.0).round() / 100.0;
    let ok = report["ok"] == true;
    let problems = report["problems"].as_i64().unwrap_or(if ok { 0 } else { 1 }) as i32;
    let by = caller.name().to_string();
    let new = ductus_store::NewCheck {
        release: &id,
        environment: &engine.environment,
        instance: &engine.instance,
        host: &engine.host,
        by: &by,
        ok,
        problems,
        seconds,
        report: &report,
        ductus_version: env!("CARGO_PKG_VERSION"),
    };
    let check_id = engine.store.record_check(&new, &caller.audit("release.check")).await?;
    Ok(CheckInfo {
        id: check_id,
        at: Utc::now(),
        environment: engine.environment.clone(),
        instance: engine.instance.clone(),
        host: engine.host.clone(),
        by,
        ok,
        problems,
        seconds,
        report,
    })
}

/// Write the stored release as an archive again (its id checked) and run `ductus release check
/// --json` on it: the report, without the manifest.
async fn gate(
    engine: &Shared,
    id: &[u8],
    contents: ductus_store::ReleaseContents,
    all_tests: bool,
) -> Result<serde_json::Value, ApiError> {
    let folder = tempfile::Builder::new()
        .prefix("ductus-check-")
        .tempdir()
        .map_err(|e| anyhow::anyhow!("making a temporary folder for the check: {e}"))?;
    let path = folder.path().join("release.tar.gz");
    let expected = crate::code::hex(id);
    {
        let path = path.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let files: Vec<release::File> =
                contents.files.into_iter().map(|(path, bytes, _)| release::File::new(path, bytes)).collect();
            let recomputed = crate::code::hex(&release::id(&files));
            if recomputed != expected {
                anyhow::bail!(
                    "the release stored as {} reads back as {}: the database was changed by hand",
                    release::short(&expected),
                    release::short(&recomputed)
                );
            }
            let manifest: Manifest =
                serde_json::from_value(contents.manifest).map_err(|e| anyhow::anyhow!("reading its manifest: {e}"))?;
            release::write(&path, &manifest, &contents.ancestry, &files)?;
            Ok(())
        })
        .await
        .map_err(|e| anyhow::anyhow!("writing the release for its check: {e}"))??;
    }

    let mut command = tokio::process::Command::new(crate::handover::executable()?);
    command.args(["release", "check", "--json", "--env", &engine.environment]).arg(&path);
    if all_tests {
        command.arg("--all-tests");
    }
    // The URL in the environment, never on a command line (other users could read its password).
    command
        .env("DUCTUS_DATABASE_URL", &engine.database_url)
        .env_remove("RUST_LOG")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    lower_priority(&mut command);
    let out = tokio::time::timeout(CHECK_TIMEOUT, command.output())
        .await
        .map_err(|_| {
            anyhow::anyhow!("the check ran longer than {} minutes and was stopped", CHECK_TIMEOUT.as_secs() / 60)
        })?
        .map_err(|e| anyhow::anyhow!("running `ductus release check`: {e}"))?;
    let mut report: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|_| {
        anyhow::anyhow!(
            "`ductus release check` didn't answer with its report: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )
    })?;
    if let Some(r) = report.as_object_mut() {
        r.remove("manifest");
    }
    Ok(report)
}

/// The check's process (and the `validate` and `test` it runs, which inherit it) yields the CPU
/// to the engine: nice 10 on Linux, below normal on Windows.
fn lower_priority(command: &mut tokio::process::Command) {
    #[cfg(target_os = "linux")]
    // SAFETY: `setpriority` is async-signal-safe and touches only the child being started.
    unsafe {
        command.pre_exec(|| {
            libc::setpriority(libc::PRIO_PROCESS, 0, 10);
            Ok(())
        });
    }
    #[cfg(windows)]
    {
        const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;
        command.creation_flags(BELOW_NORMAL_PRIORITY_CLASS);
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    let _ = command;
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if text.len() != 64 {
        return None;
    }
    (0..64).step_by(2).map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok()).collect()
}
