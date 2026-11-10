//! Releases (0.7.4.3.2): one exact, checksummed artifact of a project, made by `ductus release` and
//! checked against an environment by `ductus release check` before anything deploys it.
//!
//! A release is a gzipped tar: `release.json` (the manifest) first, then `ancestry` (the commit's
//! whole history, 20 bytes a commit, sorted), then every file it carries under `project/`. Files
//! come from git's object store (HEAD's tree, at the project's folder when the project is inside a
//! bigger repository), never from `git archive` or the working folder: on a Windows checkout with
//! `core.autocrlf` both give CRLF, and the same commit must give the same release anywhere. Texts
//! are carried as code versions read them (no byte order mark, `\n` line ends); a file that isn't
//! text (a NUL byte, or not UTF-8) is carried byte for byte and listed in the manifest.
//!
//! **The release id** is a hash of what it carries, never of the archive's bytes (gzip's and tar's
//! bytes may differ between builds of the same files): SHA-256 over
//!
//! ```text
//! ductus release 1
//! <sha-256 of the file>  interfaces/adt-to-lab/interface.yaml
//! <sha-256 of the file>  lib/codes.ts
//! ```
//!
//! every file in path order, paths relative to the project with `/`. So the same commit always
//! gives the same id, whoever makes it and wherever, and anyone can recompute it from the files.
//! The manifest is not in the id: two releases of one commit with different reasons are one
//! release made twice.

use crate::code;
use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// The manifest's name in the archive: the first entry.
pub const MANIFEST: &str = "release.json";
/// The commit's ancestry in the archive: 20-byte commit ids, sorted.
pub const ANCESTRY: &str = "ancestry";
/// Where the project's files are in the archive.
pub const FILES: &str = "project/";
/// The manifest's format; a later version that reads a release only adds fields.
pub const FORMAT: u32 = 1;
/// What a release may hold, unpacked: a project is kilobytes to a few megabytes (1,001 interfaces
/// with tests and fixtures: 6.2 MB as tar), so this only stops a damaged or hostile file.
const MAX_BYTES: u64 = 1 << 30;

/// One file a release carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    /// Relative to the project, with `/`.
    pub path: String,
    /// A text normalized as code versions are (`ductus_script::normalize`), else the bytes as they are.
    pub bytes: Vec<u8>,
    /// Not text: a NUL byte, or not UTF-8.
    pub binary: bool,
}

impl File {
    /// `raw` as a release carries it: text normalized, anything else as it is.
    pub fn new(path: impl Into<String>, raw: Vec<u8>) -> Self {
        let path = path.into();
        if raw.contains(&0) {
            return Self { path, bytes: raw, binary: true };
        }
        match String::from_utf8(raw) {
            Ok(text) => Self { path, bytes: ductus_script::normalize(&text).into_bytes(), binary: false },
            Err(e) => Self { path, bytes: e.into_bytes(), binary: true },
        }
    }

    pub fn hash(&self) -> [u8; 32] {
        Sha256::digest(&self.bytes).into()
    }
}

/// The release id of `files`: see the module's documentation.
pub fn id(files: &[File]) -> [u8; 32] {
    let mut sorted: Vec<&File> = files.iter().collect();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));
    let mut text = String::from("ductus release 1\n");
    for f in sorted {
        text.push_str(&code::hex(&f.hash()));
        text.push_str("  ");
        text.push_str(&f.path);
        text.push('\n');
    }
    Sha256::digest(text.as_bytes()).into()
}

/// The project version of `files` (`code::project_version`) and the files it covers.
pub fn project_version(files: &[File]) -> ([u8; 32], Vec<String>) {
    let hashes: Vec<(&str, [u8; 32])> = files.iter().map(|f| (f.path.as_str(), f.hash())).collect();
    code::project_version(hashes.iter().map(|(p, h)| (*p, h)))
}

/// What `pick` found: the files a release of the project carries, and where they came from.
#[derive(Debug)]
pub struct Picked {
    /// In path order.
    pub files: Vec<File>,
    /// None without git.
    pub git: Option<GitSource>,
    /// Files that aren't HEAD's: changed, deleted, added but not committed, or never added and not
    /// ignored (in path order). Carried as they are in the folder only when asked (`uncommitted`).
    pub uncommitted: Vec<String>,
    /// Things a release can't carry (a symbolic link, a submodule), each said in words.
    pub refused: Vec<String>,
}

