//! `tls:` on what the engine listens on (MLLP and HTTP sources, the API), and the client
//! certificates it asks for.
//!
//! ```yaml
//! source: { mllp: { port: 6661, tls: { certificate: ductus-mllp } } }
//! source:
//!   mllp:
//!     port: 6662
//!     tls:
//!       certificate: ductus-mllp            # a `certificates:` entry in resources.yaml
//!       clients:                            # senders must present a certificate
//!         ca: hospital-ca                   # issued by this CA (another `certificates:` entry)
//!         subjects: ["CN=bridges*.hospital.org", "O=Quest, OU=Lab"]   # any one; none = any
//! ```
//!
//! A subject pattern is `ATTR=value` parts separated by commas, every part matching (`*` and
//! `?` wildcards, case doesn't matter): `CN`, `O`, `OU`, `C`, `L`, `ST`, `DC`, or `DNS` for the
//! certificate's DNS names. Values can't contain commas.
//!
//! And on what the engine connects to: a server is trusted when the platform trusts it (the
//! system's certificates) or the named CA issued it, and its certificate must cover the host
//! connected to. `certificate` is presented as the client certificate (mTLS).
//!
//! ```yaml
//! destination: { mllp: { host: hie.example.org, port: 7443, tls: true } }       # the platform's roots
//! destination: { mllp: { host: hie.example.org, port: 7443, tls: { ca: hie-ca, certificate: ductus-client } } }
//! destination: { http: { url: "https://lab.example.org/orders", ca: lab-ca, certificate: ductus-client } }
//! apis: { empi: { url: "https://empi.local/api", ca: hospital-ca, certificate: ductus-client } }   # resources.yaml
//! ```

use crate::InterfaceConfig;
use crate::alerts::AlertSettings;
use crate::config::{Destination, Source};
use crate::resources::{CertificateKind, Kind, Resources};
use anyhow::{Result, bail};
use serde::Deserialize;

/// TLS on a connection the engine makes: whom it trusts besides the platform, and the client
/// certificate it presents. Both name `certificates:` entries (a CA, and a pair).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectTls {
    #[serde(default)]
    pub ca: Option<String>,
    #[serde(default)]
    pub certificate: Option<String>,
}

impl ConnectTls {
    pub fn new(ca: &Option<String>, certificate: &Option<String>) -> Self {
        Self { ca: ca.clone(), certificate: certificate.clone() }
    }

    /// Whether it names anything (else: the platform's roots, no client certificate).
    pub fn is_set(&self) -> bool {
        self.ca.is_some() || self.certificate.is_some()
    }

    /// How it reads in a sentence: `with hie-ca trusted and client certificate ductus-client`.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        parts.extend(self.ca.as_ref().map(|ca| format!("{ca} trusted")));
        parts.extend(self.certificate.as_ref().map(|c| format!("client certificate {c}")));
        if parts.is_empty() { String::new() } else { format!(" ({})", parts.join(", ")) }
    }
}

/// `tls: true` (the platform's roots), `tls: false` (none), or `tls: { ca, certificate }`.
pub fn connect_tls_opt<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Option<ConnectTls>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Written {
        On(bool),
        With(ConnectTls),
    }
    match Option::<serde_yaml_ng::Value>::deserialize(d)? {
        None => Ok(None),
        Some(value) => match Written::deserialize(value.clone()) {
            Ok(Written::On(on)) => Ok(on.then(ConnectTls::default)),
            Ok(Written::With(tls)) => Ok(Some(tls)),
            // Untagged enums lose the reason: read it again as the map, for its own message.
            Err(_) => ConnectTls::deserialize(value)
                .map(Some)
                .map_err(|e| serde::de::Error::custom(format!("tls: `true`, or {{ ca, certificate }}: {e}"))),
        },
    }
}

