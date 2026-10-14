//! The reports the CLI prints, as data: `impact`, `owners` and `doctor`, for the interfaces
//! this engine runs.

use super::{ApiError, ApiResult, Caller, Eng, Shared};
use crate::doctor::{self, Report, Target};
use axum::Json;
use axum::extract::Query;
use ductus_core::InterfaceConfig;
use ductus_core::about::HostQuery;
use ductus_core::impact::{self, Query as ImpactQuery};
use ductus_store::Role;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

pub fn routes() -> OpenApiRouter<Shared> {
    OpenApiRouter::new()
        .routes(routes!(impact_report))
        .routes(routes!(owners))
        .routes(routes!(reassign_preview))
        .routes(routes!(doctor_report))
        .routes(routes!(docs_report))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ImpactParams {
    /// e.g. `Epic Systems` (ignores case).
    vendor: Option<String>,
    /// e.g. `Sunquest Lab` (ignores case).
    application: Option<String>,
    /// An address, subnet (`10.20.30.0/24`) or host name.
    host: Option<String>,
    /// Look host names up in DNS (from the engine's server), so names and addresses match.
    #[serde(default)]
    resolve: bool,
}

#[derive(Serialize, ToSchema)]
pub struct ImpactHit {
    interface: String,
    owner: Option<String>,
    /// `source`, `destination lab`, `database patient_db`, `api empi`.
    role: String,
    /// Why it matched.
    reason: String,
    /// Hosts and ports, listening ports, folders to look at.
    endpoints: Vec<String>,
}

#[derive(Serialize, ToSchema)]
pub struct ImpactReport {
    /// What was looked for, e.g. `vendor "Epic Systems"`.
    query: String,
    /// Interfaces affected, of `interfaces`.
    affected: usize,
    interfaces: usize,
    hits: Vec<ImpactHit>,
    /// With no hits for a vendor or application: the names that are known.
    known: Vec<String>,
    /// Interfaces that don't name every system they connect (they may be missing here).
    incomplete: Vec<String>,
}

/// Which interfaces a vendor, application or host touches, how, and who owns them (for a
/// vendor breach or a VPN/IP change). Exactly one of `vendor`, `application`, `host`.
#[utoipa::path(
    get, path = "/impact", tag = "reports", params(ImpactParams),
    responses((status = 200, body = ImpactReport), (status = 400, body = super::ErrorBody))
)]
async fn impact_report(engine: Eng, _caller: Caller, Query(p): Query<ImpactParams>) -> ApiResult<ImpactReport> {
    let query = match (p.vendor, p.application, p.host) {
        (Some(v), None, None) => ImpactQuery::Vendor(v),
        (None, Some(a), None) => ImpactQuery::Application(a),
        (None, None, Some(h)) => {
            ImpactQuery::Host(HostQuery::parse(&h).map_err(|e| ApiError::bad_request(format!("{e:#}")))?)
        }
        _ => return Err(ApiError::bad_request("give exactly one of `vendor`, `application` or `host`")),
    };
    let configs: Vec<&InterfaceConfig> = engine.interfaces.iter().map(|i| &*i.config).collect();
    let code = engine.code_uses().await?;
    let resolve = p.resolve;
    let lookup = move |name: &str| -> Vec<std::net::IpAddr> {
        use std::net::ToSocketAddrs;
        if !resolve {
            return Vec::new();
        }
        let mut ips: Vec<_> = (name, 0).to_socket_addrs().map(|a| a.map(|s| s.ip()).collect()).unwrap_or_default();
        ips.sort();
        ips.dedup();
        ips
    };
    // DNS lookups block: off the async threads.
    let hits = tokio::task::block_in_place(|| {
        impact::impact(configs.iter().copied(), &engine.resources, code, &query, &lookup)
    });
    let affected: std::collections::BTreeSet<&str> = hits.iter().map(|h| h.interface.as_str()).collect();
    let known = match &query {
        ImpactQuery::Vendor(_) | ImpactQuery::Application(_) if hits.is_empty() => {
            impact::known_values(configs.iter().copied(), &engine.resources, &query)
        }
        _ => Vec::new(),
    };
    Ok(Json(ImpactReport {
        query: query.to_string(),
        affected: affected.len(),
        interfaces: configs.len(),
        known,
        incomplete: configs
            .iter()
            .filter(|c| !impact::about_gaps(c, &engine.resources).is_empty())
            .map(|c| c.name.clone())
            .collect(),
        hits: hits
            .into_iter()
            .map(|h| ImpactHit {
                interface: h.interface,
                owner: (!h.owner.is_empty()).then_some(h.owner),
                role: h.role,
                reason: h.reason,
                endpoints: h.endpoints,
            })
            .collect(),
    }))
}

