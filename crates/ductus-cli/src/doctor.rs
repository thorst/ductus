//! `ductus doctor`: what the project connects to and listens on, checked from this server.
//! This file decides what to check (from the configs, resources.yaml and what code uses); the
//! checks themselves are `ductus_runtime::doctor`.

use crate::project::{InterfaceDir, code_uses, first_line, interface_dirs};
use anyhow::{Context, Result, bail};
use ductus_core::config::{Destination, Source};
use ductus_core::resources::Kind;
use ductus_core::{Environment, InterfaceConfig, Resources};
use ductus_runtime::doctor::{self, Probe, Report, Target};
use std::collections::{BTreeMap, BTreeSet};
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
    let probes = probes(&configs, &resources, &code, &o.database_url, &o.interfaces);

    let started = Instant::now();
    let checks = doctor::check(probes, o.timeout).await;
    let report = Report {
        generated_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S %:z").to_string(),
        environment: o.env.clone(),
        host: hostname(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        seconds: started.elapsed().as_secs_f64(),
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

/// Each thing to check once, however many use it, in report order.
#[derive(Default)]
struct Plan {
    /// By (section, key): 1 databases, 2 APIs, 3 SFTP servers, 4 destinations, 5 folders,
    /// 6 listening ports.
    entries: BTreeMap<(u8, String), Entry>,
}

struct Entry {
    what: String,
    target: Target,
    used_by: Vec<String>,
    interfaces: BTreeSet<String>,
}

/// Users shown for a resource nothing uses.
const UNUSED: &str = "nothing yet (defined in resources.yaml)";

impl Plan {
    fn add(&mut self, section: u8, key: String, what: &str, target: Target, user: String, interface: Option<&str>) {
        let e = self.entries.entry((section, key)).or_insert_with(|| Entry {
            what: what.into(),
            target,
            used_by: Vec::new(),
            interfaces: BTreeSet::new(),
        });
        if !e.used_by.contains(&user) {
            e.used_by.push(user);
        }
        e.interfaces.extend(interface.map(str::to_string));
    }

    /// A resources.yaml entry; names that aren't defined are left to `ductus validate`.
    fn resource(&mut self, resources: &Resources, kind: Kind, name: &str, user: String, interface: Option<&str>) {
        let (section, target) = match kind {
            Kind::Database => match resources.databases.get(name) {
                Some(db) => (1, Target::Database(name.into(), db.clone())),
                None => return,
            },
            Kind::Api => match resources.apis.get(name) {
                Some(api) => (2, Target::Api(name.into(), api.clone())),
                None => return,
            },
            Kind::Sftp => match resources.sftp.get(name) {
                Some(s) => (3, Target::Sftp(name.into(), s.clone())),
                None => return,
            },
        };
        self.add(section, name.into(), &format!("{} {name}", kind.label()), target, user, interface);
    }
}

/// What to check: the engine's database first, then everything the interfaces (or only
/// `only`) connect to, read from and listen on, and the resources nothing uses yet.
fn probes(
    configs: &[&InterfaceConfig],
    resources: &Resources,
    code: &ductus_core::impact::CodeUses,
    database_url: &str,
    only: &[String],
) -> Vec<Probe> {
    let mut plan = Plan::default();
    for c in configs {
        let me = Some(c.name.as_str());
        match &c.source {
            Source::Mllp { host, port } | Source::Http { host, port, .. } => {
                let protocol = if matches!(c.source, Source::Mllp { .. }) { "MLLP" } else { "HTTP" };
                let target = Target::Listener { host: host.clone(), port: *port, protocol };
                plan.add(6, format!("{port:05} {host}"), "listening port", target, c.name.clone(), me);
            }
            Source::File(f) => {
                let path = c.dir.join(&f.dir);
                let target = Target::Folder { path: path.clone(), created_if_missing: false };
                plan.add(5, path.display().to_string(), "source folder", target, c.name.clone(), me);
            }
            Source::Sftp(f) => {
                let server = f.server.as_deref().unwrap_or_default();
                plan.resource(resources, Kind::Sftp, server, format!("{} (source)", c.name), me);
            }
            Source::Schedule(_) => {}
        }
        for (name, d) in c.destinations() {
            let label = if c.destination.is_some() { c.name.clone() } else { format!("{} → {name}", c.name) };
            match &d.connector {
                Destination::Mllp { host, port, .. } => {
                    let target = Target::Mllp { host: host.clone(), port: *port };
                    plan.add(4, format!("{host}:{port}"), "MLLP destination", target, label, me);
                }
                Destination::Http { url: Some(url), .. } => {
                    let target = Target::Http { url: url.clone() };
                    plan.add(4, url.clone(), "HTTP destination", target, label, me);
                }
                Destination::Http { api: Some(api), .. } => plan.resource(resources, Kind::Api, api, label, me),
                Destination::Http { .. } => {}
                Destination::Sftp { server, .. } => plan.resource(resources, Kind::Sftp, server, label, me),
                Destination::File { dir } => {
                    let path = c.dir.join(dir);
                    let target = Target::Folder { path: path.clone(), created_if_missing: true };
                    plan.add(5, path.display().to_string(), "destination folder", target, label, me);
                }
            }
        }
        for db in c.dedicated_pools.keys() {
            plan.resource(resources, Kind::Database, db, format!("{} (dedicated pool)", c.name), me);
        }
        for (kind, name) in code.get(&c.name).into_iter().flatten() {
            plan.resource(resources, *kind, name, format!("{} (code)", c.name), me);
        }
    }
    let defined = [
        (Kind::Database, resources.databases.keys().collect::<Vec<_>>()),
        (Kind::Api, resources.apis.keys().collect()),
        (Kind::Sftp, resources.sftp.keys().collect()),
    ];
    for (kind, names) in defined {
        for name in names {
            plan.resource(resources, kind, name, UNUSED.into(), None);
        }
    }

    let store = Probe {
        what: "engine database".into(),
        target: Target::Store(database_url.into()),
        used_by: vec!["the engine (DUCTUS_DATABASE_URL)".into()],
    };
    let wanted = |e: &Entry| only.is_empty() || only.iter().any(|i| e.interfaces.contains(i));
    let rest = plan.entries.into_values().filter(wanted).map(|mut e| {
        if e.used_by.len() > 1 {
            e.used_by.retain(|u| u != UNUSED);
        }
        Probe { what: e.what, target: e.target, used_by: e.used_by }
    });
    std::iter::once(store).chain(rest).collect()
}

/// This server's name, for the report header.
fn hostname() -> String {
    ["HOSTNAME", "COMPUTERNAME"]
        .iter()
        .find_map(|v| std::env::var(v).ok())
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "this server".into())
}
