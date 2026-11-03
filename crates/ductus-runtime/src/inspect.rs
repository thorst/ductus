//! The inspector server (0.7.2): debuggers (VS Code, Chrome DevTools, JetBrains, anything that
//! speaks the Chrome DevTools Protocol) attach to interface code here, with no extension.
//!
//! What a client expects of a V8 program, as Node and Deno give it: `GET /json/version`,
//! `GET /json/list` (one target per inspected isolate) and a WebSocket per target at its
//! `webSocketDebuggerUrl`. Each WebSocket becomes an inspector session on the isolate's thread
//! (deno_core's session channels): the protocol's messages pass through as they are.
//!
//! **Loopback only.** A debugger runs any code it likes as the engine's user and reads every
//! value: an address other than 127.0.0.1 or ::1 is refused, with how to reach a remote machine
//! instead (SSH or VS Code Remote's port forwarding).
//!
//! While a client is attached to an isolate, its time limit is off (`DebugSwitch`): a paused call
//! would otherwise be stopped as a runaway the moment it resumed. It applies again from the moment
//! the last client detaches.

use anyhow::{Context, Result, bail};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path as UrlPath, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use ductus_script::{DebugSwitch, InspectorSessions};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

/// The address `--inspect` listens on without one: what VS Code and Chrome look at first.
pub const DEFAULT: &str = "127.0.0.1:9229";

/// `--inspect[=ADDR]` read: `9229`, `127.0.0.1:9229`, `[::1]:9229`, `localhost:9229`. Anything
/// that isn't this machine is refused.
pub fn address(text: &str) -> Result<SocketAddr> {
    let text = text.trim();
    let with_host = if text.chars().all(|c| c.is_ascii_digit()) { format!("127.0.0.1:{text}") } else { text.into() };
    let with_host = with_host.replacen("localhost:", "127.0.0.1:", 1);
    let addr: SocketAddr = with_host.parse().with_context(|| {
        format!("`--inspect={text}`: write a port (9229) or this machine's address and a port (127.0.0.1:9229)")
    })?;
    if !addr.ip().is_loopback() {
        bail!(
            "`--inspect={text}`: the debugger listens on this machine only (127.0.0.1 or ::1): whoever reaches it runs any code as this user and reads every message. To debug from another machine, forward the port: `ssh -L 9229:127.0.0.1:9229 <this machine>`, or VS Code Remote's port forwarding"
        );
    }
    Ok(addr)
}

/// One isolate a debugger can attach to.
pub struct Target {
    /// What a client lists it as: `adt-in tests`.
    pub title: String,
    /// The `file://` URL of its main file (a client shows it; VS Code matches it to the workspace).
    pub url: String,
    pub sessions: InspectorSessions,
    pub switch: DebugSwitch,
    /// Says what happened to it (`attached`, `detached`), where the program says things: stderr
    /// for `ductus test` (its stdout may be `--json`), the log for an engine.
    pub say: Arc<dyn Fn(Attach) + Send + Sync>,
}

/// What happened to a target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attach {
    /// A debugger attached: the time limit is off.
    Attached,
    /// The last one detached: the time limit applies again.
    Detached,
}

struct Served {
    id: String,
    target: Target,
}

struct Shared {
    addr: SocketAddr,
    targets: Mutex<Vec<Arc<Served>>>,
}

/// The running server. Dropping it stops it.
pub struct Inspector {
    shared: Arc<Shared>,
    _stop: tokio::sync::oneshot::Sender<()>,
}

impl Inspector {
    /// Listen on `addr` (from `address`), serving on a thread of its own: the isolate's thread
    /// blocks while it waits for a client and while it's paused.
    pub fn listen(addr: SocketAddr) -> Result<Self> {
        let listener = std::net::TcpListener::bind(addr).map_err(|e| match e.kind() {
            std::io::ErrorKind::AddrInUse => anyhow::anyhow!(
                "the debugger's port {addr} is in use (another `--inspect` still running, or a Node or Deno debugger): stop it, or give another port with `--inspect=127.0.0.1:9230`"
            ),
            _ => anyhow::anyhow!("the debugger's port {addr}: {e}"),
        })?;
        listener.set_nonblocking(true)?;
        let shared = Arc::new(Shared { addr, targets: Mutex::new(Vec::new()) });
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let state = shared.clone();
        std::thread::Builder::new()
            .name("inspector".into())
            .spawn(move || {
                let runtime =
                    tokio::runtime::Builder::new_current_thread().enable_all().build().expect("inspector runtime");
                runtime.block_on(async move {
                    let listener = tokio::net::TcpListener::from_std(listener).expect("inspector listener");
                    let app = Router::new()
                        .route("/json/version", get(version))
                        .route("/json", get(list))
                        .route("/json/list", get(list))
                        .route("/{id}", get(socket))
                        .with_state(state);
                    let _ = axum::serve(listener, app).with_graceful_shutdown(async { drop(stopped.await) }).await;
                });
            })
            .context("starting the debugger's thread")?;
        Ok(Self { shared, _stop: stop })
    }

