//! `ductus impact`: which interfaces a vendor, application or host touches, and how. For the
//! calls integration teams get: "vendor X was breached", "the VPN to Y changes IP tonight".
//!
//! It reads what's configured (`about:`, destinations, resources.yaml) and what code uses
//! (`db("...")`/`api("...")` calls, found by the type checker and passed in as `code`).

use crate::InterfaceConfig;
use crate::about::{HostQuery, System, given};
use crate::config::{Destination, Source};
use crate::ports::destination_endpoint;
use crate::resources::{Endpoint, Kind, Resources};
use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;

/// What to look for.
#[derive(Debug, Clone)]
pub enum Query {
    Vendor(String),
    Application(String),
    Host(HostQuery),
}

impl std::fmt::Display for Query {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Query::Vendor(v) => write!(f, "vendor \"{v}\""),
            Query::Application(a) => write!(f, "application \"{a}\""),
            Query::Host(h) => write!(f, "host {h}"),
        }
    }
}

/// One way an interface is affected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub interface: String,
    /// `about.owner`, or empty.
    pub owner: String,
    /// `source`, `destination`, `destination lab`, `database patient_db`, `api empi`.
    pub role: String,
    /// Why it matched: `about.from: Epic (Epic Systems)`, `sender host 10.20.30.0/24`.
    pub reason: String,
    /// What to look at: hosts and ports, listening ports, folders.
    pub endpoints: Vec<String>,
}

/// Resources an interface's code uses, by interface name.
pub type CodeUses = BTreeMap<String, Vec<(Kind, String)>>;

/// Every way each interface is affected by `query`, in interface order. `resolve` looks up
/// host names (DNS, or nothing) so names and addresses can match each other.
pub fn impact<'a>(
    configs: impl IntoIterator<Item = &'a InterfaceConfig>,
    resources: &Resources,
    code: &CodeUses,
    query: &Query,
    resolve: &dyn Fn(&str) -> Vec<IpAddr>,
) -> Vec<Hit> {
    let system_reason = |where_: &str, system: &System| -> Option<String> {
        let (wanted, value) = match query {
            Query::Vendor(v) => (v, given(&system.vendor)?),
            Query::Application(a) => (a, given(&system.application)?),
            Query::Host(_) => return None,
        };
        (value.to_lowercase() == wanted.trim().to_lowercase()).then(|| format!("{where_}: {}", system.label()))
    };
    let host_reason = |where_: &str, hosts: &[String]| -> Option<String> {
        let Query::Host(h) = query else { return None };
        let found: Vec<&str> = hosts.iter().filter(|c| h.matches(c, resolve)).map(String::as_str).collect();
        (!found.is_empty()).then(|| format!("{where_}: {}", found.join(", ")))
    };
    // A resources.yaml entry: matched by its system or its hosts.
    let resource_reasons = |kind: Kind, name: &str| -> Vec<String> {
        let where_ = format!("{} `{name}` in resources.yaml", kind.label());
        let hosts: Vec<String> = resources.endpoints(kind, name).into_iter().map(|e| e.host).collect();
        [system_reason(&where_, &resources.system(kind, name)), host_reason(&format!("{where_} at"), &hosts)]
            .into_iter()
            .flatten()
            .collect()
    };
    let show = |endpoints: Vec<Endpoint>| endpoints.iter().map(ToString::to_string).collect::<Vec<_>>();

    let mut hits = Vec::new();
    for c in configs {
        let owner = given(&c.about.owner).unwrap_or("").to_string();
        let mut hit = |role: String, reasons: Vec<String>, endpoints: Vec<String>| {
            if !reasons.is_empty() {
                let reason = reasons.join("; ");
                hits.push(Hit { interface: c.name.clone(), owner: owner.clone(), role, reason, endpoints });
            }
        };

        // The sending side.
        let mut reasons: Vec<String> = [
            system_reason("about.from", &c.about.from.system()),
            host_reason("sender host (about.from.hosts)", &c.about.from.hosts),
        ]
        .into_iter()
        .flatten()
        .collect();
        let endpoints = match &c.source {
            Source::Mllp { host, port } => vec![listening(host, *port, "MLLP")],
            Source::Http { host, port, path, method, .. } => {
                vec![format!("{} {} {path}", listening(host, *port, "HTTP"), method.to_ascii_uppercase())]
            }
            Source::File(f) => vec![format!("folder {}", f.dir)],
            Source::Sftp(f) => {
                let server = f.server.as_deref().unwrap_or_default();
                reasons.extend(resource_reasons(Kind::Sftp, server));
                show(resources.endpoints(Kind::Sftp, server)).into_iter().map(|e| format!("{e} {}", f.dir)).collect()
            }
            Source::Schedule(_) => vec!["schedule".into()],
        };
        hit("source".into(), reasons, endpoints);

        // Each destination.
        let mut covered = BTreeSet::new();
        for (name, d) in c.destinations() {
            let role = if c.destination.is_some() { "destination".to_string() } else { format!("destination {name}") };
            let own = d.system();
            let mut reasons: Vec<String> = if own.is_known() {
                system_reason(&role, &own).into_iter().collect()
            } else {
                system_reason("about.to", &c.about.to).into_iter().collect()
            };
            let mut endpoints = Vec::new();
            if let Some(e) = destination_endpoint(&d.connector) {
                reasons.extend(host_reason("host", std::slice::from_ref(&e.host)));
                endpoints.push(e.to_string());
            }
            if let Some((kind, resource)) = d.resource() {
                covered.insert((kind, resource.to_string()));
                reasons.extend(resource_reasons(kind, resource));
                endpoints.extend(show(resources.endpoints(kind, resource)));
            }
            if let Destination::File { dir } = &d.connector {
                endpoints.push(format!("folder {}", dir.display()));
            }
            hit(role, reasons, endpoints);
        }

        // Databases and APIs its code (or dedicated_pools) uses.
        if let Source::Sftp(f) = &c.source {
            covered.extend(f.server.iter().map(|s| (Kind::Sftp, s.clone())));
        }
        let used: BTreeSet<(Kind, String)> = code
            .get(&c.name)
            .into_iter()
            .flatten()
            .cloned()
            .chain(c.dedicated_pools.keys().map(|db| (Kind::Database, db.clone())))
            .filter(|r| !covered.contains(r))
            .collect();
        for (kind, name) in used {
            let role = format!("{} {name}", kind.label());
            hit(role, resource_reasons(kind, &name), show(resources.endpoints(kind, &name)));
        }
    }
    hits
}

