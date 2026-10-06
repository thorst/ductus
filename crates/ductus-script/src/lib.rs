//! Embedded V8 runtime for interface logic written in TypeScript.
//!
//! Interface code is real ES modules: `export function transform(msg)` in a `.ts`
//! file, free to import its own sibling modules and `@ductus/sdk`. One isolate can
//! host many interfaces; they share the SDK and the global scope.

mod check;
mod loader;
mod ops;

pub use check::{Diagnostic, type_check};
pub use ops::{BoxFuture, FetchRequest, FetchResponse, KILL, QUARANTINE, Services, TRANSIENT};

use anyhow::{Context, Result, anyhow, bail};
use deno_core::error::JsError;
use deno_core::{JsRuntime, ModuleId, ModuleSpecifier, PollEventLoopOptions, RuntimeOptions, v8};
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context as TaskContext, Poll};

/// An async transform still running: resolves once the event loop settles its promise.
/// The isolate isn't borrowed while waiting, so other interfaces on the thread keep running.
pub struct PendingCall(Pin<Box<dyn Future<Output = CallResult>>>);

/// What a `PendingCall` resolves to; pass it to `Isolate::settle`.
pub type CallResult = Result<v8::Global<v8::Value>, Box<JsError>>;

impl Future for PendingCall {
    type Output = CallResult;
    fn poll(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Self::Output> {
        self.0.as_mut().poll(cx)
    }
}

/// Result of starting a transform.
pub enum Call {
    /// Finished synchronously: the outgoing message, or None if filtered.
    Done(Option<String>),
    /// Async transform: await it (while something drives the event loop), then `settle`.
    Pending(PendingCall),
}

enum RawResult {
    Value(Option<String>),
    Promise(v8::Global<v8::Value>),
}

pub use loader::transpile;

/// Name of the global function the test and typegen glue modules install.
const ENTRY: &str = "__ductus_entry";

/// Formats for interfaces loaded without a config: HL7 in, HL7 out.
pub const DEFAULT_FORMATS: &str = r#"{"input":{"kind":"hl7"},"output":{"kind":"hl7"}}"#;

/// Handle to an interface loaded into an [`Isolate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InterfaceId(usize);

/// One V8 isolate hosting one or more interfaces (or a test suite).
pub struct Isolate {
    runtime: JsRuntime,
    /// Callable entry points, indexed by `InterfaceId`.
    entries: Vec<v8::Global<v8::Function>>,
}

impl Isolate {
    /// An empty isolate without engine services. Must be created inside a tokio runtime.
    pub fn host() -> Self {
        Self::host_with(None)
    }

    /// An empty isolate whose interface code can use `services` (databases, env, secrets).
    pub fn host_with(services: Option<Rc<dyn Services>>) -> Self {
        let runtime = JsRuntime::new(RuntimeOptions {
            module_loader: Some(Rc::new(loader::Loader)),
            extensions: vec![ops::ductus_ops::init()],
            ..Default::default()
        });
        if let Some(services) = services {
            runtime.op_state().borrow_mut().put(services);
        }
        Self { runtime, entries: Vec::new() }
    }

    /// An isolate with a single interface loaded; use `transform`.
    pub async fn interface(module: &Path) -> Result<Self> {
        let mut isolate = Self::host();
        isolate.add_interface(module).await?;
        Ok(isolate)
    }

    /// Load another HL7 interface module (which must export `transform(msg)`) into this isolate.
    pub async fn add_interface(&mut self, module: &Path) -> Result<InterfaceId> {
        self.add_interface_with(module, DEFAULT_FORMATS).await
    }

    /// Load an interface module with its formats (`InterfaceConfig::format_spec`): how
    /// payloads are parsed for `transform`, and how its results are serialized.
    pub async fn add_interface_with(&mut self, module: &Path, formats_json: &str) -> Result<InterfaceId> {
        let url = file_url(module)?;
        // The glue module sits beside the interface, so each interface gets a unique one.
        let glue_url = file_url(&module.with_extension("ts.__ductus__.js"))?;
        let glue = format!(
            r#"import * as entry from "{url}";
import {{ __invoke }} from "@ductus/sdk";
if (typeof entry.transform !== "function") throw new Error("{name} must export a function named `transform`");
const formats = {formats_json};
export const run = (raw) => __invoke(entry.transform, raw, formats);
"#,
            name = module.display(),
        );
        let id = self.runtime.load_side_es_module_from_code(&glue_url, glue).await.map_err(|e| anyhow!("{e}"))?;
        self.evaluate(id).await?;

        let namespace = self.runtime.get_module_namespace(id).map_err(|e| anyhow!("{e}"))?;
        let function = {
            deno_core::scope!(scope, &mut self.runtime);
            let namespace = v8::Local::new(scope, namespace);
            let key = v8::String::new(scope, "run").context("export name")?;
            let value = namespace.get(scope, key.into()).context("glue export")?;
            let function =
                v8::Local::<v8::Function>::try_from(value).map_err(|_| anyhow!("glue export is not a function"))?;
            v8::Global::new(scope, function)
        };
        self.entries.push(function);
        Ok(InterfaceId(self.entries.len() - 1))
    }

    /// Load test modules; `run_tests()` then executes everything they registered.
    /// `formats_json` is the interface's formats, used by the `input()` test helper.
    pub async fn tests(modules: &[&Path], fixtures_json: &str, formats_json: &str) -> Result<Self> {
        let Some(first) = modules.first() else { bail!("no test files") };
        let mut imports = String::new();
        for m in modules {
            imports.push_str(&format!("import \"{}\";\n", file_url(m)?));
        }
        let glue = format!(
            r#"import {{ __setFixtures, __runTests }} from "@ductus/sdk";
__setFixtures({fixtures_json}, {formats_json});
{imports}globalThis.{ENTRY} = () => __runTests();
"#
        );
        let mut isolate = Self::host();
        isolate.load_main(first, glue).await?;
        Ok(isolate)
    }

