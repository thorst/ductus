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
#[derive(Debug, Clone)]
pub struct Releases {
    pub mode: Mode,
    pub running: Option<Arc<Running>>,
}

impl Releases {
    /// The project this engine runs: its release's, else the one its settings name.
    pub fn project(&self) -> Option<&str> {
        self.running.as_ref().map(|r| r.manifest.project.as_str()).or(self.mode.project.as_deref())
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
        return Ok((root, Releases { mode: mode.clone(), running: None }));
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
    let folder = mode.dir.join(&hex);
    let unpacked = std::fs::read_to_string(folder.join(MARKER)).is_ok_and(|m| m.trim() == hex);
    if !unpacked {
        let contents = store
            .release_contents(&id)
            .await?
            .with_context(|| format!("release {} isn't stored", release::short(&hex)))?;
        let (dir, hex) = (mode.dir.clone(), hex.clone());
        tokio::task::spawn_blocking(move || unpack(&dir, &hex, contents)).await.context("unpacking the release")??;
    }
    tracing::info!(
        release = release::short(&hex),
        project = %manifest.project,
        unpacked = !unpacked,
        took = ?started.elapsed(),
        "release mode: running release {}",
        release::short(&hex)
    );
    let running = Arc::new(Running { id, manifest, ancestry });
    Ok((folder, Releases { mode: mode.clone(), running: Some(running) }))
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

/// Remove unpacked releases but the one running and the `KEPT` used last, and folders a stopped
/// unpack left. Beside the start; what can't be removed is said and left.
pub fn tidy(dir: &Path, running: &str) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut releases = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        let leftover =
            name.starts_with('.') && name.contains(".partial-") && !name.ends_with(&format!("-{}", std::process::id()));
        if leftover {
            remove(&path);
        } else if name.len() == 64 && name.bytes().all(|b| b.is_ascii_hexdigit()) && name != running {
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
    let ancestry = ancestry(source, suspects.iter().any(|c| c.git_commit.is_some())).await;
    Ok(suspects
        .iter()
        .filter_map(|c| Some((c.interface.clone(), judge(c, ancestry.as_ref(), source, true)?)))
        .collect())
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
    let ancestry = ancestry(source, before.git_commit.is_some()).await;
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
        Who { how: "handover", by: by.into(), reason: None }
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

    #[test]
    fn hex_reads_back() {
        assert_eq!(unhex(&code::hex(&[0, 0xab, 0xff])), Some(vec![0, 0xab, 0xff]));
        assert_eq!(unhex("abc"), None);
        assert_eq!(unhex(""), None);
    }
}
