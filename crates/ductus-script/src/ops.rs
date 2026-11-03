//! Native operations interface code can call (through `@ductus/sdk`): database queries, HTTP
//! requests, environment variables and secrets. The engine supplies the implementation as
//! `Services`.

use deno_core::{OpState, op2};
use deno_error::JsErrorBox;
use ductus_core::ConsoleLevel;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = anyhow::Result<T>>>>;

/// Error text marking a failure as temporary (e.g. database unreachable): the engine retries
/// the message instead of quarantining it.
pub const TRANSIENT: &str = "[ductus:transient]";

/// Prefix of a transform result meaning "drop this message"; the rest is the reason.
/// Produced by the SDK's `kill()`.
pub const KILL: &str = "\u{0}ductus:kill\u{0}";

/// Error text marking a deliberate quarantine (`throw quarantine("reason")`).
pub const QUARANTINE: &str = "[ductus:quarantine]";

/// What the engine provides to interface code.
pub trait Services {
    /// Run SQL against a named database resource; rows come back as JSON objects.
    fn db_query(&self, resource: &str, sql: &str, params: Vec<serde_json::Value>) -> BoxFuture<Vec<serde_json::Value>>;
    /// Run a statement; returns the number of rows affected.
    fn db_execute(&self, resource: &str, sql: &str, params: Vec<serde_json::Value>) -> BoxFuture<u64>;
    /// Send an HTTP request. Network errors and timeouts are errors (marked `TRANSIENT`); every
    /// status code, 5xx included, is a response (the SDK decides what to retry).
    fn fetch(&self, request: FetchRequest) -> BoxFuture<FetchResponse>;
    fn variable(&self, name: &str) -> Option<String>;
    fn secret(&self, name: &str) -> anyhow::Result<Option<String>>;
}

/// An HTTP request from interface code (`fetch()` in the SDK).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchRequest {
    /// `api(name)` in the SDK: an API in resources.yaml, whose URL `url` (a path) is under and
    /// whose headers the engine adds.
    #[serde(default)]
    pub api: Option<String>,
    pub url: String,
    pub method: String,
    pub headers: BTreeMap<String, String>,
    pub body: Option<String>,
    /// None: the API's `timeout`, else 30 s.
    pub timeout_ms: Option<u64>,
}

/// The response; header names are lower-case, repeated headers are joined with ", ".
#[derive(Debug, Clone, Serialize)]
pub struct FetchResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: String,
}

fn services(state: &OpState) -> Result<Rc<dyn Services>, JsErrorBox> {
    state
        .try_borrow::<Rc<dyn Services>>()
        .cloned()
        .ok_or_else(|| JsErrorBox::generic("resources are not available here (tests use mocks; see docs)"))
}

fn js_error(e: anyhow::Error) -> JsErrorBox {
    JsErrorBox::generic(format!("{e:#}"))
}

#[op2]
#[serde]
async fn op_ductus_db_query(
    state: Rc<RefCell<OpState>>,
    #[string] resource: String,
    #[string] sql: String,
    #[serde] params: Vec<serde_json::Value>,
) -> Result<Vec<serde_json::Value>, JsErrorBox> {
    let services = services(&state.borrow())?;
    services.db_query(&resource, &sql, params).await.map_err(js_error)
}

#[op2]
#[number]
async fn op_ductus_db_execute(
    state: Rc<RefCell<OpState>>,
    #[string] resource: String,
    #[string] sql: String,
    #[serde] params: Vec<serde_json::Value>,
) -> Result<u64, JsErrorBox> {
    let services = services(&state.borrow())?;
    services.db_execute(&resource, &sql, params).await.map_err(js_error)
}

#[op2]
#[serde]
async fn op_ductus_fetch(
    state: Rc<RefCell<OpState>>,
    #[serde] request: FetchRequest,
) -> Result<FetchResponse, JsErrorBox> {
    let services = services(&state.borrow())?;
    services.fetch(request).await.map_err(js_error)
}

#[op2]
#[string]
fn op_ductus_variable(state: &mut OpState, #[string] name: String) -> Result<Option<String>, JsErrorBox> {
    Ok(services(state)?.variable(&name))
}

#[op2]
#[string]
fn op_ductus_secret(state: &mut OpState, #[string] name: String) -> Result<Option<String>, JsErrorBox> {
    services(state)?.secret(&name).map_err(js_error)
}

