//! The engine's own connection checks (`doctor:` in ductus.yaml): every `schedule` (6 h by
//! default; the first a minute after start, plus up to a minute at random), the same checks as
//! `ductus doctor`, certificates only unless `mode: full`. Each run is kept (`doctor_runs`, the last
//! 30), and every certificate it saw too (`certificate_status`), which `alerts.rs` judges.

use crate::control::Engine;
use crate::doctor::{self, Mode, Probe, Report, Target};
use anyhow::Result;
use chrono::Utc;
use ductus_core::InterfaceConfig;
use ductus_store::{CertificateRow, DoctorRunRow};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long each step of a scheduled check may wait.
const STEP_TIMEOUT: Duration = Duration::from_secs(10);
/// The first scheduled check, after the engine starts (plus up to as much again at random).
const FIRST_AFTER: Duration = Duration::from_secs(60);

/// A certificate the engine connects to, as alerts know it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertificateTarget {
    /// `cert:<resource>:<host>:<port>`.
    pub key: String,
    pub resource: String,
    pub host: String,
    pub port: u16,
    /// Who owns it, when anyone is named (a URL destination's owner, or the resources.yaml entry's).
    pub owner: String,
    /// The interface whose `alert:` options apply (a URL destination's).
    pub interface: Option<String>,
}

/// Everything to check, as `ductus doctor` would (listening ports aside: the engine holds them),
/// plus what alerts connect to; `only` limits it to what those interfaces use.
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
        .filter(|p| !matches!(p.target, Target::Listener { .. }))
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
        out.push(CertificateTarget { key, resource, host, port, owner, interface: destination.map(|(i, _)| i) });
    }
    out
}

/// Run a check now: `who` is `schedule` or the person or token who asked. Keeps the run and the
/// certificates it saw.
pub async fn run(engine: &Engine, mode: Mode, who: &str, only: &[String], timeout: Duration) -> Result<Report> {
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

/// Start the scheduled checks (if `doctor.schedule` isn't `off`). Ends with the engine.
pub fn start(engine: &Arc<Engine>) {
    let Some(every) = engine.doctor.schedule else {
        tracing::info!("scheduled connection checks are off (`doctor.schedule: off`)");
        return;
    };
    let mode = engine.doctor.mode;
    if mode == Mode::Certificates && engine.certificates.is_empty() {
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
}
