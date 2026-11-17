//! Deploys (0.7.4.4): an engine in **release mode** (`ductus run --releases DIR`) runs a release
//! stored in its database instead of a project folder, and `POST /deployments` replaces it with
//! another through the handover.
//!
//! **Which release.** The one a deploy's handover names (`handover::Asked::release`), else the
//! release last deployed to the engine's environment for its project on this database (the
//! deployments record which release each start ran). Engines carry no identity across runs, and
//! one engine runs each interface, so "here" is the database, the environment and the project.
//! Before the first deploy there's none: the engine starts with no interfaces, its API up.
//!
//! **Where.** Each release is unpacked from the database into `DIR/<id>/` the first time it runs
//! there, into a temporary folder first and renamed once whole, with a marker file written last
//! (`.ductus-release`, holding the id): a folder without it is never used. So a container needs no
//! volume, and the database stays the one source. A start whose folder is there reads one row.
//!
//! **The "replaces" check.** For each interface a deploy changes, the version running now must be
//! the deployer's own, or come from a commit in the new release's history (a colleague's merged
//! work), and not from files never committed; the files outside every interface (the project
//! version) are held to the same rule. A release made without git is held against the release it
//! names as its base (`ductus release --over`). Folder mode runs the same check at every start as
//! a warning (0.7.3.3); a deploy refuses unless forced.

use crate::code;
use crate::deployment::Who;
use crate::history::parts::Touches;
use crate::release::{self, Manifest};
use anyhow::{Context, Result, bail};
use ductus_store::{Current, Store};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// The marker written last into an unpacked release's folder: its id, in hex.
const MARKER: &str = ".ductus-release";
/// The folder an engine runs before anything is deployed (empty: no interfaces).
const NOTHING_YET: &str = "none";
/// Unpacked releases kept besides the one running (rollback unpacks again if needed).
const KEPT: usize = 2;

/// Release mode's settings (`ductus run --releases DIR`).
#[derive(Debug, Clone)]
pub struct Mode {
    /// Where releases are unpacked, each in a folder named by its id.
    pub dir: PathBuf,
    /// The project this engine runs (`DUCTUS_RELEASE_PROJECT`): needed only when several projects
    /// are deployed to its environment on one database.
    pub project: Option<String>,
    /// How long a deploy may reuse a passing check instead of running the gate again (the pre-run;
    /// `DUCTUS_DEPLOY_REUSE_CHECK`); zero: never.
    pub reuse_check: Duration,
    /// Environments a release must have been deployed to before this one takes it
    /// (`DUCTUS_DEPLOY_REQUIRES`); empty: none.
    pub requires: Vec<String>,
    /// Branches a release may be made on to be deployed here (`DUCTUS_DEPLOY_BRANCHES`; a trailing
    /// `*` matches the start); empty: any.
    pub branches: Vec<String>,
    /// The engine this one pulls releases from (`DUCTUS_PROMOTE_FROM`, 0.7.4.5.2): prod's engine
    /// names test's, and the Releases page lists test's releases to bring here.
    pub promote_from: Option<Upstream>,
}

/// Another engine releases are pulled from: its address and a token on it (`download` is
/// enough). A lower environment never holds a key to a higher one: prod pulls from test.
#[derive(Clone)]
pub struct Upstream {
    /// Its address as `ductus deploy --to` takes it: `https://ductus-test:8443`.
    pub url: String,
    pub token: String,
}

impl Upstream {
    /// Its API's base: `<url>/api/v1`.
    pub fn api(&self) -> String {
        format!("{}/api/v1", self.url.trim_end_matches('/'))
    }
}

impl std::fmt::Debug for Upstream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Upstream").field("url", &self.url).field("token", &"…").finish()
    }
}

/// Why this environment's rules refuse a release, before anything runs (`DUCTUS_DEPLOY_REQUIRES`,
/// `DUCTUS_DEPLOY_BRANCHES`); empty: they don't. `branches`: every branch it was made on (the
/// same files made on a feature branch and on main after a fast-forward merge are one release:
/// any of them may match). A rollback is never held to them (the caller skips this): the release
/// ran here before.
pub fn rules(mode: &Mode, manifest: &Manifest, branches: &[String], places: &[ductus_store::Place]) -> Vec<String> {
    let mut refused = Vec::new();
    let missing: Vec<&str> =
        mode.requires.iter().filter(|e| !places.iter().any(|p| &p.environment == *e)).map(String::as_str).collect();
    if !missing.is_empty() {
        let went: BTreeSet<&str> = places.iter().map(|p| p.environment.as_str()).collect();
        let went = if went.is_empty() {
            "it hasn't been deployed anywhere that this engine knows of".to_string()
        } else {
            format!("it was deployed to {} only", went.into_iter().collect::<Vec<_>>().join(", "))
        };
        refused.push(format!(
            "this environment takes only releases deployed to {} first (DUCTUS_DEPLOY_REQUIRES), and {went}: deploy it \
             there, then bring it here with its record (`ductus deploy {} --from <{}'s engine> --to <this engine>`)",
            missing.join(", "),
            short(&manifest.id),
            missing[0]
        ));
    }
    if !mode.branches.is_empty() {
        let allowed = mode.branches.join(", ");
        let matches = branches.iter().any(|b| mode.branches.iter().any(|pattern| branch_matches(pattern, b)));
        match (&manifest.git, branches.is_empty()) {
            (None, _) => refused.push(format!(
                "this environment takes only releases made on {allowed} (DUCTUS_DEPLOY_BRANCHES), and this one was made without git"
            )),
            (Some(_), true) => refused.push(format!(
                "this environment takes only releases made on {allowed} (DUCTUS_DEPLOY_BRANCHES), and this one records no \
                 branch (made on a detached HEAD: make it with `ductus release --branch NAME`)"
            )),
            (Some(_), false) if !matches => {
                let on = branches.iter().map(|b| format!("`{b}`")).collect::<Vec<_>>().join(", ");
                refused.push(format!(
                    "this environment takes only releases made on {allowed} (DUCTUS_DEPLOY_BRANCHES), and this one was \
                     made on {on}: merge it, then release from there"
                ))
            }
            _ => {}
        }
    }
    refused
}

fn short(hex: &str) -> &str {
    release::short(hex)
}

/// `main` matches `main`; `release/*` matches `release/2026-11`.
pub fn branch_matches(pattern: &str, branch: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => branch.starts_with(prefix),
        None => pattern == branch,
    }
}

/// The release a rollback goes to when none is named: the last one run here before the current
/// run, skipping the current release and any whose last run here was left by a rollback (going
/// back to one of those is a plain deploy). `runs`: oldest first.
pub fn rollback_target<'a>(runs: &'a [ductus_store::Run], current: &[u8]) -> Option<&'a ductus_store::Run> {
    let left_by_rollback = |release: &[u8]| {
        // Its last run here, and whether the run after it was a rollback.
        let last = runs.iter().rposition(|r| r.release == release)?;
        Some(runs.get(last + 1).is_some_and(|next| next.rollback))
    };
    let now = runs.iter().rposition(|r| r.release == current).unwrap_or(runs.len());
    runs[..now].iter().rev().find(|r| r.release != current && left_by_rollback(&r.release) == Some(false))
}

/// The release an engine runs.
#[derive(Debug)]
pub struct Running {
    pub id: Vec<u8>,
    pub manifest: Manifest,
    /// 20-byte commit ids, sorted (empty without git).
    pub ancestry: Vec<u8>,
}

impl Running {
    pub fn hex(&self) -> String {
        code::hex(&self.id)
    }

    /// Its project version (the files outside every interface).
    pub fn project_version(&self) -> Option<Vec<u8>> {
        unhex(&self.manifest.project_version.version)
    }