/// Every connection the engine makes that names `ca` or `certificate`, with where it is: MLLP and
/// HTTP destinations, `apis:` and `ldap:` entries, the alerts' mail server, webhooks and heartbeat.
pub fn outbound<'a>(
    configs: impl IntoIterator<Item = &'a InterfaceConfig>,
    resources: &Resources,
    alerts: Option<&AlertSettings>,
) -> Vec<(String, ConnectTls)> {
    let mut all = Vec::new();
    for c in configs {
        for (name, d) in c.destinations() {
            let tls = match &d.connector {
                Destination::Mllp { tls: Some(t), .. } => t.clone(),
                Destination::Http { ca, certificate, .. } => ConnectTls::new(ca, certificate),
                _ => continue,
            };
            all.push((format!("interface `{}` destination `{name}`", c.name), tls));
        }
    }
    for (name, api) in &resources.apis {
        all.push((format!("api `{name}` (resources.yaml)"), ConnectTls::new(&api.ca, &api.certificate)));
    }
    for (name, l) in &resources.ldap {
        all.push((format!("ldap `{name}` (resources.yaml)"), ConnectTls::new(&l.ca, &None)));
    }
    if let Some(a) = alerts {
        if let Some(s) = &a.smtp {
            all.push(("alerts.smtp".to_string(), ConnectTls::new(&s.ca, &s.certificate)));
        }
        for (name, channel) in &a.channels {
            if let Some(w) = &channel.webhook {
                all.push((format!("alerts.channels.{name}.webhook"), ConnectTls::new(&w.ca, &w.certificate)));
            }
        }
        if let Some(h) = &a.heartbeat {
            all.push(("alerts.heartbeat".to_string(), ConnectTls::new(&h.ca, &h.certificate)));
        }
    }
    all.retain(|(_, tls)| tls.is_set());
    all
}

/// Problems with what connections name: names resources.yaml doesn't define, a pair where a CA
/// belongs (`ca`), or a CA where a certificate with its key belongs (`certificate`).
pub fn check_outbound(connections: &[(String, ConnectTls)], resources: &Resources) -> Vec<String> {
    let mut problems = Vec::new();
    for (place, tls) in connections {
        if let Some(ca) = &tls.ca {
            match resources.certificates.get(ca).map(|c| c.kind()) {
                None => problems.push(format!("{place}: ca: {}", missing(resources, ca))),
                Some(CertificateKind::Pair) => problems.push(format!(
                    "{place}: ca: `{ca}` is a certificate with its key; name an entry with `ca` or `ca_file` (the CA that issued the server's certificate)"
                )),
                Some(CertificateKind::Ca) => {}
            }
        }
        if let Some(certificate) = &tls.certificate {
            match resources.certificates.get(certificate).map(|c| c.kind()) {
                None => problems.push(format!("{place}: certificate: {}", missing(resources, certificate))),
                Some(CertificateKind::Ca) => problems.push(format!(
                    "{place}: certificate: `{certificate}` is a CA; a client certificate needs its key (`cert_file` and `key_file`, or `cert` and `key`)"
                )),
                Some(CertificateKind::Pair) => {}
            }
        }
    }
    problems
}

/// `tls:` on a listener.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListenTls {
    /// The `certificates:` entry (resources.yaml) this listener presents.
    pub certificate: String,
    /// Ask senders for a certificate, and refuse those without one these allow.
    #[serde(default)]
    pub clients: Option<Clients>,
}

/// Which client certificates a listener takes.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Clients {
    /// The `certificates:` entry with the CA that issues them.
    pub ca: String,
    /// Subject patterns; a certificate matching any one is taken. Empty: any the CA issued.
    #[serde(default)]
    pub subjects: Vec<String>,
}

/// The attributes a subject pattern may name.
const ATTRIBUTES: [&str; 8] = ["CN", "O", "OU", "C", "L", "ST", "DC", "DNS"];

/// One `subjects:` entry, parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubjectPattern {
    /// (attribute in upper case, pattern in lower case), all of which must match.
    parts: Vec<(String, String)>,
}