#[derive(Serialize, ToSchema)]
pub struct OwnerGroup {
    /// Absent for what nobody owns (listed last).
    owner: Option<String>,
    owns: Vec<Owned>,
}

#[derive(Serialize, ToSchema)]
pub struct Owned {
    interface: String,
    /// Set for a destination owned by someone other than its interface's owner.
    destination: Option<String>,
}

/// Who owns which interface (`about.owner`) and destination (`owner` on one someone else owns).
#[utoipa::path(get, path = "/owners", tag = "reports", responses((status = 200, body = Vec<OwnerGroup>)))]
async fn owners(engine: Eng, _caller: Caller) -> ApiResult<Vec<OwnerGroup>> {
    let items = ductus_core::owners::owned(engine.interfaces.iter().map(|i| &*i.config));
    Ok(Json(
        ductus_core::owners::by_owner(items)
            .into_iter()
            .map(|(owner, items)| OwnerGroup {
                owner: (!owner.is_empty()).then_some(owner),
                owns: items.into_iter().map(|o| Owned { interface: o.interface, destination: o.destination }).collect(),
            })
            .collect(),
    ))
}

#[derive(Deserialize, ToSchema)]
pub struct ReassignRequest {
    /// The current owner (ignores case).
    from: String,
    /// The new owner.
    to: String,
    /// Only these interfaces (all when empty).
    #[serde(default)]
    interfaces: Vec<String>,
}

#[derive(Serialize, ToSchema)]
pub struct ReassignPreview {
    /// Files that would change, each with its changed lines.
    files: Vec<ReassignFile>,
    /// Files with `owner: <from>` that can't be changed safely by a program (an unusual
    /// layout): change them by hand. While any is listed, `patch` and `command` are empty.
    refused: Vec<ReassignRefused>,
    /// Owner values that would change, over all files.
    changed: usize,
    /// The whole change as a unified diff, for `git apply` in the project's repository.
    patch: String,
    /// The same change made with the CLI in the project's folder.
    command: String,
    /// The owners there are, when nothing matched `from`.
    known: Vec<String>,
}

#[derive(Serialize, ToSchema)]
pub struct ReassignFile {
    interface: String,
    /// Relative to the project, `/`-separated.
    path: String,
    lines: Vec<ReassignLine>,
}

#[derive(Serialize, ToSchema)]
pub struct ReassignLine {
    /// 1-based.
    line: usize,
    before: String,
    after: String,
}

#[derive(Serialize, ToSchema)]
pub struct ReassignRefused {
    interface: String,
    path: String,
    error: String,
}