    /// Where its code came from, for the "replaces" check.
    pub fn source(&self) -> Source {
        Source::Release {
            commit: self.manifest.git.as_ref().map(|g| g.commit.clone()),
            ancestry: Arc::new(self.ancestry.clone()),
            over: self.manifest.over.as_deref().and_then(unhex),
        }
    }
}

/// Release mode, as an engine runs it: its settings and its release (None before the first deploy).
/// Clones share the release: a deploy in place moves it for all of them.
#[derive(Debug, Clone)]
pub struct Releases {
    pub mode: Mode,
    running: Arc<std::sync::RwLock<Option<Arc<Running>>>>,
}

impl Releases {
    pub fn new(mode: Mode, running: Option<Arc<Running>>) -> Self {
        Self { mode, running: Arc::new(std::sync::RwLock::new(running)) }
    }

    /// The release it runs now (a deploy in place changes it).
    pub fn running(&self) -> Option<Arc<Running>> {
        self.running.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// It runs `release` from now on (a deploy in place applied it).
    pub fn now_runs(&self, release: Arc<Running>) {
        *self.running.write().unwrap_or_else(|e| e.into_inner()) = Some(release);
    }

    /// The project this engine runs: its release's, else the one its settings name.
    pub fn project(&self) -> Option<String> {
        self.running().map(|r| r.manifest.project.clone()).or_else(|| self.mode.project.clone())
    }
}

/// The release to run (`asked`, a deploy's, in hex; else the last deployed to `environment`),
/// unpacked: the folder to run as the project, and what runs.
pub async fn resolve(
    store: &Store,
    environment: &str,
    mode: &Mode,
    asked: Option<&str>,
) -> Result<(PathBuf, Releases)> {
    let started = std::time::Instant::now();
    let id = match asked {
        Some(hex) => Some(unhex(hex).with_context(|| format!("the release to run (`{hex}`) isn't an id in hex"))?),
        None => {
            let last = store.last_deployed(environment).await?;
            let mine: Vec<_> = match &mode.project {
                Some(p) => last.into_iter().filter(|l| &l.project == p).collect(),
                None => last,
            };
            let projects: BTreeSet<&str> = mine.iter().map(|l| l.project.as_str()).collect();
            if projects.len() > 1 {
                bail!(
                    "several projects are deployed to `{environment}` on this database ({}): name the one this engine runs with DUCTUS_RELEASE_PROJECT",
                    projects.into_iter().collect::<Vec<_>>().join(", ")
                );
            }
            mine.first().map(|l| l.release.clone())
        }
    };
    let Some(id) = id else {
        let root = mode.dir.join(NOTHING_YET);
        std::fs::create_dir_all(root.join("interfaces")).with_context(|| format!("making {}", root.display()))?;
        tracing::info!(
            environment,
            project = mode.project.as_deref(),
            "release mode: nothing is deployed to this environment yet; the engine runs no interface until a release is (`ductus deploy`)"
        );
        return Ok((root, Releases::new(mode.clone(), None)));
    };
    let hex = code::hex(&id);
    let (manifest, ancestry) = store
        .release_manifest(&id)
        .await?
        .with_context(|| format!("release {} isn't stored in this database", release::short(&hex)))?;
    let manifest: Manifest = serde_json::from_value(manifest).context("reading the release's manifest")?;
    if let Some(project) = &mode.project
        && project != &manifest.project
    {
        bail!(
            "release {} is of project `{}`, and this engine runs `{project}` (DUCTUS_RELEASE_PROJECT)",
            release::short(&hex),
            manifest.project
        );
    }
    let (folder, unpacked) = unpacked(store, mode, &id).await?;
    tracing::info!(
        release = release::short(&hex),
        project = %manifest.project,
        unpacked,
        took = ?started.elapsed(),
        "release mode: running release {}",
        release::short(&hex)
    );
    let running = Arc::new(Running { id, manifest, ancestry });
    Ok((folder, Releases::new(mode.clone(), Some(running))))
}

/// Release `id`'s folder under `mode.dir`, unpacked from the database unless it's there whole (its
/// marker names it); true when it was unpacked now. A start runs it; a deploy in place loads the
/// changed interfaces from it.
pub async fn unpacked(store: &Store, mode: &Mode, id: &[u8]) -> Result<(PathBuf, bool)> {
    let hex = code::hex(id);
    let folder = mode.dir.join(&hex);
    if std::fs::read_to_string(folder.join(MARKER)).is_ok_and(|m| m.trim() == hex) {
        return Ok((folder, false));
    }
    let contents =
        store.release_contents(id).await?.with_context(|| format!("release {} isn't stored", release::short(&hex)))?;
    let dir = mode.dir.clone();
    tokio::task::spawn_blocking(move || unpack(&dir, &hex, contents)).await.context("unpacking the release")??;
    Ok((folder, true))
}

/// Write a release's files into `dir/<hex>/`: into a temporary folder first, its id checked, the
/// marker written last, then renamed (a folder left half written by a crash is never used).
fn unpack(dir: &Path, hex: &str, contents: ductus_store::ReleaseContents) -> Result<()> {
    let files: Vec<release::File> =
        contents.files.into_iter().map(|(path, bytes, _)| release::File::new(path, bytes)).collect();
    let recomputed = code::hex(&release::id(&files));
    if recomputed != hex {
        bail!(
            "the release stored as {} reads back as {}: the database was changed by hand",
            release::short(hex),
            release::short(&recomputed)
        );
    }
    let partial = dir.join(format!(".{hex}.partial-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&partial);
    release::unpack(&files, &partial)?;
    std::fs::write(partial.join(MARKER), format!("{hex}\n")).context("marking the release unpacked")?;
    let folder = dir.join(hex);
    if folder.exists() {
        std::fs::remove_dir_all(&folder).with_context(|| format!("removing {} (left incomplete)", folder.display()))?;
    }
    std::fs::rename(&partial, &folder).with_context(|| format!("moving the release into {}", folder.display()))
}

/// The unpacked releases (their folder names under `dir`) that interfaces in `folders` (each one's
/// `InterfaceConfig::dir`) run from. After deploys in place an engine runs from several: an
/// interface a deploy left alone keeps running the code it loaded from the release it came with,
/// and its code may read more of that folder later (a dynamic `import()`).
pub fn in_use<'a>(dir: &Path, folders: impl IntoIterator<Item = &'a Path>) -> BTreeSet<String> {
    folders
        .into_iter()
        .filter_map(|f| f.strip_prefix(dir).ok()?.components().next())
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect()
}

/// Remove unpacked releases but those in `running` (what the engine runs from: `in_use`) and the
/// `KEPT` used last, and folders a stopped unpack left. Beside the start and after each deploy in
/// place; what can't be removed is said and left.
pub fn tidy(dir: &Path, running: &BTreeSet<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut releases = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        let leftover =
            name.starts_with('.') && name.contains(".partial-") && !name.ends_with(&format!("-{}", std::process::id()));
        if leftover {
            remove(&path);
        } else if name.len() == 64 && name.bytes().all(|b| b.is_ascii_hexdigit()) && !running.contains(&name) {
            let used = entry.metadata().and_then(|m| m.modified()).ok();
            releases.push((used, path));
        }
    }
    releases.sort_by_key(|a| std::cmp::Reverse(a.0));
    for (_, path) in releases.into_iter().skip(KEPT) {
        remove(&path);
    }
}

fn remove(path: &Path) {
    if let Err(e) = std::fs::remove_dir_all(path) {
        tracing::warn!(folder = %path.display(), error = %e, "couldn't remove an unpacked release no longer needed");
    }
}

/// An interface's interface.yaml among a release's files (`interfaces/<folder>/interface.yaml`).
fn interface_yaml(i: &release::Interface) -> Option<&str> {
    i.files.iter().map(String::as_str).find(|f| f.starts_with("interfaces/") && f.ends_with("/interface.yaml"))
}

/// What deploying `next` (unpacked in `folder`) over `running` (unpacked in `dir`) changes, by
/// kind (`changes::plan`), for an engine in `environment`: from the two manifests, each
/// interface.yaml read only where the versions differ (an unchanged interface costs nothing),
/// unless the environment's variables change: then every interface.yaml and code file of `next`
/// (to find what each variable reaches). Read beside nothing: the deploy waits for it
/// (milliseconds for a handful of changes).
pub fn plan_over(
    dir: &Path,
    running: &Running,
    next: &Manifest,
    folder: &Path,
    environment: &str,
) -> Result<crate::changes::Plan> {
    let read = |root: PathBuf| {
        move |path: &str| {
            let path = root.join(path);
            std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))
        }
    };
    plan_between(&running.manifest, read(dir.join(running.hex())), next, read(folder.to_path_buf()), environment)
}

