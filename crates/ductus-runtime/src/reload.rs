//! Reloads (0.7.5.3.1): an engine in **folder mode** applies its project folder as it is now, the
//! way a deploy in place applies a release (`deploy::apply_in_place`): only the interfaces whose
//! files or filled-in settings changed take the change (between two messages, or restarting
//! alone), interfaces added start and removed ones stop, the others never pause. Only what a start
//! alone can read (ductus.yaml, resources.yaml, docs-schema.yaml, the environment's secrets file,
//! a new binary, the service's ductus.env) goes by the handover, said in the answer.
//!
//! **Asked by** `ductus reload` (over the control socket, or the API's `POST /engine/reload` where
//! there's none: Windows), and by the system's reload: `systemctl reload` (the unit runs `ductus
//! reload --with-environment`), `SIGHUP`, the Windows service's `paramchange`. Every request goes
//! through the engine's control loop (`handover::Control`), one at a time, as a handover does:
//! what needs the handover then takes it there. `ductus handover` stays the full handover.
//!
//! **What it compares with.** By the time anyone asks, the folder may already hold the new files,
//! so "before" is what the engine runs, never the disk: each interface's code version as recorded
//! (`Engine::deployment`), its files' hashes and its interface.yaml's text read back from the
//! database (where its version saved them, `code_versions` and `code_files`), and the project's
//! own files as the start read them (`Engine::project_files`). "After" is the folder: each running
//! interface's files hashed again (all the same: the same version, nothing read further), every
//! interface.yaml loaded with the environment as it is now (an interface added, removed or
//! renamed is found there), the project's own files read again. Then the plan is the deploy's
//! (`deploy::plan_between`, `changes::plan`).
//!
//! **Code from a folder.** A file's path doesn't change when its text does, and V8 knows a module
//! by its URL: a reload loads its code under a generation of its own
//! (`ductus_script::Isolate::set_generation`), so changed files are read again, while code put back
//! after a failure loads under its old generation and gets the modules it ran.

use crate::changes::Plan;
use crate::control::Engine;
use crate::deployment::Who;
use crate::handover::Asked;
use crate::release::{self, Manifest};
use anyhow::{Context, Result};
use ductus_core::project_files::ProjectFiles;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;

/// How a reload went (`reload`).
#[derive(Debug)]
pub struct Reloaded {
    /// It runs the folder as it is now (applied in place, or there was nothing to apply).
    pub ok: bool,
    pub how: How,
    /// In words: what it did, or why it didn't.
    pub outcome: String,
    /// What the folder changes here; None when it couldn't be worked out.
    pub plan: Option<Plan>,
    /// What it did, a line per interface.
    pub steps: Vec<String>,
    /// The deployment it recorded (in place).
    pub deployment: Option<crate::deployment::Deployment>,
    /// The "replaces" check's warnings, by interface (folder mode warns, never refuses).
    pub warnings: HashMap<String, String>,
    pub took: std::time::Duration,
}

/// What a reload came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum How {
    /// Applied in place.
    InPlace,
    /// The folder is what runs: nothing to apply.
    Nothing,
    /// Only the handover applies it (`Reloaded::outcome` says why): the caller takes it.
    Handover,
    /// Refused, nothing changed: the folder doesn't load, or a start would refuse what it would
    /// leave running, or it needs the handover and only in place was asked.
    Refused,
}

impl How {
    /// As the API and `--json` name it.
    pub fn as_str(&self) -> &'static str {
        match self {
            How::InPlace => "in_place",
            How::Nothing => "nothing",
            How::Handover => "handover",
            How::Refused => "refused",
        }
    }
}

/// How a reload is asked.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Refuse a change that needs the handover instead of taking it (`--in-place-only`).
    pub in_place_only: bool,
    /// A new binary on disk is a reason to hand over: the handover would start a new process,
    /// which runs it (an in-process one runs this binary again).
    pub new_binary_counts: bool,
    /// This engine can't hand over at all (`ductus dev`: its Postgres, its debugger and its lock
    /// belong to the process): what needs the handover is refused with "restart it".
    pub no_handover: bool,
    /// Interfaces under a debugger (`ductus dev --inspect`): a change to one is refused, since
    /// loading its new code waits for the debugger whenever it's paused at a breakpoint.
    pub debugged: Vec<String>,
}

