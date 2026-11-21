//! HTTP sources. Interfaces sharing a port share one server; each owns a method + path.
//!
//! - `queue` mode: the body is stored like any other message, then 202 `{"id": ...}`.
//! - `respond` mode: the request goes to the interface's `transform` (on its worker thread)
//!   and its result is the response.

use crate::gate::Closed;
use crate::ingest::{Accepted, Intake};
use anyhow::{Context, Result, anyhow, bail};
use axum::Router;
use axum::body::Bytes;
use axum::extract::RawPathParams;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{MethodFilter, on};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

/// A request for a respond-mode interface, answered by its worker.
pub struct HttpCall {
    /// JSON: method, path, params, query, headers, body. The SDK builds `HttpRequest` from it.
    pub envelope: String,
    /// The SDK's response envelope (status, headers, body), or the transform's error.
    pub reply: oneshot::Sender<Result<String>>,
}

#[derive(Clone)]
pub enum Route {
    Queue(Arc<Intake>),
    Respond { interface: String, calls: mpsc::Sender<HttpCall> },
}

#[derive(Clone)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
    pub method: String,
    pub path: String,
    pub route: Route,
    /// Its `tls:` and `allow:` as written: a server whose differ from what an endpoint joining
    /// or changing it says takes the endpoint's (`Server::reroute`).
    pub listen: Listen,
    /// HTTPS (every endpoint on a port has the same: `check_endpoints`).
    pub tls: Option<Arc<rustls::ServerConfig>>,
    /// Who may connect (the port's `allow:`, the same on every endpoint: `check_endpoints`).
    pub admission: crate::tls::Admission,
}

/// An HTTP source's `tls:` and `allow:` as written.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Listen {
    pub tls: Option<ductus_core::tls::ListenTls>,
    pub allow: Option<ductus_core::allow::Allow>,
}

const RESPOND_TIMEOUT: Duration = Duration::from_secs(30);

/// One HTTP server: an address, its routes, and its own stop.
pub struct Server {
    pub host: String,
    pub port: u16,
    /// Every route it answers, each its interface's.
    pub endpoints: Vec<Endpoint>,
    /// The router answering them: each request is answered by the one current when it came, so
    /// one in flight when a deploy in place swaps it finishes on the one it started on.
    router: Arc<std::sync::RwLock<Router>>,
    /// What it accepts connections with now, and the `tls:`/`allow:` that was made from.
    pub accepting: Arc<crate::accepting::Accepting>,
    listen: Listen,
    /// A child of the `stop` it was served with: cancelled alone, only this server stops.
    pub stop: tokio_util::sync::CancellationToken,
    /// Ends once it answered the requests in flight and closed.
    pub task: tokio::task::JoinHandle<()>,
}

impl Route {
    pub(crate) fn interface(&self) -> &str {
        match self {
            Route::Queue(intake) => intake.name(),
            Route::Respond { interface, .. } => interface,
        }
    }
}

/// Start one server per (host, port) with every endpoint's route. On `stop` (or its own) each
/// answers the requests in flight, then closes (respond-mode workers end once nothing can reach
/// them); its task ends then.
pub async fn serve(endpoints: Vec<Endpoint>, stop: &tokio_util::sync::CancellationToken) -> Result<Vec<Server>> {
    let mut servers: BTreeMap<(String, u16), Vec<Endpoint>> = BTreeMap::new();
    for e in endpoints {
        servers.entry((e.host.clone(), e.port)).or_default().push(e);
    }
    let mut started = Vec::with_capacity(servers.len());
    for ((host, port), endpoints) in servers {
        started.push(Server::start(host, port, endpoints, stop).await?);
    }
    Ok(started)
}