/// The files of `from` that working out the plan against `other` reads first (paths in the
/// release): the project's own files when the project versions differ, and the interface.yaml of
/// the interfaces whose versions differ or that `other` doesn't have. When the environment's
/// variables change (`variables_change`), `next`'s `plan_reads_for_variables` too.
pub fn plan_reads(from: &Manifest, other: &Manifest) -> Vec<String> {
    let project = (from.project_version.version != other.project_version.version)
        .then(|| from.project_version.files.iter().cloned())
        .into_iter()
        .flatten();
    from.interfaces
        .iter()
        .filter(|(name, i)| other.interfaces.get(*name).is_none_or(|o| o.version != i.version))
        .filter_map(|(_, i)| interface_yaml(i).map(str::to_string))
        .chain(project)
        .collect()
}

/// Every interface.yaml and code file of `next`: what the plan reads when the environment's
/// variables change (each interface's settings filled in, its code searched for their names).
pub fn plan_reads_for_variables(next: &Manifest) -> Vec<String> {
    let all: BTreeSet<&str> = next.interfaces.values().flat_map(|i| i.files.iter().map(String::as_str)).collect();
    all.into_iter().map(str::to_string).collect()
}

/// Whether the environment file of `environment` (`environments/<name>.yaml`) gives other
/// variables in `next` than in `running` (each read by its reader, among `plan_reads`).
pub fn variables_change(
    running: &Manifest,
    before: impl Fn(&str) -> Result<String>,
    next: &Manifest,
    after: impl Fn(&str) -> Result<String>,
    environment: &str,
) -> Result<bool> {
    if running.project_version.version == next.project_version.version {
        return Ok(false);
    }
    let path = environment_file(environment);
    let (b, a) = (variables_of(running, &before, &path)?, variables_of(next, &after, &path)?);
    Ok(b.0 != a.0)
}

/// `environments/<name>.yaml`.
fn environment_file(environment: &str) -> String {
    format!("environments/{environment}.yaml")
}

/// A release's file among its project files, read; None when it hasn't one.
fn project_text(release: &Manifest, read: &impl Fn(&str) -> Result<String>, path: &str) -> Result<Option<String>> {
    match release.project_version.files.iter().any(|f| f == path) {
        true => read(path).map(Some),
        false => Ok(None),
    }
}

type EnvironmentFile = (BTreeMap<String, ductus_core::yaml::Value>, Option<ductus_core::ConsoleLevel>);

/// A release's environment file's variables and `console:` (none when it hasn't the file).
fn variables_of(release: &Manifest, read: &impl Fn(&str) -> Result<String>, path: &str) -> Result<EnvironmentFile> {
    match project_text(release, read, path)? {
        Some(text) => ductus_core::env::read_file(&text)
            .with_context(|| format!("release {}: reading {path}", release::short(&release.id))),
        None => Ok((BTreeMap::new(), None)),
    }
}

/// What the project's own files change for an engine in `environment` (0.7.5.2.3), file by file:
/// another environment's `environments/` or `secrets/` file changes nothing here; this
/// environment's file goes in place (its variables reach the interfaces `changes::plan` finds,
/// `console:` is swapped), unless a variable changes what `resources.yaml` or ductus.yaml's
/// `alerts:` fill in; `secrets/<env>.json`, ductus.yaml, resources.yaml and docs-schema.yaml
/// changed as written go by the handover (each read at a start), the reason naming each.
pub fn project_plan(
    running: &Manifest,
    before: impl Fn(&str) -> Result<String>,
    next: &Manifest,
    after: impl Fn(&str) -> Result<String>,
    environment: &str,
) -> Result<crate::changes::Project> {
    let mut project = crate::changes::Project::default();
    if running.project_version.version == next.project_version.version {
        return Ok(project);
    }
    let (env_file, secrets_file) = (environment_file(environment), format!("secrets/{environment}.json"));
    let paths: BTreeSet<&str> =
        running.project_version.files.iter().chain(&next.project_version.files).map(String::as_str).collect();
    let mut handover = Vec::new();
    for path in paths {
        let (b, a) = (project_text(running, &before, path)?, project_text(next, &after, path)?);
        if b == a {
            continue;
        }
        let why = match path {
            p if p == env_file => continue, // compared below, by what it says
            p if p == secrets_file => "secrets are decrypted at a start",
            p if p.starts_with("environments/") || p.starts_with("secrets/") => {
                project.in_place.push(format!("{path}: another environment's file, nothing changes here"));
                continue;
            }
            "ductus.yaml" => "the engine's settings are read at a start",
            "resources.yaml" => "databases, APIs and their pools are made at a start",
            "docs-schema.yaml" => "documentation fields are read at a start",
            _ => "it's read at a start",
        };
        handover.push(format!("{path} changed ({why})"));
    }
    let ((vars_before, console_before), (vars_after, console_after)) =
        (variables_of(running, &before, &env_file)?, variables_of(next, &after, &env_file)?);
    project.variables.before = vars_before;
    project.variables.after = vars_after;
    let changed: Vec<String> = {
        let vars = &project.variables;
        vars.changed().into_iter().map(|n| vars.change(n).to_string()).collect()
    };
    if !changed.is_empty() {
        project.in_place.push(format!("{env_file}: {}", changed.join(", ")));
    }
    if console_before != console_after {
        let level = |c: Option<ductus_core::ConsoleLevel>| c.unwrap_or_default().as_str();
        project.in_place.push(format!("{env_file}: console {} → {}", level(console_before), level(console_after)));
    }
    project.environment_changes = !changed.is_empty() || console_before != console_after;
    // What a start fills in from the variables, beside the interfaces: compared filled in when
    // the file itself is the same (a changed file is a handover already).
    if !changed.is_empty() {
        let filled = |text: &str, variables| -> Result<ductus_core::yaml::Value> {
            let mut value = ductus_core::yaml::parse(text)?;
            ductus_core::env::fill_variables(&mut value, variables);
            Ok(value)
        };
        for (path, key) in [("resources.yaml", None), ("ductus.yaml", Some("alerts"))] {
            let (Some(b), Some(a)) = (project_text(running, &before, path)?, project_text(next, &after, path)?) else {
                continue;
            };
            if b != a {
                continue;
            }
            let pick = |v: ductus_core::yaml::Value| match key {
                Some(k) => v.get(k).cloned().unwrap_or_default(),
                None => v,
            };
            let vars = &project.variables;
            let (fb, fa) = (pick(filled(&b, &vars.before)?), pick(filled(&a, &vars.after)?));
            if fb != fa {
                let mut named = BTreeSet::new();
                ductus_core::env::variable_names(&pick(ductus_core::yaml::parse(&a)?), &mut named);
                let by: Vec<&str> = vars.changed().into_iter().filter(|n| named.contains(*n)).collect();
                let what = match key {
                    Some(k) => format!("{path}'s `{k}:`"),
                    None => path.to_string(),
                };
                let why = match key {
                    Some(_) => "alert rules are read at a start",
                    None => "databases, APIs and their pools are made at a start",
                };
                handover.push(format!("{} changes what {what} fills in ({why})", by.join(", ")));
            }
        }
    }
    if !handover.is_empty() {
        project.handover = Some(handover.join("; "));
    }
    Ok(project)
}