fn listening(host: &str, port: u16, protocol: &str) -> String {
    if host == "0.0.0.0" || host == "::" {
        format!("listens on :{port} {protocol}")
    } else {
        format!("listens on {host}:{port} {protocol}")
    }
}

/// Every vendor (or application) named anywhere, for "no match; known vendors: ..." hints.
pub fn known_values<'a>(
    configs: impl IntoIterator<Item = &'a InterfaceConfig>,
    resources: &Resources,
    query: &Query,
) -> Vec<String> {
    let pick = |s: &System| match query {
        Query::Vendor(_) => given(&s.vendor).map(str::to_string),
        Query::Application(_) => given(&s.application).map(str::to_string),
        Query::Host(_) => None,
    };
    let mut systems: Vec<System> = Vec::new();
    for c in configs {
        systems.push(c.about.from.system());
        systems.push(c.about.to.clone());
        systems.extend(c.destinations().map(|(_, d)| d.system()));
    }
    for name in resources.databases.keys() {
        systems.push(resources.system(Kind::Database, name));
    }
    for name in resources.apis.keys() {
        systems.push(resources.system(Kind::Api, name));
    }
    for name in resources.sftp.keys() {
        systems.push(resources.system(Kind::Sftp, name));
    }
    let set: BTreeSet<String> = systems.iter().filter_map(pick).collect();
    set.into_iter().collect()
}