    /// Contents of the editor typings for `@ductus/sdk` (`ductus-sdk.d.ts`).
    pub async fn generate_types() -> Result<String> {
        let glue = format!(
            "import {{ __generateTypes }} from \"@ductus/sdk\";\nglobalThis.{ENTRY} = () => __generateTypes();\n"
        );
        let mut isolate = Self::host();
        isolate.load_main(&std::env::current_dir()?.join("types"), glue).await?;
        isolate.run_tests().await.context("type generator returned nothing")
    }

    /// Load a main module that installs `globalThis.__ductus_entry`, and register it as entry 0.
    async fn load_main(&mut self, beside: &Path, glue: String) -> Result<()> {
        let dir = beside.parent().unwrap_or(Path::new("."));
        let specifier = file_url(&dir.join("__ductus_main__.js"))?;
        let id = self.runtime.load_main_es_module_from_code(&specifier, glue).await.map_err(|e| anyhow!("{e}"))?;
        self.evaluate(id).await?;
        let function = {
            deno_core::scope!(scope, &mut self.runtime);
            let global = scope.get_current_context().global(scope);
            let key = v8::String::new(scope, ENTRY).context("entry name")?;
            let value = global.get(scope, key.into()).context("entry lookup")?;
            let function = v8::Local::<v8::Function>::try_from(value).map_err(|_| anyhow!("entry not installed"))?;
            v8::Global::new(scope, function)
        };
        self.entries.push(function);
        Ok(())
    }

    async fn evaluate(&mut self, id: ModuleId) -> Result<()> {
        let evaluated = self.runtime.mod_evaluate(id);
        self.runtime.run_event_loop(Default::default()).await.map_err(|e| anyhow!("{e}"))?;
        evaluated.await.map_err(|e| anyhow!("{e}"))
    }

    /// Run the (single) loaded interface's `transform` on a raw message (sync or async).
    pub async fn transform(&mut self, raw: &str) -> Result<Option<String>> {
        self.transform_with(InterfaceId(0), raw).await
    }

    /// Run a loaded interface's `transform`, driving the event loop itself until an async
    /// transform finishes. Use `begin`/`settle` instead when several interfaces share the isolate.
    /// Returns the outgoing message, or `None` when it was filtered out.
    pub async fn transform_with(&mut self, interface: InterfaceId, raw: &str) -> Result<Option<String>> {
        match self.begin(interface, raw)? {
            Call::Done(out) => Ok(out),
            Call::Pending(pending) => {
                let value = self.runtime.with_event_loop_promise(pending, PollEventLoopOptions::default()).await;
                self.settle(value)
            }
        }
    }

    /// Start a transform. Synchronous transforms finish immediately; async ones return a
    /// `PendingCall` to await while the event loop is driven (see `poll_event_loop`).
    pub fn begin(&mut self, interface: InterfaceId, raw: &str) -> Result<Call> {
        match self.call(interface, Some(raw))? {
            RawResult::Value(out) => Ok(Call::Done(out)),
            RawResult::Promise(promise) => Ok(Call::Pending(PendingCall(Box::pin(self.runtime.resolve(promise))))),
        }
    }

    /// Turn a finished `PendingCall` into the outgoing message (or the transform's error).
    pub fn settle<E: std::fmt::Display>(&mut self, result: Result<v8::Global<v8::Value>, E>) -> Result<Option<String>> {
        let value = result.map_err(|e| anyhow!("{e}"))?;
        deno_core::scope!(scope, &mut self.runtime);
        let value = v8::Local::new(scope, value);
        Ok(if value.is_null_or_undefined() { None } else { Some(value.to_rust_string_lossy(scope)) })
    }

    /// Run the JavaScript event loop (pending database calls, timers...). Ready when idle.
    pub fn poll_event_loop(&mut self, cx: &mut TaskContext<'_>) -> Poll<Result<()>> {
        self.runtime.poll_event_loop(cx, PollEventLoopOptions::default()).map_err(|e| anyhow!("{e}"))
    }

    /// Run the loaded test suite (tests may be async); returns the JSON report.
    pub async fn run_tests(&mut self) -> Result<String> {
        let report = match self.call(InterfaceId(0), None)? {
            RawResult::Value(report) => report,
            RawResult::Promise(promise) => {
                let pending = self.runtime.resolve(promise);
                let value = self.runtime.with_event_loop_promise(pending, PollEventLoopOptions::default()).await;
                self.settle(value)?
            }
        };
        report.context("test runner returned nothing")
    }

    fn call(&mut self, entry: InterfaceId, arg: Option<&str>) -> Result<RawResult> {
        let function = self.entries.get(entry.0).context("no such interface in this isolate")?.clone();
        deno_core::scope!(scope, &mut self.runtime);
        v8::tc_scope!(let scope, scope);
        let function = v8::Local::new(scope, function);
        let args: Vec<v8::Local<v8::Value>> = match arg {
            Some(a) => vec![v8::String::new(scope, a).context("argument")?.into()],
            None => vec![],
        };
        let receiver = v8::undefined(scope);
        match function.call(scope, receiver.into(), &args) {
            Some(result) if result.is_null_or_undefined() => Ok(RawResult::Value(None)),
            Some(result) if result.is_promise() => Ok(RawResult::Promise(v8::Global::new(scope, result))),
            Some(result) => Ok(RawResult::Value(Some(result.to_rust_string_lossy(scope)))),
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
