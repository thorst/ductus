//! The project's git commit, read from `.git` with gitoxide (nothing outside the binary: no `git`
//! needed on the server). A deployment records the commit and which of the files its interfaces run
//! aren't the commit's: changed since, or not in it. That's "clean" for code versions: a log file
//! nobody committed doesn't count, an edited transform does. Line ends don't count either (a
//! Windows checkout's CRLF is the commit's LF), as for code versions.

use crate::code::File;
use anyhow::{Context, Result};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

/// What git says about the files a start runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    /// HEAD's commit, in hex. None in a repository without commits yet.
    pub id: Option<String>,
    /// Files (as `code::File::path`) that differ from the commit or aren't in it, in path order.
    pub uncommitted: Vec<String>,
}

/// Look for the repository holding `root` (the project's folder or one above it) and compare
/// `files` with its HEAD. None when the project isn't in a repository.
pub fn read<'a>(root: &Path, files: impl IntoIterator<Item = &'a File>) -> Option<Result<Commit>> {
    let repository = match gix::discover(root) {
        Ok(r) => r,
        // A `.git` that can't be read is said; no `.git` at all is the usual case of no git.
        Err(e) if root.ancestors().any(|d| d.join(".git").exists()) => {
            return Some(Err(anyhow::Error::new(e).context("reading the project's git repository")));
        }
        Err(_) => return None,
    };
    Some(compare(&repository, root, files))
}

fn compare<'a>(repository: &gix::Repository, root: &Path, files: impl IntoIterator<Item = &'a File>) -> Result<Commit> {
    let mut files: Vec<&File> = files.into_iter().collect();
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files.dedup_by(|a, b| a.path == b.path);
    let Ok(head) = repository.head_commit() else {
        return Ok(Commit { id: None, uncommitted: files.iter().map(|f| f.path.clone()).collect() });
    };
    // The project's folder in the commit, read once (looking each file up from the top would read
    // the `interfaces` tree again for every file: thousands of times at a thousand interfaces).
    let project = project_prefix(repository, root)?;
    let tree = head.tree().context("reading HEAD's tree")?;
    let mut blobs = BTreeMap::new();
    let project_tree = if project.is_empty() {
        Some(tree)
    } else {
        match tree.lookup_entry_by_path(&project).context("finding the project in HEAD")? {
            Some(entry) => Some(entry.object().context("reading the project's tree")?.peel_to_tree()?),
            None => None, // the project folder isn't committed at all
        }
    };
    if let Some(t) = project_tree {
        let mut recorder = gix::traverse::tree::Recorder::default();
        t.traverse().breadthfirst(&mut recorder).context("reading the project's files in HEAD")?;
        for entry in recorder.records {
            if entry.mode.is_blob() {
                blobs.insert(entry.filepath.to_string(), entry.oid);
            }
        }
    }
    let kind = repository.object_hash();
    let mut uncommitted = Vec::new();
    for f in files {
        let same = match blobs.get(&f.path) {
            None => false,
            Some(id) => {
                let ours = gix::objs::compute_hash(kind, gix::objs::Kind::Blob, f.text.as_bytes())?;
                // Equal as written; else the commit may hold CRLF (or a byte order mark).
                ours == *id || {
                    let blob = repository.find_object(*id).context("reading a file in HEAD")?;
                    ductus_script::normalize(&String::from_utf8_lossy(&blob.data)) == f.text
                }
            }
        };
        if !same {
            uncommitted.push(f.path.clone());
        }
    }
    Ok(Commit { id: Some(head.id.to_string()), uncommitted })
}

fn slashes(path: &Path) -> String {
    path.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/")
}

/// The project's folder in the repository's working folder, with `/` ("" at its top).
pub(crate) fn project_prefix(repository: &gix::Repository, root: &Path) -> Result<String> {
    let workdir = repository.workdir().context("the repository has no working folder (a bare repository)")?;
    let workdir = std::fs::canonicalize(workdir).with_context(|| format!("finding {}", workdir.display()))?;
    let root = std::fs::canonicalize(root).with_context(|| format!("finding {}", root.display()))?;
    Ok(slashes(root.strip_prefix(&workdir).unwrap_or(Path::new(""))))
}

/// How a commit changed a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum How {
    Added,
    Modified,
    Deleted,
}