    /// Offer `target` to debuggers; its id is in its URL.
    pub fn add(&self, target: Target) -> String {
        let id = new_id();
        self.shared.targets.lock().unwrap_or_else(|e| e.into_inner()).push(Arc::new(Served { id: id.clone(), target }));
        id
    }

    pub fn addr(&self) -> SocketAddr {
        self.shared.addr
    }
}

/// A target id as Node's look (a UUID's shape), unique enough for one machine's debuggers.
fn new_id() -> String {
    use sha2::Digest;
    static COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
    let count = COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let digest = sha2::Sha256::digest(format!("{n} {} {count}", std::process::id()));
    let h: String = digest.iter().take(16).map(|b| format!("{b:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

async fn version() -> Json<serde_json::Value> {
    Json(json!({ "Browser": format!("Ductus/{}", env!("CARGO_PKG_VERSION")), "Protocol-Version": "1.3" }))
}

async fn list(State(shared): State<Arc<Shared>>) -> Json<serde_json::Value> {
    let targets = shared.targets.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let host = shared.addr;
    Json(serde_json::Value::Array(
        targets
            .iter()
            .map(|t| {
                let ws = format!("{host}/{}", t.id);
                json!({
                    "description": "ductus",
                    "devtoolsFrontendUrl": format!("devtools://devtools/bundled/js_app.html?experiments=true&v8only=true&ws={ws}"),
                    "id": t.id,
                    "title": t.target.title,
                    "type": "node",
                    "url": t.target.url,
                    "webSocketDebuggerUrl": format!("ws://{ws}"),
                })
            })
            .collect(),
    ))
}

async fn socket(
    UrlPath(id): UrlPath<String>,
    State(shared): State<Arc<Shared>>,
    upgrade: WebSocketUpgrade,
) -> Response {
    let found = shared.targets.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|t| t.id == id).cloned();
    match found {
        Some(served) => upgrade.on_upgrade(move |ws| bridge(ws, served)),
        None => (axum::http::StatusCode::NOT_FOUND, "no such target: GET /json/list names them").into_response(),
    }
}

/// One client: its WebSocket to and from a new session on the isolate's thread.
async fn bridge(ws: WebSocket, served: Arc<Served>) {
    let target = &served.target;
    let Some((mut from_isolate, to_isolate)) = target.sessions.open() else { return };
    let first = !target.switch.is_attached();
    target.switch.attached();
    if first {
        (target.say)(Attach::Attached);
    }
    let (mut out, mut incoming) = ws.split();
    let send = async move {
        while let Some(message) = from_isolate.next().await {
            if out.send(Message::Text(message.into())).await.is_err() {
                break;
            }
        }
    };
    let receive = async {
        while let Some(Ok(message)) = incoming.next().await {
            match message {
                Message::Text(text) if to_isolate.send(text.to_string()) => {}
                Message::Text(_) | Message::Close(_) => break,
                _ => {}
            }
        }
    };
    tokio::select! { _ = send => {}, _ = receive => {} }
    // The session ends with `to_isolate`: V8 lets a paused call go on, without breakpoints.
    drop(to_isolate);
    target.switch.detached();
    if !target.switch.is_attached() {
        (target.say)(Attach::Detached);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_debugger_listens_on_this_machine_only() {
        assert_eq!(address("9229").unwrap().to_string(), "127.0.0.1:9229");
        assert_eq!(address("localhost:9230").unwrap().to_string(), "127.0.0.1:9230");
        assert_eq!(address("[::1]:9229").unwrap().to_string(), "[::1]:9229");
        let refused = address("0.0.0.0:9229").unwrap_err().to_string();
        assert!(refused.contains("this machine only") && refused.contains("ssh -L 9229:127.0.0.1:9229"), "{refused}");
        assert!(address("10.1.2.3:9229").is_err());
        assert!(address("lab-server:9229").unwrap_err().to_string().contains("write a port"));
    }
}
