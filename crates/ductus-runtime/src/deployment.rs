//! Recording what runs (0.7.3.1): at every start and handover, each interface's code version
//! (`code`), the project's git commit (`git`), who and why, as a deployment
//! (`Store::record_deployment`). The engine keeps what it recorded for the API.
//!
//! The versions are saved first, on their own (`save`): every delivery records the version that
//! decided it (0.7.3.2), so the workers wait for its id before saving a decision (`Decided`),
//! never before making one, and not for the deployment's record, which comes after.
//!
//! Like the audit log, a deployment that can't be recorded is an error in the log, never a reason
//! to refuse to start: the engine may be all that's left to move messages.

use crate::code::{self, Version};
use crate::handover::Asked;
use anyhow::{Context, Result};
use ductus_core::InterfaceConfig;
use ductus_script::CodeFile;
use ductus_store::{NewDeployment, NewVersion, Running, Store, VersionIds};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::watch;

/// How many changed interfaces the start's log line names.
const NAMED: usize = 10;
/// How long `save` keeps trying while the database refuses, before decisions are saved without
/// their version (said).
const SAVE_TRIES: u32 = 30;

/// The code versions as saved: each one's id (`code_versions.id`), by hash and by interface.
#[derive(Debug, Default)]
pub struct Saved {
    pub ids: VersionIds,
    pub by_interface: HashMap<String, i32>,
}

/// Each interface's code version id, once `save` has it: what the workers record with every
/// decision (`deliveries.code_version`).
#[derive(Clone)]
pub struct Decided(watch::Receiver<Option<Arc<Saved>>>);

/// Where `save` says what it saved.
pub type Saving = watch::Sender<Option<Arc<Saved>>>;

impl Decided {
    pub fn channel() -> (Saving, Self) {
        let (tx, rx) = watch::channel(None);
        (tx, Self(rx))
    }

    /// The versions saved, once they are (at once after that). None when nothing will be: the
    /// start ended first.
    pub async fn saved(&self) -> Option<Arc<Saved>> {
        let mut rx = self.0.clone();
        rx.wait_for(Option::is_some).await.ok().and_then(|saved| saved.clone())
    }

    /// `interface`'s version id, once saved; None when it couldn't be (said in the log).
    pub async fn id(&self, interface: &str) -> Option<i32> {
        self.saved().await.and_then(|s| s.by_interface.get(interface).copied())
    }
}

/// `versions` as the store saves them.
fn new_versions(versions: &[Version]) -> Vec<NewVersion<'_>> {
    versions
        .iter()
        .map(|v| NewVersion {
            interface: &v.interface,
            hash: &v.hash,
            files: v.files.iter().map(|f| (f.path.as_str(), f.hash.as_slice(), f.text.as_str())).collect(),
        })
        .collect()
}