/// A commit that changed some of the files asked about (`history`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// In hex.
    pub id: String,
    /// The commit's time (the committer's: when it landed where it is).
    pub at: chrono::DateTime<chrono::Utc>,
    pub author: String,
    pub email: String,
    /// The message's first line.
    pub summary: String,
    /// The files asked about it changed (against its first parent), in path order.
    pub files: Vec<(String, How)>,
    /// Each `interface.yaml` among `files` as it was before and after (None: absent, or not text),
    /// to tell which parts of the interface the commit changed (history by destination, 0.7.4.2).
    pub yaml: Vec<Yaml>,
}

/// An interface.yaml a commit changed, before and after.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Yaml {
    pub path: String,
    pub before: Option<String>,
    pub after: Option<String>,
}

/// The commits since HEAD that changed any of `paths` (relative to the project at `root`, with
/// `/`), newest first: what `git log -- <paths>` lists, read from `.git` with gitoxide. A merge
/// counts only when it differs from every parent in those files (as `git log` simplifies: the
/// commit on the branch is the change). None when the project isn't in a repository.
pub fn history(root: &Path, paths: &BTreeSet<String>) -> Option<Result<std::sync::Arc<Vec<Change>>>> {
    let mut repository = match gix::discover(root) {
        Ok(r) => r,
        Err(e) if root.ancestors().any(|d| d.join(".git").exists()) => {
            return Some(Err(anyhow::Error::new(e).context("reading the project's git repository")));
        }
        Err(_) => return None,
    };
    // The same files' history until HEAD moves: the History tab's pages and refreshes read it again
    // (a walk is ~0.1 s per thousand commits at a thousand interfaces; the cache a few KB each).
    let head = repository.head_id().ok().map(|id| id.detach());
    let key = (root.to_path_buf(), paths.clone());
    if let Some(head) = head
        && let Some(found) =
            WALKED.lock().ok().and_then(|w| w.get(&key).filter(|(h, _)| *h == head).map(|(_, c)| c.clone()))
    {
        return Some(Ok(found));
    }
    // Each commit is read twice (the walk, then its tree) and a tree once per name looked up in it:
    // kept decompressed while the walk needs them, for this walk only. And the pack's delta bases:
    // a tree is stored as a change to an older one, often dozens deep (`git gc`'s depth is 50), so
    // without them each tree read decompresses its whole chain again (git keeps them too).
    repository.object_cache_size_if_unset(16 << 20);
    repository.objects.set_pack_cache(|| Box::<gix::odb::pack::cache::lru::StaticLinkedList<64>>::default());
    let changes = match walk(&repository, root, paths) {
        Ok(c) => std::sync::Arc::new(c),
        Err(e) => return Some(Err(e)),
    };
    if let (Some(head), Ok(mut walked)) = (head, WALKED.lock()) {
        if walked.len() >= WALKS_KEPT {
            walked.clear(); // rare: as many interfaces' histories read since HEAD last moved
        }
        walked.insert(key, (head, changes.clone()));
    }
    Some(Ok(changes))
}

/// How many walks `history` keeps (one per interface whose history was read).
const WALKS_KEPT: usize = 256;

type Walked = HashMap<(std::path::PathBuf, BTreeSet<String>), (gix::ObjectId, std::sync::Arc<Vec<Change>>)>;

static WALKED: std::sync::LazyLock<std::sync::Mutex<Walked>> = std::sync::LazyLock::new(Default::default);

