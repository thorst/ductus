//! Settings that still work but are going away, and where a project uses them (`ductus upgrade
//! --check`, so the warning comes before the version that drops one, not with it).
//!
//! To deprecate a setting: keep accepting it, add a row to `DEPRECATED` with the version that
//! deprecated it and what to write instead, and say so in the release notes. It is removed only
//! at a major version (required stops, from v1.0), after at least one release with the row.

use anyhow::{Context, Result};
use serde_yaml_ng::Value;
use std::path::{Path, PathBuf};

/// The project files settings live in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum File {
    /// `ductus.yaml`
    Settings,
    /// `resources.yaml`
    Resources,
    /// Every `interfaces/*/interface.yaml`
    Interface,
}

/// A setting that still works but is going away.
#[derive(Debug, Clone, Copy)]
pub struct Deprecated {
    pub file: File,
    /// Its place in the file, by key: `monitor.silent_after`; `*` is any key of a map or any item
    /// of a list (`databases.*.pool`).
    pub path: &'static str,
    /// The version that deprecated it.
    pub since: &'static str,
    /// What to write instead.
    pub instead: &'static str,
}

/// Every deprecated setting. None yet.
pub const DEPRECATED: &[Deprecated] = &[];

/// A deprecated setting a project uses.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    /// The file, relative to the project.
    pub file: PathBuf,
    /// Where in it, with the keys `*` stood for (`databases.billing.pool`).
    pub at: String,
    pub since: &'static str,
    pub instead: &'static str,
}

/// Where the project at `root` uses any setting in `table` (`DEPRECATED`, or a test's).
pub fn find(table: &[Deprecated], root: &Path) -> Result<Vec<Found>> {
    let mut found = Vec::new();
    if table.is_empty() {
        return Ok(found);
    }
    let mut files =
        vec![(File::Settings, PathBuf::from("ductus.yaml")), (File::Resources, PathBuf::from("resources.yaml"))];
    if let Ok(dirs) = std::fs::read_dir(root.join("interfaces")) {
        let mut interfaces: Vec<PathBuf> = dirs
            .filter_map(|e| e.ok())
            .map(|e| Path::new("interfaces").join(e.file_name()).join("interface.yaml"))
            .filter(|p| root.join(p).is_file())
            .collect();
        interfaces.sort();
        files.extend(interfaces.into_iter().map(|p| (File::Interface, p)));
    }
    for (kind, file) in files {
        let rows: Vec<&Deprecated> = table.iter().filter(|d| d.file == kind).collect();
        let full = root.join(&file);
        if rows.is_empty() || !full.is_file() {
            continue;
        }
        let text = std::fs::read_to_string(&full).with_context(|| format!("reading {}", full.display()))?;
        let yaml = crate::env::parse_yaml(&text).with_context(|| format!("reading {}", full.display()))?;
        for d in rows {
            let keys: Vec<&str> = d.path.split('.').collect();
            for at in matches(&yaml, &keys, String::new()) {
                found.push(Found { file: file.clone(), at, since: d.since, instead: d.instead });
            }
        }
    }
    Ok(found)
}

/// The places in `value` that `keys` lead to, written with the keys taken.
fn matches(value: &Value, keys: &[&str], at: String) -> Vec<String> {
    let Some((first, rest)) = keys.split_first() else {
        return vec![at];
    };
    let join = |key: &str| if at.is_empty() { key.to_string() } else { format!("{at}.{key}") };
    match (value, *first) {
        (Value::Mapping(map), "*") => map.iter().flat_map(|(k, v)| matches(v, rest, join(&key_text(k)))).collect(),
        (Value::Sequence(items), "*") => {
            items.iter().enumerate().flat_map(|(i, v)| matches(v, rest, join(&i.to_string()))).collect()
        }
        (Value::Mapping(map), key) => map.get(key).map(|v| matches(v, rest, join(key))).unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn key_text(key: &Value) -> String {
    match key {
        Value::String(s) => s.clone(),
        other => serde_yaml_ng::to_string(other).unwrap_or_default().trim().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: &[Deprecated] = &[
        Deprecated { file: File::Settings, path: "api.old_listen", since: "0.9.0", instead: "api.listen" },
        Deprecated { file: File::Resources, path: "databases.*.pool", since: "0.9.0", instead: "connections" },
        Deprecated { file: File::Interface, path: "destinations.*.legacy", since: "0.9.1", instead: "nothing" },
    ];

    fn write(root: &Path, file: &str, text: &str) {
        let path = root.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn finds_deprecated_settings_where_a_project_uses_them() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "ductus.yaml", "api:\n  old_listen: 127.0.0.1:7700\n");
        write(root, "resources.yaml", "databases:\n  billing:\n    pool: 4\n  lab:\n    url: x\n");
        write(root, "interfaces/a/interface.yaml", "destinations:\n  - name: x\n    legacy: true\n  - name: y\n");
        write(root, "interfaces/b/interface.yaml", "name: b\n");

        let found = find(TABLE, root).unwrap();
        let places: Vec<(String, &str)> =
            found.iter().map(|f| (f.file.to_string_lossy().replace('\\', "/"), f.at.as_str())).collect();
        assert_eq!(
            places,
            [
                ("ductus.yaml".to_string(), "api.old_listen"),
                ("resources.yaml".to_string(), "databases.billing.pool"),
                ("interfaces/a/interface.yaml".to_string(), "destinations.0.legacy"),
            ]
        );
        assert_eq!((found[1].since, found[1].instead), ("0.9.0", "connections"));
    }

    #[test]
    fn nothing_to_find_reads_nothing() {
        // No table rows: the project isn't even read (a broken file is validate's to report).
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "ductus.yaml", ": not yaml [");
        assert!(find(&[], dir.path()).unwrap().is_empty());
        assert!(find(TABLE, dir.path()).is_err());
    }
}
