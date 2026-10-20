//! The reports the CLI prints, as data: `impact`, `owners` and `doctor`, for the interfaces
//! this engine runs.

use super::{ApiError, ApiResult, Caller, Eng, Shared};
use crate::doctor::{Mode, Report};
use axum::Json;
use axum::extract::Query;
use ductus_core::InterfaceConfig;
use ductus_core::about::HostQuery;
use ductus_core::impact::{self, Query as ImpactQuery};
use ductus_store::Role;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

pub fn routes() -> OpenApiRouter<Shared> {
    OpenApiRouter::new()
        .routes(routes!(impact_report))
        .routes(routes!(owners))
        .routes(routes!(owner_people))
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
    /// Who the owner is: `user` (an account, in `person`), `disabled` (an account that is
    /// disabled or refused by the directory, in `person`), `several` (names more than one
    /// account: none is chosen), `team` (in `owners:` in ductus.yaml), `address` (an email
    /// address, not an account) or `nobody`. Absent for the unowned.
    reach: Option<String>,
    person: Option<OwnerPerson>,
    /// The owner is the caller (or the person whose token calls).
    you: bool,
    /// Interfaces, and destinations owned by someone other than their interface's owner.
    owns: Vec<Owned>,
    /// resources.yaml entries with this `owner`.
    resources: Vec<OwnedResource>,
}

#[derive(Serialize, ToSchema)]
pub struct Owned {
    interface: String,
    /// Set for a destination owned by someone other than its interface's owner.
    destination: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct OwnedResource {
    /// `database`, `api`, `sftp server` or `directory`.
    kind: String,
    name: String,
}

/// An account, as an owner.
#[derive(Serialize, ToSchema)]
pub struct OwnerPerson {
    username: String,
    display_name: Option<String>,
    email: Option<String>,
    /// Disabled, or refused by the directory.
    disabled: bool,
}

impl From<&ductus_core::owners::Person<'_>> for OwnerPerson {
    fn from(p: &ductus_core::owners::Person) -> Self {
        Self {
            username: p.username.into(),
            display_name: p.display_name.map(Into::into),
            email: p.email.map(Into::into),
            disabled: p.disabled,
        }
    }
}

fn reach_label(reach: &ductus_core::owners::Reach) -> &'static str {
    use ductus_core::owners::Reach;
    match reach {
        Reach::User(_) => "user",
        Reach::Disabled(_) => "disabled",
        Reach::Several(_) => "several",
        Reach::Team => "team",
        Reach::Address => "address",
        Reach::Nobody => "nobody",
    }
}

/// Who owns which interface (`about.owner`), destination (`owner` on one someone else owns) and
/// resources.yaml entry, and whether each owner is an account (username, email or display name,
/// ignoring case), a team in `owners:`, or nobody the engine knows.
#[utoipa::path(get, path = "/owners", tag = "reports", responses((status = 200, body = Vec<OwnerGroup>)))]
async fn owners(engine: Eng, caller: Caller) -> ApiResult<Vec<OwnerGroup>> {
    use ductus_core::owners::{self, Found, What};
    let users = engine.store.users().await?;
    let people = crate::alerts::people(&users);
    let me = caller.who.user_id.and_then(|id| users.iter().find(|u| u.id == id)).map(|u| u.username.as_str());
    let items = owners::owned(engine.interfaces.iter().map(|i| &*i.config), &engine.resources);
    Ok(Json(
        owners::by_owner(items)
            .into_iter()
            .map(|(owner, items)| {
                let person = match owners::find(&owner, &people) {
                    Found::One(p) => Some(p),
                    _ => None,
                };
                let (mut owns, mut resources) = (Vec::new(), Vec::new());
                for item in items {
                    match item.what {
                        What::Interface(interface) => owns.push(Owned { interface, destination: None }),
                        What::Destination { interface, destination } => {
                            owns.push(Owned { interface, destination: Some(destination) })
                        }
                        What::Resource { kind, name } => {
                            resources.push(OwnedResource { kind: kind.label().into(), name })
                        }
                    }
                }
                OwnerGroup {
                    reach: (!owner.is_empty())
                        .then(|| reach_label(&owners::reach(&owner, &people, &engine.teams)).to_string()),
                    you: me.is_some() && person.map(|p| p.username) == me,
                    person: person.map(Into::into),
                    owner: (!owner.is_empty()).then_some(owner),
                    owns,
                    resources,
                }
            })
            .collect(),
    ))
}