fn walk(repository: &gix::Repository, root: &Path, paths: &BTreeSet<String>) -> Result<Vec<Change>> {
    let Ok(head) = repository.head_commit() else { return Ok(Vec::new()) }; // no commit yet
    let prefix = project_prefix(repository, root)?;
    let paths: Vec<&String> = paths.iter().collect();
    let full: Vec<Vec<&str>> =
        paths.iter().map(|p| prefix.split('/').filter(|c| !c.is_empty()).chain(p.split('/')).collect()).collect();
    let mut states = States { repository, full: &full, entries: HashMap::new(), commits: HashMap::new() };
    let walk = repository
        .rev_walk([head.id])
        .sorting(gix::revision::walk::Sorting::ByCommitTime(Default::default()))
        .all()
        .context("walking the commits")?;
    let mut changes = Vec::new();
    for info in walk {
        let info = info.context("reading a commit")?;
        let mine = states.of(info.id)?;
        let parents: Vec<gix::ObjectId> = info.parent_ids().map(|p| p.detach()).collect();
        let mut theirs = Vec::with_capacity(parents.len());
        for p in &parents {
            theirs.push(states.of(*p)?);
        }
        let differs =
            if theirs.is_empty() { mine.iter().any(Option::is_some) } else { theirs.iter().all(|t| *t != mine) };
        if !differs {
            continue;
        }
        let before = theirs.first();
        let mut yaml = Vec::new();
        let mut files = Vec::new();
        for (i, path) in paths.iter().enumerate() {
            let was = before.and_then(|b| b[i]);
            let how = match (was, mine[i]) {
                (None, Some(_)) => How::Added,
                (Some(_), None) => How::Deleted,
                (Some(a), Some(b)) if a != b => How::Modified,
                _ => continue,
            };
            if path.rsplit('/').next() == Some("interface.yaml") {
                let text = |id: Option<gix::ObjectId>| -> Result<Option<String>> {
                    let Some(id) = id else { return Ok(None) };
                    let blob = repository.find_blob(id).context("reading a file of a commit")?;
                    Ok(String::from_utf8(blob.data.clone()).ok())
                };
                yaml.push(Yaml { path: path.to_string(), before: text(was)?, after: text(mine[i])? });
            }
            files.push((path.to_string(), how));
        }
        let commit = repository.find_commit(info.id).context("reading a commit")?;
        let author = commit.author().context("reading a commit's author")?;
        let time = commit.time().context("reading a commit's time")?;
        let message = commit.message_raw_sloppy().to_string();
        changes.push(Change {
            id: info.id.to_string(),
            at: chrono::DateTime::from_timestamp(time.seconds, 0).unwrap_or_default(),
            author: author.name.to_string().trim().to_string(),
            email: author.email.to_string().trim().to_string(),
            summary: message.lines().next().unwrap_or_default().trim().to_string(),
            files,
            yaml,
        });
    }
    Ok(changes)
}

/// Each commit's blobs at the paths asked about, worked out once per commit, and each name looked
/// up once per tree: commits share most of their trees, so a commit costs the few trees it changed
/// (at a thousand interfaces, the `interfaces` tree is read once per commit that changed it, not
/// once per file asked about).
struct States<'r, 'p> {
    repository: &'r gix::Repository,
    /// Each path's components from the repository's top.
    full: &'p [Vec<&'p str>],
    /// (tree, a name in it) → that entry: its id, and whether it's a tree (else a blob, or None).
    entries: HashMap<(gix::ObjectId, &'p str), Option<(gix::ObjectId, bool)>>,
    commits: HashMap<gix::ObjectId, std::rc::Rc<Vec<Option<gix::ObjectId>>>>,
}

impl<'p> States<'_, 'p> {
    fn of(&mut self, commit: gix::ObjectId) -> Result<std::rc::Rc<Vec<Option<gix::ObjectId>>>> {
        if let Some(state) = self.commits.get(&commit) {
            return Ok(state.clone());
        }
        let root = self.repository.find_commit(commit).context("reading a commit")?.tree_id()?.detach();
        let mut state = Vec::with_capacity(self.full.len());
        for parts in self.full {
            let mut at = Some((root, true));
            for part in parts {
                at = match at {
                    Some((tree, true)) => self.entry(tree, part)?,
                    _ => None, // a file where a folder is expected, or nothing there
                };
            }
            state.push(at.and_then(|(id, tree)| (!tree).then_some(id)));
        }
        let state = std::rc::Rc::new(state);
        self.commits.insert(commit, state.clone());
        Ok(state)
    }

    /// The entry `name` in `tree`: its id and whether it's a tree (blobs and links are files).
    fn entry(&mut self, tree: gix::ObjectId, name: &'p str) -> Result<Option<(gix::ObjectId, bool)>> {
        if let Some(found) = self.entries.get(&(tree, name)) {
            return Ok(*found);
        }
        let found = self
            .repository
            .find_tree(tree)
            .context("reading a tree")?
            .find_entry(name)
            .filter(|e| e.mode().is_tree() || e.mode().is_blob())
            .map(|e| (e.object_id(), e.mode().is_tree()));
        self.entries.insert((tree, name), found);
        Ok(found)
    }
}

