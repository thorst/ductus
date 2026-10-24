//! The engine's own connection checks (`doctor:` in ductus.yaml): every `schedule` (6 h by
//! default; the first a minute after start, plus up to a minute at random), the same checks as
//! `ductus doctor`, certificates only unless `mode: full`. Each run is kept (`doctor_runs`, the last
//! 30), and every certificate it saw too (`certificate_status`), which `alerts.rs` judges.
//!
//! Our own certificates (the `certificates:` pairs the engine presents) are read from what it
//! loaded, never over the network: their rows are written at the start, and again only when the
//! minute's file check (`tls::Certificates::watch`) finds a renewal or a renewal that can't be
//! used. Alerts and the API build them from memory too, so they cost no database work at all
//! while nothing changes.

use crate::control::{ApiListener, Engine};
use crate::doctor::{self, Mode, Probe, Report, Target};
use anyhow::Result;
use chrono::{DateTime, Utc};
use ductus_core::InterfaceConfig;
use ductus_store::{CertificateRow, DoctorRunRow};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long each step of a scheduled check may wait.
const STEP_TIMEOUT: Duration = Duration::from_secs(10);
/// The first scheduled check, after the engine starts (plus up to as much again at random).
const FIRST_AFTER: Duration = Duration::from_secs(60);

/// Which side of a connection a certificate is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// A server the engine connects to: read by its checks.
    Remote,
    /// A pair the engine presents (to senders, or as its client certificate): read from memory.
    Ours,
}

impl Side {
    /// As stored (`certificate_status.side`) and in the API.
    pub fn label(self) -> &'static str {
        match self {
            Side::Remote => "remote",
            Side::Ours => "ours",
        }
    }
}

/// A certificate the engine connects to or presents, as alerts know it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertificateTarget {
    /// `cert:<resource>:<host>:<port>`; ours `cert:<name>:<this server>:0`.
    pub key: String,
    /// Ours: the `certificates:` entry.
    pub resource: String,
    /// Ours: this server's name.
    pub host: String,
    /// Ours: 0 (one pair can be presented on several ports).
    pub port: u16,
    /// Who owns it, when anyone is named (a URL destination's owner, or the resources.yaml entry's).
    pub owner: String,
    /// The interface whose `alert:` options apply (a URL destination's; ours: the one interface
    /// it is presented for, if only one).
    pub interface: Option<String>,
    pub side: Side,
    /// Ours: where it is presented (`0.0.0.0:6661 MLLP over TLS (adt)`, ...).
    pub presented: Vec<String>,
}

/// The pairs the engine presents, one target each: on a TLS listener (interfaces', the API's) or
/// as a client certificate. The owner is the entry's (`owner:` in resources.yaml), else that of
/// the one interface it serves.
pub fn ours(
    configs: &[&InterfaceConfig],
    resources: &ductus_core::Resources,
    alerts: Option<&ductus_core::alerts::AlertSettings>,
    api: Option<&ApiListener>,
    host: &str,
) -> Vec<CertificateTarget> {
    let api = api.and_then(|a| Some((a.host.as_str(), a.port, a.tls.as_ref()?)));
    let places = ductus_core::tls::presented(configs.iter().copied(), api, resources, alerts);
    let mut out: Vec<CertificateTarget> = Vec::new();
    for p in places {
        if let Some(t) = out.iter_mut().find(|t| t.resource == p.certificate) {
            if t.interface != p.interface {
                t.interface = None; // serves several: no one interface's options
            }
            t.presented.push(p.place);
            continue;
        }
        out.push(CertificateTarget {
            key: ductus_core::alerts::certificate_key(&p.certificate, host, 0),
            resource: p.certificate,
            host: host.to_string(),
            port: 0,
            owner: String::new(),
            interface: p.interface,
            side: Side::Ours,
            presented: vec![p.place],
        });
    }
    for t in &mut out {
        let interface = t.interface.as_deref().and_then(|i| configs.iter().find(|c| c.name == i));
        let owner =
            resources.owner(&t.resource).or_else(|| interface.and_then(|c| ductus_core::about::given(&c.about.owner)));
        t.owner = owner.unwrap_or("").to_string();
    }
    out
}

