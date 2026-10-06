//! Embedded V8 runtime for interface logic written in TypeScript.

use anyhow::{Context, Result};
use deno_core::{JsRuntime, RuntimeOptions, v8};

/// Transpile TypeScript source to JavaScript.
pub fn transpile(name: &str, ts: &str) -> Result<String> {
    let specifier = deno_ast::ModuleSpecifier::parse(&format!("file:///{name}"))?;
    let parsed = deno_ast::parse_module(deno_ast::ParseParams {
        specifier,
        text: ts.into(),
        media_type: deno_ast::MediaType::TypeScript,
        capture_tokens: false,
        scope_analysis: false,
        maybe_syntax: None,
    })?;
    let emitted = parsed
        .transpile(
            &Default::default(),
            &Default::default(),
            &Default::default(),
        )?
        .into_source();
    Ok(emitted.text)
}

/// One isolate hosting one or more interfaces' scripts.
pub struct Isolate {
    runtime: JsRuntime,
}

impl Isolate {
    pub fn new() -> Self {
        Self { runtime: JsRuntime::new(RuntimeOptions::default()) }
    }

    /// Load transpiled JavaScript into the isolate's global scope.
    pub fn load(&mut self, name: &'static str, js: String) -> Result<()> {
        self.runtime.execute_script(name, js).context("loading script")?;
        Ok(())
    }

    /// Call a global function with one string argument. `null`/`undefined` results come back as `None`.
    pub fn call(&mut self, func: &str, arg: &str) -> Result<Option<String>> {
        deno_core::scope!(scope, &mut self.runtime);
        let global = scope.get_current_context().global(scope);
        let key = v8::String::new(scope, func).context("function name")?;
        let value = global.get(scope, key.into()).context("function lookup")?;
        let function = v8::Local::<v8::Function>::try_from(value)
            .map_err(|_| anyhow::anyhow!("`{func}` is not a function"))?;
        let arg = v8::String::new(scope, arg).context("argument")?;
        let receiver = v8::undefined(scope);
        let result = function
            .call(scope, receiver.into(), &[arg.into()])
            .with_context(|| format!("`{func}` threw"))?;
        if result.is_null_or_undefined() {
            return Ok(None);
        }
        Ok(Some(result.to_rust_string_lossy(scope)))
    }
}

impl Default for Isolate {
    fn default() -> Self {
        Self::new()
    }
}
