//! Pulling releases from another engine (0.7.4.5.2): an engine in release mode may name one it
//! pulls from (`DUCTUS_PROMOTE_FROM`, with a token there holding `download`). Prod's engine names
//! test's: its Releases page lists test's releases of the same project, and **Bring here** fetches
//! one with its record of where it went (`GET /releases/{id}/file` on test) and stores it here, as
//! `POST /releases` would. Deploying it is then a deploy like any other, held to this
//! environment's rules (`DUCTUS_DEPLOY_REQUIRES` reads the record the file brought).
//!
//! The direction is the point: a lower environment never holds a key to a higher one. Test's
//! engine can't deploy to prod; prod's token on test can only download.

use super::releases::{MAX_UPLOAD, Stored, store_bytes};
use super::{ApiError, Caller, Eng, Shared};
use crate::deploy::Upstream;
use axum::Json;
use axum::extract::Path;
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

/// How long a call to the other engine may take (a release file at a thousand interfaces is
/// ~150 KB).
const CALL_TIMEOUT: Duration = Duration::from_secs(60);
/// How many of its releases are listed (newest first).
const LISTED: usize = 50;

pub fn routes() -> OpenApiRouter<Shared> {
    OpenApiRouter::new().routes(routes!(list)).routes(routes!(pull))
}

/// The engine this one pulls from, and its releases of this engine's project.
#[derive(Serialize, ToSchema)]
pub struct UpstreamInfo {
    /// Its address (`DUCTUS_PROMOTE_FROM`).
    url: String,
    /// Its environment and Ductus version, as it says (`GET /engine` there).
    environment: String,
    version: String,
    /// Its releases of this project, newest first (at most 50), as it lists them, each with
    /// whether it's stored here already. `stored_here` false: Bring here fetches it.
    releases: Vec<UpstreamRelease>,
}