/// A router for `endpoints` (all on `port`); two with the same method and path are an error.
fn routes(port: u16, endpoints: &[Endpoint]) -> Result<Router> {
    let mut router = Router::new();
    let mut seen = HashSet::new();
    // Group methods per path: axum wants one route per path.
    let mut by_path: BTreeMap<&str, Vec<(String, &Route)>> = BTreeMap::new();
    for e in endpoints {
        let method = e.method.to_ascii_uppercase();
        if !seen.insert((method.clone(), e.path.as_str())) {
            bail!("two interfaces both handle {method} {} on port {port}", e.path);
        }
        by_path.entry(&e.path).or_default().push((method, &e.route));
    }
    for (path, endpoints) in by_path {
        let mut method_router = axum::routing::MethodRouter::new();
        for (method, route) in endpoints {
            let filter = MethodFilter::try_from(Method::from_bytes(method.as_bytes())?)
                .map_err(|e| anyhow!("unsupported HTTP method: {e}"))?;
            let route = route.clone();
            method_router = method_router.merge(on(
                filter,
                move |params: RawPathParams, method: Method, uri: Uri, headers: HeaderMap, body: Bytes| {
                    handle(route.clone(), params, method, uri, headers, body)
                },
            ));
        }
        router = router.route(path, method_router);
    }
    Ok(router)
}

impl Server {
    /// Listen on `host:port` and answer `endpoints` (their first's TLS and `allow:`), until
    /// `stop` or its own.
    pub async fn start(
        host: String,
        port: u16,
        endpoints: Vec<Endpoint>,
        stop: &tokio_util::sync::CancellationToken,
    ) -> Result<Self> {
        let router = Arc::new(std::sync::RwLock::new(routes(port, &endpoints)?));
        let first = endpoints.first().context("an HTTP server with no routes")?;
        let (tls, admission, listen) = (first.tls.clone(), first.admission.clone(), first.listen.clone());
        let protocol = if tls.is_some() { "HTTPS" } else { "HTTP" };
        let listener =
            crate::systemd::listen(&host, port).await.with_context(|| format!("binding {protocol} {host}:{port}"))?;
        let accepting = crate::accepting::Accepting::new(&format!("{protocol} {host}:{port}"), tls, admission);
        let listener = crate::tls::Listener::with(listener, accepting.clone())?;
        tracing::info!(%host, port, "listening ({protocol})");
        // Every request goes to the router current when it came (`reroute` swaps it).
        let current = router.clone();
        let app = Router::new().fallback(move |request: axum::extract::Request| {
            let router = current.read().unwrap_or_else(|e| e.into_inner()).clone();
            async move {
                use tower::ServiceExt;
                router.oneshot(request).await.unwrap_or_else(|never| match never {})
            }
        });
        let own = stop.child_token();
        let shutdown = own.clone().cancelled_owned();
        let task = tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app).with_graceful_shutdown(shutdown).await {
                tracing::error!(error = %e, "HTTP server stopped");
            }
        });
        Ok(Self { host, port, endpoints, router, accepting, listen, stop: own, task })
    }

    /// The interfaces it routes to.
    pub fn interfaces(&self) -> impl Iterator<Item = &str> {
        self.endpoints.iter().map(|e| e.route.interface())
    }

    /// Answer `endpoints` from now on (none: the caller stops it): requests in flight finish on
    /// the router they started on. When `changed` (an endpoint joining or changed) says other
    /// `tls:`/`allow:` than the server's, it accepts with that one's from now on (every
    /// interface on a port changes them together: `check_together` saw the set it leaves).
    pub fn reroute(&mut self, endpoints: Vec<Endpoint>, changed: Option<&Endpoint>) -> Result<()> {
        let router = routes(self.port, &endpoints)?;
        *self.router.write().unwrap_or_else(|e| e.into_inner()) = router;
        self.endpoints = endpoints;
        if let Some(e) = changed.filter(|e| e.listen != self.listen) {
            let closed = self.accepting.swap(e.tls.clone(), e.admission.clone());
            tracing::info!(host = %self.host, port = self.port, closed, "its TLS or allow: changed in place");
            self.listen = e.listen.clone();
            // One admission per port, as at a start (what `allow::on_port` finds counts refusals).
            for other in &mut self.endpoints {
                (other.tls, other.admission, other.listen) = (e.tls.clone(), e.admission.clone(), e.listen.clone());
            }
        }
        Ok(())
    }
}

fn json(status: StatusCode, body: serde_json::Value) -> Response {
    (status, [("content-type", "application/json")], body.to_string()).into_response()
}

