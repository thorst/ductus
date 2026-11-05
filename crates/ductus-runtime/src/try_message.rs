//! `ductus try` (0.7.2.2): one message through an interface's code, every destination, decided as
//! the engine would decide it now, sending nothing: "what would this message do?", and, with a
//! debugger, stepping through it.
//!
//! It is the replay's machinery for one message (`replay.rs`): the code on a thread of its own,
//! the clock pinned to when the message was received (now, for one from a file) and `Math.random`
//! seeded as the replay seeds it, lookups made read-only (`db().query`, GET), writes refused and
//! reported (`db().execute`, other `fetch()`es). Nothing is connected, stored or sent.

use crate::inspect::{Attach, Inspector, Target};
use crate::replay::{Calls, Decision, ReplayServices, decide, seed};
use crate::services::{EngineServices, SharedServices};
use crate::worker;
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use ductus_core::{Environment, InterfaceConfig};
use ductus_script::Isolate;
use ductus_store::MessageRef;
use serde::Serialize;
use std::cell::RefCell;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

/// The message to try.
pub struct Message {
    /// Its id where it's stored (the code's `meta.id`); a message from a file has none (`0`).
    pub id: Option<i64>,
    /// When it was received: the code's clock (`new Date()`) and `meta.receivedAt`.
    pub received_at: DateTime<Utc>,
    /// Routed from another interface: that message's id (`meta.origin`).
    pub origin: Option<i64>,
    /// As received.
    pub payload: String,
    /// What each destination recorded for it then (a stored message).
    pub recorded: Vec<Recorded>,
}

/// What a destination recorded for a stored message.
#[derive(Debug, Clone, Serialize)]
pub struct Recorded {
    pub destination: String,
    /// `pending`, `sent`, `filtered` or `quarantined`.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// What was sent, when the destination keeps it (`save_output`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// The code version that decided it (64 hex characters), when known (0.7.3.2 on).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code_version: Option<String>,
}

pub struct Options {
    pub root: PathBuf,
    pub env: String,
    pub interface: String,
    pub message: Message,
    /// Under a debugger listening here (loopback; `inspect::address`).
    pub inspect: Option<SocketAddr>,
    /// Told the debugger's WebSocket URL once it waits for one.
    pub waiting: Arc<dyn Fn(String) + Send + Sync>,
    /// Told when a debugger attaches and when the last one detaches.
    pub say: Arc<dyn Fn(Attach) + Send + Sync>,
}

/// What one destination's code decided.
#[derive(Debug, Clone, Serialize)]
pub struct Tried {
    pub destination: String,
    #[serde(flatten)]
    pub decision: Decision,
    /// The code looked something up (`db().query`, a GET): the answer is today's data.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub looked_up: bool,
    /// What its call wrote to `console`, a line each.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub console: String,
    /// What it recorded then, for a stored message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recorded: Option<Recorded>,
}

/// The message through every destination, in the config's order.
#[derive(Debug, Clone, Serialize)]
pub struct Trial {
    pub interface: String,
    /// The code version tried (64 hex characters): what the project's files make now, as a start
    /// would compute it (`code`). None when it couldn't be computed (a file it loaded went away).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code_version: Option<String>,
    /// What the code wrote to `console` while its modules loaded.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub loading: String,
    pub destinations: Vec<Tried>,
    /// resources.yaml couldn't be loaded for this environment (a secret not set, say): the code
    /// ran without databases and APIs, and this says why.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub without_resources: Option<String>,
}

