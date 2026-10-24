//! Which ports a project listens on and which hosts it connects to: `ductus ports` shows
//! them (with conflicts from `config::check_endpoints`), and `ductus new` picks free ports.

use crate::config::{Destination, HttpMode, Source};
use crate::resources::{Endpoint, Kind, LdapSecurity};
use crate::tls::{ConnectTls, ListenTls};
use crate::{InterfaceConfig, Resources};

/// A port an interface listens on.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Listener {
    pub port: u16,
    pub host: String,
    /// `MLLP`, `MLLP over TLS`, `HTTP` or `HTTPS`.
    pub protocol: &'static str,
    pub interface: String,
    /// HTTP only: `GET /patients/{mrn} (respond)`.
    pub route: Option<String>,
    /// With TLS, what it presents and asks for (`presents ductus-mllp, clients need a
    /// certificate from hospital-ca`); `none` without.
    pub tls: String,
}

/// The engine's REST API (and web UI) as a listener.
pub fn api_listener(host: &str, port: u16, tls: Option<&ListenTls>) -> Listener {
    Listener {
        port,
        host: host.to_string(),
        protocol: if tls.is_some() { "HTTPS" } else { "HTTP" },
        interface: "(engine REST API)".into(),
        route: Some("/api/v1".into()),
        tls: listen_tls(tls),
    }
}

fn listen_tls(tls: Option<&ListenTls>) -> String {
    tls.map_or_else(|| "none".to_string(), ListenTls::summary)
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
    /// For MLLP, HTTP and directories: `none`, or TLS with what it trusts and presents
    /// (`TLS: hie-ca trusted, client certificate ductus-client`). Empty for the rest (SFTP is
    /// SSH; databases are 0.6.4's).
    pub tls: String,
}

/// TLS on a connection, for a table: `TLS` (the platform's trusted certificates), or with what it
/// names.
fn connect_tls(tls: Option<&ConnectTls>) -> String {
    match tls.map(ConnectTls::summary) {
        None => "none".into(),
        Some(s) if s.is_empty() => "TLS".into(),
        Some(s) => format!("TLS: {s}"),
    }
}

/// Every listening port, sorted by port then interface.
pub fn listeners<'a>(configs: impl IntoIterator<Item = &'a InterfaceConfig>) -> Vec<Listener> {
    let mut out: Vec<Listener> = configs
        .into_iter()
        .filter_map(|c| match &c.source {
            Source::Mllp { host, port, tls } => Some(Listener {
                port: *port,
                host: host.clone(),
                protocol: if tls.is_some() { "MLLP over TLS" } else { "MLLP" },
                interface: c.name.clone(),
                route: None,
                tls: listen_tls(tls.as_ref()),
            }),
            Source::Http { host, port, path, method, mode, tls } => Some(Listener {
                port: *port,
                host: host.clone(),
                protocol: if tls.is_some() { "HTTPS" } else { "HTTP" },
                interface: c.name.clone(),
                route: Some(format!(
                    "{} {path} ({})",
                    method.to_ascii_uppercase(),
                    if *mode == HttpMode::Respond { "respond" } else { "queue" }
                )),
                tls: listen_tls(tls.as_ref()),
            }),
            Source::File(_) | Source::Sftp(_) | Source::Schedule(_) | Source::Internal => None,
        })
        .collect();
    out.sort();
    out
}

/// Where a destination connects to directly (not through resources.yaml). Empty for files,
/// SFTP servers and APIs (those are listed from resources.yaml) and routes to other interfaces.
pub fn destination_endpoint(d: &Destination) -> Option<Endpoint> {
    match d {
        Destination::Mllp { host, port, tls, .. } => {
            Some(Endpoint::new(host, *port, if tls.is_some() { "MLLP over TLS" } else { "MLLP" }))
        }
        Destination::Http { url: Some(url), .. } => Endpoint::from_url(url),
        Destination::Http { .. } | Destination::Sftp { .. } | Destination::File { .. } | Destination::Interface(_) => {
            None
        }
    }
}