/// What moving ownership from one owner to another would change, as `ductus owners reassign
/// --dry-run` shows it, plus a patch to apply in the project's repository. Writes nothing: the
/// interfaces' files are the project's (in git), so the change is made there and deployed. Reads
/// the files as they are on the engine's server now.
#[utoipa::path(
    post, path = "/owners/reassign/preview", tag = "reports", request_body = ReassignRequest,
    responses((status = 200, body = ReassignPreview), (status = 400, body = super::ErrorBody))
)]
async fn reassign_preview(
    engine: Eng,
    _caller: Caller,
    Json(request): Json<ReassignRequest>,
) -> ApiResult<ReassignPreview> {
    let unknown: Vec<&str> =
        request.interfaces.iter().filter(|n| engine.interface(n).is_none()).map(String::as_str).collect();
    if !unknown.is_empty() {
        return Err(ApiError::bad_request(format!("no interface named {}", unknown.join(", "))));
    }
    if request.from.trim().is_empty() {
        return Err(ApiError::bad_request("say whose interfaces move (`from`)"));
    }
    let mut preview = ReassignPreview {
        files: Vec::new(),
        refused: Vec::new(),
        changed: 0,
        patch: String::new(),
        command: String::new(),
        known: Vec::new(),
    };
    let mut patch = String::new();
    for i in engine
        .interfaces
        .iter()
        .filter(|i| request.interfaces.is_empty() || request.interfaces.contains(&i.config.name))
    {
        let file = i.config.dir.join("interface.yaml");
        let path = file.strip_prefix(&engine.root).unwrap_or(&file).display().to_string().replace('\\', "/");
        let text = tokio::fs::read_to_string(&file)
            .await
            .map_err(|e| ApiError::bad_request(format!("reading {path}: {e}")))?;
        match ductus_core::owners::reassign(&text, &request.from, &request.to) {
            Ok((_, changes)) if changes.is_empty() => {}
            Ok((out, changes)) => {
                preview.changed += changes.len();
                patch.push_str(&ductus_core::owners::patch(&path, &text, &out));
                preview.files.push(ReassignFile {
                    interface: i.config.name.clone(),
                    path,
                    lines: changes
                        .into_iter()
                        .map(|c| ReassignLine { line: c.line, before: c.before, after: c.after })
                        .collect(),
                });
            }
            Err(e) => preview.refused.push(ReassignRefused {
                interface: i.config.name.clone(),
                path,
                error: format!("{e:#}"),
            }),
        }
    }
    if preview.files.is_empty() && preview.refused.is_empty() {
        let groups =
            ductus_core::owners::by_owner(ductus_core::owners::owned(engine.interfaces.iter().map(|i| &*i.config)));
        preview.known = groups.into_iter().map(|(owner, _)| owner).filter(|o| !o.is_empty()).collect();
    }
    if preview.refused.is_empty() && !preview.files.is_empty() {
        preview.patch = patch;
        let only: String = request.interfaces.iter().map(|n| format!(" --interface {}", shell_quote(n))).collect();
        preview.command = format!(
            "ductus owners reassign --from {} --to {}{only}",
            shell_quote(request.from.trim()),
            shell_quote(request.to.trim())
        );
    }
    Ok(Json(preview))
}

/// Quoted for a POSIX shell when it needs it: `'Ann Smith'`, `'O'"'"'Brien'`.
fn shell_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./@".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\"'\"'"))
    }
}

#[derive(Deserialize, ToSchema, Default)]
pub struct DoctorRequest {
    /// Only what these interfaces use (the engine's database is always checked).
    #[serde(default)]
    interfaces: Vec<String>,
    /// Longest wait per step, in milliseconds (default 5000, at most 60000).
    timeout_ms: Option<u64>,
}