/// The commits HEAD descends from (itself included), to tell whether a commit is in its history.
pub struct Ancestry {
    pub head: Option<String>,
    ids: std::collections::HashSet<gix::ObjectId>,
}

impl Ancestry {
    /// A release's (0.7.4.4): its commit and the ancestry it carries (20-byte ids, `release::GitSource`).
    pub fn of_release(commit: &str, ancestry: &[u8]) -> Self {
        let (ids, _) = ancestry.as_chunks::<20>();
        Self {
            head: Some(commit.to_string()),
            ids: ids.iter().map(|id| gix::ObjectId::from_bytes_or_panic(id.as_slice())).collect(),
        }
    }

    /// Whether `commit` (hex) is HEAD or one of its ancestors.
    pub fn contains(&self, commit: &str) -> bool {
        gix::ObjectId::from_hex(commit.as_bytes()).is_ok_and(|id| self.ids.contains(&id))
    }
}

/// HEAD's history, walked once (commits only, no trees). None when the project isn't in a
/// repository.
pub fn ancestry(root: &Path) -> Option<Result<Ancestry>> {
    let repository = match gix::discover(root) {
        Ok(r) => r,
        Err(e) if root.ancestors().any(|d| d.join(".git").exists()) => {
            return Some(Err(anyhow::Error::new(e).context("reading the project's git repository")));
        }
        Err(_) => return None,
    };
    let read = || -> Result<Ancestry> {
        let Ok(head) = repository.head_commit() else {
            return Ok(Ancestry { head: None, ids: Default::default() });
        };
        let mut ids = std::collections::HashSet::new();
        for info in repository.rev_walk([head.id]).all().context("walking the commits")? {
            ids.insert(info.context("reading a commit")?.id);
        }
        Ok(Ancestry { head: Some(head.id.to_string()), ids })
    };
    Some(read())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::Digest;

    fn file(path: &str, text: &str) -> File {
        File { path: path.into(), hash: sha2::Sha256::digest(text.as_bytes()).into(), text: text.into() }
    }

    fn git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(["-c", "core.autocrlf=false", "-c", "user.name=t", "-c", "user.email=t@example.org"])
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git runs");
        assert!(status.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&status.stderr));
    }

    fn write(dir: &Path, path: &str, text: &str) {
        let path = dir.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// The project in a folder of a repository (as a project inside a bigger one): files are
    /// compared as the loader reads them (LF), so one committed with CRLF is the commit's too; an
    /// edit or a file never committed isn't; the commit is HEAD's.
    #[test]
    fn files_are_compared_with_head() {
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path().join("integrations");
        write(&root, "interfaces/a/interface.yaml", "name: a\n");
        write(&root, "lib/codes.ts", "export const x = 1;\r\n");
        write(repo.path(), "README.md", "elsewhere\n");
        git(repo.path(), &["init", "-q"]);
        let files = [file("interfaces/a/interface.yaml", "name: a\n"), file("lib/codes.ts", "export const x = 1;\n")];
        // No commit yet: everything is uncommitted.
        let none = read(&root, &files).unwrap().unwrap();
        assert_eq!((none.id, none.uncommitted.len()), (None, 2));
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-qm", "first"]);
        let head = String::from_utf8(
            std::process::Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(repo.path())
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        let clean = read(&root, &files).unwrap().unwrap();
        assert_eq!(clean, Commit { id: Some(head.trim().into()), uncommitted: vec![] });

        // Edited, and a new file nobody committed.
        let edited = [file("interfaces/a/interface.yaml", "name: a\n"), file("lib/codes.ts", "export const x = 2;\n")];
        let new = file("lib/new.ts", "export const y = 1;\n");
        let found = read(&root, edited.iter().chain([&new])).unwrap().unwrap();
        assert_eq!(found.uncommitted, ["lib/codes.ts", "lib/new.ts"]);
    }

    fn head(dir: &Path) -> String {
        let out = std::process::Command::new("git").args(["rev-parse", "HEAD"]).current_dir(dir).output().unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    /// An interface's git history (0.7.3.3): the commits that changed the files asked about, in a
    /// project inside a bigger repository, newest first; a commit elsewhere isn't one; a file moved
    /// is its two paths (both asked about: every version's files); a branch's commit is the change,
    /// not the merge that brought it.
    #[test]
    fn history_lists_the_commits_that_changed_the_files() {
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path().join("integrations");
        let at = |n: i64| format!("@{} +0000", 1_800_000_000 + n * 60);
        let commit = |message: &str, n: i64| {
            let date = at(n);
            let status = std::process::Command::new("git")
                .args(["-c", "core.autocrlf=false", "-c", "user.name=Ann Example", "-c", "user.email=ann@example.org"])
                .args(["commit", "-qam", message])
                .env("GIT_AUTHOR_DATE", &date)
                .env("GIT_COMMITTER_DATE", &date)
                .current_dir(repo.path())
                .output()
                .unwrap();
            assert!(status.status.success(), "{}", String::from_utf8_lossy(&status.stderr));
        };
        git(repo.path(), &["init", "-q", "-b", "main"]);
        write(&root, "interfaces/a/interface.yaml", "name: a\n");
        write(&root, "lib/x.ts", "export const x = 1;\n");
        write(repo.path(), "README.md", "elsewhere\n");
        git(repo.path(), &["add", "."]);
        commit("first\n\nwith a body", 1);
        write(repo.path(), "README.md", "still elsewhere\n");
        commit("unrelated", 2);
        write(&root, "lib/x.ts", "export const x = 2;\n");
        commit("x is 2", 3);
        git(repo.path(), &["mv", "integrations/lib/x.ts", "integrations/lib/y.ts"]);
        commit("x moves to y", 4);
        // A branch changes y; main changes something else; the merge brings y's change.
        git(repo.path(), &["checkout", "-qb", "side"]);
        write(&root, "lib/y.ts", "export const x = 3;\n");
        commit("y on a branch", 5);
        git(repo.path(), &["checkout", "-q", "main"]);
        write(repo.path(), "README.md", "main moved on\n");
        commit("main moves on", 6);
        let date = at(7);
        let merged = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=Ann Example",
                "-c",
                "user.email=ann@example.org",
                "merge",
                "-q",
                "--no-ff",
                "side",
                "-m",
                "merge side",
            ])
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .current_dir(repo.path())
            .output()
            .unwrap();
        assert!(merged.status.success(), "{}", String::from_utf8_lossy(&merged.stderr));

        let paths: BTreeSet<String> =
            ["interfaces/a/interface.yaml", "lib/x.ts", "lib/y.ts"].into_iter().map(String::from).collect();
        let found = history(&root, &paths).unwrap().unwrap();
        let seen: Vec<(&str, Vec<(&str, How)>)> = found
            .iter()
            .map(|c| (c.summary.as_str(), c.files.iter().map(|(p, h)| (p.as_str(), *h)).collect()))
            .collect();
        assert_eq!(
            seen,
            [
                ("y on a branch", vec![("lib/y.ts", How::Modified)]),
                ("x moves to y", vec![("lib/x.ts", How::Deleted), ("lib/y.ts", How::Added)]),
                ("x is 2", vec![("lib/x.ts", How::Modified)]),
                ("first", vec![("interfaces/a/interface.yaml", How::Added), ("lib/x.ts", How::Added)]),
            ]
        );
        assert_eq!((found[0].author.as_str(), found[0].email.as_str()), ("Ann Example", "ann@example.org"));
        assert_eq!(found[3].at.timestamp(), 1_800_000_060);

        // HEAD's ancestry has every commit in it, the branch's too; not another repository's.
        let ancestry = ancestry(&root).unwrap().unwrap();
        assert_eq!(ancestry.head.as_deref(), Some(head(repo.path()).as_str()));
        assert!(found.iter().all(|c| ancestry.contains(&c.id)));
        assert!(!ancestry.contains("0123456789abcdef0123456789abcdef01234567"));
        assert!(!ancestry.contains("not hex"));
    }

    #[test]
    fn a_project_outside_any_repository_has_no_commit() {
        let dir = tempfile::tempdir().unwrap();
        // A temp folder may sit inside a repository on a developer's machine: only check when not.
        if dir.path().ancestors().any(|d| d.join(".git").exists()) {
            return;
        }
        assert!(read(dir.path(), [].iter()).is_none());
    }
}