/// What of the engine's settings outside the project (`crate::started_with`) differs when they're
/// read again as an in-process handover reads them (`rebuild`: the env file read again), by name;
/// None when nothing does. Settings that can't be read again are said as such: the handover then
/// fails on them, saying why, and the engine goes on.
pub(crate) fn settings_changed(engine: &Engine, rebuild: &crate::handover::Rebuild) -> Option<String> {
    let again = match rebuild() {
        Ok(r) => crate::started_with(&r.root, &r.database_url, &r.options),
        Err(e) => return Some(format!("they can't be read again: {e:#}")),
    };
    let changed: Vec<&str> =
        engine.started_with.iter().zip(&again).filter(|(a, b)| a.1 != b.1).map(|(a, _)| a.0).collect();
    (!changed.is_empty()).then(|| changed.join(", "))
}

/// Apply `engine`'s project folder as it is now, in place where it can, for whoever `asked`
/// (recorded with the deployment). `environment`: the environment the asker passes along
/// (`--with-environment`: systemd's, ductus.env read again), which only a handover can apply when
/// its `DUCTUS_*` values differ from this engine's; `settings_changed`: what of its settings read
/// again in this process differs (`settings_changed`), the same. Never hands over itself: `How::Handover`
/// tells the caller to. Release mode always hands over (to the release it runs: a new binary, the
/// settings read again; releases are deployed with `ductus deploy`).
pub async fn reload(
    engine: &Arc<Engine>,
    asked: &Asked,
    environment: Option<&[(String, String)]>,
    options: Options,
    settings_changed: Option<String>,
) -> Reloaded {
    let in_place_only = match options.no_handover {
        true => InPlaceOnly::NoHandover,
        false if options.in_place_only => InPlaceOnly::Asked,
        false => InPlaceOnly::No,
    };
    let started = std::time::Instant::now();
    let mut reloaded = Reloaded {
        ok: false,
        how: How::Refused,
        outcome: String::new(),
        plan: None,
        steps: Vec::new(),
        deployment: None,
        warnings: HashMap::new(),
        took: Default::default(),
    };
    let finish = |mut r: Reloaded, how: How, outcome: String| {
        r.ok = matches!(how, How::InPlace | How::Nothing);
        r.how = how;
        r.outcome = outcome;
        r.took = started.elapsed();
        r
    };
    if engine.releases.is_some() {
        let why = "this engine runs releases (release mode): a reload starts a new engine on the release it runs \
                   (deploy another with `ductus deploy`)"
            .to_string();
        return needs_handover(reloaded, why, in_place_only, finish);
    }
    // What only a new engine picks up: the binary and the service's environment.
    let mut handover = Vec::new();
    if options.new_binary_counts && crate::handover::binary_changed() {
        handover.push("a new binary is on disk (an upgrade): only a new engine runs it".to_string());
    }
    if let Some(changed) = environment.and_then(environment_changed) {
        handover.push(format!("the service's environment changed ({changed}): only a new engine reads it"));
    }
    if let Some(changed) = settings_changed {
        handover.push(format!("the engine's settings changed ({changed}): only a new engine reads them"));
    }
    if !handover.is_empty() {
        return needs_handover(reloaded, handover.join("; "), in_place_only, finish);
    }
    // What runs is compared with the start's record: a reload asked as the engine starts (a
    // SIGHUP right after it) waits for it (the start holds `recording` until it's written).
    drop(engine.recording.lock().await);
    if engine.deployment().is_none() {
        let why = "this engine's start wasn't recorded (its log says why), so there's nothing to compare the folder \
                   with: a new engine records what it runs"
            .to_string();
        return needs_handover(reloaded, why, in_place_only, finish);
    }
    let planning = std::time::Instant::now();
    let (next, plan) = match plan(engine).await {
        Ok(planned) => {
            tracing::debug!(changes = planned.1.changes.len(), took = ?planning.elapsed(), "reload: what the folder changes worked out");
            planned
        }
        Err(e) => {
            let outcome = format!("the folder doesn't load: {e:#}\nNothing changed: every interface runs what it ran.");
            return finish(reloaded, How::Refused, outcome);
        }
    };
    reloaded.plan = Some(plan);
    let plan = reloaded.plan.as_ref().expect("just set");
    if let Some(why) = &plan.project {
        let why = why.clone();
        return needs_handover(reloaded, why, in_place_only, finish);
    }
    if plan.changes.is_empty() && !plan.environment_changes {
        let outcome = "nothing changed: the folder is what runs".to_string();
        return finish(reloaded, How::Nothing, outcome);
    }
    let debugged: Vec<&str> = plan
        .changes
        .iter()
        .map(|c| c.interface.as_str())
        .filter(|name| options.debugged.iter().any(|d| d == name))
        .collect();
    if !debugged.is_empty() {
        let outcome = format!(
            "refused: {} {} under the debugger (`--inspect`), and a reload would wait for it whenever it's paused: \
             stop the debug session and start it again to run the new code\nNothing changed: every interface runs \
             what it ran.",
            debugged.join(", "),
            if debugged.len() == 1 { "is" } else { "are" }
        );
        return finish(reloaded, How::Refused, outcome);
    }
    // The "replaces" check, as a folder-mode start runs it: a warning, recorded.
    let who = Who {
        how: "reload",
        by: asked.by.clone().unwrap_or_else(crate::deployment::account),
        reason: asked.reason.clone(),
        rollback: false,
        variables: plan.reached(),
    };
    let versions: Vec<(String, Vec<u8>)> = plan
        .changes
        .iter()
        .filter(|c| !c.is_removed())
        .filter_map(|c| {
            Some((c.interface.clone(), crate::deploy::unhex(&next.manifest.interfaces.get(&c.interface)?.version)?))
        })
        .collect();
    let pairs: Vec<(&str, &[u8])> = versions.iter().map(|(n, v)| (n.as_str(), v.as_slice())).collect();
    let source = crate::deploy::Source::Folder(engine.root.clone());
    match crate::deploy::replaced(&engine.store, &source, &pairs, &who).await {
        Ok(found) => reloaded.warnings = found.into_iter().collect(),
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "couldn't read what the interfaces ran before: no warning for changes over someone else's")
        }
    }
    for (interface, warning) in &reloaded.warnings {
        tracing::warn!(%interface, "reloaded over someone else's change: {warning}");
    }
    let Folder { manifest, files, env } = next;
    match crate::deploy::apply_in_place(engine, plan, &manifest, None, &engine.root, env, &who, &reloaded.warnings)
        .await
    {
        Ok(done) => {
            engine.now_runs_files(files);
            reloaded.steps = done.steps;
            reloaded.deployment = done.deployment;
            let outcome = crate::api::in_place_outcome(&plan.changes);
            finish(reloaded, How::InPlace, outcome)
        }
        Err(e) if e.downcast_ref::<crate::ownership::Refused>().is_some() => {
            let outcome = format!("refused: {e:#}\nNothing changed: every interface runs what it ran.");
            finish(reloaded, How::Refused, outcome)
        }
        Err(e) => needs_handover(reloaded, format!("{e:#}"), in_place_only, finish),
    }
}