/// Check every connection the engine needs, from its server: DNS, TCP (with the address a
/// firewall request needs), TLS (certificate, expiry), then each system's own login. Never
/// sends anything to a destination. Listening ports aren't checked (the engine holds them).
/// Operator.
#[utoipa::path(
    post, path = "/doctor", tag = "reports", request_body(content = DoctorRequest, content_type = "application/json"),
    responses((status = 200, body = Report), (status = 400, body = super::ErrorBody), (status = 403, body = super::ErrorBody))
)]
async fn doctor_report(engine: Eng, caller: Caller, body: Option<Json<DoctorRequest>>) -> ApiResult<Report> {
    caller.require(Role::Operator)?;
    let request = body.map(|Json(b)| b).unwrap_or_default();
    let unknown: Vec<&str> =
        request.interfaces.iter().filter(|n| engine.interface(n).is_none()).map(String::as_str).collect();
    if !unknown.is_empty() {
        return Err(ApiError::bad_request(format!("no interface named {}", unknown.join(", "))));
    }
    let timeout = Duration::from_millis(request.timeout_ms.unwrap_or(5000).clamp(100, 60_000));
    let configs: Vec<&InterfaceConfig> = engine.interfaces.iter().map(|i| &*i.config).collect();
    let code = engine.code_uses().await?;
    let probes: Vec<_> = doctor::plan(&configs, &engine.resources, code, &engine.database_url, &request.interfaces)
        .into_iter()
        .filter(|p| !matches!(p.target, Target::Listener { .. }))
        .collect();
    let detail = serde_json::json!({ "checks": probes.len(), "interfaces": request.interfaces });
    super::record_first(&engine, caller.audit("doctor.run").detail(detail)).await?;
    tracing::info!(token = caller.name(), checks = probes.len(), "connection checks requested through the API");
    let started = Instant::now();
    let checks = doctor::check(probes, timeout).await;
    Ok(Json(Report {
        generated_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S %:z").to_string(),
        environment: engine.environment.clone(),
        host: doctor::hostname(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        seconds: started.elapsed().as_secs_f64(),
        checks,
    }))
}

/// A field the company declared in `docs-schema.yaml`.
#[derive(Serialize, ToSchema)]
pub struct DocsField {
    key: String,
    /// The label, else the key.
    title: String,
    /// `text`, `enum`, `date`, `url`, `person` or `list`.
    #[serde(rename = "type")]
    kind: String,
    /// What an enum (or each item of a list) may be.
    values: Vec<String>,
    required: bool,
    /// `interface`, `destination`, `resource`.
    applies_to: Vec<String>,
    help: Option<String>,
}

/// One documented thing and the values it has.
#[derive(Serialize, ToSchema)]
pub struct DocsEntry {
    /// `interface`, `destination` or `resource`.
    level: String,
    /// `adt-lab`, `adt-lab.default`, `api empi`.
    name: String,
    /// The interface it belongs to; none for a resource.
    interface: Option<String>,
    /// Values by field key, as written in `custom:`.
    #[schema(value_type = std::collections::BTreeMap<String, serde_json::Value>)]
    custom: ductus_core::docs::Custom,
    /// Required fields that have no value here.
    missing: Vec<String>,
}

#[derive(Serialize, ToSchema)]
pub struct DocsReport {
    /// In the order the file lists them.
    fields: Vec<DocsField>,
    entries: Vec<DocsEntry>,
    /// What `ductus validate` would reject (wrong or undeclared values, missing required ones).
    problems: Vec<String>,
}

/// The company-defined documentation fields and every interface's, destination's and resource's
/// values. Read-only: the project's files are the source of truth.
#[utoipa::path(get, path = "/docs", tag = "reports", responses((status = 200, body = DocsReport)))]
async fn docs_report(engine: Eng, _caller: Caller) -> ApiResult<DocsReport> {
    use ductus_core::docs;
    let schema = &engine.docs;
    let configs = || engine.interfaces.iter().map(|i| &*i.config);
    let entries = docs::entries(configs(), &engine.resources)
        .into_iter()
        .map(|e| DocsEntry {
            missing: schema.missing(e.level, &e.custom),
            level: e.level.label().into(),
            name: e.name,
            interface: e.interface,
            custom: e.custom,
        })
        .collect();
    let fields = schema
        .fields
        .iter()
        .map(|d| DocsField {
            key: d.key.clone(),
            title: d.title().into(),
            kind: serde_json::to_value(d.field.kind)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_default(),
            values: d.field.values.clone(),
            required: d.field.required,
            applies_to: d.field.applies_to.iter().map(|l| l.label().into()).collect(),
            help: d.field.help.clone(),
        })
        .collect();
    Ok(Json(DocsReport { fields, entries, problems: docs::check_project(schema, configs(), &engine.resources) }))
}
