//! Which ports a project listens on and which hosts it connects to: `ductus ports` shows
//! them (with conflicts from `config::check_endpoints`), and `ductus new` picks free ports.

use crate::config::{Destination, HttpMode, Source};
use crate::resources::{Endpoint, Kind};
use crate::{InterfaceConfig, Resources};

/// A port an interface listens on.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Listener {
    pub port: u16,
    pub host: String,
    /// `MLLP` or `HTTP`.
    pub protocol: &'static str,
    pub interface: String,
    /// HTTP only: `GET /patients/{mrn} (respond)`.
    pub route: Option<String>,
}

/// A host and port the project connects to (what a firewall request lists).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Outbound {
    pub host: String,
    pub port: u16,
    /// `MLLP`, `HTTP`, `HTTPS`, `SFTP`, `Postgres`, `SQL Server` or `SQL Browser (UDP)`.
    pub protocol: &'static str,
    /// The interface, or `resources.yaml: <name>`.
    pub used_by: String,
}

/// Every listening port, sorted by port then interface.
pub fn listeners<'a>(configs: impl IntoIterator<Item = &'a InterfaceConfig>) -> Vec<Listener> {
    let mut out: Vec<Listener> = configs
        .into_iter()
        .filter_map(|c| match &c.source {
            Source::Mllp { host, port } => Some(Listener {
                port: *port,
                host: host.clone(),
                protocol: "MLLP",
                interface: c.name.clone(),
                route: None,
            }),
            Source::Http { host, port, path, method, mode } => Some(Listener {
                port: *port,
                host: host.clone(),
                protocol: "HTTP",
                interface: c.name.clone(),
                route: Some(format!(
                    "{} {path} ({})",
                    method.to_ascii_uppercase(),
                    if *mode == HttpMode::Respond { "respond" } else { "queue" }
                )),
            }),
            Source::File(_) | Source::Sftp(_) | Source::Schedule(_) => None,
        })
        .collect();
    out.sort();
    out
}

/// Where a destination connects to directly (not through resources.yaml). Empty for files,
/// SFTP servers and APIs (those are listed from resources.yaml).
pub fn destination_endpoint(d: &Destination) -> Option<Endpoint> {
    match d {
        Destination::Mllp { host, port, .. } => Some(Endpoint::new(host, *port, "MLLP")),
        Destination::Http { url: Some(url), .. } => Endpoint::from_url(url),
        Destination::Http { .. } | Destination::Sftp { .. } | Destination::File { .. } => None,
    }
}

/// Every host the project connects to: destinations, and the databases, SFTP servers and APIs
/// in resources.yaml. Sorted by host then port.
pub fn outbound<'a>(configs: impl IntoIterator<Item = &'a InterfaceConfig>, resources: &Resources) -> Vec<Outbound> {
    let mut out = Vec::new();
    for c in configs {
        for (name, d) in c.destinations() {
            if let Some(e) = destination_endpoint(&d.connector) {
                let used_by = if c.destination.is_some() { c.name.clone() } else { format!("{} → {name}", c.name) };
                out.push(Outbound { host: e.host, port: e.port, protocol: e.protocol, used_by });
            }
        }
    }
    let defined = [
        (Kind::Database, resources.databases.keys().collect::<Vec<_>>()),
        (Kind::Sftp, resources.sftp.keys().collect()),
        (Kind::Api, resources.apis.keys().collect()),
    ];
    for (kind, names) in defined {
        for name in names {
            for e in resources.endpoints(kind, name) {
                let used_by = match e.note {
                    Some(note) => format!("resources.yaml: {name} ({note})"),
                    None => format!("resources.yaml: {name}"),
                };
                out.push(Outbound { host: e.host, port: e.port, protocol: e.protocol, used_by });
            }
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(yaml: &str) -> InterfaceConfig {
        serde_yaml_ng::from_str(yaml).unwrap()
    }

    #[test]
    fn lists_listeners_and_outbound_connections() {
        let configs = [
            config("name: api\nsource: { http: { port: 8080, path: '/patients/{mrn}', method: get, mode: respond } }"),
            config(
                "name: adt\nsource: { mllp: { port: 6661 } }\ndestination: { mllp: { host: epic.local, port: 5000 } }",
            ),
            config(
                "name: hook\nsource: { http: { port: 8080, path: /orders } }\ndestination: { http: { url: 'https://lab.example.org/api' } }",
            ),
            config("name: files\nsource: { file: { dir: in } }\ndestination: { sftp: { server: lab, dir: out } }"),
            config("name: charges\nsource: { file: { dir: in } }\ndestination: { http: { api: billing_api } }"),
        ];
        let l = listeners(&configs);
        let shown: Vec<_> = l.iter().map(|l| (l.port, l.protocol, l.interface.as_str(), l.route.as_deref())).collect();
        assert_eq!(
            shown,
            [
                (6661, "MLLP", "adt", None),
                (8080, "HTTP", "api", Some("GET /patients/{mrn} (respond)")),
                (8080, "HTTP", "hook", Some("POST /orders (queue)")),
            ]
        );

        let resources: Resources = serde_yaml_ng::from_str(
            "databases:\n  pdb: { postgres: { host: db1, database: p, user: u } }\n  billing: { sqlserver: { host: sql1, instance: PROD, database: b, user: u } }\nsftp:\n  lab: { host: sftp.lab, user: u, password: x }\napis:\n  billing_api: { url: 'https://billing.example.org:8443/v2' }\n",
        )
        .unwrap();
        let o = outbound(&configs, &resources);
        let shown: Vec<_> = o.iter().map(|o| (o.host.as_str(), o.port, o.protocol, o.used_by.as_str())).collect();
        assert_eq!(
            shown,
            [
                ("billing.example.org", 8443, "HTTPS", "resources.yaml: billing_api"),
                ("db1", 5432, "Postgres", "resources.yaml: pdb"),
                ("epic.local", 5000, "MLLP", "adt"),
                ("lab.example.org", 443, "HTTPS", "hook"),
                ("sftp.lab", 22, "SFTP", "resources.yaml: lab"),
                (
                    "sql1",
                    1434,
                    "SQL Browser (UDP)",
                    "resources.yaml: billing (instance PROD; also its TCP port, set on the server)"
                ),
            ]
        );
    }
}