/// Where interface code's `console` lines go (kept in the isolate's `OpState`; `Log` with no root
/// and every level when nothing was set).
pub enum Console {
    /// The engine's log (target `script`), each line with the place in the project it was
    /// written from; `lowest` is the environment's `console:`.
    Log { root: Option<crate::errors::Root>, lowest: ConsoleLevel },
    /// Kept for whoever reads them (`ductus test`, per test): every level, no places.
    Captured(String),
}

/// A line's level as ops.js numbers it.
fn console_level(level: u32) -> ConsoleLevel {
    match level {
        0 => ConsoleLevel::Debug,
        1 => ConsoleLevel::Info,
        _ => ConsoleLevel::Warn,
    }
}

/// Whether the log takes a line of this level: the environment's `console:`, then the log's
/// filter (`RUST_LOG`), so a line nobody reads isn't even formatted.
fn logged(lowest: ConsoleLevel, level: ConsoleLevel) -> bool {
    lowest.lets_through(level)
        && match level {
            ConsoleLevel::Debug => tracing::enabled!(target: "script", tracing::Level::DEBUG),
            ConsoleLevel::Info => tracing::enabled!(target: "script", tracing::Level::INFO),
            _ => tracing::enabled!(target: "script", tracing::Level::WARN),
        }
}

/// 0: drop the line; 1: its text; 2: its text and the stack it was written from.
#[op2(fast)]
fn op_ductus_console_wants(state: &mut OpState, level: u32) -> u32 {
    match state.try_borrow::<Console>() {
        Some(Console::Captured(_)) => 1,
        Some(Console::Log { lowest, .. }) => 2 * logged(*lowest, console_level(level)) as u32,
        None => 2 * logged(ConsoleLevel::Debug, console_level(level)) as u32,
    }
}

#[op2(fast)]
fn op_ductus_console(state: &mut OpState, level: u32, #[string] text: &str, #[string] stack: &str) {
    let root = match state.try_borrow_mut::<Console>() {
        Some(Console::Captured(lines)) => {
            lines.push_str(text);
            lines.push('\n');
            return;
        }
        Some(Console::Log { root, .. }) => root.as_ref(),
        None => None,
    };
    // The first frame in project code (the SDK's and ops.js's own aren't `file://`).
    let place = stack
        .lines()
        .filter(|l| !crate::errors::internal(l))
        .find_map(crate::errors::frame)
        .map(|f| {
            let file = root.map(|r| r.relative(&f.url)).unwrap_or(f.url);
            format!("{file}:{}:{}", f.line, f.column)
        })
        .unwrap_or_default();
    match console_level(level) {
        ConsoleLevel::Debug => tracing::debug!(target: "script", at = %place, "{text}"),
        ConsoleLevel::Info => tracing::info!(target: "script", at = %place, "{text}"),
        _ => tracing::warn!(target: "script", at = %place, "{text}"),
    }
}

/// The captured lines since the last take (empty when they go to the log).
#[op2]
#[string]
fn op_ductus_console_take(state: &mut OpState) -> String {
    match state.try_borrow_mut::<Console>() {
        Some(Console::Captured(lines)) => std::mem::take(lines),
        _ => String::new(),
    }
}

deno_core::extension!(
    ductus_ops,
    ops = [
        op_ductus_db_query,
        op_ductus_db_execute,
        op_ductus_fetch,
        op_ductus_variable,
        op_ductus_secret,
        op_ductus_console_wants,
        op_ductus_console,
        op_ductus_console_take
    ],
    esm_entry_point = "ext:ductus_ops/ops.js",
    esm = [dir "src", "ops.js"],
);

/// The ops extension with its JavaScript inside the binary. deno_core records an `esm` file by
/// its path on the build machine and reads it there at every isolate's start (the bytes are
/// only embedded by a startup snapshot), so an installed binary, with no source folder, couldn't
/// start one.
pub fn extension() -> deno_core::Extension {
    let mut extension = ductus_ops::init();
    extension.esm_files = std::borrow::Cow::Owned(vec![deno_core::ExtensionFileSource::new(
        "ext:ductus_ops/ops.js",
        deno_core::ascii_str_include!("ops.js"),
    )]);
    extension
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_extension_reads_nothing_from_disk() {
        let extension = super::extension();
        let files = [&extension.js_files, &extension.esm_files, &extension.lazy_loaded_esm_files];
        for file in files.iter().flat_map(|f| f.iter()) {
            assert!(file.is_runtime_loadable(), "{} would be read from the build machine's disk", file.specifier);
        }
        assert_eq!(extension.esm_files.len(), 1);
    }
}