/// Where a release's files came from in git.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GitSource {
    /// HEAD's commit, in hex.
    pub commit: String,
    /// The project's folder in the repository, with `/` ("" at its top).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub folder: String,
    /// How many commits `ancestry` holds (HEAD's whole history, HEAD included).
    pub commits: usize,
    /// The commits, 20 bytes each, sorted: not in the manifest's JSON (its own archive entry).
    #[serde(skip)]
    pub ancestry: Vec<u8>,
}

/// The files a release of the project at `root` carries. In a git repository: HEAD's tree at the
/// project's folder, or, with `uncommitted`, the folder's files as they are where they aren't
/// HEAD's (changes, files added or new and not ignored; deleted ones left out). Without git: every
/// file in the folder but `.ductus/` (the engine's own, and where releases are written).
pub fn pick(root: &Path, uncommitted: bool) -> Result<Picked> {
    let repository = match gix::discover(root) {
        Ok(r) => r,
        Err(e) if root.ancestors().any(|d| d.join(".git").exists()) => {
            return Err(anyhow::Error::new(e).context("reading the project's git repository"));
        }
        Err(_) => {
            let mut files = Vec::new();
            folder_files(root, Path::new(""), &mut files)?;
            files.sort_by(|a, b| a.path.cmp(&b.path));
            return Ok(Picked { files, git: None, uncommitted: Vec::new(), refused: Vec::new() });
        }
    };
    from_git(&repository, root, uncommitted)
}

/// Every file under `dir` (relative to the project: `at`), but `.ductus/` and `.git`.
fn folder_files(root: &Path, at: &Path, out: &mut Vec<File>) -> Result<()> {
    let dir = root.join(at);
    let entries = std::fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("reading {}", dir.display()))?;
        let name = entry.file_name();
        if at.as_os_str().is_empty() && (name == ".ductus" || name == ".git") {
            continue;
        }
        let path = at.join(&name);
        if entry.file_type()?.is_dir() {
            folder_files(root, &path, out)?;
        } else {
            let raw = std::fs::read(root.join(&path)).with_context(|| format!("reading {}", path.display()))?;
            out.push(File::new(slashes(&path), raw));
        }
    }
    Ok(())
}

fn slashes(path: &Path) -> String {
    path.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/")
}

