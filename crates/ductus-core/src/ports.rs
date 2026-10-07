//! Which ports a project listens on and which hosts it connects to: `ductus ports` shows
//! them (with conflicts from `config::check_endpoints`), and `ductus new` picks free ports.

use crate::config::{Destination, HttpMode, Source};
use crate::resources::Database;
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

/// Every host the project connects to: destinations, and the databases and SFTP servers in
/// resources.yaml. Sorted by host then port.
pub fn outbound<'a>(configs: impl IntoIterator<Item = &'a InterfaceConfig>, resources: &Resources) -> Vec<Outbound> {
    let mut out = Vec::new();
    for c in configs {
        for (name, d) in c.destinations() {
            let used_by = if c.destination.is_some() { c.name.clone() } else { format!("{} → {name}", c.name) };
            match &d.connector {
                Destination::Mllp { host, port, .. } => {
                    out.push(Outbound { host: host.clone(), port: *port, protocol: "MLLP", used_by });
                }
                Destination::Http { url, .. } => {
                    if let Ok(u) = url::Url::parse(url)
                        && let (Some(host), Some(port)) = (u.host_str(), u.port_or_known_default())
                    {
                        let protocol = if u.scheme() == "https" { "HTTPS" } else { "HTTP" };
                        out.push(Outbound { host: host.to_string(), port, protocol, used_by });
                    }
                }
                // SFTP servers are listed once, from resources.yaml, below.
                Destination::Sftp { .. } | Destination::File { .. } => {}
            }
        }
    }
    for (name, db) in &resources.databases {
        let used_by = format!("resources.yaml: {name}");
        let (host, port, protocol, used_by) = match db {
            Database::Postgres(c) => (c.host.clone(), c.port.unwrap_or(5432), "Postgres", used_by),
            // A named instance without a port is found through the SQL Server Browser; the
            // instance's own TCP port is whatever the server assigned it.
            Database::Sqlserver(c) => match (&c.instance, c.port) {
                (Some(i), None) => (
                    c.host.clone(),
                    1434,
                    "SQL Browser (UDP)",
                    format!("{used_by} (instance {i}; also its TCP port, set on the server)"),
                ),
                _ => (c.host.clone(), c.port.unwrap_or(1433), "SQL Server", used_by),
            },
        };
        out.push(Outbound { host, port, protocol, used_by });
    }
    for (name, s) in &resources.sftp {
        out.push(Outbound {
            host: s.host.clone(),
            port: s.port,
            protocol: "SFTP",
            used_by: format!("resources.yaml: {name}"),
        });
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
            "databases:\n  pdb: { postgres: { host: db1, database: p, user: u } }\n  billing: { sqlserver: { host: sql1, instance: PROD, database: b, user: u } }\nsftp:\n  lab: { host: sftp.lab, user: u, password: x }\n",
        )
        .unwrap();
        let o = outbound(&configs, &resources);
        let shown: Vec<_> = o.iter().map(|o| (o.host.as_str(), o.port, o.protocol, o.used_by.as_str())).collect();
        assert_eq!(
            shown,
            [
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
