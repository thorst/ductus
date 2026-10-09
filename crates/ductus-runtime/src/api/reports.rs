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
    OpenApiRouter::new().routes(routes!(impact_report)).routes(routes!(owners)).routes(routes!(doctor_report))
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
