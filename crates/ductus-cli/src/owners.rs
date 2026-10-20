//! `ductus owners`: who owns which interface, destination and resources.yaml entry, whether each
//! owner is an account, and moving ownership in place.

use crate::project::{first_line, interface_dirs};
use anyhow::{Context, Result};
use ductus_core::owners::{self, Owned, Person, Reach};
use ductus_core::{Environment, InterfaceConfig, Resources};
use ductus_store::Store;
use std::path::{Path, PathBuf};

/// Owner → interfaces, destinations and resources, the unowned last; with a database, each owner
/// marked as an account or not. Returns false if some interfaces couldn't be read, or (`check`)
/// if an owner is nobody the engine knows, a disabled account or several accounts.
pub async fn report(root: &Path, env: &str, database_url: Option<String>, check: bool) -> Result<bool> {
    let environment = Environment::load(root, env)?;
    let interfaces = interface_dirs(root, &environment)?;
    let resources = Resources::load(root, &environment)?;
    let settings = ductus_core::settings::Settings::load(root)?;
    let users = match database_url.filter(|u| !u.trim().is_empty()) {
        Some(url) => {
            let store = Store::connect_read_only(&url).await.context("reading the accounts")?;
            Some(store.users().await?)
        }
        None if check => return Err(crate::tools::database_url(None).unwrap_err()),
        None => None,
    };
    let people: Option<Vec<Person>> = users.as_ref().map(|users| {
        users
            .iter()
            .map(|u| Person {
                username: &u.username,
                display_name: Some(u.display_name.as_str()).filter(|s| !s.trim().is_empty()),
                email: Some(u.email.as_str()).filter(|s| !s.trim().is_empty()),
                disabled: u.disabled_at.is_some() || u.directory_refused_at.is_some(),
            })
            .collect()
    });

    let mut complete = true;
    for i in &interfaces {
        if let Err(e) = &i.config {
            complete = false;
            println!("✗ {}: not shown, its config doesn't load: {}", i.label(), first_line(&format!("{e:#}")));
        }
    }
    let configs: Vec<&InterfaceConfig> = interfaces.iter().filter_map(|i| i.config.as_ref().ok()).collect();
    let groups = owners::by_owner(owners::owned(configs.iter().copied(), &resources));
    if groups.is_empty() {
        println!("no interfaces");
        return Ok(complete);
    }
    let (mut unowned, mut problems) = (0, 0);
    for (n, (owner, items)) in groups.iter().enumerate() {
        if n > 0 {
            println!();
        }
        if owner.is_empty() {
            unowned = items.len();
            println!("no owner ({})", items.len());
        } else if let Some(people) = &people {
            let reach = owners::reach(owner, people, &settings.owners);
            if !reach.ok() {
                problems += 1;
            }
            println!("{} {owner} ({}): {}", if reach.ok() { "✓" } else { "✗" }, items.len(), said(&reach, people));
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
    if people.is_none() {
        println!("\nnote: owners aren't matched to accounts without the database (set DUCTUS_DATABASE_URL)");
    } else if problems > 0 {
        println!(
            "\n{problems} owner(s) reach nobody or the wrong person: make them an account's username or email, add them to `owners:` in ductus.yaml, or move their things with `ductus owners reassign`"
        );
    }
    Ok(complete && !(check && problems > 0))
}

/// What an owner is, in words.
fn said(reach: &Reach, people: &[Person]) -> String {
    let who = |username: &str| {
        let p = people.iter().find(|p| p.username == username);
        let more: Vec<&str> = p.into_iter().flat_map(|p| [p.display_name, p.email]).flatten().collect();
        if more.is_empty() { username.to_string() } else { format!("{username} ({})", more.join(", ")) }
    };
    match reach {
        Reach::User(u) => format!("user {}", who(u)),
        Reach::Disabled(u) => format!("account {} is disabled", who(u)),
        Reach::Several(n) => format!("names {n} accounts; use a username or an email address"),
        Reach::Team => "team (`owners:` in ductus.yaml)".into(),
        Reach::Address => "an email address, not an account".into(),
        Reach::Nobody => "not a user, not in `owners:`, not an address".into(),
    }
}

fn label(item: &Owned) -> String {
    item.what.label()
}

/// Change `owner: <from>` to `<to>` in every interface.yaml and resources.yaml (or only in the
/// named interfaces and resources), keeping everything else in the files as it is. Nothing is
/// written unless every file can be changed safely. Returns false if nothing matched or a file
/// couldn't be changed.
pub fn reassign(
    root: &Path,
    env: &str,
    from: &str,
    to: &str,
    only: &[String],
    only_resources: &[String],
    dry_run: bool,
) -> Result<bool> {
    let environment = Environment::load(root, env)?;
    let interfaces = interface_dirs(root, &environment)?;
    let resources = Resources::load(root, &environment)?;
    let names: Vec<String> = interfaces.iter().map(|i| i.label()).collect();
    let unknown: Vec<&str> = only.iter().filter(|o| !names.contains(o)).map(String::as_str).collect();
    if !unknown.is_empty() {
        anyhow::bail!("no interface named {} (interfaces: {})", unknown.join(", "), names.join(", "));
    }
    let defined: Vec<&String> = resources
        .databases
        .keys()
        .chain(resources.apis.keys())
        .chain(resources.sftp.keys())
        .chain(resources.ldap.keys())
        .collect();
    let unknown: Vec<&str> = only_resources.iter().filter(|o| !defined.contains(o)).map(String::as_str).collect();
    if !unknown.is_empty() {
        anyhow::bail!("no resource named {} in resources.yaml", unknown.join(", "));
    }
    let everything = only.is_empty() && only_resources.is_empty();

    let mut edits: Vec<(PathBuf, String)> = Vec::new();
    let (mut changed, mut failed) = (0, 0);
    let shown = |file: &Path| file.strip_prefix(root).unwrap_or(file).display().to_string().replace('\\', "/");
    let print = |shown: &str, lines: Vec<(usize, &str, &str, String)>| {
        println!("{shown}");
        let width = lines.iter().map(|l| l.0.to_string().len()).max().unwrap_or(1);
        for (line, before, after, whose) in lines {
            println!("  {line:>width$}  - {}{whose}", before.trim_start());
            println!("  {:>width$}  + {}", "", after.trim_start());
        }
    };
    for i in interfaces.iter().filter(|i| everything || only.contains(&i.label())) {
        let file = i.dir.join("interface.yaml");
        let text = std::fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?;
        match owners::reassign(&text, from, to) {
            Ok((_, changes)) if changes.is_empty() => {}
            Ok((out, changes)) => {
                print(
                    &shown(&file),
                    changes.iter().map(|c| (c.line, c.before.as_str(), c.after.as_str(), String::new())).collect(),
                );
                changed += changes.len();
                edits.push((file, out));
            }
            Err(e) => {
                failed += 1;
                println!("✗ {}: {e:#}", shown(&file));
            }
        }
    }
    let file = root.join("resources.yaml");
    if (everything || !only_resources.is_empty()) && file.is_file() {
        let text = std::fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?;
        match owners::reassign_resources(&text, from, to, only_resources) {
            Ok((_, changes)) if changes.is_empty() => {}
            Ok((out, changes)) => {
                print(
                    &shown(&file),
                    changes
                        .iter()
                        .map(|c| {
                            let whose = format!("   ({} {})", c.kind.label(), c.name);
                            (c.change.line, c.change.before.as_str(), c.change.after.as_str(), whose)
                        })
                        .collect(),
                );
                changed += changes.len();
                edits.push((file, out));
            }
            Err(e) => {
                failed += 1;
                println!("✗ {}: {e:#}", shown(&file));
            }
        }
    }

    if failed > 0 {
        println!("\nnothing changed: fix the file(s) above by hand, or leave them out with --interface / --resource");
        return Ok(false);
    }
    if edits.is_empty() {
        let configs = interfaces.iter().filter_map(|i| i.config.as_ref().ok());
        let known = owners::by_owner(owners::owned(configs, &resources));
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
