//! The project's git commit, read from `.git` with gitoxide (nothing outside the binary: no `git`
//! needed on the server). A deployment records the commit and which of the files its interfaces run
//! aren't the commit's: changed since, or not in it. That's "clean" for code versions: a log file
//! nobody committed doesn't count, an edited transform does. Line ends don't count either (a
//! Windows checkout's CRLF is the commit's LF), as for code versions.

use crate::code::File;
use anyhow::{Context, Result};
use std::collections::BTreeMap;
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
    let workdir = repository.workdir().context("the repository has no working folder (a bare repository)")?;
    let workdir = std::fs::canonicalize(workdir).with_context(|| format!("finding {}", workdir.display()))?;
    let root = std::fs::canonicalize(root).with_context(|| format!("finding {}", root.display()))?;
    // The project's folder in the commit, read once (looking each file up from the top would read
    // the `interfaces` tree again for every file: thousands of times at a thousand interfaces).
    let project = slashes(root.strip_prefix(&workdir).unwrap_or(Path::new("")));
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