/// Where an interface doesn't say which systems it connects (what `validate` warns about).
/// A side counts as known when `about:`, the destination, or the resources.yaml entry it goes
/// through names an application or vendor. Schedules have no sender to name.
pub fn about_gaps(c: &InterfaceConfig, resources: &Resources) -> Vec<String> {
    let mut gaps = Vec::new();
    let sender_known = match &c.source {
        Source::Schedule(_) => true,
        Source::Sftp(f) => {
            c.about.from.system().is_known()
                || resources.system(Kind::Sftp, f.server.as_deref().unwrap_or("")).is_known()
        }
        _ => c.about.from.system().is_known(),
    };
    if !sender_known {
        let who = if c.responds() { "calling" } else { "sending" };
        gaps.push(format!("about.from: add the {who} system's `application` or `vendor`"));
    }
    let several = c.destination.is_none();
    for (name, d) in c.destinations() {
        let through = d.resource().map(|(kind, r)| resources.system(kind, r).is_known()).unwrap_or(false);
        if !(d.system().is_known() || c.about.to.is_known() || through) {
            gaps.push(if several {
                format!("destination `{name}`: add `application` or `vendor` (on the destination, or about.to for all)")
            } else {
                "about.to: add the receiving system's `application` or `vendor`".to_string()
            });
        }
    }
    gaps
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(yaml: &str) -> InterfaceConfig {
        let c: InterfaceConfig = serde_yaml_ng::from_str(yaml).unwrap();
        c
    }

    fn project() -> (Vec<InterfaceConfig>, Resources, CodeUses) {
        let configs = vec![
            config(
                r#"
name: adt-in
source: { mllp: { port: 6661 } }
about:
  owner: Integration team
  from: { application: Epic, vendor: Epic Systems, hosts: [10.20.30.0/24] }
  to: { application: Sunquest Lab, vendor: Sunquest }
destination: { mllp: { host: 10.50.0.7, port: 6662 } }
"#,
            ),
            config(
                r#"
name: results
source: { sftp: { server: quest, dir: /out } }
about: { owner: Lab team }
destinations:
  emr: { mllp: { host: epic-int.local, port: 7000 }, application: Epic, vendor: Epic Systems }
  billing: { http: { api: billing }, vendor: Experian }
"#,
            ),
            config(
                r#"
name: nightly
source: { schedule: { every: 1h } }
script: t.ts
format: csv
destination: { file: { dir: out } }
"#,
            ),
        ];
        let resources: Resources = serde_yaml_ng::from_str(
            r#"
databases:
  patient_db: { postgres: { host: 10.20.30.9, database: p, user: u, vendor: Epic Systems, application: Clarity } }
sftp:
  quest: { host: sftp.quest.example.com, user: u, password: p, vendor: Quest }
apis:
  billing: { url: 'https://billing.example.org/v2', vendor: Experian }
  empi: { url: 'https://empi.local/api', application: EMPI }
"#,
        )
        .unwrap();
        let code = CodeUses::from([
            ("nightly".to_string(), vec![(Kind::Database, "patient_db".to_string())]),
            ("results".to_string(), vec![(Kind::Api, "empi".to_string()), (Kind::Api, "billing".to_string())]),
        ]);
        (configs, resources, code)
    }

    fn no_dns(_: &str) -> Vec<IpAddr> {
        Vec::new()
    }

    fn run(query: Query) -> Vec<(String, String, String)> {
        let (configs, resources, code) = project();
        impact(&configs, &resources, &code, &query, &no_dns)
            .into_iter()
            .map(|h| (h.interface, h.role, h.reason))
            .collect()
    }

    fn strings(rows: &[(&str, &str, &str)]) -> Vec<(String, String, String)> {
        rows.iter().map(|(a, b, c)| (a.to_string(), b.to_string(), c.to_string())).collect()
    }

    #[test]
    fn finds_a_vendor_everywhere_it_appears() {
        assert_eq!(
            run(Query::Vendor("epic systems".into())),
            strings(&[
                ("adt-in", "source", "about.from: Epic (Epic Systems)"),
                ("results", "destination emr", "destination emr: Epic (Epic Systems)"),
                ("nightly", "database patient_db", "database `patient_db` in resources.yaml: Clarity (Epic Systems)"),
            ])
        );
        // Through a resource the destination uses; code using the same API isn't listed twice.
        assert_eq!(
            run(Query::Vendor("Experian".into())),
            strings(&[(
                "results",
                "destination billing",
                "destination billing: (Experian); api `billing` in resources.yaml: (Experian)"
            )])
        );
        assert_eq!(
            run(Query::Vendor("Quest".into())),
            strings(&[("results", "source", "sftp server `quest` in resources.yaml: (Quest)")])
        );
        assert!(run(Query::Vendor("Cerner".into())).is_empty());
    }

    #[test]
    fn finds_applications_including_code_only_uses() {
        assert_eq!(
            run(Query::Application("EMPI".into())),
            strings(&[("results", "api empi", "api `empi` in resources.yaml: EMPI")])
        );
        assert_eq!(
            run(Query::Application("sunquest lab".into())),
            strings(&[("adt-in", "destination", "about.to: Sunquest Lab (Sunquest)")])
        );
    }

    #[test]
    fn finds_hosts_and_subnets() {
        let host = |q: &str| run(Query::Host(HostQuery::parse(q).unwrap()));
        assert_eq!(
            host("10.20.30.0/24"),
            strings(&[
                ("adt-in", "source", "sender host (about.from.hosts): 10.20.30.0/24"),
                ("nightly", "database patient_db", "database `patient_db` in resources.yaml at: 10.20.30.9"),
            ])
        );
        assert_eq!(host("10.50.0.7"), strings(&[("adt-in", "destination", "host: 10.50.0.7")]));
        assert_eq!(
            host("SFTP.quest.example.com"),
            strings(&[("results", "source", "sftp server `quest` in resources.yaml at: sftp.quest.example.com")])
        );

        // With a lookup, a configured name matches its address.
        let (configs, resources, code) = project();
        let dns = |n: &str| if n == "epic-int.local" { vec!["10.60.1.2".parse().unwrap()] } else { vec![] };
        let hits = impact(&configs, &resources, &code, &Query::Host(HostQuery::parse("10.60.1.0/24").unwrap()), &dns);
        assert_eq!(hits.len(), 1);
        assert_eq!(
            (hits[0].role.as_str(), hits[0].endpoints.as_slice()),
            ("destination emr", &["epic-int.local:7000 MLLP".to_string()][..])
        );
        assert_eq!(hits[0].owner, "Lab team");
    }

    #[test]
    fn shows_what_to_look_at() {
        let (configs, resources, code) = project();
        let hits = impact(&configs, &resources, &code, &Query::Vendor("epic systems".into()), &no_dns);
        let endpoints: Vec<_> = hits.iter().map(|h| h.endpoints.join(", ")).collect();
        assert_eq!(endpoints, ["listens on :6661 MLLP", "epic-int.local:7000 MLLP", "10.20.30.9:5432 Postgres"]);
        assert_eq!(hits[0].owner, "Integration team");
        assert_eq!(
            known_values(&configs, &resources, &Query::Vendor(String::new())),
            ["Epic Systems", "Experian", "Quest", "Sunquest"]
        );
    }

    #[test]
    fn reports_sides_without_a_system() {
        let (configs, resources, _) = project();
        let gaps: Vec<_> = configs.iter().map(|c| about_gaps(c, &resources)).collect();
        assert!(gaps[0].is_empty(), "{:?}", gaps[0]);
        assert!(gaps[1].is_empty(), "the SFTP server says Quest: {:?}", gaps[1]);
        assert_eq!(
            gaps[2],
            ["about.to: add the receiving system's `application` or `vendor`"],
            "a schedule has no sender"
        );

        let bare = config(
            "name: x\nsource: { mllp: { port: 1 } }\ndestinations:\n  a: { file: { dir: o } }\n  b: { file: { dir: p }, vendor: V }\n",
        );
        assert_eq!(
            about_gaps(&bare, &resources),
            [
                "about.from: add the sending system's `application` or `vendor`",
                "destination `a`: add `application` or `vendor` (on the destination, or about.to for all)",
            ]
        );
    }
}
