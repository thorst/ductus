//! Code versions (0.7.3): what each interface runs, as one hash, computed and never typed. It
//! covers the interface's interface.yaml as written and every project file its code loaded (its
//! script and everything that imports, shared files too: the module loader records them as it
//! reads them, so the version is of exactly the text V8 got). Not the SDK: the binary's version is
//! recorded beside each deployment instead, so an upgrade doesn't make every interface "changed".
//!
//! The hash is SHA-256 over lines a person can recompute with `sha256sum`:
//!
//! ```text
//! ductus code version 1
//! interface adt-to-lab
//! <sha-256 of the file>  interfaces/adt-to-lab/interface.yaml
//! <sha-256 of the file>  interfaces/adt-to-lab/transform.ts
//! <sha-256 of the file>  lib/codes.ts
//! ```
//!
//! files in path order, paths relative to the project with `/`, each file's text without a byte
//! order mark and with `\n` line ends, so a Windows checkout and a Linux one of the same commit
//! have the same version. Shown as its first 12 hex characters.

use anyhow::{Context, Result};
use ductus_core::InterfaceConfig;
use ductus_script::CodeFile;
use sha2::{Digest, Sha256};
use std::path::Path;

/// How many hex characters of a version are shown.
pub const SHOWN: usize = 12;

/// One file of a code version.
#[derive(Debug, Clone)]
pub struct File {
    /// Relative to the project, with `/` (absolute when it's outside the project).
    pub path: String,
    /// SHA-256 of `text`.
    pub hash: [u8; 32],
    /// Normalized (`ductus_script::normalize`).
    pub text: String,
}

/// What an interface runs.
#[derive(Debug, Clone)]
pub struct Version {
    pub interface: String,
    pub hash: [u8; 32],
    /// In path order; its interface.yaml among them.
    pub files: Vec<File>,
}

impl Version {
    /// The version of `config`'s interface, whose code loaded `loaded` (`Isolate::code_files`; none
    /// without `script:`). `root` is the project's folder in the loader's form (`ductus_script::loaded_path`).
    pub fn of(root: &Path, config: &InterfaceConfig, loaded: &[CodeFile]) -> Result<Self> {
        let yaml = config.dir.join("interface.yaml");
        let text = std::fs::read_to_string(&yaml).with_context(|| format!("reading {}", yaml.display()))?;
        let text = ductus_script::normalize(&text);
        // Interfaces are `interfaces/<folder>/` of the project: no need to ask the file system.
        let folder = config.dir.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        let path = format!("interfaces/{folder}/interface.yaml");
        let mut files = vec![File { path, hash: Sha256::digest(&text).into(), text }];
        files.extend(loaded.iter().map(|f| File { path: relative(root, &f.path), hash: f.hash, text: f.text.clone() }));
        files.sort_by(|a, b| a.path.cmp(&b.path));
        files.dedup_by(|a, b| a.path == b.path);
        Ok(Self { interface: config.name.clone(), hash: hash(&config.name, &files), files })
    }

    /// The first `SHOWN` hex characters.
    pub fn short(&self) -> String {
        short(&self.hash)
    }
}

/// What `config`'s interface would run if the engine started now from the project at `root`: its
/// code loaded as a start loads it (no services: top-level code that calls a database fails here,
/// said), on a thread of its own. What `ductus history` adds to the files the recorded versions name.
pub fn current(root: &Path, config: &InterfaceConfig, heap_mb: usize) -> Result<Version> {
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
                let files = tokio::task::LocalSet::new().block_on(&runtime, async {
                    let Some(path) = config.script_path() else { return Ok(Vec::new()) };
                    let mut isolate = ductus_script::Isolate::new(None, heap_mb);
                    isolate.capture_console();
                    let functions = config.functions();
                    let functions: Vec<(&str, &str)> =
                        functions.iter().map(|(n, f)| (n.as_str(), f.as_str())).collect();
                    isolate
                        .add_functions(&path, &functions, config.timeout)
                        .await
                        .with_context(|| format!("loading `{}`'s code", config.name))?;
                    isolate.code_files(&path)
                })?;
                let root = ductus_script::loaded_path(root).with_context(|| format!("finding {}", root.display()))?;
                Version::of(&root, config, &files)
            })
            .join()
            .map_err(|_| anyhow::anyhow!("loading `{}`'s code panicked", config.name))?
    })
}

/// The version's hash: see the module's documentation.
fn hash(interface: &str, files: &[File]) -> [u8; 32] {
    let mut text = format!("ductus code version 1\ninterface {interface}\n");
    for f in files {
        text.push_str(&hex(&f.hash));
        text.push_str("  ");
        text.push_str(&f.path);
        text.push('\n');
    }
    Sha256::digest(text.as_bytes()).into()
}

/// `path` relative to `root` with `/`; absolute (with `/`) when it isn't under `root`.
fn relative(root: &Path, path: &Path) -> String {
    let parts: Vec<String> = match path.strip_prefix(root) {
        Ok(rest) => rest.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect(),
        Err(_) => return path.to_string_lossy().replace('\\', "/"),
    };
    parts.join("/")
}

/// Lower-case hex. (Thousands of hashes at a start: one allocation each, not one per byte.)
pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[usize::from(b >> 4)] as char);
        out.push(DIGITS[usize::from(b & 0xf)] as char);
    }
    out
}

/// A hash's first `SHOWN` hex characters.
pub fn short(hash: &[u8]) -> String {
    hex(&hash[..SHOWN / 2])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, text: &str) -> File {
        File { path: path.into(), hash: Sha256::digest(text.as_bytes()).into(), text: text.into() }
    }

    #[test]
    fn the_hash_is_the_documented_text() {
        let files = [file("interfaces/a/interface.yaml", "name: a\n"), file("interfaces/a/t.ts", "x\n")];
        let text = format!(
            "ductus code version 1\ninterface a\n{}  interfaces/a/interface.yaml\n{}  interfaces/a/t.ts\n",
            hex(&files[0].hash),
            hex(&files[1].hash)
        );
        let expected: [u8; 32] = Sha256::digest(text.as_bytes()).into();
        assert_eq!(hash("a", &files), expected);
        assert_eq!(short(&expected).len(), SHOWN);
    }

    #[test]
    fn paths_are_relative_with_slashes() {
        let root = Path::new("/p");
        assert_eq!(relative(root, Path::new("/p/interfaces/a/t.ts")), "interfaces/a/t.ts");
        assert_eq!(relative(root, Path::new("/elsewhere/x.ts")), "/elsewhere/x.ts");
    }
}