/// Every host the project connects to: destinations, and the databases, SFTP servers, APIs and
/// directories in resources.yaml. Sorted by host then port.
pub fn outbound<'a>(configs: impl IntoIterator<Item = &'a InterfaceConfig>, resources: &Resources) -> Vec<Outbound> {
    let mut out = Vec::new();
    for c in configs {
        for (name, d) in c.destinations() {
            if let Some(e) = destination_endpoint(&d.connector) {
                let used_by = if c.destination.is_some() { c.name.clone() } else { format!("{} → {name}", c.name) };
                let tls = match &d.connector {
                    Destination::Mllp { tls, .. } => connect_tls(tls.as_ref()),
                    Destination::Http { ca, certificate, .. } if e.protocol == "HTTPS" => {
                        connect_tls(Some(&ConnectTls::new(ca, certificate)))
                    }
                    _ => connect_tls(None),
                };
                out.push(Outbound { host: e.host, port: e.port, protocol: e.protocol, used_by, tls });
            }
        }
    }
    let defined = [
        (Kind::Database, resources.databases.keys().collect::<Vec<_>>()),
        (Kind::Sftp, resources.sftp.keys().collect()),
        (Kind::Api, resources.apis.keys().collect()),
        (Kind::Ldap, resources.ldap.keys().collect()),
    ];
    for (kind, names) in defined {
        for name in names {
            for e in resources.endpoints(kind, name) {
                let used_by = match e.note {
                    Some(note) => format!("resources.yaml: {name} ({note})"),
                    None => format!("resources.yaml: {name}"),
                };
                let tls = match kind {
                    Kind::Api if e.protocol == "HTTPS" => {
                        connect_tls(resources.apis.get(name).map(|a| a.tls()).as_ref())
                    }
                    Kind::Api => connect_tls(None),
                    Kind::Ldap => match resources.ldap.get(name) {
                        Some(l) if l.security() == LdapSecurity::Plain => "none (insecure: true)".into(),
                        Some(l) => {
                            let how = if l.security() == LdapSecurity::StartTls { "StartTLS" } else { "TLS" };
                            match &l.ca {
                                Some(ca) => format!("{how}: {ca} trusted"),
                                None => how.into(),
                            }
                        }
                        None => String::new(),
                    },
                    _ => String::new(),
                };
                out.push(Outbound { host: e.host, port: e.port, protocol: e.protocol, used_by, tls });
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
                "name: secure\nsource: { mllp: { port: 6662, tls: { certificate: c } } }\ndestination: { file: { dir: o } }",
            ),
            config(
                "name: hook\nsource: { http: { port: 8080, path: /orders } }\ndestination: { http: { url: 'https://lab.example.org/api' } }",
            ),
            config("name: files\nsource: { file: { dir: in } }\ndestination: { sftp: { server: lab, dir: out } }"),
            config("name: charges\nsource: { file: { dir: in } }\ndestination: { http: { api: billing_api } }"),
            config(
                "name: hie\nsource: { file: { dir: in } }\ndestination: { mllp: { host: hie.example.org, port: 7443, tls: true } }",
            ),
        ];
        let l = listeners(&configs);
        let shown: Vec<_> = l.iter().map(|l| (l.port, l.protocol, l.interface.as_str(), l.route.as_deref())).collect();
        assert_eq!(
            shown,
            [
                (6661, "MLLP", "adt", None),
                (6662, "MLLP over TLS", "secure", None),
                (8080, "HTTP", "api", Some("GET /patients/{mrn} (respond)")),
                (8080, "HTTP", "hook", Some("POST /orders (queue)")),
            ]
        );
        assert_eq!(l.iter().map(|l| l.tls.as_str()).collect::<Vec<_>>(), ["none", "presents c", "none", "none"]);
        let api = ListenTls { certificate: "web".into(), clients: None };
        assert_eq!(
            (api_listener("0.0.0.0", 7700, Some(&api)).protocol, api_listener("0.0.0.0", 7700, Some(&api)).tls),
            ("HTTPS", "presents web".to_string())
        );
        assert_eq!(api_listener("127.0.0.1", 7700, None).protocol, "HTTP");

        let resources: Resources = serde_yaml_ng::from_str(
            "databases:\n  pdb: { postgres: { host: db1, database: p, user: u } }\n  billing: { sqlserver: { host: sql1, instance: PROD, database: b, user: u } }\nsftp:\n  lab: { host: sftp.lab, user: u, password: x }\napis:\n  billing_api: { url: 'https://billing.example.org:8443/v2' }\nldap:\n  ad: { url: 'ldaps://dc1.hospital.org' }\n",
        )
        .unwrap();
        let o = outbound(&configs, &resources);
        let shown: Vec<_> = o.iter().map(|o| (o.host.as_str(), o.port, o.protocol, o.used_by.as_str())).collect();
        assert_eq!(
            shown,
            [
                ("billing.example.org", 8443, "HTTPS", "resources.yaml: billing_api"),
                ("db1", 5432, "Postgres", "resources.yaml: pdb"),
                ("dc1.hospital.org", 636, "LDAPS", "resources.yaml: ad"),
                ("epic.local", 5000, "MLLP", "adt"),
                ("hie.example.org", 7443, "MLLP over TLS", "hie"),
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
        let tls: Vec<_> = o.iter().map(|o| o.tls.as_str()).collect();
        assert_eq!(tls, ["TLS", "", "TLS", "none", "TLS", "TLS", "", ""], "TLS where the protocol has it");

        let configs = [config(
            "name: x\nsource: { file: { dir: in } }\ndestinations:\n  a: { mllp: { host: h, port: 1, tls: { ca: hie-ca, certificate: me } } }\n  b: { http: { url: 'http://h:2/x' } }\n",
        )];
        let resources: Resources =
            serde_yaml_ng::from_str("ldap:\n  ad: { url: 'ldap://dc1', starttls: true, ca: corp-ca }\n").unwrap();
        let tls: Vec<_> = outbound(&configs, &resources).into_iter().map(|o| o.tls).collect();
        assert_eq!(tls, ["StartTLS: corp-ca trusted", "TLS: hie-ca trusted, client certificate me", "none"]);
    }
}
