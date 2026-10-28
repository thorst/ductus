//! `ductus doctor`: what the project connects to and listens on, checked from this server.
//! This file decides what to check (from the configs, resources.yaml and what code uses); the
//! checks themselves are `ductus_runtime::doctor`.

use crate::project::{InterfaceDir, code_uses, first_line, interface_dirs};
use anyhow::{Context, Result, bail};
use ductus_core::{Environment, InterfaceConfig, Resources};
use ductus_runtime::doctor::{self, Report};
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub struct Options {
    pub root: PathBuf,
    pub env: String,
    pub database_url: String,
    /// Only what these interfaces use (and the engine's database).
    pub interfaces: Vec<String>,
    pub timeout: Duration,
    /// Also save the report here: JSON if the name ends in `.json`, else text.
    pub report: Option<PathBuf>,
}

/// Returns false if any check failed (or the project couldn't be read).
pub async fn run(o: Options) -> Result<bool> {
    let environment = Environment::load(&o.root, &o.env)?;
    let interfaces = interface_dirs(&o.root, &environment)?;
    let names: Vec<String> = interfaces.iter().map(InterfaceDir::label).collect();
    let unknown: Vec<&str> = o.interfaces.iter().filter(|i| !names.contains(i)).map(String::as_str).collect();
    if !unknown.is_empty() {
        bail!("no interface named {} (interfaces: {})", unknown.join(", "), names.join(", "));
    }
    let mut complete = true;
    for i in &interfaces {
        if let Err(e) = &i.config {
            complete = false;
            println!("✗ {}: not checked, its config doesn't load: {}", i.label(), first_line(&format!("{e:#}")));
        }
    }
    let resources = Resources::load(&o.root, &environment).context("resources.yaml")?;
    let code = code_uses(&o.root, &interfaces).await?;
    let configs: Vec<&InterfaceConfig> = interfaces.iter().filter_map(|i| i.config.as_ref().ok()).collect();
    let mut probes = doctor::plan(&configs, &resources, &code, &o.database_url, &o.interfaces);
    // What alerts connect to (mail server, webhooks, heartbeat); broken alert settings are
    // `ductus validate`'s to report.
    if o.interfaces.is_empty() {
        let alerts = ductus_core::settings::Settings::load(&o.root).and_then(|s| s.alerts(&environment));
        match alerts {
            Ok(Some(a)) => probes.extend(doctor::alert_probes(&a)),
            Ok(None) => {}
            Err(e) => println!("⚠ alerts: not checked, their settings don't load: {}", first_line(&format!("{e:#}"))),
        }
    }

    // As the engine loads them; one that doesn't load fails the checks that name it (with why).
    let certificates = std::sync::Arc::new(ductus_runtime::tls::Certificates::load_what_loads(&resources.certificates));
    let started = Instant::now();
    let checks = doctor::check(probes, o.timeout, &certificates).await;
    let report = Report {
        generated_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S %:z").to_string(),
        environment: o.env.clone(),
        host: doctor::hostname(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        seconds: started.elapsed().as_secs_f64(),
        notes: crate::service::notes(Some(&resources), true),
        checks,
    };
    let text = report.to_text();
    print!("{text}");
    if let Some(path) = &o.report {
        let saved = if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("json")) {
            serde_json::to_string_pretty(&report)?
        } else {
            text
        };
        std::fs::write(path, saved).with_context(|| format!("writing {}", path.display()))?;
        println!("\nreport saved to {}", path.display());
    }
    Ok(complete && report.failed() == 0)
}