/// Save `versions` (`Store::save_versions`) and tell `saving`. Beside the start, as soon as they're
/// computed; a takeover's too, before it takes over: the rows are shared by content and change
/// nothing any engine runs, and the workers it starts then don't wait. While the database refuses
/// it tries again for a while; then (or without versions) every decision is saved without one,
/// and the log says so.
pub async fn save(store: Store, versions: Option<Arc<Vec<Version>>>, saving: Saving) {
    let started = std::time::Instant::now();
    let Some(versions) = versions else {
        saving.send_replace(Some(Arc::default()));
        return;
    };
    let new = new_versions(&versions);
    let mut tries = 0;
    let ids = loop {
        tries += 1;
        match store.save_versions(&new).await {
            Ok(ids) => break ids,
            Err(e) if tries < SAVE_TRIES => {
                tracing::warn!(error = %format!("{e:#}"), "saving the code versions; trying again");
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
            Err(e) => {
                tracing::error!(
                    error = %format!("{e:#}"),
                    "the code versions couldn't be saved: this run's deliveries are saved without the code version that decided them, and its deployment isn't recorded"
                );
                saving.send_replace(Some(Arc::default()));
                return;
            }
        }
    };
    let by_interface =
        versions.iter().filter_map(|v| Some((v.interface.clone(), *ids.get(v.hash.as_slice())?))).collect();
    tracing::debug!(took = ?started.elapsed(), versions = versions.len(), "code versions saved");
    saving.send_replace(Some(Arc::new(Saved { ids, by_interface })));
}

/// Warnings for the interfaces a start changes, by interface (`check`).
pub type Warnings = Arc<HashMap<String, String>>;

/// Where `check` says what it found: None until it has (then a map, empty when nothing's wrong).
pub fn warnings_channel() -> (watch::Sender<Option<Warnings>>, watch::Receiver<Option<Warnings>>) {
    watch::channel(None)
}

/// The warnings once `check` has them; empty when it couldn't (said) or the start ended first.
pub async fn warnings(rx: &watch::Receiver<Option<Warnings>>) -> Warnings {
    let mut rx = rx.clone();
    rx.wait_for(Option::is_some).await.ok().and_then(|w| w.clone()).unwrap_or_default()
}

/// Folder mode, two people (0.7.3.3): for each interface this start changes, whether the version it
/// replaces is someone else's work that this one may undo: it came from a commit that isn't in this
/// project's history (their checkout had commits this one hasn't pulled), or it ran files never
/// committed, and someone else deployed it. Refusing needs releases (0.7.4); this warns, in the
/// log, the deployment's record and `ductus handover`'s answer. Beside the start, before a
/// takeover is ready (so never in its sending pause); reads only.
pub async fn check(
    store: Store,
    root: std::path::PathBuf,
    versions: Arc<Vec<Version>>,
    who: Who,
    tx: watch::Sender<Option<Warnings>>,
) {
    let started = std::time::Instant::now();
    let names: Vec<&str> = versions.iter().map(|v| v.interface.as_str()).collect();
    let current = match store.current_versions(&names).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "couldn't read what the interfaces ran before: no warning for changes over someone else's");
            tx.send_replace(Some(Arc::default()));
            return;
        }
    };
    let hashes: HashMap<&str, &[u8]> = versions.iter().map(|v| (v.interface.as_str(), v.hash.as_slice())).collect();
    // Replaced, by someone else, with something to say about where it came from.
    let suspects: Vec<&ductus_store::Current> = current
        .iter()
        .filter(|c| hashes.get(c.interface.as_str()).is_some_and(|h| *h != c.version.as_slice()))
        .filter(|c| !same_person(&who, c))
        .filter(|c| c.git_commit.is_some() || !dirty(c).is_empty())
        .collect();
    let ancestry = if suspects.iter().any(|c| c.git_commit.is_some()) {
        let root = root.clone();
        match tokio::task::spawn_blocking(move || crate::git::ancestry(&root)).await {
            Ok(Some(Ok(a))) => Some(a),
            Ok(Some(Err(e))) => {
                tracing::warn!(error = %format!("{e:#}"), "couldn't read the project's git history: no warning for changes from commits it doesn't have");
                None
            }
            _ => None,
        }
    } else {
        None
    };
    let found: HashMap<String, String> =
        suspects.iter().filter_map(|c| Some((c.interface.clone(), judge(c, ancestry.as_ref())?))).collect();
    tracing::debug!(took = ?started.elapsed(), warnings = found.len(), "checked what the changes replace");
    tx.send_replace(Some(Arc::new(found)));
}

/// The same person deployed it: both are people (not a service manager) with the same name.
fn same_person(who: &Who, before: &ductus_store::Current) -> bool {
    let person = |how: &str, by: &str| how != "service" && !by.starts_with("systemd");
    person(who.how, &who.by) && person(&before.how, &before.by) && who.by == before.by
}

/// Files the version replaced ran that weren't committed when it was deployed.
fn dirty(before: &ductus_store::Current) -> Vec<&str> {
    let uncommitted = before.uncommitted.as_deref().unwrap_or_default();
    before.files.iter().filter(|f| uncommitted.contains(f)).map(String::as_str).collect()
}

/// Why replacing `before` may undo someone's work, in words; None when it can't.
fn judge(before: &ductus_store::Current, ancestry: Option<&crate::git::Ancestry>) -> Option<String> {
    let mut why = Vec::new();
    if let (Some(commit), Some(ancestry)) = (&before.git_commit, ancestry)
        && ancestry.head.is_some()
        && !ancestry.contains(commit)
    {
        why.push(format!(
            "came from commit {}, which this project's history doesn't have",
            &commit[..commit.len().min(12)]
        ));
    }
    let dirty = dirty(before);
    match dirty.len() {
        0 => {}
        1 => why.push(format!("ran a file never committed ({})", dirty[0])),
        n => why.push(format!("ran {n} files never committed ({}, …)", dirty[0])),
    }
    if why.is_empty() {
        return None;
    }
    Some(format!(
        "replaces {} (deployed {} by {} on {}): it {}, so their change may be undone (`ductus history {}`)",
        code::short(&before.version),
        before.at.format("%Y-%m-%d %H:%M UTC"),
        before.by,
        before.host,
        why.join(" and "),
        before.interface
    ))
}

/// Who started this engine and why.
#[derive(Debug, Clone)]
pub struct Who {
    /// `start`, `service` or `handover`.
    pub how: &'static str,
    pub by: String,
    pub reason: Option<String>,
}