fn from_git(repository: &gix::Repository, root: &Path, uncommitted: bool) -> Result<Picked> {
    let folder = crate::git::project_prefix(repository, root)?;
    let in_repository = |path: &str| if folder.is_empty() { path.to_string() } else { format!("{folder}/{path}") };
    let mut refused = Vec::new();

    // HEAD's files at the project's folder (none before the first commit).
    let mut head: BTreeMap<String, gix::ObjectId> = BTreeMap::new();
    let mut git = None;
    if let Ok(commit) = repository.head_commit() {
        let tree = commit.tree().context("reading HEAD's tree")?;
        let project = if folder.is_empty() {
            Some(tree)
        } else {
            match tree.lookup_entry_by_path(&folder).context("finding the project in HEAD")? {
                Some(entry) => Some(entry.object().context("reading the project's tree")?.peel_to_tree()?),
                None => None,
            }
        };
        if let Some(tree) = project {
            let mut recorder = gix::traverse::tree::Recorder::default();
            tree.traverse().breadthfirst(&mut recorder).context("reading the project's files in HEAD")?;
            for entry in recorder.records {
                let path = entry.filepath.to_string();
                use gix::objs::tree::EntryKind;
                match entry.mode.kind() {
                    EntryKind::Tree => {}
                    EntryKind::Blob | EntryKind::BlobExecutable => {
                        head.insert(path, entry.oid);
                    }
                    EntryKind::Link => refused.push(format!(
                        "{path} is a symbolic link: a release carries files only (commit the file it points to instead)"
                    )),
                    EntryKind::Commit => refused
                        .push(format!("{path} is a git submodule: a release carries the project's own files only")),
                }
            }
        }
        git = Some(GitSource {
            commit: commit.id.to_string(),
            folder: folder.clone(),
            commits: 0,
            ancestry: ancestry(repository, commit.id)?,
        });
        if let Some(g) = git.as_mut() {
            g.commits = g.ancestry.len() / 20;
        }
    }

    // What git tracks now (the index) and what's ignored, to tell new files from ignored ones.
    let index = repository.index_or_empty().context("reading git's index")?;
    let tracked: BTreeSet<String> = index
        .entries()
        .iter()
        .filter_map(|e| {
            let path = e.path(&index).to_string();
            match folder.is_empty() {
                true => Some(path),
                false => path.strip_prefix(&format!("{folder}/")).map(str::to_string),
            }
        })
        .collect();
    let mut on_disk = Vec::new();
    let worktree = repository.worktree().context("the repository has no working folder (a bare repository)")?;
    let mut excludes = worktree.excludes(None).context("reading git's ignore rules")?;
    working_files(root, Path::new(""), &in_repository, &mut excludes, &tracked, &head, &mut on_disk)?;
    let on_disk: BTreeSet<String> = on_disk.into_iter().collect();

    let mut files = Vec::new();
    let mut changed = Vec::new();
    // HEAD's files: as committed, unless changed in the folder.
    for (path, oid) in &head {
        let blob = repository.find_object(*oid).with_context(|| format!("reading {path} in HEAD"))?;
        let committed = File::new(path.clone(), blob.data.clone());
        let now = match std::fs::read(root.join(path)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None, // deleted in the folder
            Err(e) => return Err(anyhow::Error::new(e).context(format!("reading {path}"))),
            Ok(raw) => Some(File::new(path.clone(), raw)),
        };
        let same = now.as_ref().is_some_and(|n| n.bytes == committed.bytes);
        if !same {
            changed.push(path.clone());
        }
        match (uncommitted && !same, now) {
            (true, Some(now)) => files.push(now),
            (true, None) => {} // deleted, and the folder is what's asked for
            (false, _) => files.push(committed),
        }
    }
    // Files in the folder that HEAD doesn't have: added (tracked) or new (not ignored).
    for path in on_disk.iter().filter(|p| !head.contains_key(*p)) {
        changed.push(path.clone());
        if uncommitted {
            let raw = std::fs::read(root.join(path)).with_context(|| format!("reading {path}"))?;
            files.push(File::new(path.clone(), raw));
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    changed.sort();
    changed.dedup();
    Ok(Picked { files, git, uncommitted: changed, refused })
}

/// The project's files in the folder that git tracks or would (not ignored), relative to the
/// project, but `.ductus/` and anything in a `.git`.
fn working_files(
    root: &Path,
    at: &Path,
    in_repository: &dyn Fn(&str) -> String,
    excludes: &mut gix::AttributeStack<'_>,
    tracked: &BTreeSet<String>,
    head: &BTreeMap<String, gix::ObjectId>,
    out: &mut Vec<String>,
) -> Result<()> {
    let dir = root.join(at);
    let Ok(entries) = std::fs::read_dir(&dir) else { return Ok(()) };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name == ".git" || (at.as_os_str().is_empty() && name == ".ductus") {
            continue;
        }
        let path = at.join(&name);
        let relative = slashes(&path);
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() {
            let ignored = excludes
                .at_path(in_repository(&relative), Some(gix::index::entry::Mode::DIR))
                .is_ok_and(|p| p.is_excluded());
            // A tracked file can live in an ignored folder (added with `git add -f`).
            let inside = format!("{relative}/");
            let holds_tracked = tracked.range(inside.clone()..).next().is_some_and(|t| t.starts_with(&inside))
                || head.range(inside.clone()..).next().is_some_and(|(t, _)| t.starts_with(&inside));
            if !ignored || holds_tracked {
                working_files(root, &path, in_repository, excludes, tracked, head, out)?;
            }
            continue;
        }
        if tracked.contains(&relative) || head.contains_key(&relative) {
            out.push(relative);
            continue;
        }
        let ignored = excludes
            .at_path(in_repository(&relative), Some(gix::index::entry::Mode::FILE))
            .is_ok_and(|p| p.is_excluded());
        if !ignored {
            out.push(relative);
        }
    }
    Ok(())
}

/// Every commit `head` descends from (itself included), 20 bytes each, sorted.
fn ancestry(repository: &gix::Repository, head: gix::ObjectId) -> Result<Vec<u8>> {
    let mut ids = Vec::new();
    for info in repository.rev_walk([head]).all().context("walking the commits")? {
        ids.push(info.context("reading a commit")?.id);
    }
    ids.sort();
    let mut bytes = Vec::with_capacity(ids.len() * 20);
    for id in ids {
        bytes.extend_from_slice(id.as_bytes());
    }
    Ok(bytes)
}

/// Whether `commit` (hex) is in `ancestry` (`GitSource::ancestry`'s bytes).
pub fn in_ancestry(ancestry: &[u8], commit: &str) -> bool {
    let Ok(id) = gix::ObjectId::from_hex(commit.as_bytes()) else { return false };
    let (ids, _) = ancestry.as_chunks::<20>();
    ids.binary_search_by(|c| c.as_slice().cmp(id.as_bytes())).is_ok()
}

/// A text file holding a private key (PEM), or a file named as key stores are: a release would
/// carry it in the clear (and, once stored, into the database).
pub fn private_key(file: &File) -> bool {
    let named = [".key", ".p12", ".pfx", ".jks"].iter().any(|e| file.path.to_ascii_lowercase().ends_with(e));
    let pem = !file.binary && {
        let text = String::from_utf8_lossy(&file.bytes);
        text.contains("-----BEGIN") && text.contains("PRIVATE KEY-----")
    };
    named || pem
}

/// The manifest: `release.json`, the archive's first entry. Later versions only add fields.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Manifest {
    pub format: u32,
    /// The release id, in hex (see the module's documentation): recomputed from the files by
    /// whoever reads it.
    pub id: String,
    /// The project's folder name.
    pub project: String,
    pub made: Made,
    /// None without git.
    #[serde(default)]
    pub git: Option<GitSource>,
    /// Files carried as they were in the folder, not as committed (`--uncommitted`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uncommitted: Vec<String>,
    /// The release this one was made over (`--over`): what a deploy without git holds it against.
    #[serde(default)]
    pub over: Option<String>,
    pub project_version: ProjectVersion,
    /// Each interface's code version, by name.
    pub interfaces: BTreeMap<String, Interface>,
    pub tests: Tests,
    /// How many files it carries, and their bytes (unpacked).
    pub files: usize,
    pub bytes: u64,
    /// Files carried byte for byte (not text).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub binary: Vec<String>,
}

