//! `ductus owners`: who owns which interface and destination, and moving ownership in place.

use crate::project::{first_line, interface_dirs};
use anyhow::{Context, Result};
use ductus_core::owners::{self, Owned};
use ductus_core::{Environment, InterfaceConfig};
use std::path::{Path, PathBuf};

/// Owner → interfaces and destinations, the unowned last. Returns false if some interfaces
/// couldn't be read.
pub fn report(root: &Path, env: &str) -> Result<bool> {
    let environment = Environment::load(root, env)?;
    let interfaces = interface_dirs(root, &environment)?;
    let mut complete = true;
    for i in &interfaces {
        if let Err(e) = &i.config {
            complete = false;
            println!("✗ {}: not shown, its config doesn't load: {}", i.label(), first_line(&format!("{e:#}")));
        }
    }
    let configs: Vec<&InterfaceConfig> = interfaces.iter().filter_map(|i| i.config.as_ref().ok()).collect();
    if configs.is_empty() {
        println!("no interfaces");
        return Ok(complete);
    }
    let groups = owners::by_owner(owners::owned(configs.iter().copied()));
    let mut unowned = 0;
    for (n, (owner, items)) in groups.iter().enumerate() {
        if n > 0 {
            println!();
        }
        if owner.is_empty() {
            unowned = items.len();
            println!("no owner ({})", items.len());
        } else {
            println!("{owner} ({})", items.len());
        }
        for item in items {
            println!("  {}", label(item));
        }
    }
    if unowned > 0 {
        println!("\nnote: name an owner with `about: {{ owner: ... }}` in interface.yaml");
    }
    Ok(complete)
}

fn label(item: &Owned) -> String {
    match &item.destination {
        Some(d) => format!("{} → {d}", item.interface),
        None => item.interface.clone(),
    }
}

/// Change `owner: <from>` to `<to>` in every interface.yaml (or only `only`'s), keeping
/// everything else in the files as it is. Nothing is written unless every file can be
/// changed safely. Returns false if nothing matched or a file couldn't be changed.
pub fn reassign(root: &Path, env: &str, from: &str, to: &str, only: &[String], dry_run: bool) -> Result<bool> {
    let environment = Environment::load(root, env)?;
    let interfaces = interface_dirs(root, &environment)?;
    let names: Vec<String> = interfaces.iter().map(|i| i.label()).collect();
    let unknown: Vec<&str> = only.iter().filter(|o| !names.contains(o)).map(String::as_str).collect();
    if !unknown.is_empty() {
        anyhow::bail!("no interface named {} (interfaces: {})", unknown.join(", "), names.join(", "));
    }

    let mut edits: Vec<(PathBuf, String)> = Vec::new();
    let (mut changed, mut failed) = (0, 0);
    for i in interfaces.iter().filter(|i| only.is_empty() || only.contains(&i.label())) {
        let file = i.dir.join("interface.yaml");
        let shown = file.strip_prefix(root).unwrap_or(&file).display().to_string().replace('\\', "/");
        let text = std::fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?;
        match owners::reassign(&text, from, to) {
            Ok((_, changes)) if changes.is_empty() => {}
            Ok((out, changes)) => {
                println!("{shown}");
                let width = changes.iter().map(|c| c.line.to_string().len()).max().unwrap_or(1);
                for c in &changes {
                    println!("  {:>width$}  - {}", c.line, c.before.trim_start());
                    println!("  {:>width$}  + {}", "", c.after.trim_start());
                }
                changed += changes.len();
                edits.push((file, out));
            }
            Err(e) => {
                failed += 1;
                println!("✗ {shown}: {e:#}");
            }
        }
    }

    if failed > 0 {
        println!("\nnothing changed: fix the file(s) above by hand, or leave them out with --interface");
        return Ok(false);
    }
    if edits.is_empty() {
        let known = owners::by_owner(owners::owned(interfaces.iter().filter_map(|i| i.config.as_ref().ok())));
        let known: Vec<&str> = known.iter().map(|(o, _)| o.as_str()).filter(|o| !o.is_empty()).collect();
        println!(
            "no `owner: {from}` found (owners: {})",
            if known.is_empty() { "none".into() } else { known.join(", ") }
        );
        return Ok(false);
    }
    let summary = format!("{changed} owner value(s) in {} file(s), {from} → {to}", edits.len());
    if dry_run {
        println!("\ndry run: would change {summary}");
        return Ok(true);
    }
    for (file, text) in &edits {
        std::fs::write(file, text).with_context(|| format!("writing {}", file.display()))?;
    }
    println!("\nchanged {summary}");
    Ok(true)
}