/// Our certificate's row, from what the engine loaded (None if it isn't loaded: not ours).
pub fn our_row(
    t: &CertificateTarget,
    certificates: &crate::tls::Certificates,
    at: DateTime<Utc>,
) -> Option<CertificateRow> {
    let pair = certificates.pair(&t.resource).filter(|_| t.side == Side::Ours)?;
    let chain = doctor::Chain::read(pair.chain().as_slice());
    let leaf = chain.leaf.as_ref();
    Some(CertificateRow {
        side: Side::Ours.label().into(),
        resource: t.resource.clone(),
        host: t.host.clone(),
        port: i32::from(t.port),
        subject: leaf.map(|c| c.subject.clone()),
        issuer: leaf.map(|c| c.issuer.clone()),
        fingerprint: leaf.map(|c| c.fingerprint.clone()),
        not_before: leaf.map(|c| c.not_before),
        not_after: leaf.map(|c| c.not_after),
        chain_min_not_after: chain.not_after,
        chain_min_subject: chain.earliest,
        trusted: None,
        error: pair.renewal_error(),
        checked_at: at,
        read_at: Some(at),
    })
}

/// Save our certificates' rows (`only`: those pairs; empty: all of them).
async fn save_ours(engine: &Engine, only: &[String]) -> Result<usize> {
    let now = Utc::now();
    let rows: Vec<CertificateRow> = engine
        .certificates
        .iter()
        .filter(|t| only.is_empty() || only.contains(&t.resource))
        .filter_map(|t| our_row(t, &engine.tls, now))
        .collect();
    if !rows.is_empty() {
        engine.store.save_certificates(&rows).await?;
    }
    Ok(rows.len())
}

/// Everything to check, as `ductus doctor` would, plus what alerts connect to; `only` limits it
/// to what those interfaces use. Of the listening ports (the engine holds them) only the TLS
/// ones, for the chain they serve.
pub fn probes(
    configs: &[&InterfaceConfig],
    resources: &ductus_core::Resources,
    database_url: &str,
    code: &ductus_core::impact::CodeUses,
    alerts: Option<&ductus_core::alerts::AlertSettings>,
    only: &[String],
) -> Vec<Probe> {
    let mut probes: Vec<Probe> = doctor::plan(configs, resources, code, database_url, only)
        .into_iter()
        .filter_map(|mut p| match &mut p.target {
            // The engine holds its ports: a plain listener has nothing to check, unless its
            // `allow:` names hosts to look up.
            Target::Listener { tls: None, allow: None, .. } => None,
            Target::Listener { held, .. } => {
                *held = true;
                Some(p)
            }
            _ => Some(p),
        })
        .collect();
    if only.is_empty() {
        probes.extend(alerts.map(doctor::alert_probes).unwrap_or_default());
    }
    probes
}

