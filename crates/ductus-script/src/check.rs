//! Type checking with the embedded TypeScript compiler (vendored, v6, see vendor/typescript).

use anyhow::{Context, Result, anyhow};
use deno_core::{JsRuntime, RuntimeOptions, v8};
use serde::Deserialize;
use std::collections::BTreeMap;

include!(concat!(env!("OUT_DIR"), "/ts_libs.rs"));

const TYPESCRIPT: &str = include_str!("../../../vendor/typescript/lib/typescript.js");
const CHECK: &str = include_str!("check.js");

#[derive(Debug, Clone, Deserialize)]
pub struct Diagnostic {
    /// Absolute path, or `None` for global errors.
    pub file: Option<String>,
    pub line: u32,
    pub column: u32,
    pub code: u32,
    /// "error", "warning", "suggestion" or "message".
    pub category: String,
    pub message: String,
}

impl Diagnostic {
    pub fn is_error(&self) -> bool {
        self.category == "error"
    }
}

/// A resource named in code: `db("patient_db")` or `api("empi")`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ResourceReference {
    pub file: String,
    pub line: u32,
    pub column: u32,
    /// `db` or `api`.
    pub kind: String,
    pub name: String,
}

/// What the checker found in a set of files.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Analysis {
    /// Empty unless diagnostics were asked for.
    pub diagnostics: Vec<Diagnostic>,
    pub references: Vec<ResourceReference>,
    /// Each file's imports that are project files (same path form as the keys passed in).
    pub imports: BTreeMap<String, Vec<String>>,
}

impl Analysis {
    /// The resources code reachable from `entry` (through its imports) names, sorted, each once.
    pub fn references_from(&self, entry: &str) -> Vec<&ResourceReference> {
        let mut seen = std::collections::BTreeSet::from([entry]);
        let mut todo = vec![entry];
        while let Some(file) = todo.pop() {
            for next in self.imports.get(file).into_iter().flatten() {
                if seen.insert(next) {
                    todo.push(next);
                }
            }
        }
        let mut found: Vec<_> = self.references.iter().filter(|r| seen.contains(r.file.as_str())).collect();
        found.sort_by(|a, b| (&a.kind, &a.name).cmp(&(&b.kind, &b.name)));
        found.dedup_by(|a, b| (&a.kind, &a.name) == (&b.kind, &b.name));
        found
    }
}

/// Type-check a set of files and run the shared-pool safety lints. Keys are absolute paths
/// using `/` separators; include the generated `@ductus/sdk` typings as one of them.
/// `shared_modules` are files imported by interfaces but not inside one (shared libraries):
/// top-level mutable state in them is flagged.
pub fn type_check(files: &BTreeMap<String, String>, shared_modules: &[String]) -> Result<Vec<Diagnostic>> {
    Ok(analyze(files, shared_modules, true)?.diagnostics)
}

/// Like `type_check`, and also finds the resources code names and the imports between files.
/// Without `diagnostics` it only parses and resolves names, which is much faster.
pub fn analyze(files: &BTreeMap<String, String>, shared_modules: &[String], diagnostics: bool) -> Result<Analysis> {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    runtime.execute_script("typescript.js", TYPESCRIPT).map_err(|e| anyhow!("loading TypeScript: {e}"))?;
    runtime.execute_script("check.js", CHECK).map_err(|e| anyhow!("loading checker: {e}"))?;

    let files_json = serde_json::to_string(files)?;
    let libs: BTreeMap<&str, &str> = TS_LIBS.iter().copied().collect();
    let libs_json = serde_json::to_string(&libs)?;

    deno_core::scope!(scope, &mut runtime);
    v8::tc_scope!(let scope, scope);
    let global = scope.get_current_context().global(scope);
    let key = v8::String::new(scope, "__ductus_check").context("name")?;
    let function = global
        .get(scope, key.into())
        .and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())
        .context("checker not installed")?;
    let shared_json = serde_json::to_string(shared_modules)?;
    let args = [
        v8::String::new(scope, &files_json).context("files")?.into(),
        v8::String::new(scope, &libs_json).context("libs")?.into(),
        v8::String::new(scope, &shared_json).context("shared")?.into(),
        v8::Boolean::new(scope, diagnostics).into(),
    ];
    let receiver = v8::undefined(scope);
    let result = match function.call(scope, receiver.into(), &args) {
        Some(r) => r.to_rust_string_lossy(scope),
        None => {
            let message = scope.exception().map(|e| e.to_rust_string_lossy(scope)).unwrap_or_default();
            return Err(anyhow!("type checker failed: {message}"));
        }
    };
    Ok(serde_json::from_str(&result)?)
}
