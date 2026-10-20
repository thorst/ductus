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

pub struct Endpoint {
    pub host: String,
    pub port: u16,
    pub method: String,
    pub path: String,
    pub route: Route,
    /// HTTPS (every endpoint on a port has the same: `check_endpoints`).
    pub tls: Option<Arc<rustls::ServerConfig>>,
}

const RESPOND_TIMEOUT: Duration = Duration::from_secs(30);

/// Start one server per (host, port) with every endpoint's route. On `stop` each answers the
/// requests in flight, then closes (respond-mode workers end once nothing can reach them).
pub async fn serve(endpoints: Vec<Endpoint>, stop: tokio_util::sync::CancellationToken) -> Result<()> {
    let mut servers: BTreeMap<(String, u16), Vec<Endpoint>> = BTreeMap::new();
    for e in endpoints {
        servers.entry((e.host.clone(), e.port)).or_default().push(e);
    }
    for ((host, port), endpoints) in servers {
        let tls = endpoints.first().and_then(|e| e.tls.clone());
        let mut router = Router::new();
        let mut seen = HashSet::new();
        // Group methods per path: axum wants one route per path.
        let mut by_path: BTreeMap<String, Vec<Endpoint>> = BTreeMap::new();
        for e in endpoints {
            let method = e.method.to_ascii_uppercase();
            if !seen.insert((method.clone(), e.path.clone())) {
                bail!("two interfaces both handle {method} {} on port {port}", e.path);
            }
            by_path.entry(e.path.clone()).or_default().push(Endpoint { method, ..e });
        }
        for (path, endpoints) in by_path {
            let mut method_router = axum::routing::MethodRouter::new();
            for e in endpoints {
                let filter = MethodFilter::try_from(Method::from_bytes(e.method.as_bytes())?)
                    .map_err(|e| anyhow!("unsupported HTTP method: {e}"))?;
                let route = e.route.clone();
                method_router = method_router.merge(on(
                    filter,
                    move |params: RawPathParams, method: Method, uri: Uri, headers: HeaderMap, body: Bytes| {
                        handle(route.clone(), params, method, uri, headers, body)
                    },
                ));
            }
            router = router.route(&path, method_router);
        }
        let protocol = if tls.is_some() { "HTTPS" } else { "HTTP" };
        let listener = tokio::net::TcpListener::bind((host.as_str(), port))
            .await
            .with_context(|| format!("binding {protocol} {host}:{port}"))?;
        let listener = crate::tls::Listener::new(listener, tls, &format!("{protocol} {host}:{port}"))?;
        tracing::info!(%host, port, "listening ({protocol})");
        let stop = stop.clone();
        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, router).with_graceful_shutdown(stop.cancelled_owned()).await {
                tracing::error!(error = %e, "HTTP server stopped");
            }
        });
    }
    Ok(())
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
            Err(e) if e.is::<Closed>() => {
                let mut response = json(StatusCode::SERVICE_UNAVAILABLE, serde_json::json!({ "error": e.to_string() }));
                response.headers_mut().insert("retry-after", axum::http::HeaderValue::from_static("30"));
                response
            }
            Err(e) => {
                tracing::error!(interface = %intake.config.name, error = %e, "failed to store message");
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