/// Whether code text names `variable` as a string (`env("EMPI_URL")`, `'EMPI_URL'`, `` `EMPI_URL` ``):
/// how an interface whose code reads a changed variable is found to be reloaded. A name built at
/// run time isn't found (said in the releases nuance).
fn names_in_code(text: &str, variable: &str) -> bool {
    ['"', '\'', '`'].iter().any(|q| text.contains(&format!("{q}{variable}{q}")))
}

/// `plan_over` with each side's files read by `before` and `after` (a path in the release: only
/// those `plan_reads` names are asked for, and `plan_reads_for_variables`' when
/// `variables_change`).
pub fn plan_between(
    running: &Manifest,
    before: impl Fn(&str) -> Result<String>,
    next: &Manifest,
    after: impl Fn(&str) -> Result<String>,
    environment: &str,
) -> Result<crate::changes::Plan> {
    /// `from`'s interfaces by name: version, and interface.yaml's text where `other`'s version
    /// differs or `all` (empty elsewhere: never compared).
    fn texts<'a>(
        from: &'a Manifest,
        read: &impl Fn(&str) -> Result<String>,
        other: &Manifest,
        all: bool,
    ) -> Result<BTreeMap<&'a str, (&'a str, String)>> {
        let mut side = BTreeMap::new();
        for (name, i) in &from.interfaces {
            let text = match all || other.interfaces.get(name).is_none_or(|o| o.version != i.version) {
                true => read(
                    interface_yaml(i)
                        .with_context(|| format!("release {}: `{name}` has no interface.yaml", from.id))?,
                )?,
                false => String::new(),
            };
            side.insert(name.as_str(), (i.version.as_str(), text));
        }
        Ok(side)
    }
    fn side<'a>(s: &'a BTreeMap<&'a str, (&'a str, String)>) -> crate::changes::Side<'a> {
        s.iter().map(|(n, (v, t))| (*n, (*v, t.as_str()))).collect()
    }
    let mut project = project_plan(running, &before, next, &after, environment)?;
    let changed: Vec<String> = project.variables.changed().into_iter().map(str::to_string).collect();
    let all = !changed.is_empty();
    let mut code_reach: BTreeMap<&str, Touches> = BTreeMap::new();
    let after_texts = texts(next, &after, running, all)?;
    let mut before_texts = texts(running, &before, next, false)?;
    if all {
        // An interface whose version is the same has the same interface.yaml: read once.
        for (name, (version, text)) in before_texts.iter_mut() {
            if let Some((v, t)) = after_texts.get(name)
                && v == version
            {
                text.clone_from(t);
            }
        }
        // Each interface whose code names a changed variable, read once per file, and the parts
        // whose code those files are.
        let mut code: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
        for (name, i) in &next.interfaces {
            let mut touches = Touches::default();
            for file in i.files.iter().filter(|f| !f.ends_with("/interface.yaml")) {
                if !code.contains_key(file.as_str()) {
                    let text = after(file)?;
                    code.insert(file, changed.iter().filter(|v| names_in_code(&text, v)).cloned().collect());
                }
                let named = &code[file.as_str()];
                if !named.is_empty() {
                    project.variables.in_code.entry(name.clone()).or_default().extend(named.iter().cloned());
                    touches = touches.with(code_touches(&i.parts, file));
                }
            }
            if touches != Touches::default() {
                code_reach.insert(name.as_str(), touches);
            }
        }
    }
    let mut plan = crate::changes::plan(&side(&before_texts), &side(&after_texts), &project)?;
    for change in &mut plan.changes {
        if let (Some(reached), Some(touches)) = (&mut change.reached, code_reach.remove(change.interface.as_str())) {
            reached.touches = std::mem::take(&mut reached.touches).with(touches);
        }
    }
    Ok(plan)
}

/// The parts whose code `file` is (a version's `parts`): every part when none names it (a version
/// saved before 0.7.4.2 has none).
fn code_touches(parts: &code::Parts, file: &str) -> Touches {
    let mut touches = Touches { source: parts.source.iter().any(|f| f == file), ..Touches::default() };
    for (destination, files) in &parts.destinations {
        if files.iter().any(|f| f == file) {
            touches = touches.with(Touches { destinations: vec![destination.clone()], ..Touches::default() });
        }
    }
    match touches == Touches::default() {
        true => Touches { everything: true, ..Touches::default() },
        false => touches,
    }
}

/// A deploy applied in place (`apply_in_place`).
pub struct InPlace {
    /// What it did, a line per interface (and the record), for the deploy's answer.
    pub steps: Vec<String>,
    /// What it recorded; None when it couldn't be (said in `steps`): the interfaces run their new
    /// code all the same.
    pub deployment: Option<crate::deployment::Deployment>,
}