/// One of its releases, as it lists it (`GET /releases` there).
#[derive(Serialize, Deserialize, ToSchema)]
pub struct UpstreamRelease {
    id: String,
    short: String,
    project: String,
    #[serde(default)]
    made_at: Option<DateTime<Utc>>,
    #[serde(default)]
    made_by: String,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    commit: Option<String>,
    #[serde(default)]
    interfaces: i32,
    stored_at: DateTime<Utc>,
    /// The branches it was made on and the environments it went to, as that engine knows them
    /// (empty from an engine older than 0.7.4.5).
    #[serde(default)]
    branches: Vec<String>,
    #[serde(default)]
    environments: Vec<String>,
    /// Its last check there per environment.
    #[serde(default)]
    checks: Vec<UpstreamCheck>,
    /// Stored on this engine's database already (bringing it again adds only what its record
    /// says that this database doesn't know).
    #[serde(default)]
    stored_here: bool,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct UpstreamCheck {
    environment: String,
    ok: bool,
    at: DateTime<Utc>,
}

/// What a pull did.
#[derive(Serialize, ToSchema)]
pub struct Pulled {
    /// Where it came from.
    from: String,
    environment: String,
    #[serde(flatten)]
    stored: Stored,
}

fn upstream(engine: &Shared) -> Result<&Upstream, ApiError> {
    engine.releases.as_ref().and_then(|r| r.mode.promote_from.as_ref()).ok_or_else(|| {
        ApiError::not_found(
            "this engine pulls from no other: set DUCTUS_PROMOTE_FROM to the engine releases come from (prod's names \
             test's) and DUCTUS_PROMOTE_FROM_TOKEN to a token there with the `download` permission, then reload",
        )
    })
}

fn client() -> Result<reqwest::Client, ApiError> {
    let builder = reqwest::Client::builder()
        .user_agent(concat!("ductus/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        .timeout(CALL_TIMEOUT);
    Ok(crate::tls::http_client(None, builder)?)
}

/// A call to the other engine failed: said with its address, in its own words when it answered.
fn unreachable(from: &Upstream, e: impl std::fmt::Display) -> ApiError {
    ApiError::new(StatusCode::BAD_GATEWAY, format!("the engine this one pulls from ({}) didn't answer: {e}", from.url))
}

/// `GET path` on the other engine, its JSON.
async fn get(client: &reqwest::Client, from: &Upstream, path: &str) -> Result<serde_json::Value, ApiError> {
    let answer = client
        .get(format!("{}{path}", from.api()))
        .bearer_auth(&from.token)
        .send()
        .await
        .map_err(|e| unreachable(from, format!("{e:#}")))?;
    refused(from, &answer)?;
    let body = answer.bytes().await.map_err(|e| unreachable(from, format!("reading its answer: {e:#}")))?;
    serde_json::from_slice(&body).map_err(|e| unreachable(from, format!("reading its answer: {e}")))
}

/// Its refusals, worded for what to do here.
fn refused(from: &Upstream, answer: &reqwest::Response) -> Result<(), ApiError> {
    let status = answer.status();
    if status.is_success() {
        return Ok(());
    }
    let what = match status.as_u16() {
        401 => {
            "it doesn't know the token: make one there with the `download` permission and set DUCTUS_PROMOTE_FROM_TOKEN"
        }
        403 => {
            "its token lacks the `download` permission there: give it (`ductus token create NAME --permission download`)"
        }
        404 => "it has no such release (or no Releases API: it runs an older Ductus)",
        _ => "",
    };
    let text = if what.is_empty() { format!("it answered {status}") } else { format!("it answered {status}: {what}") };
    Err(ApiError::new(StatusCode::BAD_GATEWAY, format!("the engine this one pulls from ({}): {text}", from.url)))
}

/// The engine this one pulls from (`DUCTUS_PROMOTE_FROM`) and its releases of this project, each
/// with whether it's stored here. 404 when this engine pulls from none; 502 when that engine
/// can't be reached or refuses the token.
#[utoipa::path(
    get, path = "/upstream", operation_id = "list_upstream", tag = "releases",
    responses(
        (status = 200, body = UpstreamInfo),
        (status = 404, body = super::ErrorBody, description = "This engine pulls from no other"),
        (status = 502, body = super::ErrorBody, description = "That engine didn't answer, or refused the token")
    )
)]
async fn list(engine: Eng, _caller: Caller) -> Result<Json<UpstreamInfo>, ApiError> {
    let from = upstream(&engine)?;
    let client = client()?;
    let listed = format!("/releases?limit={LISTED}");
    let (other, releases) = tokio::try_join!(get(&client, from, "/engine"), get(&client, from, &listed))?;
    let project = engine.releases.as_ref().and_then(|r| r.project());
    let mut releases: Vec<UpstreamRelease> = serde_json::from_value(releases)
        .map_err(|e| unreachable(from, format!("reading its list of releases: {e}")))?;
    releases.retain(|r| project.is_none_or(|p| r.project == p));
    let ids: Vec<Vec<u8>> = releases.iter().filter_map(|r| crate::deploy::unhex(&r.id)).collect();
    let stored = engine.store.stored_releases(&ids).await?;
    for r in &mut releases {
        r.stored_here = crate::deploy::unhex(&r.id).is_some_and(|id| stored.contains(&id));
    }
    Ok(Json(UpstreamInfo {
        url: from.url.clone(),
        environment: other["environment"].as_str().unwrap_or_default().to_string(),
        version: other["version"].as_str().unwrap_or_default().to_string(),
        releases,
    }))
}

/// Bring a release here from the engine this one pulls from: its file (with its record of where it
/// went, brought up to date there) is fetched and stored, as `POST /releases` stores one; a release
/// stored here before keeps its first manifest and gains the records it didn't have. Nothing is
/// deployed. Needs the `deploy` permission here; audited as `release.pull` (when new).
#[utoipa::path(
    post, path = "/upstream/releases/{id}", operation_id = "pull_release", tag = "releases",
    params(("id" = String, Path, description = "The release id there, or its first 12 characters")),
    responses(
        (status = 201, body = Pulled, description = "Brought here and stored"),
        (status = 200, body = Pulled, description = "Stored here before: only records of where it went were added"),
        (status = 404, body = super::ErrorBody, description = "This engine pulls from no other"),
        (status = 400, body = super::ErrorBody, description = "The file it sent isn't a release, or is damaged"),
        (status = 502, body = super::ErrorBody, description = "That engine didn't answer, or refused"),
        (status = 403, body = super::ErrorBody)
    )
)]
async fn pull(engine: Eng, caller: Caller, Path(id): Path<String>) -> Result<axum::response::Response, ApiError> {
    use axum::response::IntoResponse;
    caller.require_permission("deploy")?;
    let from = upstream(&engine)?;
    if id.len() < 4 || id.len() > 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ApiError::bad_request(format!(
            "`{id}` isn't a release id: give its first 12 characters (or more)"
        )));
    }
    let client = client()?;
    let other = get(&client, from, "/engine").await?;
    let environment = other["environment"].as_str().unwrap_or_default().to_string();
    let answer = client
        .get(format!("{}/releases/{id}/file", from.api()))
        .bearer_auth(&from.token)
        .send()
        .await
        .map_err(|e| unreachable(from, format!("{e:#}")))?;
    refused(from, &answer)?;
    if answer.content_length().is_some_and(|n| n as usize > MAX_UPLOAD) {
        return Err(ApiError::bad_request("the release it sent is larger than this engine takes (256 MB)"));
    }
    let body = answer.bytes().await.map_err(|e| unreachable(from, format!("reading the release: {e:#}")))?;
    let audit =
        caller.audit("release.pull").detail(serde_json::json!({ "from": from.url, "environment": environment }));
    let stored = store_bytes(&engine, &caller, body, audit, "the release it sent").await?;
    let status = if stored.new { StatusCode::CREATED } else { StatusCode::OK };
    Ok((status, Json(Pulled { from: from.url.clone(), environment, stored })).into_response())
}
