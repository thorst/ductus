//! `ductus cert show`: what a certificate file (or a `certificates:` entry) holds, judged the way
//! the engine and doctor judge it: the chain's order and completeness (`doctor::judge_chain`),
//! the days left (the earliest in the chain), and whether the key is the certificate's (the check
//! the engine's start makes). Reads files only: nothing connects anywhere.

use crate::doctor::{Certificate, EXPIRY_WARNING_DAYS, Outcome, Step, judge_chain};
use crate::tls::{About, Certificates, check_pair};
use chrono::Utc;
use rustls::pki_types::{CertificateDer, pem::PemObject};

/// What was found, step by step (as doctor prints a check), and what to do.
#[derive(Debug, Default)]
pub struct Shown {
    pub steps: Vec<Step>,
    pub advice: Vec<String>,
}

impl Shown {
    /// The worst step.
    pub fn outcome(&self) -> Outcome {
        self.steps.iter().map(|s| s.outcome).max().unwrap_or(Outcome::Ok)
    }

    pub fn to_text(&self) -> String {
        use std::fmt::Write;
        let mut out = String::new();
        for s in &self.steps {
            let mark = if s.outcome == Outcome::Ok { " " } else { s.outcome.mark() };
            let _ = writeln!(out, "{mark} {:<8} {}", s.name, s.detail);
        }
        for a in &self.advice {
            let _ = writeln!(out, "→ {a}");
        }
        out
    }

    fn step(&mut self, name: &'static str, outcome: Outcome, detail: impl Into<String>) {
        self.steps.push(Step { name, outcome, detail: detail.into() });
    }
}

/// The key to check against the certificate: where it came from (said in the output), its PEM.
pub struct Key<'a> {
    pub from: String,
    pub pem: &'a [u8],
}

/// Judge `pem` (a certificate file: the certificate first, then the ones that issued it; or a CA
/// file: one or more CA certificates) and, if given, `key`. `certificates`: the project's, so a
/// chain ending at one of its CAs counts as complete (as doctor counts it).
pub fn show(pem: &[u8], key: Option<Key<'_>>, certificates: &Certificates) -> Shown {
    let mut shown = Shown::default();
    let text = String::from_utf8_lossy(pem);
    let chain: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(pem).filter_map(Result::ok).collect();
    let chain = if !chain.is_empty() {
        chain
    } else if text.contains("-----BEGIN CERTIFICATE REQUEST-----")
        || text.contains("-----BEGIN NEW CERTIFICATE REQUEST-----")
    {
        shown.step("file", Outcome::Fail, "a certificate signing request (CSR), not a certificate");
        shown.advice.push(
            "Send the request to the CA; what comes back is the certificate (and usually the CA's own certificates: put ours first, then theirs, in one file)."
                .into(),
        );
        return shown;
    } else if text.contains("PRIVATE KEY-----") {
        shown.step("file", Outcome::Fail, "a private key, not a certificate");
        shown.advice.push("Show the certificate's file, with the key beside it (--key FILE).".into());
        return shown;
    } else if About::read(pem).is_ok() {
        // DER (binary), as Windows CAs often hand out .cer files: readable here, not by the engine.
        shown.step("file", Outcome::Fail, "DER (binary); cert_file must be PEM text");
        shown.advice.push(
            "Convert it to PEM: `openssl x509 -inform der -in FILE -out FILE.pem` (or, on Windows, export it as \"Base-64 encoded X.509\")."
                .into(),
        );
        vec![CertificateDer::from(pem.to_vec())]
    } else {
        shown.step("file", Outcome::Fail, "no certificate in it (PEM text starting -----BEGIN CERTIFICATE-----)");
        return shown;
    };
    let read: Vec<(About, Certificate, bool)> =
        chain.iter().filter_map(|der| Some((About::read(der).ok()?, Certificate::read(der)?, is_ca(der)))).collect();
    if read.len() < chain.len() {
        shown.step(
            "file",
            Outcome::Fail,
            format!("{} of its {} certificates can't be read", chain.len() - read.len(), chain.len()),
        );
        return shown;
    }
    let (about, leaf, leaf_is_ca) = &read[0];
    // A CA file: a list of CAs (often several roots), not a chain to judge.
    if *leaf_is_ca && key.is_none() && read.iter().all(|(_, _, ca)| *ca) {
        for (about, c, _) in &read {
            let (outcome, until) = until(about);
            shown.step("CA", outcome, format!("{}, {}, {until}", about.subject, issued(c)));
        }
        return shown;
    }
    shown.step("subject", Outcome::Ok, about.subject.clone());
    if about.names.is_empty() {
        shown.step("names", Outcome::Warn, "none: senders that check the name they connect to refuse it");
        shown.advice.push("Ask the CA for one with the DNS names (and IP addresses) senders connect to (`ductus cert request NAME --dns HOST`).".into());
    } else {
        shown.step("names", Outcome::Ok, about.names.join(", "));
    }
    shown.step("issuer", Outcome::Ok, issued(leaf));
    if *leaf_is_ca {
        shown.step("kind", Outcome::Warn, "a CA's certificate (CA:TRUE), not a server's or client's");
        shown.advice.push("TLS clients that check (Ductus's own connections among them) refuse a CA's certificate as a server's or client's: ask the CA for a server certificate (`openssl req -x509` makes a CA's by default).".into());
    }
    let (outcome, until) = until(about);
    shown.step("valid", outcome, format!("{} → {until}", about.not_before.format("%Y-%m-%d")));
    if read.len() > 1 {
        let listed: Vec<String> = read
            .iter()
            .enumerate()
            .map(|(n, (a, c, _))| format!("{}. {} (until {})", n + 1, c.subject, a.not_after.format("%Y-%m-%d")))
            .collect();
        shown.step("chain", Outcome::Ok, listed.join("  "));
        if let Some((a, c, _)) =
            read[1..].iter().min_by_key(|(a, ..)| a.not_after).filter(|(a, ..)| a.not_after < about.not_after)
        {
            let days = (a.not_after - Utc::now()).num_days();
            let outcome = if days < 0 {
                Outcome::Fail
            } else if days < EXPIRY_WARNING_DAYS {
                Outcome::Warn
            } else {
                Outcome::Ok
            };
            shown.step(
                "expiry",
                outcome,
                format!("the chain's {} ends sooner: {} ({days} days)", c.subject, a.not_after.format("%Y-%m-%d")),
            );
        }
    }
    let served: Vec<Vec<u8>> = chain.iter().map(|c| c.to_vec()).collect();
    let (outcome, detail, advice) = judge_chain(&served, certificates);
    shown.step(if read.len() > 1 { "order" } else { "chain" }, outcome, detail);
    shown.advice.extend(advice);
    match key {
        None => shown.step("key", Outcome::Ok, "not checked (give the key with --key FILE)"),
        Some(key) => match check_pair(pem, key.pem) {
            Ok(_) => shown.step("key", Outcome::Ok, format!("matches ({})", key.from)),
            Err(e) => {
                shown.step("key", Outcome::Fail, format!("{} ({})", e, key.from));
                shown.advice.push("The engine refuses a pair whose key isn't the certificate's: use the key the request was made with (the CSR's .key file).".into());
            }
        },
    }
    shown
}