/// Who made a release, when, why, and with which binary.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Made {
    /// RFC 3339, UTC.
    pub at: String,
    pub by: String,
    pub host: String,
    #[serde(default)]
    pub reason: Option<String>,
    /// The binary's version (`ductus --version`) and the system it ran on (`linux-x86_64`).
    pub binary: String,
    pub platform: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ProjectVersion {
    /// In hex.
    pub version: String,
    /// The files it covers.
    pub files: Vec<String>,
}

/// An interface's code version (`code::Version`) as the manifest has it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Interface {
    /// In hex.
    pub version: String,
    /// Its files, relative to the project.
    pub files: Vec<String>,
    pub parts: code::Parts,
}

/// The tests' result when the release was made.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Tests {
    pub ran: bool,
    /// The environment they ran in.
    #[serde(default)]
    pub environment: Option<String>,
    #[serde(default)]
    pub passed: u64,
    #[serde(default)]
    pub failed: u64,
    #[serde(default)]
    pub ms: u64,
}

/// A release read back: its manifest, the commits it descends from, and its files.
#[derive(Debug)]
pub struct Bundle {
    pub manifest: Manifest,
    pub ancestry: Vec<u8>,
    pub files: Vec<File>,
}

/// Write the release to `path`: a gzipped tar, the manifest first, the same bytes for the same
/// contents (every header's time, owner and mode fixed). Returns the file's size.
pub fn write(path: &Path, manifest: &Manifest, ancestry: &[u8], files: &[File]) -> Result<u64> {
    let out = std::fs::File::create(path).with_context(|| format!("writing {}", path.display()))?;
    let gz = flate2::GzBuilder::new().mtime(0).write(std::io::BufWriter::new(out), flate2::Compression::best());
    let mut tar = tar::Builder::new(gz);
    let mut add = |name: &str, bytes: &[u8]| -> Result<()> {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        tar.append_data(&mut header, name, bytes).with_context(|| format!("adding {name}"))
    };
    // Compact: at a thousand interfaces the manifest is the archive's biggest entry (pretty, 558
    // KB; compact, measured below in architecture.md). `ductus release --json` prints it pretty.
    let mut json = serde_json::to_vec(manifest)?;
    json.push(b'\n');
    add(MANIFEST, &json)?;
    add(ANCESTRY, ancestry)?;
    let mut sorted: Vec<&File> = files.iter().collect();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));
    for f in sorted {
        add(&format!("{FILES}{}", f.path), &f.bytes)?;
    }
    let gz = tar.into_inner().context("finishing the archive")?;
    let mut buffered = gz.finish().context("finishing the archive")?;
    buffered.flush().with_context(|| format!("writing {}", path.display()))?;
    Ok(std::fs::metadata(path)?.len())
}