impl Who {
    /// A start: the account running it; `service` when a service manager started it (systemd, the
    /// Windows service), where that account is the service's.
    pub fn start(service: bool) -> Self {
        Self { how: if service { "service" } else { "start" }, by: account(), reason: None }
    }

    /// A handover: whoever asked for it (`ductus handover`), and why if they said.
    pub fn handover(asked: &Asked) -> Self {
        Self {
            how: "handover",
            by: asked.by.clone().unwrap_or_else(|| "unknown (an engine before 0.7.3 asked)".into()),
            reason: asked.reason.clone(),
        }
    }
}

/// What this engine runs, as recorded (`GET /engine`, `GET /interfaces/{name}`).
#[derive(Debug, Clone)]
pub struct Deployment {
    pub id: i32,
    pub at: chrono::DateTime<chrono::Utc>,
    pub how: &'static str,
    pub by: String,
    pub reason: Option<String>,
    /// HEAD's commit; None without git (or before the first commit).
    pub git_commit: Option<String>,
    /// Files the interfaces run that aren't the commit's; None without git.
    pub uncommitted: Option<Vec<String>>,
    /// Interfaces whose version is new for them here.
    pub changed: usize,
    /// By interface.
    pub interfaces: BTreeMap<String, Running>,
}

/// The code versions of `configs`, whose code loaded `loaded` (by interface name). Reads every
/// interface.yaml again (14 ms at a thousand interfaces, measured); run it off the async threads.
pub fn versions(
    root: &Path,
    configs: &[Arc<InterfaceConfig>],
    loaded: &HashMap<String, Vec<CodeFile>>,
) -> Result<Vec<Version>> {
    // In the loader's form, or on Windows no file would be under it (`loaded_path`).
    let root = ductus_script::loaded_path(root).with_context(|| format!("finding {}", root.display()))?;
    configs.iter().map(|c| Version::of(&root, c, loaded.get(&c.name).map(Vec::as_slice).unwrap_or_default())).collect()
}

/// What git says about the files `versions` run: None without a repository, or when it can't be
/// read (said: the deployment is then recorded without its commit). About 20 ms at a thousand
/// interfaces (measured), so it's the recording's, beside the start; run it off the async threads.
pub fn commit(root: &Path, versions: &[Version]) -> Option<crate::git::Commit> {
    match crate::git::read(root, versions.iter().flat_map(|v| &v.files))? {
        Ok(c) => Some(c),
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "couldn't read the project's git repository: this deployment is recorded without its commit");
            None
        }
    }
}

/// What a recording needs besides the engine: the versions, their ids once saved, the warnings
/// once checked, the project's folder and its documentation.
pub struct Recording {
    pub versions: Arc<Vec<Version>>,
    pub decided: Decided,
    pub warned: watch::Receiver<Option<Warnings>>,
    pub root: std::path::PathBuf,
    pub docs: Vec<ductus_store::DocsRow>,
}

/// Save the documentation without a deployment (when it can't be recorded): history rows name none.
pub async fn docs_alone(store: &Store, docs: &[ductus_store::DocsRow]) {
    if let Err(e) = store.save_docs_snapshot(docs).await {
        tracing::warn!(error = format!("{e:#}"), "could not save the documentation snapshot");
    }
}