fn issued(c: &Certificate) -> String {
    if c.issuer == c.subject { "self-signed".into() } else { format!("issued by {}", c.issuer) }
}

/// Until when, with the days left, and how that counts.
fn until(about: &About) -> (Outcome, String) {
    let days = (about.not_after - Utc::now()).num_days();
    let date = about.not_after.format("%Y-%m-%d");
    if about.not_after < Utc::now() {
        (Outcome::Fail, format!("{date}: expired"))
    } else if about.not_before > Utc::now() {
        (Outcome::Warn, format!("{date}: not valid yet (is this server's clock right?)"))
    } else if days < EXPIRY_WARNING_DAYS {
        (Outcome::Warn, format!("{date}: {days} days left"))
    } else {
        (Outcome::Ok, format!("{date}: {days} days left"))
    }
}

fn is_ca(der: &[u8]) -> bool {
    use x509_parser::prelude::*;
    X509Certificate::from_der(der).is_ok_and(|(_, c)| c.is_ca())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tls::testing::*;
    use std::collections::BTreeMap;

    fn step<'a>(shown: &'a Shown, name: &str) -> &'a Step {
        shown.steps.iter().find(|s| s.name == name).unwrap_or_else(|| panic!("no step {name}: {shown:#?}"))
    }

    #[test]
    fn a_complete_chain_with_its_key() {
        let root = ca("Test Root");
        let intermediate = intermediate(&root, "Test Intermediate");
        let (cert, key) = leaf(&intermediate, &[(rcgen::DnType::CommonName, "engine")], &["engine.example.org"], 90);
        let pem = format!("{cert}{}", intermediate.cert.pem());
        let shown = show(
            pem.as_bytes(),
            Some(Key { from: "engine.key".into(), pem: key.as_bytes() }),
            &Certificates::default(),
        );
        assert_eq!(shown.outcome(), Outcome::Warn, "only the order step warns: {}", shown.to_text());
        assert_eq!(step(&shown, "subject").detail, "CN=engine");
        assert_eq!(step(&shown, "names").detail, "engine.example.org");
        assert_eq!(step(&shown, "issuer").detail, "issued by Test Intermediate");
        assert!(
            step(&shown, "valid").detail.ends_with(": 90 days left")
                || step(&shown, "valid").detail.ends_with(": 89 days left"),
            "{shown:#?}"
        );
        assert!(step(&shown, "chain").detail.starts_with("1. engine (until "), "{shown:#?}");
        // The root isn't served and this server doesn't know it: said both ways.
        let order = step(&shown, "order");
        assert_eq!(order.outcome, Outcome::Warn);
        assert!(order.detail.starts_with("incomplete: it stops at Test Intermediate"), "{order:?}");
        assert_eq!(step(&shown, "key").detail, "matches (engine.key)");

        // With the root as a project CA it's complete.
        let entries = BTreeMap::from([("hospital-ca".to_string(), ca_entry(&root))]);
        let shown = show(pem.as_bytes(), None, &Certificates::load(&entries).unwrap());
        assert_eq!(shown.outcome(), Outcome::Ok, "{}", shown.to_text());
        assert!(step(&shown, "order").detail.contains("the CA `hospital-ca` in resources.yaml"), "{shown:#?}");
        assert_eq!(step(&shown, "key").detail, "not checked (give the key with --key FILE)");
    }

    #[test]
    fn wrong_order_wrong_key_and_expiry_fail_or_warn() {
        let root = ca("Test Root");
        let intermediate = intermediate(&root, "Test Intermediate");
        let (soon, _) = leaf(&intermediate, &[(rcgen::DnType::CommonName, "engine")], &["engine"], 10);
        let (_, other_key) = leaf(&root, &[(rcgen::DnType::CommonName, "other")], &["other"], 90);
        let pem = format!("{}{soon}", intermediate.cert.pem());
        let key = Key { from: "other.key".into(), pem: other_key.as_bytes() };
        let shown = show(pem.as_bytes(), Some(key), &Certificates::default());
        assert_eq!(shown.outcome(), Outcome::Fail, "{}", shown.to_text());
        assert_eq!(step(&shown, "order").outcome, Outcome::Warn);
        assert!(step(&shown, "order").detail.starts_with("out of order"), "{shown:#?}");
        assert_eq!(step(&shown, "key").detail, "the private key isn't this certificate's key (other.key)");

        let pem = format!("{soon}{}", intermediate.cert.pem());
        let shown = show(pem.as_bytes(), None, &Certificates::default());
        assert_eq!(step(&shown, "valid").outcome, Outcome::Warn, "10 days: renew soon");

        let (expired, _) = leaf(&root, &[(rcgen::DnType::CommonName, "old")], &["old"], -2);
        let shown = show(expired.as_bytes(), None, &Certificates::default());
        assert_eq!(step(&shown, "valid").outcome, Outcome::Fail);
        assert!(step(&shown, "valid").detail.ends_with(": expired"), "{shown:#?}");
    }

    #[test]
    fn not_a_certificate_is_said_plainly() {
        let none = Certificates::default();
        let (csr, key) = crate::tls::request(&["engine.example.org".into()], "", crate::tls::KeyType::Ecdsa).unwrap();
        let shown = show(csr.as_bytes(), None, &none);
        assert_eq!(step(&shown, "file").detail, "a certificate signing request (CSR), not a certificate");
        let shown = show(key.as_bytes(), None, &none);
        assert_eq!(step(&shown, "file").detail, "a private key, not a certificate");
        // DER: read, with how to convert it.
        let root = ca("Test Root");
        let (cert, _) = leaf(&root, &[(rcgen::DnType::CommonName, "engine")], &["engine"], 90);
        let der = CertificateDer::from_pem_slice(cert.as_bytes()).unwrap();
        let shown = show(der.as_ref(), None, &none);
        assert_eq!(step(&shown, "file").outcome, Outcome::Fail);
        assert_eq!(step(&shown, "subject").detail, "CN=engine");
        assert!(shown.advice[0].contains("openssl x509 -inform der"), "{shown:#?}");
        let shown = show(b"hello", None, &none);
        assert!(step(&shown, "file").detail.starts_with("no certificate in it"));
    }

    #[test]
    fn a_ca_certificate_used_as_a_pair_warns() {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(vec!["localhost".to_string()]).unwrap();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let cert = params.self_signed(&key).unwrap().pem();
        let key = key.serialize_pem();
        let shown =
            show(cert.as_bytes(), Some(Key { from: "lab.key".into(), pem: key.as_bytes() }), &Certificates::default());
        assert_eq!(step(&shown, "kind").outcome, Outcome::Warn, "{shown:#?}");
        assert_eq!(step(&shown, "key").detail, "matches (lab.key)");
    }

    #[test]
    fn a_ca_file_lists_its_cas() {
        let (a, b) = (ca("Root A"), ca("Root B"));
        let pem = format!("{}{}", a.cert.pem(), b.cert.pem());
        let shown = show(pem.as_bytes(), None, &Certificates::default());
        assert_eq!(shown.steps.len(), 2, "{shown:#?}");
        assert!(shown.steps.iter().all(|s| s.name == "CA" && s.detail.contains("self-signed")), "{shown:#?}");
    }
}