/// Read a release and check it: the manifest first, every path a plain relative one, and the id
/// recomputed from the files equal to the manifest's (else it was damaged or changed).
pub fn read(path: &Path) -> Result<Bundle> {
    let file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(std::io::BufReader::new(file)));
    let mut manifest: Option<Manifest> = None;
    let mut ancestry = Vec::new();
    let mut files = Vec::new();
    let mut seen = BTreeSet::new();
    let mut total = 0u64;
    let not_a_release = || format!("{} isn't a Ductus release (made by `ductus release`)", path.display());
    for (n, entry) in archive.entries().with_context(not_a_release)?.enumerate() {
        let mut entry = entry.with_context(not_a_release)?;
        let name = entry.path().with_context(not_a_release)?.to_string_lossy().replace('\\', "/");
        // A folder carries nothing (`ductus release` writes none; `tar` does, packing one again).
        if entry.header().entry_type() == tar::EntryType::Directory && n > 0 {
            continue;
        }
        if entry.header().entry_type() != tar::EntryType::Regular {
            bail!("{}: `{name}` isn't a plain file", path.display());
        }
        total += entry.size();
        if total > MAX_BYTES {
            bail!("{}: more than {} MB unpacked: not a release this version reads", path.display(), MAX_BYTES >> 20);
        }
        let mut bytes = Vec::with_capacity(entry.size() as usize);
        entry.read_to_end(&mut bytes).with_context(|| format!("reading `{name}` in {}", path.display()))?;
        if n == 0 {
            if name != MANIFEST {
                bail!("{}", not_a_release());
            }
            let m: Manifest = serde_json::from_slice(&bytes)
                .with_context(|| format!("{}: reading its manifest ({MANIFEST})", path.display()))?;
            if m.format > FORMAT {
                bail!(
                    "{}: release format {} is newer than this version reads ({FORMAT}): check it with the version that made it ({})",
                    path.display(),
                    m.format,
                    m.made.binary
                );
            }
            manifest = Some(m);
        } else if name == ANCESTRY {
            ancestry = bytes;
        } else if let Some(relative) = name.strip_prefix(FILES) {
            if !plain(relative) || !seen.insert(relative.to_string()) {
                bail!("{}: `{name}` isn't a path a release carries (or is there twice)", path.display());
            }
            files.push(File::new(relative, bytes));
        } else {
            bail!("{}: unexpected `{name}` (not a release this version reads)", path.display());
        }
    }
    let manifest = manifest.with_context(not_a_release)?;
    let recomputed = code::hex(&id(&files));
    if recomputed != manifest.id {
        bail!(
            "{}: its files don't match its id (the manifest says {}, the files are {}): the file was damaged or changed after it was made",
            path.display(),
            short(&manifest.id),
            short(&recomputed)
        );
    }
    Ok(Bundle { manifest, ancestry, files })
}

/// A relative path of plain names with `/` (no `..`, `.`, empty part, drive or backslash).
fn plain(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\\')
        && !path.contains(':')
        && path.split('/').all(|part| !part.is_empty() && part != "." && part != "..")
}

