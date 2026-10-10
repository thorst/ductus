//! Embedded V8 runtime for interface logic written in TypeScript.
//!
//! Interface code is real ES modules: `export function transform(msg)` in a `.ts`
//! file, free to import its own sibling modules and `@ductus/sdk`. One isolate can
//! host many interfaces; they share the SDK and the global scope.
//!
//! Every call into JavaScript has a time budget and every isolate a heap limit (`guard.rs`):
//! a runaway interface is stopped with a [`Runaway`] error instead of stalling the thread.

mod check;
mod guard;
mod loader;
mod ops;

pub use check::{Analysis, Diagnostic, ResourceReference, analyze, type_check};
pub use guard::Cause;
pub use ops::{BoxFuture, FetchRequest, FetchResponse, KILL, QUARANTINE, Services, TRANSIENT};

use anyhow::{Context, Result, anyhow, bail};
use deno_core::error::JsError;
use deno_core::{JsRuntime, ModuleId, ModuleSpecifier, PollEventLoopOptions, RuntimeOptions, v8};
use guard::{Guard, Trip};
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context as TaskContext, Poll};
use std::time::Duration;

/// Time budget for interfaces loaded without one, for tests and for loading modules.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
/// Heap limit for isolates created without one.
pub const DEFAULT_HEAP_MB: usize = 512;

/// Interface code that was stopped: it ran past its time budget without pausing, or the
/// isolate's heap filled up while it ran. The isolate stays usable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Runaway {
    /// The interface whose code was running. `None` when it couldn't be told: code in a
    /// shared module, resumed after an `await`, has no interface's file on its stack.
    pub interface: Option<InterfaceId>,
    pub cause: Cause,
}

impl std::fmt::Display for Runaway {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.cause {
            Cause::Timeout(budget) => {
                write!(f, "stopped: the script ran longer than its {budget:?} timeout without pausing (endless loop?)")
            }
            Cause::Heap(mb) => write!(f, "stopped: the script used up the {mb} MB script heap"),
        }
    }
}

impl std::error::Error for Runaway {}

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

/// Handle to one of an interface's functions (e.g. one per destination), see `add_functions`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FunctionId(usize);

/// A callable entry point and the interface it belongs to.
struct Entry {
    function: v8::Global<v8::Function>,
    interface: InterfaceId,
}

/// One V8 isolate hosting one or more interfaces (or a test suite).
pub struct Isolate {
    runtime: JsRuntime,
    /// Callable entry points, indexed by `FunctionId`.
    entries: Vec<Entry>,
    /// Each interface's first entry, indexed by `InterfaceId` (the guard's table is indexed the same).
    interfaces: Vec<FunctionId>,
    guard: Guard,
}

/// The `Runaway` a guard trip stands for; `known` is the interface when the caller knows it.
fn runaway(known: Option<InterfaceId>, trip: Trip) -> Runaway {
    Runaway { interface: known.or(trip.interface.map(InterfaceId)), cause: trip.cause }
}

impl Isolate {
    /// An empty isolate without engine services. Must be created inside a tokio runtime.
    pub fn host() -> Self {
        Self::host_with(None)
    }

    /// An empty isolate whose interface code can use `services` (databases, env, secrets).
    pub fn host_with(services: Option<Rc<dyn Services>>) -> Self {
        Self::new(services, DEFAULT_HEAP_MB)
    }

    /// An empty isolate with engine services and a heap limit of `heap_mb`.
    pub fn new(services: Option<Rc<dyn Services>>, heap_mb: usize) -> Self {
        let mut runtime = JsRuntime::new(RuntimeOptions {
            module_loader: Some(Rc::new(loader::Loader)),
            extensions: vec![ops::extension()],
            create_params: Some(v8::Isolate::create_params().heap_limits(0, heap_mb << 20)),
            ..Default::default()
        });
        if let Some(services) = services {
            runtime.op_state().borrow_mut().put(services);
        }
        let context = runtime.main_context();
        let guard = Guard::new(runtime.v8_isolate(), context, heap_mb);
        runtime.add_near_heap_limit_callback(guard.heap_callback());
        Self { runtime, entries: Vec::new(), interfaces: Vec::new(), guard }
    }

    /// An isolate with a single interface loaded; use `transform`.
    pub async fn interface(module: &Path) -> Result<Self> {
        let mut isolate = Self::host();
        isolate.add_interface(module).await?;
        Ok(isolate)
    }

    /// Load another HL7 interface module (which must export `transform(msg)`) into this isolate.
    pub async fn add_interface(&mut self, module: &Path) -> Result<InterfaceId> {
        self.add_interface_with(module, DEFAULT_FORMATS, DEFAULT_TIMEOUT).await
    }