impl SubjectPattern {
    pub fn parse(text: &str) -> Result<Self> {
        let mut parts = Vec::new();
        for part in text.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let Some((attribute, value)) = part.split_once('=') else {
                bail!("subject `{text}`: write each part as ATTR=value, like CN=bridges*.hospital.org");
            };
            let attribute = attribute.trim().to_ascii_uppercase();
            if !ATTRIBUTES.contains(&attribute.as_str()) {
                bail!("subject `{text}`: `{attribute}` isn't one of {}", ATTRIBUTES.join(", "));
            }
            let value = value.trim();
            if value.is_empty() {
                bail!("subject `{text}`: `{attribute}=` needs a value (`*` for any)");
            }
            parts.push((attribute, value.to_lowercase()));
        }
        if parts.is_empty() {
            bail!("a subject pattern can't be empty");
        }
        Ok(Self { parts })
    }

    /// Whether a certificate with these subject attributes and DNS names matches: every part
    /// must match one of the certificate's values for its attribute.
    pub fn matches(&self, presented: &Presented) -> bool {
        self.parts.iter().all(|(attribute, pattern)| {
            let mut values: Box<dyn Iterator<Item = &String>> = if attribute == "DNS" {
                Box::new(presented.dns.iter())
            } else {
                Box::new(presented.subject.iter().filter(|(a, _)| a == attribute).map(|(_, v)| v))
            };
            values.any(|v| crate::glob(pattern, &v.to_lowercase()))
        })
    }
}

/// What a client certificate says about who it is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Presented {
    /// Subject attributes in upper case (`CN`, `O`, ...) with their values.
    pub subject: Vec<(String, String)>,
    /// DNS names (subject alternative names).
    pub dns: Vec<String>,
}

/// Whether `presented` is allowed by `subjects` (none: everyone the CA issued).
pub fn allowed(subjects: &[SubjectPattern], presented: &Presented) -> bool {
    subjects.is_empty() || subjects.iter().any(|s| s.matches(presented))
}

/// Every listener's `tls`, with where it is (`interface `adt``, `api`).
pub fn listeners<'a>(
    configs: impl IntoIterator<Item = &'a InterfaceConfig>,
    api: Option<&ListenTls>,
) -> Vec<(String, ListenTls)> {
    let mut all: Vec<(String, ListenTls)> = configs
        .into_iter()
        .filter_map(|c| match &c.source {
            Source::Mllp { tls: Some(t), .. } | Source::Http { tls: Some(t), .. } => {
                Some((format!("interface `{}`", c.name), t.clone()))
            }
            _ => None,
        })
        .collect();
    all.extend(api.map(|t| ("api (ductus.yaml)".to_string(), t.clone())));
    all
}

/// Problems with `tls:` across a project: names resources.yaml doesn't define, a CA where a
/// certificate with its key belongs (or the other way round), and subject patterns that don't
/// parse.
pub fn check(listeners: &[(String, ListenTls)], resources: &Resources) -> Vec<String> {
    let mut problems = Vec::new();
    for (place, tls) in listeners {
        match resources.certificates.get(&tls.certificate).map(|c| c.kind()) {
            None => problems.push(format!("{place}: tls.certificate: {}", missing(resources, &tls.certificate))),
            Some(CertificateKind::Ca) => problems.push(format!(
                "{place}: tls.certificate: `{}` is a CA; a listener needs a certificate with its key (`cert_file` and `key_file`, or `cert` and `key`)",
                tls.certificate
            )),
            Some(CertificateKind::Pair) => {}
        }
        let Some(clients) = &tls.clients else { continue };
        match resources.certificates.get(&clients.ca).map(|c| c.kind()) {
            None => problems.push(format!("{place}: tls.clients.ca: {}", missing(resources, &clients.ca))),
            Some(CertificateKind::Pair) => problems.push(format!(
                "{place}: tls.clients.ca: `{}` is a certificate with its key; name an entry with `ca` or `ca_file`",
                clients.ca
            )),
            Some(CertificateKind::Ca) => {}
        }
        for subject in &clients.subjects {
            if let Err(e) = SubjectPattern::parse(subject) {
                problems.push(format!("{place}: tls.clients.subjects: {e}"));
            }
        }
    }
    problems
}