/// The accounts that can own things (enabled ones), for choosing a new owner: name and email.
#[utoipa::path(
    get, path = "/owners/people", tag = "reports", operation_id = "owner_people",
    responses((status = 200, body = Vec<OwnerPerson>))
)]
async fn owner_people(engine: Eng, _caller: Caller) -> ApiResult<Vec<OwnerPerson>> {
    let users = engine.store.users().await?;
    let mut people: Vec<OwnerPerson> =
        crate::alerts::people(&users).iter().filter(|p| !p.disabled).map(Into::into).collect();
    people.sort_by_key(|p| p.display_name.as_deref().unwrap_or(&p.username).to_lowercase());
    Ok(Json(people))
}

#[derive(Deserialize, ToSchema)]
pub struct ReassignRequest {
    /// The current owner (ignores case).
    from: String,
    /// The new owner.
    to: String,
    /// Only these interfaces. With neither `interfaces` nor `resources`, everything moves.
    #[serde(default)]
    interfaces: Vec<String>,
    /// Only these resources.yaml entries (by name).
    #[serde(default)]
    resources: Vec<String>,
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
    /// The new owner, as `reach` in `/owners` says it.
    to_reach: String,
    /// The account the new owner names, if one does.
    to_person: Option<OwnerPerson>,
    /// Said when the new owner isn't an account (or names a disabled one, or several).
    warning: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct ReassignFile {
    /// The interface whose interface.yaml this is; absent for resources.yaml.
    interface: Option<String>,
    /// resources.yaml entries whose owner changes.
    resources: Vec<String>,
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
    /// Absent for resources.yaml.
    interface: Option<String>,
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
    use ductus_core::owners::{self, Reach};
    let unknown: Vec<&str> =
        request.interfaces.iter().filter(|n| engine.interface(n).is_none()).map(String::as_str).collect();
    if !unknown.is_empty() {
        return Err(ApiError::bad_request(format!("no interface named {}", unknown.join(", "))));
    }
    let unknown: Vec<&str> =
        request.resources.iter().filter(|n| !resource_exists(&engine.resources, n)).map(String::as_str).collect();
    if !unknown.is_empty() {
        return Err(ApiError::bad_request(format!("no resource named {} in resources.yaml", unknown.join(", "))));
    }
    if request.from.trim().is_empty() {
        return Err(ApiError::bad_request("say whose interfaces move (`from`)"));
    }
    let users = engine.store.users().await?;
    let people = crate::alerts::people(&users);
    let reach = owners::reach(&request.to, &people, &engine.teams);
    let to_person = match owners::find(&request.to, &people) {
        owners::Found::One(p) => Some(p.into()),
        _ => None,
    };
    let to = request.to.trim();
    let warning = match &reach {
        Reach::User(_) | Reach::Team => None,
        Reach::Disabled(u) => Some(format!("`{to}` is the account {u}, which is disabled")),
        Reach::Several(n) => Some(format!("`{to}` names {n} accounts: use a username or an email address")),
        Reach::Address => Some(format!("`{to}` isn't an account (alerts still reach the address)")),
        Reach::Nobody => {
            Some(format!("`{to}` isn't an account or a team in `owners:`: alerts for these would reach nobody"))
        }
    };
    let mut preview = ReassignPreview {
        files: Vec::new(),
        refused: Vec::new(),
        changed: 0,
        patch: String::new(),
        command: String::new(),
        known: Vec::new(),
        to_reach: reach_label(&reach).into(),
        to_person,
        warning,
    };
    let everything = request.interfaces.is_empty() && request.resources.is_empty();
    let relative = |file: &std::path::Path| {
        file.strip_prefix(&engine.root).unwrap_or(file).display().to_string().replace('\\', "/")
    };
    let mut patch = String::new();
    for i in engine.interfaces.iter().filter(|i| everything || request.interfaces.contains(&i.config.name)) {
        let file = i.config.dir.join("interface.yaml");
        let path = relative(&file);
        let text = tokio::fs::read_to_string(&file)
            .await
            .map_err(|e| ApiError::bad_request(format!("reading {path}: {e}")))?;
        match owners::reassign(&text, &request.from, &request.to) {
            Ok((_, changes)) if changes.is_empty() => {}
            Ok((out, changes)) => {
                preview.changed += changes.len();
                patch.push_str(&owners::patch(&path, &text, &out));
                preview.files.push(ReassignFile {
                    interface: Some(i.config.name.clone()),
                    resources: Vec::new(),
                    path,
                    lines: changes
                        .into_iter()
                        .map(|c| ReassignLine { line: c.line, before: c.before, after: c.after })
                        .collect(),
                });
            }
            Err(e) => preview.refused.push(ReassignRefused {
                interface: Some(i.config.name.clone()),
                path,
                error: format!("{e:#}"),
            }),
        }
    }
    let file = engine.root.join("resources.yaml");
    if (everything || !request.resources.is_empty()) && file.is_file() {
        let path = relative(&file);
        let text = tokio::fs::read_to_string(&file)
            .await
            .map_err(|e| ApiError::bad_request(format!("reading {path}: {e}")))?;
        match owners::reassign_resources(&text, &request.from, &request.to, &request.resources) {
            Ok((_, changes)) if changes.is_empty() => {}
            Ok((out, changes)) => {
                preview.changed += changes.len();
                patch.push_str(&owners::patch(&path, &text, &out));
                let mut names: Vec<String> = changes.iter().map(|c| c.name.clone()).collect();
                names.dedup();
                preview.files.push(ReassignFile {
                    interface: None,
                    resources: names,
                    path,
                    lines: changes
                        .into_iter()
                        .map(|c| ReassignLine { line: c.change.line, before: c.change.before, after: c.change.after })
                        .collect(),
                });
            }
            Err(e) => preview.refused.push(ReassignRefused { interface: None, path, error: format!("{e:#}") }),
        }
    }
    if preview.files.is_empty() && preview.refused.is_empty() {
        let groups = owners::by_owner(owners::owned(engine.interfaces.iter().map(|i| &*i.config), &engine.resources));
        preview.known = groups.into_iter().map(|(owner, _)| owner).filter(|o| !o.is_empty()).collect();
    }
    if preview.refused.is_empty() && !preview.files.is_empty() {
        preview.patch = patch;
        let only: String = request
            .interfaces
            .iter()
            .map(|n| format!(" --interface {}", shell_quote(n)))
            .chain(request.resources.iter().map(|n| format!(" --resource {}", shell_quote(n))))
            .collect();
        preview.command = format!(
            "ductus owners reassign --from {} --to {}{only}",
            shell_quote(request.from.trim()),
            shell_quote(request.to.trim())
        );
    }
    Ok(Json(preview))
}

fn resource_exists(resources: &ductus_core::Resources, name: &str) -> bool {
    use ductus_core::resources::Kind;
    [Kind::Database, Kind::Api, Kind::Sftp, Kind::Ldap, Kind::Certificate]
        .into_iter()
        .any(|k| resources.contains(k, name))
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
    /// `full` (the default: logins included) or `certificates` (DNS, TCP and the TLS handshake of
    /// what has a certificate; nothing logs in).
    #[serde(default)]
    mode: DoctorRunMode,
}

#[derive(Deserialize, ToSchema, Default, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum DoctorRunMode {
    #[default]
    Full,
    Certificates,
}

/// Check every connection the engine needs, from its server: DNS, TCP (with the address a
/// firewall request needs), TLS (certificate, expiry), then each system's own login; also the
/// alerts' mail server, webhooks and heartbeat. Never sends anything to a destination. Listening
/// ports aren't checked (the engine holds them). The run is kept (`/doctor/runs`), and the
/// certificates it saw (`/certificates`). Operator.
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
    let mode = match request.mode {
        DoctorRunMode::Full => Mode::Full,
        DoctorRunMode::Certificates => Mode::Certificates,
    };
    let detail = serde_json::json!({ "mode": mode.label(), "interfaces": request.interfaces });
    super::record_first(&engine, caller.audit("doctor.run").detail(detail)).await?;
    tracing::info!(by = caller.name(), mode = mode.label(), "connection checks requested through the API");
    let report = crate::certificates::run(&engine, mode, caller.name(), &request.interfaces, timeout).await?;
    Ok(Json(report))
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