/// Apply release `next` (unpacked in `folder`) in place: the interfaces `plan` adds start
/// (`Engine::add_interface`), then each interface it changes runs its new code and settings from
/// its next message (`Engine::replace_interface`; all at once, each on its own threads), then
/// those it removes stop (`Engine::remove_interface`), the others untouched. When the engine's
/// environment file changes (`Plan::environment_changes`), its new variables and `console:` are
/// what code reads from before the first of those (`SharedServices::swap_environment`; an
/// interface whose code names a changed variable is among the changed, so it loads again). When it adds or
/// removes any, the set it would leave running is checked first as a start checks it
/// (`check_together`: ports, routes, resources, TLS). Then the deployment is recorded (by `who`, with the
/// warnings it was let through with), the engine runs `next` from then on, and unpacked releases
/// nothing runs from are tidied. `plan` must be `in_place()`.
///
/// All or nothing: when one interface can't be applied (its new interface.yaml doesn't load, its
/// code doesn't load on a thread, another engine runs an added one, `in_place::NeedsHandover`),
/// those already applied are put back on what they ran (the code their threads loaded before),
/// those added removed again, and the `Err` says why, so the engine
/// runs one release, never a mix: the deploy then takes the handover (or, `--in-place-only`,
/// refuses).
pub async fn apply_in_place(
    engine: &Arc<crate::control::Engine>,
    plan: &crate::changes::Plan,
    next: Arc<Running>,
    folder: &Path,
    who: &Who,
    warnings: &HashMap<String, String>,
) -> Result<InPlace> {
    let started = std::time::Instant::now();
    let env = Arc::new(ductus_core::Environment::load(folder, &engine.environment)?);
    let load = |name: &str| -> Result<ductus_core::InterfaceConfig> {
        let yaml = next.manifest.interfaces.get(name).and_then(interface_yaml);
        let dir = yaml
            .and_then(|y| Path::new(y).parent())
            .with_context(|| format!("`{name}`: the release has no interface.yaml for it"))?;
        ductus_core::InterfaceConfig::load_in(&folder.join(dir), &env)
            .with_context(|| format!("`{name}`: loading its new interface.yaml"))
    };
    // Added, changed (each with what it ran) and removed, in the plan's order.
    let mut added = Vec::new();
    let mut changed = Vec::new();
    let mut removed = Vec::new();
    for change in &plan.changes {
        let name = &change.interface;
        if change.is_removed() {
            engine.interface(name).with_context(|| format!("`{name}` isn't running here"))?;
            removed.push((name.clone(), change));
        } else if change.is_added() {
            added.push((load(name)?, change));
        } else {
            let running = engine.interface(name).with_context(|| format!("`{name}` isn't running here"))?;
            changed.push((load(name)?, running.config.clone(), change));
        }
    }
    // The set it leaves running, checked as a start checks it, before anything changes.
    let sources_change = changed.iter().any(|(new, before, _)| new.source != before.source);
    if !added.is_empty() || !removed.is_empty() || sources_change {
        let running = engine.interfaces();
        let mut next_set: Vec<&ductus_core::InterfaceConfig> = running
            .iter()
            .filter(|r| !removed.iter().any(|(n, _)| *n == r.config.name))
            .map(|r| changed.iter().find(|(c, ..)| c.name == r.config.name).map_or(&*r.config, |(c, ..)| c))
            .collect();
        next_set.extend(added.iter().map(|(c, _)| c));
        let api = engine.api.as_ref();
        let alerts = engine.alerts.as_ref().map(|a| &a.settings);
        // Refused as the next start would be (a handover would fail the same way).
        let api_tls = api.and_then(|a| a.tls.as_ref());
        if let Err(e) = crate::check_together(&next_set, &engine.resources, api.map(|a| a.port), api_tls, alerts) {
            return Err(crate::ownership::Refused(format!("the interfaces it would leave running: {e:#}")).into());
        }
        // Their sources start, change and stop alone, unless a port moves between interfaces.
        let now: Vec<&ductus_core::InterfaceConfig> = running.iter().map(|r| &*r.config).collect();
        let adding: Vec<&ductus_core::InterfaceConfig> = added.iter().map(|(c, _)| c).collect();
        let changing: Vec<(&ductus_core::InterfaceConfig, &ductus_core::InterfaceConfig)> = changed
            .iter()
            .filter(|(new, before, _)| new.source != before.source)
            .map(|(new, before, _)| (&**before, new))
            .collect();
        crate::sources::in_place(&now, &adding, &changing)?;
    }
    // The environment file's new values, before any interface loads (code reading `env()` at its
    // top level gets them); put back if the deploy fails.
    let swapped = plan.environment_changes.then(|| engine.kept.swap_environment(env.clone())).flatten();
    let put_back = || {
        if let Some(before) = &swapped {
            engine.kept.swap_environment(before.clone());
        }
    };
    let timed = |result, started: std::time::Instant| (result, started.elapsed());
    // Added first: a changed interface may route to one.
    let adding = added.iter().map(|(config, _)| async {
        let started = std::time::Instant::now();
        timed(engine.add_interface(config.clone(), folder).await, started)
    });
    let added_results = futures_util::future::join_all(adding).await;
    let names: Vec<String> =
        added.iter().map(|(c, _)| c.name.clone()).chain(changed.iter().map(|(c, ..)| c.name.clone())).collect();
    // On a failure: those added removed again.
    let take_back_added = |results: &[(Result<crate::deployment::Applied>, std::time::Duration)]| {
        let names: Vec<String> =
            results.iter().filter_map(|(r, _)| r.as_ref().ok().map(|a| a.interface.clone())).collect();
        async move {
            for name in names {
                if let Err(e) = engine.remove_interface(&name).await {
                    tracing::error!(interface = %name, error = %format!("{e:#}"), "couldn't remove an interface a failed deploy in place added");
                }
            }
        }
    };
    if added_results.iter().any(|(r, _)| r.is_err()) {
        put_back();
        take_back_added(&added_results).await;
        engine.shown_after_replacing(&names).await;
        let first = added_results.into_iter().find_map(|(r, _)| r.err());
        return Err(first.expect("one failed"));
    }
    let applying = changed.iter().map(|(config, ..)| async {
        let started = std::time::Instant::now();
        timed(engine.replace_interface(config.clone(), folder).await, started)
    });
    let results = futures_util::future::join_all(applying).await;
    if results.iter().any(|(r, _)| r.is_err()) {
        // Back on what they ran: the same files, from the release each came from, with the
        // environment they ran with.
        put_back();
        let mut first = None;
        for ((result, _), (_, before, _)) in results.into_iter().zip(&changed) {
            match result {
                Err(e) => {
                    first.get_or_insert(e);
                }
                Ok(_) => {
                    let root = before.dir.parent().and_then(Path::parent).unwrap_or(folder);
                    if let Err(e) = engine.replace_interface((**before).clone(), root).await {
                        tracing::error!(interface = %before.name, error = %format!("{e:#}"), "couldn't put an interface back on what it ran after a deploy in place failed: it runs the new release's code");
                    }
                }
            }
        }
        take_back_added(&added_results).await;
        engine.shown_after_replacing(&names).await;
        return Err(first.expect("one failed"));
    }
    // Removed last: nothing to put back after them. Each stops between messages.
    let mut steps = Vec::with_capacity(plan.changes.len() + 1);
    for (name, _) in &removed {
        let started = std::time::Instant::now();
        match engine.remove_interface(name).await {
            Ok(()) => steps.push(format!("{name}: removed in place ({} ms)", started.elapsed().as_millis())),
            Err(e) => steps.push(format!("⚠ {name}: {e:#}")),
        }
    }
    engine.shown_after_replacing(&names).await;
    let mut applied = Vec::with_capacity(names.len());
    let done = added_results.into_iter().zip(added.iter().map(|(_, c)| *c));
    for ((result, took), change) in done.chain(results.into_iter().zip(changed.iter().map(|(.., c)| *c))) {
        let a = result.expect("none failed");
        let version = a.version.as_ref().map_or("its version unknown".to_string(), |v| v.short());
        steps.push(format!(
            "{}: {} → {version}, applied in place ({} ms)",
            a.interface,
            change.what.join(", "),
            took.as_millis()
        ));
        applied.push(a);
    }
    let deployment = match crate::deployment::record_in_place(engine, who, &applied, Some(next.clone()), warnings).await
    {
        Ok(d) => Some(d),
        Err(e) => {
            tracing::error!(error = %format!("{e:#}"), "a deploy applied in place wasn't recorded");
            steps.push(format!("⚠ not recorded: {e:#}"));
            if let Some(releases) = &engine.releases {
                releases.now_runs(next.clone()); // what runs all the same
            }
            None
        }
    };
    engine.tidy_releases();
    tracing::info!(
        release = release::short(&next.hex()),
        interfaces = applied.len(),
        took = ?started.elapsed(),
        "deploy applied in place: the other interfaces never paused"
    );
    Ok(InPlace { steps, deployment })
}

/// Where the code a deployment brings came from, for the "replaces" check.
#[derive(Debug, Clone)]
pub enum Source {
    /// Folder mode: the project's git history (read only when needed).
    Folder(PathBuf),
    /// A release: its commit and ancestry, or (made without git) the release it was made over.
    Release { commit: Option<String>, ancestry: Arc<Vec<u8>>, over: Option<Vec<u8>> },
}

/// What the "replaces" check calls the files outside every interface (`ductus.yaml`,
/// `resources.yaml`, `environments/`, ...), held to the same rule as an interface's.
pub const PROJECT: &str = "the project's own files";

/// The interfaces among `versions` (name, version) that a deployment by `who` would replace with
/// a different version over someone else's work it may undo, each with why.
pub async fn replaced(
    store: &Store,
    source: &Source,
    versions: &[(&str, &[u8])],
    who: &Who,
) -> Result<BTreeMap<String, String>> {
    let names: Vec<&str> = versions.iter().map(|(n, _)| *n).collect();
    let current = store.current_versions(&names).await?;
    let hashes: HashMap<&str, &[u8]> = versions.iter().copied().collect();
    let suspects: Vec<&Current> = current
        .iter()
        .filter(|c| hashes.get(c.interface.as_str()).is_some_and(|h| *h != c.version.as_slice()))
        .filter(|c| suspect(c, source, who))
        .collect();
    // Where else the suspects' versions came from: read only when there's a history to judge by.
    let after: Vec<(&str, i32)> = suspects.iter().map(|c| (c.interface.as_str(), c.deployment)).collect();
    let later = match source {
        Source::Release { commit: None, .. } => HashMap::new(),
        _ => store.later_sources(&after).await?,
    };
    let needed = suspects.iter().any(|c| c.git_commit.is_some() || later.contains_key(&c.interface));
    let ancestry = ancestry(source, needed).await;
    Ok(suspects
        .iter()
        .filter(|c| !brought_again(c, later.get(&c.interface).map(Vec::as_slice), ancestry.as_ref()))
        .filter_map(|c| Some((c.interface.clone(), judge(c, ancestry.as_ref(), source, true)?)))
        .collect())
}