async fn handle(
    route: Route,
    params: RawPathParams,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    match route {
        Route::Queue(intake) => match intake.accept(&body).await {
            Ok(Accepted::Queued(m)) => json(StatusCode::ACCEPTED, serde_json::json!({ "id": m.id })),
            Ok(Accepted::Duplicate) => json(StatusCode::OK, serde_json::json!({ "duplicate": true })),
            // Received and kept: the sender's part is done (sending again would quarantine again).
            Ok(Accepted::Quarantined(m)) => {
                json(StatusCode::ACCEPTED, serde_json::json!({ "id": m.id, "quarantined": true }))
            }
            Err(e) if e.is::<Closed>() => {
                let mut response = json(StatusCode::SERVICE_UNAVAILABLE, serde_json::json!({ "error": e.to_string() }));
                response.headers_mut().insert("retry-after", axum::http::HeaderValue::from_static("30"));
                response
            }
            Err(e) => {
                tracing::error!(interface = %intake.name(), error = %e, "failed to store message");
                json(
                    StatusCode::SERVICE_UNAVAILABLE,
                    serde_json::json!({ "error": "temporarily unable to store message" }),
                )
            }
        },
        Route::Respond { interface, calls } => {
            let query: HashMap<String, String> = uri
                .query()
                .map(|q| url::form_urlencoded::parse(q.as_bytes()).into_owned().collect())
                .unwrap_or_default();
            let headers: HashMap<String, String> = headers
                .iter()
                .filter_map(|(k, v)| v.to_str().ok().map(|v| (k.as_str().to_ascii_lowercase(), v.to_string())))
                .collect();
            let params: HashMap<&str, &str> = params.iter().collect();
            let envelope = serde_json::json!({
                "method": method.as_str(),
                "path": uri.path(),
                "params": params,
                "query": query,
                "headers": headers,
                "body": String::from_utf8_lossy(&body),
            })
            .to_string();

            let (reply, answer) = oneshot::channel();
            if calls.send(HttpCall { envelope, reply }).await.is_err() {
                return json(StatusCode::SERVICE_UNAVAILABLE, serde_json::json!({ "error": "interface not running" }));
            }
            match tokio::time::timeout(RESPOND_TIMEOUT, answer).await {
                Ok(Ok(Ok(envelope))) => response_from(&envelope),
                Ok(Ok(Err(e))) => error_response(&interface, &format!("{e:#}")),
                Ok(Err(_)) => json(StatusCode::INTERNAL_SERVER_ERROR, serde_json::json!({ "error": "no response" })),
                Err(_) => json(StatusCode::GATEWAY_TIMEOUT, serde_json::json!({ "error": "timed out" })),
            }
        }
    }
}

/// Build the HTTP response from the SDK's envelope: `{"status", "headers", "body"}`.
fn response_from(envelope: &str) -> Response {
    #[derive(serde::Deserialize)]
    struct Envelope {
        status: u16,
        headers: HashMap<String, String>,
        body: String,
    }
    let Ok(e) = serde_json::from_str::<Envelope>(envelope) else {
        return json(
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({ "error": "invalid response from transform" }),
        );
    };
    let mut response = (StatusCode::from_u16(e.status).unwrap_or(StatusCode::OK), e.body).into_response();
    for (k, v) in e.headers {
        if let (Ok(k), Ok(v)) = (k.parse::<axum::http::HeaderName>(), v.parse::<axum::http::HeaderValue>()) {
            response.headers_mut().insert(k, v);
        }
    }
    response
}

/// Transform errors: deliberate quarantine -> 422 with the reason; temporary -> 503;
/// anything else -> 500 (details only in the log, never sent to the caller).
fn error_response(interface: &str, error: &str) -> Response {
    if let Some((_, reason)) = error.split_once(ductus_script::QUARANTINE) {
        let reason = reason.lines().next().unwrap_or("").trim();
        return json(StatusCode::UNPROCESSABLE_ENTITY, serde_json::json!({ "error": reason }));
    }
    if error.contains(ductus_script::TRANSIENT) {
        tracing::warn!(%interface, %error, "temporary failure answering request");
        return json(StatusCode::SERVICE_UNAVAILABLE, serde_json::json!({ "error": "temporarily unavailable" }));
    }
    tracing::error!(%interface, %error, "transform failed answering request");
    json(StatusCode::INTERNAL_SERVER_ERROR, serde_json::json!({ "error": "internal error" }))
}