/// Whether what needs the handover is refused, and why.
#[derive(Clone, Copy)]
enum InPlaceOnly {
    /// It goes to the handover.
    No,
    /// `--in-place-only` was asked.
    Asked,
    /// This engine can't hand over (`ductus dev`).
    NoHandover,
}

/// The reload needs the handover because of `why`: said so, or refused (`in_place_only`).
fn needs_handover(
    reloaded: Reloaded,
    why: String,
    in_place_only: InPlaceOnly,
    finish: impl FnOnce(Reloaded, How, String) -> Reloaded,
) -> Reloaded {
    let unchanged = "Nothing changed: every interface runs what it ran.";
    match in_place_only {
        InPlaceOnly::Asked => finish(
            reloaded,
            How::Refused,
            format!(
                "refused: only the handover applies it ({why}), and only in place was asked: reload without that when a handover suits\n{unchanged}"
            ),
        ),
        InPlaceOnly::NoHandover => finish(
            reloaded,
            How::Refused,
            format!(
                "refused: only a new engine applies it ({why}), and `ductus dev` doesn't hand over: restart it (Ctrl-C, then `ductus dev` again)\n{unchanged}"
            ),
        ),
        InPlaceOnly::No => finish(reloaded, How::Handover, why),
    }
}

/// The `DUCTUS_*` variables the asker's environment sets differently from this engine's (their
/// names only: values may be secrets), or None when they're the same. Only those are the
/// engine's settings; the rest (`MAINPID`, `INVOCATION_ID`...) differ between any two processes.
fn environment_changed(asker: &[(String, String)]) -> Option<String> {
    // Set by a handover for the engine it starts (its sockets), never by a person.
    let settings = |k: &str| k.starts_with("DUCTUS_") && !matches!(k, "DUCTUS_TAKEOVER" | "DUCTUS_LISTEN_FDS");
    let ours: BTreeMap<String, String> = std::env::vars().filter(|(k, _)| settings(k)).collect();
    let theirs: BTreeMap<&str, &str> =
        asker.iter().filter(|(k, _)| settings(k)).map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let mut names: Vec<&str> = ours
        .iter()
        .filter(|(k, v)| theirs.get(k.as_str()) != Some(&v.as_str()))
        .map(|(k, _)| k.as_str())
        .chain(theirs.keys().copied().filter(|k| !ours.contains_key(*k)))
        .collect();
    names.sort_unstable();
    names.dedup();
    (!names.is_empty()).then(|| names.join(", "))
}