/// A later deployment ran `before`'s version from a commit in this history, every file of it
/// committed there: the version is that commit's too, so replacing it undoes nothing the history
/// lacks (0.7.5: the same files from a rewritten history, a fresh repository or a squash, are
/// accepted once any deploy brought them from a commit the history has).
fn brought_again(
    before: &Current,
    later: Option<&[ductus_store::Brought]>,
    ancestry: Option<&crate::git::Ancestry>,
) -> bool {
    let Some(ancestry) = ancestry.filter(|a| a.head.is_some()) else { return false };
    later.unwrap_or_default().iter().any(|b| {
        let uncommitted = b.uncommitted.as_deref().unwrap_or_default();
        ancestry.contains(&b.git_commit) && !before.files.iter().any(|f| uncommitted.contains(f))
    })
}

/// The same for the files outside every interface: the project version `project` runs now in
/// `environment` (release mode records one) replaced by `version`. None when it may undo nothing.
pub async fn project_replaced(
    store: &Store,
    environment: &str,
    project: &str,
    version: &[u8],
    source: &Source,
    who: &Who,
) -> Result<Option<String>> {
    let Some(p) = store.project_running(environment, project).await? else { return Ok(None) };
    if p.version == version {
        return Ok(None);
    }
    let own: Vec<String> = p.uncommitted.iter().flatten().filter(|f| !f.starts_with("interfaces/")).cloned().collect();
    let before = Current {
        interface: PROJECT.into(),
        version: p.version,
        files: own,
        deployment: p.deployment,
        at: p.at,
        how: p.how,
        by: p.by,
        host: p.host,
        git_commit: p.git_commit,
        uncommitted: p.uncommitted,
        release: p.release,
    };
    if !suspect(&before, source, who) {
        return Ok(None);
    }
    let later = match source {
        Source::Release { commit: None, .. } => Vec::new(),
        _ => store.later_project_sources(environment, &before.version, before.deployment).await?,
    };
    // `before.files` names only the project files it ran uncommitted: a later deployment vouches
    // for the version only with every project file committed.
    let later: Vec<_> =
        later.into_iter().filter(|b| b.uncommitted.iter().flatten().all(|f| f.starts_with("interfaces/"))).collect();
    let ancestry = ancestry(source, before.git_commit.is_some() || !later.is_empty()).await;
    if brought_again(&before, Some(&later), ancestry.as_ref()) {
        return Ok(None);
    }
    Ok(judge(&before, ancestry.as_ref(), source, false))
}

/// Replaced by someone else, with something to say about where it came from.
fn suspect(c: &Current, source: &Source, who: &Who) -> bool {
    let without_git = matches!(source, Source::Release { commit: None, .. });
    !same_person(who, c) && (c.git_commit.is_some() || !dirty(c).is_empty() || without_git)
}

/// The history to judge commits by: the release's, or (folder mode, when needed) the project's.
async fn ancestry(source: &Source, needed: bool) -> Option<crate::git::Ancestry> {
    match source {
        Source::Release { commit: Some(commit), ancestry, .. } => {
            Some(crate::git::Ancestry::of_release(commit, ancestry))
        }
        Source::Release { commit: None, .. } => None,
        Source::Folder(_) if !needed => None,
        Source::Folder(root) => {
            let root = root.clone();
            match tokio::task::spawn_blocking(move || crate::git::ancestry(&root)).await {
                Ok(Some(Ok(a))) => Some(a),
                Ok(Some(Err(e))) => {
                    tracing::warn!(error = %format!("{e:#}"), "couldn't read the project's git history: no warning for changes from commits it doesn't have");
                    None
                }
                _ => None,
            }
        }
    }
}

/// The same person deployed it: both are people (not a service manager) with the same name.
pub(crate) fn same_person(who: &Who, before: &Current) -> bool {
    let person = |how: &str, by: &str| how != "service" && !by.starts_with("systemd");
    person(who.how, &who.by) && person(&before.how, &before.by) && who.by == before.by
}

/// Files the version replaced ran that weren't committed when it was deployed.
fn dirty(before: &Current) -> Vec<&str> {
    let uncommitted = before.uncommitted.as_deref().unwrap_or_default();
    before.files.iter().filter(|f| uncommitted.contains(f)).map(String::as_str).collect()
}

/// Why replacing `before` may undo someone's work, in words; None when it can't. `history`: end
/// with where to look (`ductus history <interface>`).
fn judge(before: &Current, ancestry: Option<&crate::git::Ancestry>, source: &Source, history: bool) -> Option<String> {
    let mut why = Vec::new();
    if let (Some(commit), Some(ancestry)) = (&before.git_commit, ancestry)
        && ancestry.head.is_some()
        && !ancestry.contains(commit)
    {
        let whose =
            if matches!(source, Source::Folder(_)) { "this project's history" } else { "this release's history" };
        why.push(format!("came from commit {}, which {whose} doesn't have", &commit[..commit.len().min(12)]));
    }
    let dirty = dirty(before);
    match dirty.len() {
        0 => {}
        1 => why.push(format!("ran a file never committed ({})", dirty[0])),
        n => why.push(format!("ran {n} files never committed ({}, …)", dirty[0])),
    }
    // Without git, only the base it names tells what it was made over: none named, nothing is known.
    if let Source::Release { commit: None, over, .. } = source
        && (over.is_none() || before.release.as_deref() != over.as_deref())
    {
        let came = match &before.release {
            Some(r) => format!("came from release {}", release::short(&code::hex(r))),
            None => "came from a project folder".into(),
        };
        let base = match over {
            Some(o) => format!("was made over {}", release::short(&code::hex(o))),
            None => "names no base (`ductus release --over`)".into(),
        };
        why.push(format!("{came}, and this release, made without git, {base}"));
    }
    if why.is_empty() {
        return None;
    }
    let mut text = format!(
        "replaces {} (deployed {} by {} on {}): it {}, so their change may be undone",
        code::short(&before.version),
        before.at.format("%Y-%m-%d %H:%M UTC"),
        before.by,
        before.host,
        why.join(" and "),
    );
    if history {
        text.push_str(&format!(" (`ductus history {}`)", before.interface));
    }
    Some(text)
}