/// Read the project's commit (`commit`), record the deployment of the versions by `who` once
/// they're saved (`decided`) and checked (`check`), with the project's documentation (its snapshot
/// and history: in the same transaction), and say so in the log. None when it couldn't be recorded
/// (said; the documentation is saved alone then). Beside the start, once the engine holds its
/// interfaces (a takeover's after it took over).
pub async fn record(engine: &crate::control::Engine, host: &str, who: &Who, r: Recording) -> Option<Deployment> {
    let started = std::time::Instant::now();
    let Recording { versions, decided, warned, root, docs } = r;
    let reading = versions.clone();
    let commit = tokio::task::spawn_blocking(move || self::commit(&root, &reading)).await.ok().flatten();
    let commit = commit.as_ref();
    let store: &Store = &engine.store;
    // Not saved: `save` said why.
    let Some(saved) = decided.saved().await.filter(|s| !s.by_interface.is_empty() || versions.is_empty()) else {
        docs_alone(store, &docs).await;
        return None;
    };
    let warnings = self::warnings(&warned).await;
    let new = new_versions(&versions);
    let deployment = NewDeployment {
        environment: &engine.environment,
        instance: &engine.instance,
        host,
        version: env!("CARGO_PKG_VERSION"),
        how: who.how,
        by: &who.by,
        reason: who.reason.as_deref(),
        git_commit: commit.and_then(|c| c.id.as_deref()),
        uncommitted: commit.map(|c| c.uncommitted.as_slice()),
        versions: &new,
        warnings: &warnings,
        docs: Some(&docs),
    };
    let recorded = match store.record_deployment(&deployment, &saved.ids).await.context("recording the deployment") {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(
                error = %format!("{e:#}"),
                "this start's deployment (what each interface runs) wasn't recorded; the engine runs all the same"
            );
            docs_alone(store, &docs).await;
            return None;
        }
    };
    for (interface, warning) in warnings.iter() {
        tracing::warn!(%interface, deployment = recorded.id, "deployed over someone else's change: {warning}");
    }
    let changed = recorded.changed();
    let named: Vec<String> = recorded
        .interfaces
        .iter()
        .filter(|i| i.changed)
        .take(NAMED)
        .map(|i| match &i.previous {
            Some(before) => format!("{} {} → {}", i.interface, code::short(before), code::short(&i.version)),
            None => format!("{} {} (new here)", i.interface, code::short(&i.version)),
        })
        .collect();
    let more = changed.saturating_sub(NAMED);
    let git = match commit {
        None => "no git repository".to_string(),
        Some(c) => {
            let id = c.id.as_deref().map_or("no commit yet".into(), |id| format!("commit {}", &id[..id.len().min(12)]));
            match c.uncommitted.len() {
                0 => id,
                1 => format!("{id}, 1 file not committed: {}", c.uncommitted[0]),
                n => format!("{id}, {n} files not committed (first: {})", c.uncommitted[0]),
            }
        }
    };
    tracing::info!(
        deployment = recorded.id,
        changed,
        interfaces = recorded.interfaces.len(),
        by = %who.by,
        how = who.how,
        reason = who.reason.as_deref(),
        git = %git,
        took = ?started.elapsed(),
        "deployed: {}{}",
        if named.is_empty() { "nothing changed".to_string() } else { named.join(", ") },
        if more > 0 { format!(" and {more} more") } else { String::new() }
    );
    Some(Deployment {
        id: recorded.id,
        at: recorded.at,
        how: who.how,
        by: who.by.clone(),
        reason: who.reason.clone(),
        git_commit: commit.and_then(|c| c.id.clone()),
        uncommitted: commit.map(|c| c.uncommitted.clone()),
        changed,
        interfaces: recorded.interfaces.into_iter().map(|i| (i.interface.clone(), i)).collect(),
    })
}

/// The account this process runs as.
pub fn account() -> String {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: getpwuid_r writes into the buffers given, and `found` points into them (or is
        // null) once it returns; nothing outlives them.
        unsafe {
            let uid = libc::geteuid();
            let mut entry: libc::passwd = std::mem::zeroed();
            let mut buffer = [0 as libc::c_char; 4096];
            let mut found = std::ptr::null_mut();
            if libc::getpwuid_r(uid, &mut entry, buffer.as_mut_ptr(), buffer.len(), &mut found) == 0 && !found.is_null()
            {
                return std::ffi::CStr::from_ptr(entry.pw_name).to_string_lossy().into_owned();
            }
            format!("uid {uid}")
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let user = std::env::var("USERNAME").or_else(|_| std::env::var("USER")).unwrap_or_else(|_| "unknown".into());
        match std::env::var("USERDOMAIN") {
            Ok(domain) if !domain.is_empty() => format!("{domain}\\{user}"),
            _ => user,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Files are named relative to the project on every system: on Windows `canonicalize` gives
    /// `\\?\C:\…` and the loader's paths `C:\…` (CI's Windows tests caught it), so every file was
    /// named by its full path there and the same code had another version than on Linux.
    #[test]
    fn files_are_named_relative_to_the_project_on_every_system() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("interfaces").join("a");
        std::fs::create_dir_all(&folder).unwrap();
        let yaml = "name: a\nsource: { mllp: { port: 6661 } }\ndestination: { file: { dir: out } }\n";
        std::fs::write(folder.join("interface.yaml"), yaml).unwrap();
        std::fs::write(folder.join("t.ts"), "export const x = 1;\n").unwrap();
        let env = ductus_core::Environment::load(dir.path(), "dev").unwrap();
        let config = Arc::new(InterfaceConfig::load_in(&folder, &env).unwrap());
        let path = ductus_script::loaded_path(&folder.join("t.ts")).unwrap();
        let file = CodeFile { path, hash: [0; 32], text: "export const x = 1;\n".into() };
        let loaded = HashMap::from([("a".to_string(), vec![file])]);
        let versions = versions(dir.path(), &[config], &loaded).unwrap();
        let paths: Vec<&str> = versions[0].files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["interfaces/a/interface.yaml", "interfaces/a/t.ts"]);
    }
}