/// The folder as it is now: a manifest of it (in memory: no release is made), its project files as
/// read and its environment.
pub(crate) struct Folder {
    pub manifest: Manifest,
    pub files: ProjectFiles,
    pub env: Arc<ductus_core::Environment>,
}

/// What a reload of `engine`'s folder would change (see the module), with the folder as read.
pub(crate) async fn plan(engine: &Arc<Engine>) -> Result<(Folder, Plan)> {
    let (running, recorded) = running(engine).await?;
    let (root, environment) = (engine.root.clone(), engine.environment.clone());
    let (manifest, files, env) = {
        let (running, root, environment) = (running.clone(), root.clone(), environment.clone());
        tokio::task::spawn_blocking(move || folder(&root, &environment, &running, &recorded)).await??
    };
    // The running interface.yaml texts the plan reads (those whose versions differ), from where
    // their versions saved them.
    let wanted = crate::deploy::plan_reads(&running.manifest, &manifest);
    let hashes: Vec<Vec<u8>> = wanted.iter().filter_map(|p| running.paths.get(p).cloned()).collect();
    let texts = engine.store.file_texts(&hashes).await?;
    let before_files = engine.project_files();
    let folder = Folder { manifest, files, env };
    let plan = {
        let (next, after_files, root) = (folder.manifest.clone(), folder.files.clone(), root.clone());
        tokio::task::spawn_blocking(move || {
            let before = |path: &str| -> Result<String> {
                if let Some(text) = before_files.text(path) {
                    return Ok(text.to_string());
                }
                let hash = running.paths.get(path).with_context(|| format!("{path}: not in what runs"))?;
                texts.get(hash).cloned().with_context(|| format!("{path}: its running text isn't in the database"))
            };
            let after = |path: &str| -> Result<String> {
                if let Some(text) = after_files.text(path) {
                    return Ok(text.to_string());
                }
                let file = root.join(path);
                std::fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))
            };
            crate::deploy::plan_between(&running.manifest, before, &next, after, &environment)
        })
        .await??
    };
    Ok((folder, plan))
}

/// What runs, as a manifest, with each file's hash by path and each interface's files by name.
#[derive(Clone)]
struct Running {
    manifest: Manifest,
    /// Each running file's hash, by path (interface.yaml files are each one interface's).
    paths: HashMap<String, Vec<u8>>,
}

/// Each running interface's files with their hashes (hex), by name.
type Recorded = HashMap<String, Vec<(String, String)>>;

/// What `engine` runs (see the module), and each interface's files.
async fn running(engine: &Engine) -> Result<(Running, Recorded)> {
    let deployment = engine.deployment().context(
        "this engine's start wasn't recorded (its log says why), so there's nothing to compare the folder with: \
         hand over instead (`ductus handover`)",
    )?;
    let hashes: Vec<Vec<u8>> = deployment.interfaces.values().map(|r| r.version.clone()).collect();
    let versions = engine.store.versions_by_hash(&hashes).await?;
    let mut interfaces = BTreeMap::new();
    let mut paths = HashMap::new();
    let mut recorded = HashMap::new();
    for running in engine.interfaces().iter() {
        let name = &running.config.name;
        let version = &deployment
            .interfaces
            .get(name)
            .with_context(|| format!("`{name}` isn't in the deployment this engine recorded"))?
            .version;
        let saved = versions.get(version).with_context(|| format!("`{name}`: its code version isn't saved"))?;
        for (path, hash) in &saved.files {
            if let Some(hash) = crate::deploy::unhex(hash) {
                paths.insert(path.clone(), hash);
            }
        }
        let parts = saved.parts.as_deref().and_then(|p| serde_json::from_str(p).ok()).unwrap_or_default();
        let files = saved.files.iter().map(|(p, _)| p.clone()).collect();
        interfaces.insert(name.clone(), release::Interface { version: crate::code::hex(version), files, parts });
        recorded.insert(name.clone(), saved.files.clone());
    }
    let manifest = manifest(&engine.root, &engine.project_files(), interfaces);
    Ok((Running { manifest, paths }, recorded))
}

