//! The project's own files an engine runs with (`ductus.yaml`, `resources.yaml`,
//! `docs-schema.yaml`, and its environment's `environments/<env>.yaml` and `secrets/<env>.json`),
//! read once at its start and kept as text: the engine is built from these texts
//! (`Environment::from_files`, `Resources::from_text`, `Settings::from_text`), so what it keeps is
//! exactly what it runs, with nothing read twice.
//!
//! What it's kept for: a reload in folder mode (0.7.5.3) compares the folder as it is now with
//! what runs, and by then the files on disk may already be the new ones. Release mode doesn't need
//! it (the running release's folder keeps its files), but keeping a few kilobytes costs nothing.
//!
//! Other environments' files aren't read: nothing in them changes this engine.

use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::path::Path;

/// `ductus.yaml`.
pub const SETTINGS: &str = "ductus.yaml";
/// `resources.yaml`.
pub const RESOURCES: &str = "resources.yaml";
/// `docs-schema.yaml`.
pub const DOCS_SCHEMA: &str = "docs-schema.yaml";

/// `environments/<env>.yaml`, relative to the project.
pub fn environment_file(env: &str) -> String {
    format!("environments/{env}.yaml")
}

/// `secrets/<env>.json`, relative to the project.
pub fn secrets_file(env: &str) -> String {
    format!("secrets/{env}.json")
}

/// The project's own files one environment runs with, as read (see the module).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectFiles {
    /// The environment they were read for.
    pub environment: String,
    /// Each file there is, by its path relative to the project (with `/`), as read.
    texts: BTreeMap<String, String>,
    /// The project has an `environments/` folder (then an environment without its file is unknown).
    pub has_environments: bool,
}

impl ProjectFiles {
    /// Read `root`'s files for environment `env` (each optional; one that can't be read is an error).
    pub fn read(root: &Path, env: &str) -> Result<Self> {
        let mut texts = BTreeMap::new();
        for path in [
            SETTINGS.to_string(),
            RESOURCES.to_string(),
            DOCS_SCHEMA.to_string(),
            environment_file(env),
            secrets_file(env),
        ] {
            let file = root.join(&path);
            if file.is_file() {
                let text = std::fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?;
                texts.insert(path, text);
            }
        }
        Ok(Self { environment: env.to_string(), texts, has_environments: root.join("environments").is_dir() })
    }

    /// The text of the file at `path` (relative to the project), if the project has it.
    pub fn text(&self, path: &str) -> Option<&str> {
        self.texts.get(path).map(String::as_str)
    }

    /// Every file there is, by path, with its text.
    pub fn files(&self) -> impl Iterator<Item = (&str, &str)> {
        self.texts.iter().map(|(p, t)| (p.as_str(), t.as_str()))
    }

    /// From texts by path (tests, and a release's files).
    pub fn of(environment: &str, texts: impl IntoIterator<Item = (String, String)>, has_environments: bool) -> Self {
        Self { environment: environment.to_string(), texts: texts.into_iter().collect(), has_environments }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_engine_s_files_and_no_other_environment_s() {
        let dir = tempfile::tempdir().unwrap();
        for (path, text) in [
            ("ductus.yaml", "retention: 7d\n"),
            ("environments/prod.yaml", "variables: { A: 1 }\n"),
            ("environments/test.yaml", "variables: { A: 2 }\n"),
            ("secrets/test.json", "{}"),
        ] {
            let file = dir.path().join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, text).unwrap();
        }
        let files = ProjectFiles::read(dir.path(), "prod").unwrap();
        let paths: Vec<&str> = files.files().map(|(p, _)| p).collect();
        assert_eq!(paths, ["ductus.yaml", "environments/prod.yaml"]);
        assert_eq!(files.text("environments/prod.yaml"), Some("variables: { A: 1 }\n"));
        assert!(files.has_environments && files.text(RESOURCES).is_none());
    }
}