/// Bytes from hex; None when it isn't.
pub fn unhex(text: &str) -> Option<Vec<u8>> {
    if text.is_empty() || !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn current(by: &str, commit: Option<&str>, release: Option<Vec<u8>>) -> Current {
        Current {
            interface: "adt".into(),
            version: vec![1; 32],
            files: vec!["interfaces/adt/interface.yaml".into()],
            deployment: 1,
            at: Utc::now(),
            how: "handover".into(),
            by: by.into(),
            host: "box".into(),
            git_commit: commit.map(str::to_string),
            uncommitted: commit.map(|_| Vec::new()),
            release,
        }
    }

    fn who(by: &str) -> Who {
        Who { how: "handover", by: by.into(), reason: None, rollback: false, variables: Vec::new() }
    }

    /// After deploys in place an engine runs from several unpacked releases: `tidy` keeps every
    /// one an interface runs from (`in_use`), however old, then the `KEPT` used last; it removes
    /// the rest and what a stopped unpack left.
    #[test]
    fn tidy_keeps_every_release_an_interface_runs_from() {
        let dir = tempfile::tempdir().unwrap();
        let release = |n: u8| format!("{n:064x}");
        for n in 1..=5 {
            std::fs::create_dir_all(dir.path().join(release(n)).join("interfaces").join("a")).unwrap();
            // Told apart by when they were used last: the oldest first.
            std::thread::sleep(Duration::from_millis(20));
        }
        let leftover = dir.path().join(format!(".{}.partial-1", release(9)));
        std::fs::create_dir_all(&leftover).unwrap();
        let folders =
            [dir.path().join(release(1)).join("interfaces/adt"), dir.path().join(release(2)).join("interfaces/lab")];
        let elsewhere = PathBuf::from("/somewhere/else/interfaces/x");
        let running = in_use(dir.path(), folders.iter().map(PathBuf::as_path).chain([elsewhere.as_path()]));
        assert_eq!(running, [release(1), release(2)].into());

        tidy(dir.path(), &running);
        let mut left: Vec<String> =
            std::fs::read_dir(dir.path()).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into()).collect();
        left.sort();
        assert_eq!(left, [release(1), release(2), release(4), release(5)], "1 and 2 run, 4 and 5 used last");
    }

    /// A release holds what runs against its own history: a colleague's merged commit passes, an
    /// unmerged one is named; the deployer's own work never refuses.
    #[test]
    fn a_release_refuses_to_undo_a_colleagues_unmerged_commit() {
        let merged = "1111111111111111111111111111111111111111";
        let theirs = "2222222222222222222222222222222222222222";
        let ids = [[0x11u8; 20], [0x33u8; 20]];
        let source = Source::Release { commit: Some("3".repeat(40)), ancestry: Arc::new(ids.concat()), over: None };
        let ancestry = crate::git::Ancestry::of_release(&"3".repeat(40), &[[0x11u8; 20], [0x33u8; 20]].concat());
        assert!(judge(&current("ann", Some(merged), None), Some(&ancestry), &source, true).is_none());
        let refused = judge(&current("ann", Some(theirs), None), Some(&ancestry), &source, true).unwrap();
        assert!(
            refused.contains("came from commit 222222222222, which this release's history doesn't have"),
            "{refused}"
        );
        assert!(refused.contains("by ann") && refused.ends_with("(`ductus history adt`)"), "{refused}");
        assert!(!suspect(&current("bob", Some(theirs), None), &source, &who("bob")), "bob's own work");
        assert!(suspect(&current("ann", Some(theirs), None), &source, &who("bob")));
    }

    /// A version first brought from a commit the release lacks (a rewritten history) is accepted
    /// once a later deploy ran it from a commit the release has, its files committed there; not
    /// when that deploy had its files uncommitted, nor from a commit the release lacks too.
    #[test]
    fn a_version_brought_again_from_the_releases_history_is_accepted() {
        let (theirs, ours) = ("2".repeat(40), "1".repeat(40));
        let ancestry = crate::git::Ancestry::of_release(&"3".repeat(40), &[[0x11u8; 20], [0x33u8; 20]].concat());
        let before = current("ann", Some(&theirs), None);
        let brought = |commit: &str, uncommitted: &[&str]| ductus_store::Brought {
            git_commit: commit.into(),
            uncommitted: Some(uncommitted.iter().map(|s| s.to_string()).collect()),
        };
        assert!(brought_again(&before, Some(&[brought(&ours, &[])]), Some(&ancestry)));
        assert!(!brought_again(&before, Some(&[brought(&ours, &["interfaces/adt/interface.yaml"])]), Some(&ancestry)));
        assert!(!brought_again(&before, Some(&[brought(&theirs, &[])]), Some(&ancestry)));
        assert!(!brought_again(&before, None, Some(&ancestry)), "nothing later");
        assert!(!brought_again(&before, Some(&[brought(&ours, &[])]), None), "no history to judge by");
    }

    /// Without git a release is held against the release it names as its base.
    #[test]
    fn a_release_without_git_is_held_against_its_base() {
        let base = vec![7u8; 32];
        let source = Source::Release { commit: None, ancestry: Arc::default(), over: Some(base.clone()) };
        assert!(judge(&current("ann", None, Some(base.clone())), None, &source, true).is_none(), "over what runs");
        let other = judge(&current("ann", None, Some(vec![8u8; 32])), None, &source, true).unwrap();
        assert!(
            other.contains(
                "came from release 080808080808, and this release, made without git, was made over 070707070707"
            ),
            "{other}"
        );
        let none = Source::Release { commit: None, ancestry: Arc::default(), over: None };
        let folder = judge(&current("ann", None, None), None, &none, false).unwrap();
        assert!(folder.contains("came from a project folder") && folder.contains("names no base"), "{folder}");
        assert!(!folder.contains("ductus history"));
        assert!(suspect(&current("ann", None, None), &none, &who("bob")), "without git, anyone else's is a suspect");
    }

    fn run(release: u8, deployment: i32, rollback: bool) -> ductus_store::Run {
        ductus_store::Run {
            release: vec![release; 32],
            deployment,
            at: Utc::now(),
            by: "ann".into(),
            reason: None,
            rollback,
        }
    }

    /// The default rollback: the last release run before the current one, skipping any a rollback
    /// left; going back again after a rollback goes further back, never to what was rolled back from.
    #[test]
    fn a_rollback_goes_to_the_last_release_before_skipping_what_a_rollback_left() {
        let target =
            |runs: &[ductus_store::Run], current: u8| rollback_target(runs, &[current; 32]).map(|r| r.release[0]);
        assert_eq!(target(&[run(1, 1, false), run(2, 2, false)], 2), Some(1));
        // Z A B, rolled back to A: the next rollback goes to Z, not B.
        let runs = [run(9, 1, false), run(1, 2, false), run(2, 3, false), run(1, 4, true)];
        assert_eq!(target(&runs, 1), Some(9));
        // A B, rolled back to A: nothing before A but A.
        assert_eq!(target(&[run(1, 1, false), run(2, 2, false), run(1, 3, true)], 1), None);
        // B deployed again after it was rolled back from: a rollback from C may go to it.
        let runs = [run(1, 1, false), run(2, 2, false), run(1, 3, true), run(2, 4, false), run(3, 5, false)];
        assert_eq!(target(&runs, 3), Some(2));
        assert_eq!(target(&[], 1), None);
    }

    fn manifest(git: Option<(&str, Option<&str>)>) -> Manifest {
        Manifest {
            format: 1,
            id: "ab".repeat(32),
            project: "p".into(),
            made: crate::release::Made {
                at: "2026-11-12T00:00:00Z".into(),
                by: "ann".into(),
                host: "h".into(),
                reason: None,
                binary: "0.7.0".into(),
                platform: "linux-x86_64".into(),
            },
            git: git.map(|(commit, branch)| crate::release::GitSource {
                commit: commit.into(),
                folder: String::new(),
                branch: branch.map(str::to_string),
                commits: 1,
                ancestry: Vec::new(),
            }),
            uncommitted: Vec::new(),
            over: None,
            project_version: crate::release::ProjectVersion { version: String::new(), files: Vec::new() },
            interfaces: BTreeMap::new(),
            tests: crate::release::Tests::default(),
            files: 0,
            bytes: 0,
            binary: Vec::new(),
            deployed: Vec::new(),
        }
    }

    fn place(environment: &str) -> ductus_store::Place {
        ductus_store::Place {
            environment: environment.into(),
            host: "box".into(),
            deployment: 1,
            at: Utc::now(),
            by: "ci".into(),
            ductus_version: "0.7.0".into(),
            here: false,
        }
    }

    /// `DUCTUS_DEPLOY_REQUIRES` and `DUCTUS_DEPLOY_BRANCHES`: each refusal says what's missing and
    /// what to do; nothing set, nothing refused.
    #[test]
    fn an_environments_rules_refuse_with_what_to_do() {
        let mode = |requires: &[&str], branches: &[&str]| Mode {
            dir: PathBuf::new(),
            project: None,
            reuse_check: Duration::ZERO,
            requires: requires.iter().map(|s| s.to_string()).collect(),
            branches: branches.iter().map(|s| s.to_string()).collect(),
            promote_from: None,
        };
        let git = manifest(Some(("1".repeat(40).as_str(), None)));
        let on = |names: &[&str]| -> Vec<String> { names.iter().map(|s| s.to_string()).collect() };
        assert!(rules(&mode(&[], &[]), &manifest(None), &[], &[]).is_empty(), "no rules");
        assert!(rules(&mode(&["test"], &["main"]), &git, &on(&["main"]), &[place("test")]).is_empty());

        let missing = rules(&mode(&["test", "uat"], &[]), &git, &[], &[place("dev"), place("test")]);
        assert_eq!(missing.len(), 1);
        assert!(
            missing[0].contains("deployed to uat first") && missing[0].contains("it was deployed to dev, test only")
        );
        assert!(missing[0].contains("ductus deploy abababababab --from <uat's engine>"), "{}", missing[0]);
        let nowhere = rules(&mode(&["test"], &[]), &git, &[], &[]);
        assert!(nowhere[0].contains("it hasn't been deployed anywhere that this engine knows of"), "{}", nowhere[0]);

        let branches = mode(&[], &["main", "release/*"]);
        let feature = rules(&branches, &git, &on(&["feature/x"]), &[]);
        assert!(feature[0].contains("made on main, release/*") && feature[0].contains("made on `feature/x`"));
        assert!(rules(&branches, &git, &on(&["release/2026-11"]), &[]).is_empty(), "a prefix");
        // The same files made on a feature branch, then on main after a fast-forward merge.
        assert!(rules(&branches, &git, &on(&["feature/x", "main"]), &[]).is_empty(), "any of its branches");
        let detached = rules(&mode(&[], &["main"]), &git, &[], &[]);
        assert!(detached[0].contains("records no branch") && detached[0].contains("--branch NAME"), "{}", detached[0]);
        let no_git = rules(&mode(&[], &["main"]), &manifest(None), &[], &[]);
        assert!(no_git[0].contains("made without git"), "{}", no_git[0]);
        assert!(!branch_matches("main", "main-old") && branch_matches("*", "anything"));
    }

    /// A release of `files` (path, text) with one interface `adt` (version `version`): its manifest
    /// and a reader of its files.
    fn released(version: &str, files: &[(&str, &str)]) -> (Manifest, impl Fn(&str) -> Result<String> + use<>) {
        let mut m = manifest(None);
        let project: Vec<String> =
            files.iter().map(|(p, _)| p.to_string()).filter(|p| code::is_project_file(p)).collect();
        m.project_version = crate::release::ProjectVersion { version: format!("{:?}", files), files: project };
        let own = files.iter().map(|(p, _)| p.to_string()).filter(|p| p.starts_with("interfaces/")).collect();
        m.interfaces.insert(
            "adt".into(),
            crate::release::Interface { version: version.into(), files: own, parts: code::Parts::default() },
        );
        let texts: BTreeMap<String, String> = files.iter().map(|(p, t)| (p.to_string(), t.to_string())).collect();
        (m, move |p: &str| texts.get(p).cloned().with_context(|| format!("no {p}")))
    }

    /// The project's own files judged one by one (0.7.5.2.3): another environment's file changes
    /// nothing here; this environment's variables reach the interface that fills one in, and the
    /// code that names one; `console:` swaps; a variable that changes what resources.yaml fills in,
    /// a secrets file or resources.yaml itself takes the handover.
    #[test]
    fn the_project_files_are_judged_one_by_one() {
        let yaml = (
            "interfaces/adt/interface.yaml",
            "name: adt\nsource: { mllp: { port: ${MLLP_PORT} } }\ndestinations: {}\n",
        );
        let code = ("interfaces/adt/transform.ts", "const url = env(\"EMPI_URL\");\n");
        let resources = ("resources.yaml", "databases:\n  emr: { host: ${DB_HOST} }\n");
        let prod = |vars: &str| ("environments/prod.yaml", format!("variables: {{ {vars} }}\n"));
        let base = "MLLP_PORT: 6661, EMPI_URL: a, DB_HOST: db1";
        let plan = |before: &[(&str, &str)], after: &[(&str, &str)]| {
            let (running, read_before) = released("v1", before);
            let (next, read_after) = released("v1", after);
            plan_between(&running, read_before, &next, read_after, "prod").unwrap()
        };
        let (p1, test) = (prod(base), ("environments/test.yaml", "variables: { MLLP_PORT: 1 }\n"));
        let files = |p: &(&'static str, String), extra: &[(&'static str, &'static str)]| {
            let mut all = vec![yaml, code, resources, (p.0, p.1.as_str())];
            all.extend_from_slice(extra);
            all.into_iter().map(|(a, b)| (a, b.to_string())).collect::<Vec<_>>()
        };
        fn refs<'a>(v: &'a [(&'static str, String)]) -> Vec<(&'static str, &'a str)> {
            v.iter().map(|(a, b)| (*a, b.as_str())).collect()
        }
        let (before, after) =
            (files(&p1, &[test]), files(&p1, &[("environments/test.yaml", "variables: { MLLP_PORT: 2 }\n")]));

        // Another environment's file: nothing changes, in place.
        let other = plan(&refs(&before), &refs(&after));
        assert!(other.in_place() && other.changes.is_empty() && !other.environment_changes, "{other:#?}");
        assert_eq!(
            other.project_in_place,
            ["environments/test.yaml: another environment's file, nothing changes here"]
        );

        // This environment's port and the variable the code names: in place, both interfaces' reasons said.
        let moved = files(&prod("MLLP_PORT: 6666, EMPI_URL: b, DB_HOST: db1"), &[test]);
        let reached = plan(&refs(&before), &refs(&moved));
        assert!(reached.in_place() && reached.environment_changes, "{reached:#?}");
        assert_eq!(reached.changes.len(), 1);
        assert_eq!(reached.changes[0].what, ["source (MLLP_PORT)", "code reads EMPI_URL"]);
        let by = reached.changes[0].reached.as_ref().unwrap();
        let names: Vec<&str> = by.variables.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, ["EMPI_URL", "MLLP_PORT"]);
        // Its source by the port; every part by the code (this version records no parts).
        assert!(by.touches.source && by.touches.everything, "{by:#?}");
        assert_eq!(
            reached.project_in_place,
            ["environments/prod.yaml: EMPI_URL \"a\" → \"b\", MLLP_PORT \"6661\" → \"6666\""]
        );

        // console: alone swaps, reaching no interface.
        let quiet = files(&("environments/prod.yaml", format!("variables: {{ {base} }}\nconsole: warn\n")), &[test]);
        let console = plan(&refs(&before), &refs(&quiet));
        assert!(console.in_place() && console.changes.is_empty() && console.environment_changes);
        assert_eq!(console.project_in_place, ["environments/prod.yaml: console info → warn"]);

        // What resources.yaml fills in, a secrets file, resources.yaml as written: the handover.
        let db = files(&prod("MLLP_PORT: 6661, EMPI_URL: a, DB_HOST: db2"), &[test]);
        let why = plan(&refs(&before), &refs(&db)).handover_because();
        assert_eq!(
            why,
            ["DB_HOST changes what resources.yaml fills in (databases, APIs and their pools are made at a start)"]
        );
        let secret = files(&p1, &[test, ("secrets/prod.json", "{}")]);
        let why = plan(&refs(&before), &refs(&secret)).handover_because();
        assert_eq!(why, ["secrets/prod.json changed (secrets are decrypted at a start)"]);
        let mut edited = files(&p1, &[test]);
        edited[2].1 = "databases:\n  emr: { host: other }\n".into();
        let why = plan(&refs(&before), &refs(&edited)).handover_because();
        assert_eq!(why, ["resources.yaml changed (databases, APIs and their pools are made at a start)"]);
    }

    #[test]
    fn hex_reads_back() {
        assert_eq!(unhex(&code::hex(&[0, 0xab, 0xff])), Some(vec![0, 0xab, 0xff]));
        assert_eq!(unhex("abc"), None);
        assert_eq!(unhex(""), None);
    }
}
