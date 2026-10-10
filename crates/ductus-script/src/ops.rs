//! Native operations interface code can call (through `@ductus/sdk`): database queries, HTTP
//! requests, environment variables and secrets. The engine supplies the implementation as
//! `Services`.

use deno_core::{OpState, op2};
use deno_error::JsErrorBox;
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

deno_core::extension!(
    ductus_ops,
    ops = [op_ductus_db_query, op_ductus_db_execute, op_ductus_fetch, op_ductus_variable, op_ductus_secret],
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