/// The folder at `root` for `environment`, as a manifest (see the module): a running interface
/// whose files all hash as they ran keeps its version; any other's version is made up from what's
/// on disk now (it differs from every recorded one: the plan reads it as changed), its files the
/// running ones still there.
fn folder(
    root: &Path,
    environment: &str,
    running: &Running,
    recorded: &Recorded,
) -> Result<(Manifest, ProjectFiles, Arc<ductus_core::Environment>)> {
    let files = ProjectFiles::read(root, environment)?;
    let env = ductus_core::Environment::from_files(root, &files)?;
    let configs = crate::load_interfaces(root, &env)?;
    let mut hashed: HashMap<String, Option<String>> = HashMap::new();
    let mut hash_of = |path: &str| -> Option<String> {
        hashed
            .entry(path.to_string())
            .or_insert_with(|| {
                let text = std::fs::read_to_string(root.join(path)).ok()?;
                Some(crate::code::hex(&Sha256::digest(ductus_script::normalize(&text).as_bytes())))
            })
            .clone()
    };
    let mut interfaces = BTreeMap::new();
    for config in &configs {
        let folder = config.dir.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        let yaml = format!("interfaces/{folder}/interface.yaml");
        let ran = recorded.get(&config.name).filter(|files| files.iter().any(|(p, _)| *p == yaml));
        let same = ran.is_some_and(|files| files.iter().all(|(path, hash)| hash_of(path).as_deref() == Some(hash)));
        let interface = match (same, running.manifest.interfaces.get(&config.name)) {
            (true, Some(ran)) => ran.clone(),
            (_, ran) => {
                let mut paths: Vec<String> = ran.map(|r| r.files.clone()).unwrap_or_default();
                paths.retain(|p| hash_of(p).is_some());
                if !paths.contains(&yaml) {
                    paths.push(yaml.clone());
                    paths.sort();
                }
                let mut made = format!("ductus reload\ninterface {}\n", config.name);
                for path in &paths {
                    made.push_str(&format!("{}  {path}\n", hash_of(path).unwrap_or_default()));
                }
                release::Interface {
                    version: crate::code::hex(&Sha256::digest(made.as_bytes())),
                    files: paths,
                    parts: ran.map(|r| r.parts.clone()).unwrap_or_default(),
                }
            }
        };
        interfaces.insert(config.name.clone(), interface);
    }
    Ok((manifest(root, &files, interfaces), files, Arc::new(env)))
}

/// A manifest of a project folder, in memory: its interfaces and its own files' project version
/// (as a release's is worked out: `code::project_version` over the normalized texts). The rest is
/// a release's alone and left empty.
fn manifest(root: &Path, files: &ProjectFiles, interfaces: BTreeMap<String, release::Interface>) -> Manifest {
    let hashes: Vec<(String, [u8; 32])> = files
        .files()
        .map(|(path, text)| (path.to_string(), Sha256::digest(ductus_script::normalize(text).as_bytes()).into()))
        .collect();
    let (version, own) = crate::code::project_version(hashes.iter().map(|(p, h)| (p.as_str(), h)));
    Manifest {
        format: 1,
        id: String::new(),
        project: root.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default(),
        made: release::Made {
            at: String::new(),
            by: String::new(),
            host: String::new(),
            reason: None,
            binary: String::new(),
            platform: String::new(),
        },
        git: None,
        uncommitted: Vec::new(),
        over: None,
        project_version: release::ProjectVersion { version: crate::code::hex(&version), files: own },
        interfaces,
        tests: Default::default(),
        files: 0,
        bytes: 0,
        binary: Vec::new(),
        deployed: Vec::new(),
        composed: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_engine_s_own_variables_count_and_their_values_are_never_named() {
        // SAFETY: set before any thread of this test reads the environment; the name is this
        // test's own.
        unsafe { std::env::set_var("DUCTUS_RELOAD_TEST_A", "1") };
        let mut asker: Vec<(String, String)> = std::env::vars().filter(|(k, _)| k.starts_with("DUCTUS_")).collect();
        asker.push(("MAINPID".into(), "123".into()));
        assert_eq!(environment_changed(&asker), None, "the same DUCTUS_ values: nothing (MAINPID differs always)");
        for (k, v) in asker.iter_mut() {
            if k == "DUCTUS_RELOAD_TEST_A" {
                *v = "secret-2".into();
            }
        }
        asker.push(("DUCTUS_RELOAD_TEST_B".into(), "x".into()));
        let said = environment_changed(&asker).unwrap();
        assert_eq!(said, "DUCTUS_RELOAD_TEST_A, DUCTUS_RELOAD_TEST_B");
        assert!(!said.contains("secret"));
    }
}
