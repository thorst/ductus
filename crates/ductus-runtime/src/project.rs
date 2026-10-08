//! What a project's code uses, found without running it: the `db("...")`/`api("...")` calls
//! each interface's script reaches (through its imports, shared libraries included). Used by
//! `ductus validate`, `impact` and `doctor`, and by the REST API's reports.

use anyhow::{Context, Result};
use ductus_core::InterfaceConfig;
use ductus_core::impact::CodeUses;
use ductus_core::resources::Kind;
use ductus_script::Isolate;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Where `ductus types` writes the SDK's typings, relative to the project.
pub const TYPES_FILE: &str = ".ductus/types/ductus-sdk.d.ts";

/// Every file under `dir` (recursively) accepted by `keep`, skipping hidden folders and node_modules.
pub fn files_under(dir: &Path, keep: &dyn Fn(&Path) -> bool) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if !name.starts_with('.') && name != "node_modules" {
                out.extend(files_under(&path, keep));
            }
        } else if keep(&path) {
            out.push(path);
        }
    }
    out.sort();
    out
}

pub fn is_ts(p: &Path) -> bool {
    p.extension().is_some_and(|e| e == "ts") && !p.to_string_lossy().ends_with(".d.ts")
}

/// Forward-slash absolute path, the form the type checker uses.
pub fn key(path: &Path) -> String {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()).to_string_lossy().replace('\\', "/")
}

/// Add every .ts file in the project to `files`, so imports of shared libraries resolve, and
/// return the shared modules: files outside every interface folder (`interface_dirs`), one
/// instance per isolate, imported by many interfaces.
pub fn project_files(
    root: &Path,
    interface_dirs: &[&Path],
    files: &mut BTreeMap<String, String>,
) -> Result<Vec<String>> {
    let interface_dirs: Vec<String> = interface_dirs.iter().map(|d| format!("{}/", key(d))).collect();
    let mut shared = Vec::new();
    for f in files_under(root, &is_ts) {
        let k = key(&f);
        if !interface_dirs.iter().any(|d| k.starts_with(d)) {
            shared.push(k.clone());
        }
        files.insert(k, std::fs::read_to_string(&f).with_context(|| format!("reading {}", f.display()))?);
    }
    Ok(shared)
}

/// The `db()`/`api()` calls reachable from an interface's script (through its imports).
pub fn code_references<'a>(
    config: &InterfaceConfig,
    analysis: &'a ductus_script::Analysis,
) -> Vec<&'a ductus_script::ResourceReference> {
    config.script_path().map(|s| analysis.references_from(&key(&s))).unwrap_or_default()
}

/// The resource kind an SDK function names (`db` → database, `api` → API).
pub fn resource_kind(function: &str) -> Option<Kind> {
    match function {
        "db" => Some(Kind::Database),
        "api" => Some(Kind::Api),
        _ => None,
    }
}

/// The databases and APIs each interface's code uses. Parsing and name resolution only, no
/// type check, so it's quick. `interface_dirs` are every interface folder, loaded or not (a
/// file inside one is that interface's, not a shared library).
pub async fn code_uses(root: &Path, interface_dirs: &[&Path], configs: &[&InterfaceConfig]) -> Result<CodeUses> {
    let mut files = BTreeMap::new();
    files.insert(key(&root.join(TYPES_FILE)), Isolate::generate_types().await?);
    let shared = project_files(root, interface_dirs, &mut files)?;
    let analysis = ductus_script::analyze(&files, &shared, false)?;
    Ok(configs
        .iter()
        .map(|c| {
            let uses = code_references(c, &analysis)
                .into_iter()
                .filter_map(|r| Some((resource_kind(&r.kind)?, r.name.clone())))
                .collect();
            (c.name.clone(), uses)
        })
        .collect())
}
