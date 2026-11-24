//! Type checking with the embedded TypeScript compiler (vendored, v6, see vendor/typescript), and
//! the analysis (`analysis.rs`) beside it.

use crate::analysis::{Analysis, Diagnostic, analyze_syntax};
use anyhow::{Context, Result, anyhow};
use deno_core::{JsRuntime, RuntimeOptions, v8};
use std::collections::BTreeMap;

include!(concat!(env!("OUT_DIR"), "/ts_libs.rs"));

const TYPESCRIPT: &str = include_str!("../../../vendor/typescript/lib/typescript.js");
const CHECK: &str = include_str!("check.js");

/// Type-check a set of files and run the lints. Keys are absolute paths using `/` separators;
/// include the generated `@ductus/sdk` typings as one of them. `shared_modules` are files imported
/// by interfaces but not inside one (shared libraries): top-level mutable state in them is flagged.
pub fn type_check(files: &BTreeMap<String, String>, shared_modules: &[String]) -> Result<Vec<Diagnostic>> {
    Ok(analyze(files, shared_modules, true)?.diagnostics)
}

/// The analysis of `files` (`analysis::analyze_syntax`: the resources code names, the imports
/// between files), and with `diagnostics` the type errors before the lints, the `console` calls and
/// the exports too. Without `diagnostics` no type checker runs (nor V8): it's quick.
pub fn analyze(files: &BTreeMap<String, String>, shared_modules: &[String], diagnostics: bool) -> Result<Analysis> {
    let mut analysis = analyze_syntax(files, shared_modules, diagnostics);
    if diagnostics {
        let mut found = type_errors(files)?;
        found.append(&mut analysis.diagnostics);
        analysis.diagnostics = found;
    }
    Ok(analysis)
}

/// TypeScript's diagnostics for `files` (in memory, with the standard library typings).
fn type_errors(files: &BTreeMap<String, String>) -> Result<Vec<Diagnostic>> {
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
    let args = [
        v8::String::new(scope, &files_json).context("files")?.into(),
        v8::String::new(scope, &libs_json).context("libs")?.into(),
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