    /// Load an interface module with its formats (`InterfaceConfig::format_spec`): how
    /// payloads are parsed for `transform`, and how its results are serialized. Each call of
    /// its code may run for `timeout` without pausing before it's stopped.
    pub async fn add_interface_with(
        &mut self,
        module: &Path,
        formats_json: &str,
        timeout: Duration,
    ) -> Result<InterfaceId> {
        Ok(self.add_functions(module, &[("transform", formats_json)], timeout).await?.0)
    }

    /// Load an interface module and the functions the engine calls in it, each with its
    /// formats (`InterfaceConfig::functions`): e.g. one per destination. The module is loaded
    /// once, so its functions share its module-level state. Each call may run for `timeout`
    /// without pausing before it's stopped.
    pub async fn add_functions(
        &mut self,
        module: &Path,
        functions: &[(&str, &str)],
        timeout: Duration,
    ) -> Result<(InterfaceId, Vec<FunctionId>)> {
        if functions.is_empty() {
            bail!("no functions to load from {}", module.display());
        }
        let url = file_url(module)?;
        // The glue module sits beside the interface, so each interface gets a unique one.
        let glue_url = file_url(&module.with_extension("ts.__ductus__.js"))?;
        let quote = |s: &str| serde_json::to_string(s).expect("strings serialize");
        let mut glue = format!(
            r#"import * as entry from "{url}";
import {{ __invoke }} from "@ductus/sdk";
const bind = (name, formats) => {{
  const f = entry[name];
  if (typeof f !== "function") throw new Error(`${{{file}}} must export a function named \`${{name}}\``);
  return (raw, meta) => __invoke(f, raw, formats, meta);
}};
"#,
            file = quote(&module.display().to_string()),
        );
        for (i, (name, formats_json)) in functions.iter().enumerate() {
            glue.push_str(&format!("export const run{i} = bind({}, {formats_json});\n", quote(name)));
        }
        let id = self.runtime.load_side_es_module_from_code(&glue_url, glue).await.map_err(|e| anyhow!("{e}"))?;
        self.evaluate(id, timeout).await?;

        let namespace = self.runtime.get_module_namespace(id).map_err(|e| anyhow!("{e}"))?;
        let interface = InterfaceId(self.interfaces.len());
        let mut ids = Vec::with_capacity(functions.len());
        for i in 0..functions.len() {
            let function = {
                deno_core::scope!(scope, &mut self.runtime);
                let namespace = v8::Local::new(scope, namespace.clone());
                let key = v8::String::new(scope, &format!("run{i}")).context("export name")?;
                let value = namespace.get(scope, key.into()).context("glue export")?;
                let function =
                    v8::Local::<v8::Function>::try_from(value).map_err(|_| anyhow!("glue export is not a function"))?;
                v8::Global::new(scope, function)
            };
            ids.push(FunctionId(self.entries.len()));
            self.entries.push(Entry { function, interface });
        }
        let folder = file_url(module.parent().unwrap_or(Path::new(".")))?.to_string();
        let folder = if folder.ends_with('/') { folder } else { folder + "/" };
        self.guard.add(Some((url.to_string(), folder)), timeout);
        self.interfaces.push(ids[0]);
        Ok((interface, ids))
    }

    /// The interface a function belongs to.
    pub fn interface_of(&self, function: FunctionId) -> InterfaceId {
        self.entries[function.0].interface
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
        self.evaluate(id, DEFAULT_TIMEOUT).await?;
        let function = {
            deno_core::scope!(scope, &mut self.runtime);
            let global = scope.get_current_context().global(scope);
            let key = v8::String::new(scope, ENTRY).context("entry name")?;
            let value = global.get(scope, key.into()).context("entry lookup")?;
            let function = v8::Local::<v8::Function>::try_from(value).map_err(|_| anyhow!("entry not installed"))?;
            v8::Global::new(scope, function)
        };
        self.guard.add(None, DEFAULT_TIMEOUT);
        let interface = InterfaceId(self.interfaces.len());
        self.interfaces.push(FunctionId(self.entries.len()));
        self.entries.push(Entry { function, interface });
        Ok(())
    }