/// The certificates the engine connects to (what scheduled checks read and alerts judge).
pub fn targets(
    configs: &[&InterfaceConfig],
    resources: &ductus_core::Resources,
    alerts: Option<&ductus_core::alerts::AlertSettings>,
) -> Vec<CertificateTarget> {
    let probes = probes(configs, resources, "", &Default::default(), alerts, &[]);
    let mut out: Vec<CertificateTarget> = Vec::new();
    for p in doctor::certificates_only(probes) {
        let Some((host, port)) = p.target.address() else { continue };
        let resource = p.resource.clone().unwrap_or_else(|| p.what.clone());
        // A URL destination (`<interface>/<destination>`): its owner, and its interface's options.
        let destination = resource.split_once('/').and_then(|(i, d)| {
            let c = configs.iter().find(|c| c.name == i)?;
            let (_, dest) = c.destinations().find(|(name, _)| *name == d)?;
            let owner = ductus_core::about::given(&dest.owner).or(ductus_core::about::given(&c.about.owner));
            Some((c.name.clone(), owner.unwrap_or("").to_string()))
        });
        let key = ductus_core::alerts::certificate_key(&resource, &host, port);
        if out.iter().any(|t| t.key == key) {
            continue;
        }
        let owner = match &destination {
            Some((_, owner)) => owner.clone(),
            None => resources.owner(&resource).unwrap_or("").to_string(),
        };
        out.push(CertificateTarget {
            key,
            resource,
            host,
            port,
            owner,
            interface: destination.map(|(i, _)| i),
            side: Side::Remote,
            presented: Vec::new(),
        });
    }
    out
}

/// Run a check now: `who` is `schedule` or the person or token who asked. Keeps the run and the
/// certificates it saw, after reading our certificates' files again.
pub async fn run(engine: &Engine, mode: Mode, who: &str, only: &[String], timeout: Duration) -> Result<Report> {
    // Our certificates' files first: a renewal (or a broken one) shows now, not at the next
    // minute's check.
    let changed = engine.tls.reload().await;
    if !changed.is_empty() {
        save_ours(engine, &changed).await?;
    }
    let configs: Vec<&InterfaceConfig> = engine.interfaces.iter().map(|i| &*i.config).collect();
    // What code uses only adds names to "used by" (every resource is checked anyway), and working
    // it out runs the TypeScript compiler: a certificates run (every few hours, by itself) goes
    // without, and a broken analysis mustn't stop a full one.
    let empty = ductus_core::impact::CodeUses::default();
    let code = match mode {
        Mode::Certificates => &empty,
        Mode::Full => match engine.code_uses().await {
            Ok(code) => code,
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "couldn't work out what code uses; checking without it");
                &empty
            }
        },
    };
    let alerts = engine.alerts.as_ref().map(|a| &a.settings);
    let mut probes = probes(&configs, &engine.resources, &engine.database_url, code, alerts, only);
    if let Some(api) = engine.api.as_ref().filter(|_| only.is_empty())
        && let Some(tls) = &api.tls
    {
        probes.push(doctor::api_probe(&api.host, api.port, tls, api.allow.as_ref()));
    }
    if mode == Mode::Certificates {
        probes = doctor::certificates_only(probes);
    }
    let at = Utc::now();
    let started = Instant::now();
    let checks = doctor::check_with(probes, timeout, mode, &engine.tls).await;
    let report = Report {
        generated_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S %:z").to_string(),
        environment: engine.environment.clone(),
        host: doctor::hostname(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        seconds: started.elapsed().as_secs_f64(),
        checks,
    };
    let rows: Vec<CertificateRow> = report
        .checks
        .iter()
        .filter_map(|c| c.certificate.as_ref())
        .map(|c| CertificateRow {
            side: Side::Remote.label().into(),
            resource: c.resource.clone(),
            host: c.host.clone(),
            port: i32::from(c.port),
            subject: c.subject.clone(),
            issuer: c.issuer.clone(),
            fingerprint: c.fingerprint.clone(),
            not_before: c.not_before,
            not_after: c.not_after,
            chain_min_not_after: c.chain_not_after,
            chain_min_subject: c.chain_earliest.clone(),
            trusted: c.fingerprint.as_ref().map(|_| c.trusted),
            error: c.error.clone(),
            checked_at: at,
            read_at: None,
        })
        .collect();
    engine.store.save_certificates(&rows).await?;
    let run = DoctorRunRow {
        id: 0,
        at,
        mode: mode.label().into(),
        started_by: who.into(),
        seconds: report.seconds,
        checks: report.checks.len() as i32,
        failed: report.failed() as i32,
    };
    engine.store.record_doctor_run(&run, &serde_json::to_value(&report)?).await?;
    Ok(report)
}