/// The first `code::SHOWN` characters of an id in hex.
pub fn short(id: &str) -> &str {
    &id[..id.len().min(code::SHOWN)]
}

/// Write `files` into `dir` (made if missing), as the project they came from.
pub fn unpack(files: &[File], dir: &Path) -> Result<()> {
    for f in files {
        if !plain(&f.path) {
            bail!("`{}` isn't a path a release carries", f.path);
        }
        let path: PathBuf = f.path.split('/').fold(dir.to_path_buf(), |p, part| p.join(part));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("making {}", parent.display()))?;
        }
        std::fs::write(&path, &f.bytes).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texts_are_normalized_and_other_files_kept_as_they_are() {
        let text = File::new("a.ts", b"\xef\xbb\xbfexport const x = 1;\r\n".to_vec());
        assert_eq!((text.bytes.as_slice(), text.binary), (b"export const x = 1;\n".as_slice(), false));
        let png = File::new("logo.png", b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec());
        assert!(png.binary);
        assert_eq!(png.bytes, b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR");
        let latin1 = File::new("old.csv", b"caf\xe9\r\n".to_vec());
        assert!(latin1.binary, "not UTF-8: carried as it is");
        // HL7's segment separator is a lone CR: only CRLF pairs become LF.
        let hl7 = File::new("fixtures/adt.hl7", b"MSH|^~\\&\rPID|1\r".to_vec());
        assert_eq!(hl7.bytes, b"MSH|^~\\&\rPID|1\r");
    }

    #[test]
    fn the_id_is_the_documented_text_and_ignores_order_and_line_ends() {
        let files =
            [File::new("lib/b.ts", b"b\n".to_vec()), File::new("interfaces/a/interface.yaml", b"name: a\n".to_vec())];
        let text = format!(
            "ductus release 1\n{}  interfaces/a/interface.yaml\n{}  lib/b.ts\n",
            code::hex(&files[1].hash()),
            code::hex(&files[0].hash())
        );
        assert_eq!(id(&files), <[u8; 32]>::from(Sha256::digest(text.as_bytes())));
        let windows = [
            File::new("interfaces/a/interface.yaml", b"name: a\r\n".to_vec()),
            File::new("lib/b.ts", b"b\r\n".to_vec()),
        ];
        assert_eq!(id(&files), id(&windows));
        let changed = [File::new("lib/b.ts", b"c\n".to_vec()), files[1].clone()];
        assert_ne!(id(&files), id(&changed));
    }

    fn manifest(files: &[File]) -> Manifest {
        let (version, own) = project_version(files);
        Manifest {
            format: FORMAT,
            id: code::hex(&id(files)),
            project: "p".into(),
            made: Made {
                at: "2026-11-10T00:00:00Z".into(),
                by: "ann".into(),
                host: "h".into(),
                reason: Some("why".into()),
                binary: "0.7.0".into(),
                platform: "linux-x86_64".into(),
            },
            git: None,
            uncommitted: Vec::new(),
            over: None,
            project_version: ProjectVersion { version: code::hex(&version), files: own },
            interfaces: BTreeMap::new(),
            tests: Tests::default(),
            files: files.len(),
            bytes: files.iter().map(|f| f.bytes.len() as u64).sum(),
            binary: Vec::new(),
        }
    }

    /// Written twice, the same bytes; read back, the same files and manifest; a long path (over
    /// tar's 100 characters) survives.
    #[test]
    fn a_release_is_the_same_bytes_every_time_and_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let long = format!("interfaces/{}/fixtures/{}.hl7", "a".repeat(60), "b".repeat(60));
        let files = vec![
            File::new("ductus.yaml", b"{}\n".to_vec()),
            File::new(long.clone(), b"MSH\r".to_vec()),
            File::new("x.bin", vec![0, 1, 2]),
        ];
        let m = manifest(&files);
        let ancestry = [7u8; 40];
        let (a, b) = (dir.path().join("a.tar.gz"), dir.path().join("b.tar.gz"));
        write(&a, &m, &ancestry, &files).unwrap();
        write(&b, &m, &ancestry, &files).unwrap();
        assert_eq!(std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap());
        let back = read(&a).unwrap();
        assert_eq!(back.manifest, m);
        assert_eq!(back.ancestry, ancestry);
        assert_eq!(back.files, files);

        // Unpacked, the project's files as they were.
        let out = dir.path().join("unpacked");
        unpack(&back.files, &out).unwrap();
        assert_eq!(std::fs::read(out.join(&long)).unwrap(), b"MSH\r");
    }

    #[test]
    fn a_changed_file_or_a_strange_path_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let files = vec![File::new("a.ts", b"a\n".to_vec())];
        let mut m = manifest(&files);
        m.id = code::hex(&id(&[File::new("a.ts", b"b\n".to_vec())]));
        let path = dir.path().join("r.tar.gz");
        write(&path, &m, &[], &files).unwrap();
        let e = read(&path).unwrap_err().to_string();
        assert!(e.contains("don't match its id"), "{e}");

        for bad in ["../x", "/etc/x", "a//b", "a/./b", "c:/x", "a\\b", ""] {
            assert!(!plain(bad), "{bad}");
        }
        assert!(plain("interfaces/a/b.ts"));
        assert!(unpack(&[File::new("../x", b"x".to_vec())], dir.path()).is_err());

        // Packed again by `tar` (folders as entries of their own): read, and the edit found.
        let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast()));
        let mut add = |name: &str, bytes: &[u8], kind: tar::EntryType| {
            let mut h = tar::Header::new_gnu();
            h.set_entry_type(kind);
            h.set_size(bytes.len() as u64);
            h.set_mode(0o755);
            tar.append_data(&mut h, name, bytes).unwrap();
        };
        let json = serde_json::to_vec(&manifest(&files)).unwrap();
        add(MANIFEST, &json, tar::EntryType::Regular);
        add("project/", b"", tar::EntryType::Directory);
        add("project/a.ts", b"edited\n", tar::EntryType::Regular);
        let packed = dir.path().join("packed.tar.gz");
        std::fs::write(&packed, tar.into_inner().unwrap().finish().unwrap()).unwrap();
        let e = read(&packed).unwrap_err().to_string();
        assert!(e.contains("don't match its id"), "{e}");

        std::fs::write(dir.path().join("not.tar.gz"), b"hello").unwrap();
        assert!(read(&dir.path().join("not.tar.gz")).is_err());
    }

    #[test]
    fn private_keys_are_found_by_content_or_name() {
        let pem =
            File::new("certs/client.pem", b"-----BEGIN PRIVATE KEY-----\nMII\n-----END PRIVATE KEY-----\n".to_vec());
        let rsa = File::new("x.txt", b"-----BEGIN RSA PRIVATE KEY-----\n".to_vec());
        let cert = File::new("certs/ca.pem", b"-----BEGIN CERTIFICATE-----\nMII\n".to_vec());
        assert!(private_key(&pem) && private_key(&rsa));
        assert!(!private_key(&cert));
        assert!(private_key(&File::new("store.P12", vec![0, 1])));
    }

    fn git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(["-c", "core.autocrlf=false", "-c", "user.name=t", "-c", "user.email=t@example.org"])
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git runs");
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    fn put(dir: &Path, path: &str, text: &[u8]) {
        let path = dir.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn paths(files: &[File]) -> Vec<&str> {
        files.iter().map(|f| f.path.as_str()).collect()
    }

    /// A project inside a bigger repository: HEAD's files at its folder; ignored files, `.ductus/`
    /// and the rest of the repository aren't the release's; a clone checked out with
    /// `core.autocrlf=true` (CRLF in every file, as Windows has it) is the same release; changed,
    /// added and new files are uncommitted, carried only when asked.
    #[test]
    fn a_release_is_head_s_files_at_the_project_s_folder() {
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path().join("integrations");
        put(&root, "interfaces/a/interface.yaml", b"name: a\n");
        put(&root, "lib/x.ts", b"export const x = 1;\n");
        put(&root, ".gitignore", b"out/\n*.key\n");
        put(&root, "notes.md", b"notes\n");
        put(repo.path(), "README.md", b"elsewhere\n");
        git(repo.path(), &["init", "-q", "-b", "main"]);
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-qm", "first"]);
        put(&root, "out/archived.hl7", b"MSH\r");
        put(&root, "client.key", b"-----BEGIN PRIVATE KEY-----\n");
        put(&root, ".ductus/releases/old.tar.gz", b"\x1f\x8b");

        let picked = pick(&root, false).unwrap();
        assert_eq!(paths(&picked.files), [".gitignore", "interfaces/a/interface.yaml", "lib/x.ts", "notes.md"]);
        assert!(picked.uncommitted.is_empty() && picked.refused.is_empty(), "{picked:?}");
        let git_source = picked.git.as_ref().unwrap();
        assert_eq!(
            (git_source.folder.as_str(), git_source.commits, git_source.ancestry.len()),
            ("integrations", 1, 20)
        );
        assert!(in_ancestry(&git_source.ancestry, &git_source.commit));

        // The same commit checked out as Windows does: CRLF in the folder, the same release.
        let windows = tempfile::tempdir().unwrap();
        let out = std::process::Command::new("git")
            .args(["-c", "core.autocrlf=true", "clone", "-q"])
            .arg(repo.path())
            .arg(windows.path().join("clone"))
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let clone = windows.path().join("clone").join("integrations");
        git(&windows.path().join("clone"), &["config", "core.autocrlf", "true"]);
        assert!(std::fs::read(clone.join("lib/x.ts")).unwrap().ends_with(b"\r\n"), "the clone has CRLF");
        let cloned = pick(&clone, false).unwrap();
        assert!(cloned.uncommitted.is_empty(), "{:?}", cloned.uncommitted);
        assert_eq!(id(&cloned.files), id(&picked.files));

        // Changed, added (staged) and new: uncommitted; the release still HEAD's unless asked.
        put(&root, "lib/x.ts", b"export const x = 2;\n");
        put(&root, "interfaces/a/t.ts", b"export function transform() {}\n");
        git(repo.path(), &["add", "integrations/interfaces/a/t.ts"]);
        put(&root, "lib/new.ts", b"export const y = 1;\n");
        std::fs::remove_file(root.join("notes.md")).unwrap();
        let picked = pick(&root, false).unwrap();
        assert_eq!(picked.uncommitted, ["interfaces/a/t.ts", "lib/new.ts", "lib/x.ts", "notes.md"]);
        assert_eq!(paths(&picked.files), [".gitignore", "interfaces/a/interface.yaml", "lib/x.ts", "notes.md"]);
        assert_eq!(picked.files[2].bytes, b"export const x = 1;\n");
        let carried = pick(&root, true).unwrap();
        assert_eq!(carried.uncommitted, picked.uncommitted);
        // The deleted notes.md isn't carried; the rest as they are in the folder.
        assert_eq!(
            paths(&carried.files),
            [".gitignore", "interfaces/a/interface.yaml", "interfaces/a/t.ts", "lib/new.ts", "lib/x.ts"]
        );
        assert_eq!(carried.files[4].bytes, b"export const x = 2;\n");
    }

    #[test]
    fn without_git_the_folder_is_the_release() {
        let dir = tempfile::tempdir().unwrap();
        if dir.path().ancestors().any(|d| d.join(".git").exists()) {
            return; // a temp folder inside a repository on a developer's machine
        }
        put(dir.path(), "interfaces/a/interface.yaml", b"name: a\r\n");
        put(dir.path(), ".ductus/types.d.ts", b"x");
        let picked = pick(dir.path(), false).unwrap();
        assert!(picked.git.is_none());
        assert_eq!(paths(&picked.files), ["interfaces/a/interface.yaml"]);
        assert_eq!(picked.files[0].bytes, b"name: a\n");
    }

    #[test]
    fn ancestry_is_searched_sorted() {
        let a = gix::ObjectId::from_hex(b"0123456789abcdef0123456789abcdef01234567").unwrap();
        let b = gix::ObjectId::from_hex(b"f123456789abcdef0123456789abcdef01234567").unwrap();
        let bytes = [a.as_bytes(), b.as_bytes()].concat();
        assert!(in_ancestry(&bytes, &a.to_string()) && in_ancestry(&bytes, &b.to_string()));
        assert!(!in_ancestry(&bytes, "1123456789abcdef0123456789abcdef01234567"));
        assert!(!in_ancestry(&bytes, "nope"));
    }
}
