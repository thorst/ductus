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

use crate::InterfaceConfig;
use crate::config::Source;
use crate::resources::{CertificateKind, Kind, Resources};
use anyhow::{Result, bail};
use serde::Deserialize;

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
}