/// Save our certificates' rows, keep them current as `changes` (the names of pairs whose files
/// were renewed, or couldn't be) come in, and start the scheduled checks (if `doctor.schedule`
/// isn't `off`). Ends with the engine.
pub async fn start(engine: &Arc<Engine>, mut changes: tokio::sync::mpsc::Receiver<Vec<String>>) {
    match save_ours(engine, &[]).await {
        Ok(0) => {}
        Ok(n) => tracing::debug!(certificates = n, "our certificates saved"),
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "could not save our certificates' rows (alerts and the API read them from memory)")
        }
    }
    let weak = Arc::downgrade(engine);
    tokio::spawn(async move {
        while let Some(names) = changes.recv().await {
            let Some(engine) = weak.upgrade() else { return };
            if let Err(e) = save_ours(&engine, &names).await {
                tracing::warn!(error = %format!("{e:#}"), "could not save a renewed certificate's row (the next change or start does)");
            }
        }
    });
    let Some(every) = engine.doctor.schedule else {
        tracing::info!("scheduled connection checks are off (`doctor.schedule: off`)");
        return;
    };
    let mode = engine.doctor.mode;
    if mode == Mode::Certificates && !engine.certificates.iter().any(|t| t.side == Side::Remote) {
        tracing::info!("no connection has a certificate: scheduled checks have nothing to do");
        return;
    }
    let weak = Arc::downgrade(engine);
    tokio::spawn(async move {
        tokio::time::sleep(FIRST_AFTER + jitter(FIRST_AFTER)).await;
        loop {
            let Some(engine) = weak.upgrade() else { return };
            match run(&engine, mode, "schedule", &[], STEP_TIMEOUT).await {
                Ok(r) => {
                    let certificates = r.checks.iter().filter(|c| c.certificate.is_some()).count();
                    tracing::info!(
                        mode = mode.label(),
                        checks = r.checks.len(),
                        failed = r.failed(),
                        certificates,
                        "connection checks done"
                    );
                }
                Err(e) => {
                    tracing::warn!(error = %format!("{e:#}"), "scheduled connection checks failed; trying at the next one")
                }
            }
            drop(engine);
            tokio::time::sleep(every).await;
        }
    });
}

