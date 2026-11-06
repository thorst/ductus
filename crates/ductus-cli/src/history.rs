//! `ductus history <interface>`: one timeline of who changed what and why
//! (`ductus_runtime::history`): git's commits on its files, the deployments that changed it, the
//! audit log's entries naming it and its documentation's changes. Reads the project folder and,
//! with `DUCTUS_DATABASE_URL`, the database (read-only); without it, git only.

use crate::project::{interface_dirs, names_interface};
use anyhow::{Context, Result};
use ductus_core::Environment;
use ductus_runtime::history::{self, Query, Sources};
use std::collections::BTreeSet;
use std::path::PathBuf;

pub struct Args {
    pub root: PathBuf,
    pub env: String,
    /// Its name, or a file or folder inside its folder.
    pub interface: String,
    pub database_url: Option<String>,
    pub all: bool,
    pub limit: usize,
    pub before: Option<String>,
    pub json: bool,
}

pub async fn run(args: Args) -> Result<()> {
    let env = Environment::load(&args.root, &args.env)?;
    let interfaces = interface_dirs(&args.root, &env)?;
    // An interface no longer in the project still has its history: it's asked for by name then.
    let found = interfaces.iter().find(|i| names_interface(&args.root, &args.interface, i));
    let name = found.map(|i| i.label()).unwrap_or_else(|| args.interface.clone());
    let mut notes = Vec::new();
    // The files it runs now (it may not have been deployed yet), loaded as a start loads them.
    let mut paths = BTreeSet::new();
    match ductus_runtime::load_interfaces(&args.root, &env)?.into_iter().find(|c| c.name == name) {
        None => notes.push(format!("`{name}` isn't in the project now: its history is what was recorded")),
        Some(config) => {
            let heap_mb = ductus_core::settings::Settings::load(&args.root)?.isolates.heap_mb;
            let (root, loading) = (args.root.clone(), config.clone());
            match tokio::task::spawn_blocking(move || ductus_runtime::code::current(&root, &loading, heap_mb)).await? {
                Ok(version) => paths.extend(version.files.into_iter().map(|f| f.path)),
                Err(e) => {
                    let yaml = config.dir.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
                    paths.insert(format!("interfaces/{yaml}/interface.yaml"));
                    notes.push(format!("its code didn't load ({e:#}): git's history is of its interface.yaml and the files its recorded versions name"));
                }
            }
        }
    }
    let store = match args.database_url.as_deref().filter(|u| !u.is_empty()) {
        Some(url) => Some(ductus_store::Store::connect_read_only(url).await.context("reading the database")?),
        None => None,
    };
    let query =
        Query { interface: name, before: args.before, limit: args.limit.max(1), all: args.all, reads: args.all };
    let sources = Sources { store: store.as_ref(), root: &args.root, paths };
    let mut page = history::read(sources, &query).await?;
    notes.append(&mut page.notes);
    page.notes = notes;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&page)?);
        return Ok(());
    }
    if page.entries.is_empty() {
        println!("{}: nothing recorded yet", page.interface);
    } else {
        println!("{}{}", page.interface, if args.all { " (everything)" } else { "" });
        print!("{}", history::text(&page));
    }
    for note in &page.notes {
        println!("note: {note}");
    }
    if let Some(next) = &page.next_before {
        println!("older: ductus history {} --before {next}{}", page.interface, if args.all { " --all" } else { "" });
    }
    Ok(())
}
