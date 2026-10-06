//! Embedded V8 runtime for interface logic written in TypeScript.
//!
//! Interface code is real ES modules: `export function transform(msg)` in a `.ts`
//! file, free to import its own sibling modules and `@ductus/sdk`.

mod check;
mod loader;

pub use check::{Diagnostic, type_check};

use anyhow::{Context, Result, anyhow, bail};
use deno_core::{JsRuntime, ModuleSpecifier, RuntimeOptions, v8};
use std::path::Path;
use std::rc::Rc;

pub use loader::transpile;

/// Name of the global function each glue module installs.
const ENTRY: &str = "__ductus_entry";

/// One V8 isolate with an interface (or test suite) loaded.
pub struct Isolate {
    runtime: JsRuntime,
}

impl Isolate {
    fn new() -> Self {
        let runtime =
            JsRuntime::new(RuntimeOptions { module_loader: Some(Rc::new(loader::Loader)), ..Default::default() });
        Self { runtime }
    }

    /// Load an interface module that exports `transform(msg)`.
    /// Must be called inside a tokio runtime.
    pub async fn interface(module: &Path) -> Result<Self> {
        let url = file_url(module)?;
        let glue = format!(
            r#"import * as entry from "{url}";
import {{ __invoke }} from "@ductus/sdk";
if (typeof entry.transform !== "function") throw new Error("{name} must export a function named `transform`");
globalThis.{ENTRY} = (raw) => __invoke(entry.transform, raw);
"#,
            name = module.display(),
        );
        let mut isolate = Self::new();
        isolate.load_main(module, glue).await?;
        Ok(isolate)
    }

    /// Load test modules; `run_tests()` then executes everything they registered.
    pub async fn tests(modules: &[&Path], fixtures_json: &str) -> Result<Self> {
        let Some(first) = modules.first() else { bail!("no test files") };
        let mut imports = String::new();
        for m in modules {
            imports.push_str(&format!("import \"{}\";\n", file_url(m)?));
        }
        let glue = format!(
            r#"import {{ __setFixtures, __runTests }} from "@ductus/sdk";
__setFixtures({fixtures_json});
{imports}globalThis.{ENTRY} = () => __runTests();
"#
        );
        let mut isolate = Self::new();
        isolate.load_main(first, glue).await?;
        Ok(isolate)
    }

    /// Contents of the editor typings for `@ductus/sdk` (`ductus-sdk.d.ts`).
    pub async fn generate_types() -> Result<String> {
        let glue = format!(
            "import {{ __generateTypes }} from \"@ductus/sdk\";\nglobalThis.{ENTRY} = () => __generateTypes();\n"
        );
        let mut isolate = Self::new();
        isolate.load_main(&std::env::current_dir()?.join("types"), glue).await?;
        isolate.call_entry(None)?.context("type generator returned nothing")
    }

    async fn load_main(&mut self, beside: &Path, glue: String) -> Result<()> {
        let dir = beside.parent().unwrap_or(Path::new("."));
        let specifier = file_url(&dir.join("__ductus_main__.js"))?;
        let id = self.runtime.load_main_es_module_from_code(&specifier, glue).await.map_err(|e| anyhow!("{e}"))?;
        let evaluated = self.runtime.mod_evaluate(id);
        self.runtime.run_event_loop(Default::default()).await.map_err(|e| anyhow!("{e}"))?;
        evaluated.await.map_err(|e| anyhow!("{e}"))?;
        Ok(())
    }

    /// Run the loaded interface's `transform` on a raw message.
    /// Returns the outgoing message, or `None` when it was filtered out.
    pub fn transform(&mut self, raw: &str) -> Result<Option<String>> {
        self.call_entry(Some(raw))
    }

    /// Run the loaded test suite; returns the JSON report.
    pub fn run_tests(&mut self) -> Result<String> {
        self.call_entry(None)?.context("test runner returned nothing")
    }

    fn call_entry(&mut self, arg: Option<&str>) -> Result<Option<String>> {
        deno_core::scope!(scope, &mut self.runtime);
        v8::tc_scope!(let scope, scope);
        let global = scope.get_current_context().global(scope);
        let key = v8::String::new(scope, ENTRY).context("entry name")?;
        let value = global.get(scope, key.into()).context("entry lookup")?;
        let function = v8::Local::<v8::Function>::try_from(value).map_err(|_| anyhow!("no module loaded"))?;
        let args: Vec<v8::Local<v8::Value>> = match arg {
            Some(a) => vec![v8::String::new(scope, a).context("argument")?.into()],
            None => vec![],
        };
        let receiver = v8::undefined(scope);
        match function.call(scope, receiver.into(), &args) {
            Some(result) if result.is_null_or_undefined() => Ok(None),
            Some(result) => Ok(Some(result.to_rust_string_lossy(scope))),
            None => {
                let message = match scope.exception() {
                    Some(e) if e.is_object() => {
                        let obj = e.to_object(scope).context("exception")?;
                        let key = v8::String::new(scope, "stack").context("stack")?;
                        obj.get(scope, key.into())
                            .filter(|s| !s.is_undefined())
                            .unwrap_or(e)
                            .to_rust_string_lossy(scope)
                    }
                    Some(e) => e.to_rust_string_lossy(scope),
                    None => "script was terminated".to_string(),
                };
                Err(anyhow!(message))
            }
        }
    }
}

/// `file://` URL for a path, with `..` and symlinks resolved so specifiers always match.
/// The file itself may not exist yet (the in-memory glue module), but its folder must.
fn file_url(path: &Path) -> Result<ModuleSpecifier> {
    let resolve = || -> std::io::Result<std::path::PathBuf> {
        match std::fs::canonicalize(path) {
            Ok(p) => Ok(p),
            Err(_) => {
                let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
                let name = path.file_name().ok_or(std::io::ErrorKind::InvalidInput)?;
                Ok(std::fs::canonicalize(parent)?.join(name))
            }
        }
    };
    let absolute = resolve().with_context(|| format!("resolving {}", path.display()))?;
    ModuleSpecifier::from_file_path(&absolute).map_err(|_| anyhow!("not a valid path: {}", path.display()))
}