/// Up to `max`, at random: engines started together don't all check at once.
fn jitter(max: Duration) -> Duration {
    use std::hash::{BuildHasher, Hasher};
    let random = std::collections::hash_map::RandomState::new().build_hasher().finish();
    Duration::from_millis(random % (max.as_millis() as u64).max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jitter_stays_under_its_bound() {
        assert!((0..100).all(|_| jitter(Duration::from_secs(60)) < Duration::from_secs(60)));
    }

    fn config(yaml: &str) -> InterfaceConfig {
        serde_yaml_ng::from_str(yaml).unwrap()
    }

    /// One target per pair however many places present it, on this server at port 0 (no remote
    /// row can be there), owned by its entry's owner or else its one interface's.
    #[test]
    fn ours_are_one_target_per_pair() {
        let configs = [
            config(
                "name: adt\nabout: { owner: ADT Team }\nsource: { mllp: { port: 6661, tls: { certificate: web } } }\ndestination: { mllp: { host: hie, port: 7443, tls: { certificate: client } } }\n",
            ),
            config(
                "name: orders\nsource: { http: { port: 8443, path: /o, tls: { certificate: web } } }\ndestination: { file: { dir: o } }",
            ),
            config("name: plain\nsource: { mllp: { port: 6662 } }\ndestination: { file: { dir: o } }"),
        ];
        let configs: Vec<&InterfaceConfig> = configs.iter().collect();
        let resources: ductus_core::Resources = serde_yaml_ng::from_str(
            "certificates:\n  web: { cert: C, key: K, owner: Infrastructure }\n  client: { cert: C, key: K }\n  unused: { cert: C, key: K }\n",
        )
        .unwrap();
        let api = ApiListener {
            host: "0.0.0.0".into(),
            port: 7700,
            tls: Some(ductus_core::tls::ListenTls { certificate: "web".into(), clients: None }),
            allow: None,
        };
        let ours = ours(&configs, &resources, None, Some(&api), "engine-1");
        let shown: Vec<_> = ours
            .iter()
            .map(|t| (t.key.as_str(), t.owner.as_str(), t.interface.as_deref(), t.presented.len()))
            .collect();
        assert_eq!(
            shown,
            [
                ("cert:client:engine-1:0", "ADT Team", Some("adt"), 1),
                ("cert:web:engine-1:0", "Infrastructure", None, 3)
            ],
            "nothing for a pair nobody presents"
        );
        assert!(ours.iter().all(|t| t.side == Side::Ours && t.port == 0 && t.host == "engine-1"));
        assert_eq!(
            ours[1].presented,
            [
                "0.0.0.0:6661 MLLP over TLS (adt)",
                "0.0.0.0:7700 HTTPS (the REST API and web UI)",
                "0.0.0.0:8443 HTTPS (orders)"
            ]
        );
    }

    /// Our row is the loaded pair's chain: its leaf, the earliest expiry (an intermediate's here),
    /// and a renewal that can't be used.
    #[tokio::test]
    async fn our_row_is_what_was_loaded() {
        use crate::tls::testing::*;
        let root = ca("Test Root");
        let middle = intermediate(&root, "Test Intermediate");
        let (leaf_pem, key) = leaf(&middle, &[(rcgen::DnType::CommonName, "engine")], &["engine.lab"], 400);
        let dir = tempfile::tempdir().unwrap();
        let (cert_file, key_file) = (dir.path().join("e.pem"), dir.path().join("e.key"));
        std::fs::write(&cert_file, format!("{leaf_pem}{}", middle.cert.pem())).unwrap();
        std::fs::write(&key_file, &key).unwrap();
        let entry = ductus_core::resources::CertificateEntry {
            cert_file: Some(cert_file.clone()),
            key_file: Some(key_file),
            ..Default::default()
        };
        let certificates =
            crate::tls::Certificates::load(&std::collections::BTreeMap::from([("web".into(), entry)])).unwrap();
        let target = |side| CertificateTarget {
            key: "cert:web:engine-1:0".into(),
            resource: "web".into(),
            host: "engine-1".into(),
            port: 0,
            owner: String::new(),
            interface: None,
            side,
            presented: vec![],
        };
        let now = Utc::now();
        assert!(our_row(&target(Side::Remote), &certificates, now).is_none(), "a remote one is read by checks");
        let row = our_row(&target(Side::Ours), &certificates, now).unwrap();
        assert_eq!(
            (row.side.as_str(), row.subject.as_deref(), row.issuer.as_deref(), row.chain_min_subject.as_deref()),
            ("ours", Some("engine"), Some("Test Intermediate"), None),
            "rcgen's CA certificates end in 4096: the leaf's 400 days end first"
        );
        assert_eq!((row.trusted, row.error.as_deref(), row.read_at), (None, None, Some(now)));
        assert_eq!(row.chain_min_not_after, row.not_after);

        // A renewal that can't be used shows on the row; the dates stay the pair's in use.
        let other = leaf(&middle, &[(rcgen::DnType::CommonName, "renewed")], &["engine.lab"], 90);
        std::fs::write(&cert_file, &other.0).unwrap();
        assert_eq!(certificates.reload().await, ["web"]);
        let broken = our_row(&target(Side::Ours), &certificates, now).unwrap();
        assert!(broken.error.unwrap().contains("the private key isn't this certificate's key"));
        assert_eq!((broken.subject, broken.not_after), (row.subject, row.not_after));
    }
}
