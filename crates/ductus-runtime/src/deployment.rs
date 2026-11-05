//! Recording what runs (0.7.3.1): at every start and handover, each interface's code version
//! (`code`), the project's git commit (`git`), who and why, as a deployment
//! (`Store::record_deployment`). The engine keeps what it recorded for the API.
//!
//! Like the audit log, a deployment that can't be recorded is an error in the log, never a reason
//! to refuse to start: the engine may be all that's left to move messages.

use crate::code::{self, Version};
use crate::handover::Asked;
use anyhow::{Context, Result};
use ductus_core::InterfaceConfig;
use ductus_script::CodeFile;
use ductus_store::{NewDeployment, NewVersion, Running, Store};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;

/// How many changed interfaces the start's log line names.
const NAMED: usize = 10;

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
    let root = std::fs::canonicalize(root).with_context(|| format!("finding {}", root.display()))?;
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

/// Read the project's commit (`commit`), record the deployment of `versions` by `who`, and say so
/// in the log. None when it couldn't be recorded (said). Beside the start, once the engine holds
/// its interfaces (a takeover's after it took over).
pub async fn record(
    engine: &crate::control::Engine,
    host: &str,
    who: &Who,
    versions: Arc<Vec<Version>>,
    root: std::path::PathBuf,
) -> Option<Deployment> {
    let started = std::time::Instant::now();
    let reading = versions.clone();
    let commit = tokio::task::spawn_blocking(move || self::commit(&root, &reading)).await.ok().flatten();
    let commit = commit.as_ref();
    let store: &Store = &engine.store;
    let new: Vec<NewVersion> = versions
        .iter()
        .map(|v| NewVersion {
            interface: &v.interface,
            hash: &v.hash,
            files: v.files.iter().map(|f| (f.path.as_str(), f.hash.as_slice(), f.text.as_str())).collect(),
        })
        .collect();
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
    };
    let recorded = match store.record_deployment(&deployment).await.context("recording the deployment") {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(
                error = %format!("{e:#}"),
                "this start's deployment (what each interface runs) wasn't recorded; the engine runs all the same"
            );
            return None;
        }
    };
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