fn missing(resources: &Resources, name: &str) -> String {
    resources.missing(Kind::Certificate, name).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn presented(subject: &[(&str, &str)], dns: &[&str]) -> Presented {
        Presented {
            subject: subject.iter().map(|(a, v)| (a.to_string(), v.to_string())).collect(),
            dns: dns.iter().map(|d| d.to_string()).collect(),
        }
    }

    #[test]
    fn subject_patterns_match_every_part() {
        let bridges = presented(&[("CN", "Bridges01.Hospital.org"), ("O", "Hospital"), ("OU", "IT")], &["b1.local"]);
        let quest = presented(&[("CN", "lab-feed"), ("O", "Quest"), ("OU", "Lab")], &[]);
        let p = |t: &str| SubjectPattern::parse(t).unwrap();
        assert!(p("CN=bridges*.hospital.org").matches(&bridges), "case doesn't matter");
        assert!(!p("CN=bridges*.hospital.org").matches(&quest));
        assert!(p("O=Quest, OU=Lab").matches(&quest));
        assert!(!p("O=Quest, OU=Billing").matches(&quest), "every part must match");
        assert!(p("dns=b?.local").matches(&bridges));
        assert!(!p("DNS=*").matches(&quest), "no DNS names: a DNS part can't match");
        assert!(p("OU=*").matches(&bridges));

        let subjects = [p("CN=bridges*"), p("O=Quest")];
        assert!(allowed(&subjects, &bridges) && allowed(&subjects, &quest));
        assert!(!allowed(&subjects, &presented(&[("CN", "someone")], &[])));
        assert!(allowed(&[], &presented(&[], &[])), "no patterns: any certificate the CA issued");
    }

    #[test]
    fn bad_subject_patterns_say_what_to_write() {
        let err = |t: &str| SubjectPattern::parse(t).unwrap_err().to_string();
        assert!(err("bridges01").contains("write each part as ATTR=value"));
        assert!(err("SERIAL=1").contains("`SERIAL` isn't one of CN, O"));
        assert!(err("CN=").contains("needs a value"));
        assert!(err(" , ").contains("can't be empty"));
    }

    #[test]
    fn checks_what_each_listener_names() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("resources.yaml"),
            "certificates:\n  mllp: { cert: CERT, key: KEY }\n  hospital-ca: { ca: CA }\n",
        )
        .unwrap();
        let resources = Resources::load(dir.path(), &crate::Environment::with_variables("dev", &[])).unwrap();
        let tls = |certificate: &str, ca: Option<&str>, subjects: &[&str]| ListenTls {
            certificate: certificate.into(),
            clients: ca.map(|ca| Clients { ca: ca.into(), subjects: subjects.iter().map(|s| s.to_string()).collect() }),
        };
        let good = tls("mllp", Some("hospital-ca"), &["CN=x*"]);
        let swapped = tls("hospital-ca", Some("mllp"), &["nonsense"]);
        let unknown = tls("nope", Some("nope-ca"), &[]);
        let problems = check(
            &[("interface `a`".into(), good), ("interface `b`".into(), swapped), ("api (ductus.yaml)".into(), unknown)],
            &resources,
        );
        assert_eq!(
            problems,
            [
                "interface `b`: tls.certificate: `hospital-ca` is a CA; a listener needs a certificate with its key (`cert_file` and `key_file`, or `cert` and `key`)",
                "interface `b`: tls.clients.ca: `mllp` is a certificate with its key; name an entry with `ca` or `ca_file`",
                "interface `b`: tls.clients.subjects: subject `nonsense`: write each part as ATTR=value, like CN=bridges*.hospital.org",
                "api (ductus.yaml): tls.certificate: no certificate named `nope` in resources.yaml (defined: hospital-ca, mllp)",
                "api (ductus.yaml): tls.clients.ca: no certificate named `nope-ca` in resources.yaml (defined: hospital-ca, mllp)",
            ]
        );
    }

    #[test]
    fn mllp_destinations_take_tls_true_or_names() {
        let destination = |tls: &str| -> std::result::Result<Destination, String> {
            let yaml = format!(
                "name: x\nsource: {{ mllp: {{ port: 1 }} }}\ndestination: {{ mllp: {{ host: h, port: 2{tls} }} }}\n"
            );
            serde_yaml_ng::from_str::<InterfaceConfig>(&yaml)
                .map(|c| c.destination.unwrap().connector)
                .map_err(|e| e.to_string())
        };
        let tls = |d: Destination| match d {
            Destination::Mllp { tls, .. } => tls,
            _ => panic!("an MLLP destination"),
        };
        assert_eq!(tls(destination("").unwrap()), None);
        assert_eq!(tls(destination(", tls: false").unwrap()), None);
        assert_eq!(tls(destination(", tls: true").unwrap()), Some(ConnectTls::default()));
        assert_eq!(
            tls(destination(", tls: { ca: hie-ca, certificate: ours }").unwrap()),
            Some(ConnectTls { ca: Some("hie-ca".into()), certificate: Some("ours".into()) })
        );
        let err = destination(", tls: { ca: hie-ca, cert: ours }").unwrap_err();
        assert!(err.contains("tls: `true`, or { ca, certificate }") && err.contains("unknown field `cert`"), "{err}");
    }

    #[test]
    fn checks_what_each_connection_names() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("resources.yaml"),
            "certificates:\n  ours: { cert: CERT, key: KEY }\n  hie-ca: { ca: CA }\napis:\n  empi: { url: 'https://empi', ca: ours }\nldap:\n  corp: { url: 'ldaps://dc1', ca: hie-ca }\n",
        )
        .unwrap();
        let resources = Resources::load(dir.path(), &crate::Environment::with_variables("dev", &[])).unwrap();
        let config: InterfaceConfig = serde_yaml_ng::from_str(
            "name: adt\nsource: { mllp: { port: 1 } }\ndestinations:\n  hie: { mllp: { host: h, port: 2, tls: { ca: hie-ca, certificate: hie-ca } } }\n  plain: { mllp: { host: h, port: 3, tls: true } }\n  lab: { http: { url: 'https://lab/x', certificate: nope } }\n",
        )
        .unwrap();
        let alerts: AlertSettings = serde_yaml_ng::from_str(
            "smtp: { host: m, from: d@example.org, ca: hie-ca }\nchannels:\n  chat: { webhook: { url: 'https://chat/h', certificate: ours } }\n",
        )
        .unwrap();
        let connections = outbound([&config], &resources, Some(&alerts));
        assert_eq!(
            connections.iter().map(|(place, _)| place.as_str()).collect::<Vec<_>>(),
            [
                "interface `adt` destination `hie`",
                "interface `adt` destination `lab`",
                "api `empi` (resources.yaml)",
                "ldap `corp` (resources.yaml)",
                "alerts.smtp",
                "alerts.channels.chat.webhook",
            ],
            "`tls: true` names nothing to check"
        );
        assert_eq!(
            check_outbound(&connections, &resources),
            [
                "interface `adt` destination `hie`: certificate: `hie-ca` is a CA; a client certificate needs its key (`cert_file` and `key_file`, or `cert` and `key`)",
                "interface `adt` destination `lab`: certificate: no certificate named `nope` in resources.yaml (defined: hie-ca, ours)",
                "api `empi` (resources.yaml): ca: `ours` is a certificate with its key; name an entry with `ca` or `ca_file` (the CA that issued the server's certificate)",
            ]
        );
    }
}