    /// Run a module's top-level code (and anything it awaits) within `timeout`. Nothing
    /// else runs on the thread while interfaces load, so the budget is simply wall time.
    async fn evaluate(&mut self, id: ModuleId, timeout: Duration) -> Result<()> {
        self.guard.arm(timeout, false);
        let evaluated = self.runtime.mod_evaluate(id);
        let result = match self.runtime.run_event_loop(Default::default()).await {
            Ok(()) => evaluated.await.map_err(|e| anyhow!("{e}")),
            Err(e) => Err(anyhow!("{e}")),
        };
        match self.guard.disarm(self.runtime.v8_isolate()) {
            Some(trip) => Err(anyhow!("loading the module: {}", Runaway { interface: None, cause: trip.cause })),
            None => result,
        }
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
                let value = self.drive(pending).await?;
                Ok(self.output(value))
            }
        }
    }

    /// Run the event loop until `pending` settles.
    async fn drive(&mut self, mut pending: PendingCall) -> Result<v8::Global<v8::Value>> {
        std::future::poll_fn(|cx| {
            let event_loop = self.poll_event_loop(cx);
            if let Poll::Ready(Err(e)) = event_loop {
                return Poll::Ready(Err(e));
            }
            if let Poll::Ready(value) = Pin::new(&mut pending).poll(cx) {
                return Poll::Ready(value.map_err(|e| anyhow!("{e}")));
            }
            match event_loop {
                Poll::Pending => Poll::Pending,
                _ => Poll::Ready(Err(anyhow!(
                    "the transform's promise never settled: nothing is left for it to wait on"
                ))),
            }
        })
        .await
    }

    /// Start an interface's (first) function on a raw message; see `begin_function`.
    pub fn begin(&mut self, interface: InterfaceId, raw: &str) -> Result<Call> {
        let function = *self.interfaces.get(interface.0).context("no such interface in this isolate")?;
        self.begin_function(function, raw, None)
    }

    /// Start a function on a raw message, with `meta` (JSON: message id etc.) as its second
    /// argument. Synchronous functions finish immediately; async ones return a `PendingCall`
    /// to await while the event loop is driven (see `poll_event_loop`).
    pub fn begin_function(&mut self, function: FunctionId, raw: &str, meta: Option<&str>) -> Result<Call> {
        let args: Vec<&str> = std::iter::once(raw).chain(meta).collect();
        match self.call(function, &args)? {
            RawResult::Value(out) => Ok(Call::Done(out)),
            RawResult::Promise(promise) => Ok(Call::Pending(PendingCall(Box::pin(self.runtime.resolve(promise))))),
        }
    }

    /// Turn a finished `PendingCall` into the outgoing message (or the transform's error).
    pub fn settle<E: std::fmt::Display>(&mut self, result: Result<v8::Global<v8::Value>, E>) -> Result<Option<String>> {
        let value = result.map_err(|e| anyhow!("{e}"))?;
        Ok(self.output(value))
    }

    /// The outgoing message a transform's result stands for (None: filtered).
    fn output(&mut self, value: v8::Global<v8::Value>) -> Option<String> {
        deno_core::scope!(scope, &mut self.runtime);
        let value = v8::Local::new(scope, value);
        if value.is_null_or_undefined() { None } else { Some(value.to_rust_string_lossy(scope)) }
    }

    /// Run the JavaScript event loop (pending database calls, timers...). Ready when idle.
    ///
    /// Code resumed after an `await` runs here. If it runs away, this returns a [`Runaway`]
    /// error. V8 then drops every queued continuation, not just the runaway's, so the
    /// async transforms in flight on this isolate will never settle: give up on all of them.
    pub fn poll_event_loop(&mut self, cx: &mut TaskContext<'_>) -> Poll<Result<()>> {
        self.guard.arm_any(DEFAULT_TIMEOUT); // continuations of any interface may run
        let poll = self.runtime.poll_event_loop(cx, PollEventLoopOptions::default());
        match self.guard.disarm(self.runtime.v8_isolate()) {
            Some(trip) => Poll::Ready(Err(runaway(None, trip).into())),
            None => poll.map_err(|e| anyhow!("{e}")),
        }
    }

    /// Run the loaded test suite (tests may be async); returns the JSON report.
    pub async fn run_tests(&mut self) -> Result<String> {
        let report = match self.call(FunctionId(0), &[])? {
            RawResult::Value(report) => report,
            RawResult::Promise(promise) => {
                let pending = PendingCall(Box::pin(self.runtime.resolve(promise)));
                let value = self.drive(pending).await?;
                self.output(value)
            }
        };
        report.context("test runner returned nothing")
    }

    /// Call an entry point within its interface's time budget.
    fn call(&mut self, entry: FunctionId, args: &[&str]) -> Result<RawResult> {
        let Entry { function, interface } = self.entries.get(entry.0).context("no such function in this isolate")?;
        let (function, interface) = (function.clone(), *interface);
        self.guard.arm_for(interface.0);
        let result = self.call_unguarded(function, args);
        match self.guard.disarm(self.runtime.v8_isolate()) {
            Some(trip) => Err(runaway(Some(interface), trip).into()),
            None => result,
        }
    }

    fn call_unguarded(&mut self, function: v8::Global<v8::Function>, args: &[&str]) -> Result<RawResult> {
        deno_core::scope!(scope, &mut self.runtime);
        v8::tc_scope!(let scope, scope);
        let function = v8::Local::new(scope, function);
        let args = args
            .iter()
            .map(|a| v8::String::new(scope, a).map(Into::into).context("argument"))
            .collect::<Result<Vec<v8::Local<v8::Value>>>>()?;
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