/// Try `options.message` through its interface. Must be called on a multi-threaded tokio runtime:
/// lookups run on it, the code on a thread of its own (with the inspector, the only way it can).
pub async fn run(options: Options) -> Result<Trial> {
    let env = Environment::load(&options.root, &options.env)?;
    let config =
        crate::load_interfaces(&options.root, &env)?.into_iter().find(|c| c.name == options.interface).with_context(
            || format!("no interface named `{}` in {}", options.interface, options.root.join("interfaces").display()),
        )?;
    if config.responds() {
        bail!(
            "`{}` answers HTTP requests itself (`respond`): `ductus try` runs interfaces whose destinations send messages on; test its function with `ductus test`",
            config.name
        );
    }
    // An interface that uses no database or API can be tried without the environment's secrets:
    // without resources.yaml, its code runs with none, and a lookup fails saying why.
    let (resources, without) = match ductus_core::Resources::load(&options.root, &env) {
        Ok(resources) => (resources, None),
        Err(e) => (Default::default(), Some(format!("{e:#}").replace('\n', " "))),
    };
    let missing = resources.missing_references([&config]);
    if !missing.is_empty() && without.is_none() {
        bail!("{}", missing.join("\n"));
    }
    let heap_mb = ductus_core::settings::Settings::load(&options.root)?.isolates.heap_mb;
    let certificates = crate::tls::Certificates::load(&resources.certificates)?;
    let shared = Arc::new(SharedServices::read_only(env, &resources, &certificates)?);
    let (done, result) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("try".into())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build();
            let tried = match runtime {
                Ok(runtime) => tokio::task::LocalSet::new().block_on(&runtime, tried(config, shared, heap_mb, options)),
                Err(e) => Err(e.into()),
            };
            let tried = tried.map(|trial| Trial { without_resources: without, ..trial });
            let _ = done.send(tried);
        })
        .context("starting the thread that runs the code")?;
    result.await.context("the code's thread ended without an answer")?
}

/// On the code's own thread: load the interface (under a debugger, once one says go) and call
/// each destination's function on the message.
async fn tried(
    config: InterfaceConfig,
    shared: Arc<SharedServices>,
    heap_mb: usize,
    options: Options,
) -> Result<Trial> {
    let calls = Rc::new(RefCell::new(Calls::default()));
    let services = ReplayServices { inner: EngineServices::new(shared, Default::default()), calls: calls.clone() };
    let mut isolate = Isolate::with_inspector(Some(Rc::new(services)), heap_mb, options.inspect.is_some());
    isolate.capture_console();
    isolate.pin_clock()?;
    let message = &options.message;
    let (id, at) = (message.id.unwrap_or(0), message.received_at);
    isolate.set_clock(at.timestamp_millis() as f64, 0)?;

    let mut server = None; // the debugger's, serving until the message is through
    if let (Some(addr), Some(script)) = (options.inspect, config.script_path()) {
        let inspector = Inspector::listen(addr)?;
        let target = inspector.add(Target {
            title: format!("{} (ductus try)", config.name),
            url: ductus_script::module_url(&script)?,
            sessions: isolate.inspector_sessions().context("the isolate has no inspector")?,
            switch: isolate.debug_switch(),
            say: options.say.clone(),
        });
        (options.waiting)(format!("ws://{addr}/{target}"));
        isolate.wait_for_debugger();
        server = Some(inspector);
    }
    let (functions, files) = match config.script_path() {
        None => (None, Ok(Vec::new())),
        Some(path) => {
            let functions = config.functions();
            let functions: Vec<(&str, &str)> = functions.iter().map(|(n, f)| (n.as_str(), f.as_str())).collect();
            let loaded = isolate.add_functions(&path, &functions, config.timeout).await?.1;
            (Some(loaded), isolate.code_files(&path))
        }
    };
    // As a start computes it, so it can be held against the version a record names.
    let code_version = crate::replay::version_of(&options.root, &config, files);
    let loading = isolate.take_console();

    let mut recorded = message.recorded.clone();
    let mut destinations = Vec::new();
    // By name, as the engine makes its workers and the replay runs them (module state sees that
    // sequence).
    for (i, (destination, _)) in config.destinations().enumerate() {
        let (decision, looked_up) = match &functions {
            None => (Decision::Sent { output: message.payload.clone() }, false),
            Some(ids) => {
                isolate.set_clock(at.timestamp_millis() as f64, seed(id, i))?;
                *calls.borrow_mut() = Calls::default();
                let meta = worker::meta(MessageRef { id, received_at: at }, destination, message.origin);
                let result = isolate.call_function(ids[i], &message.payload, Some(&meta)).await;
                let calls = std::mem::take(&mut *calls.borrow_mut());
                match decide(&message.payload, result, calls, config.project_root()) {
                    (Decision::NotComparable { why }, looked) => {
                        let why = why.replace("not made in a replay", "not made by `ductus try`");
                        (Decision::NotComparable { why }, looked)
                    }
                    decided => decided,
                }
            }
        };
        let then = recorded.iter().position(|r| r.destination == destination).map(|at| recorded.remove(at));
        destinations.push(Tried {
            destination: destination.to_string(),
            decision,
            looked_up,
            console: isolate.take_console(),
            recorded: then,
        });
    }
    drop(server);
    Ok(Trial { interface: config.name, code_version, loading, destinations, without_resources: None })
}
