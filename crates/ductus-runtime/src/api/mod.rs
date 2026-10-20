//! The REST API, `/api/v1`: everything the web UI shows and does goes through it, so anything
//! the UI can do, a script can too. The OpenAPI document (`/api/v1/openapi.json`) is generated
//! from the same handler declarations the router is built from (`utoipa_axum::routes!`), so the
//! spec can't drift from the code.
//!
//! Every call but `health` and `openapi.json` needs a token (`Authorization: Bearer dux_...`,
//! made with `ductus token create`) or a browser session (`session.rs`: a user's, signed in with a
//! password, or a token's). Roles: viewer reads, operator also drains, pauses, resends and
//! corrects messages and runs connection checks, admin also manages users and tokens. Two
//! permissions go beyond a role: `payloads` (read message payloads) and `audit`.
//!
//! The same listener serves the web UI (`ui.rs`) at every path outside `/api`.

mod actions;
mod alerts;
mod audit;
mod certificates;
mod hotspots;
mod me;
mod messages;
mod metrics;
mod reports;
mod retention;
mod session;
mod status;
mod tokens;
mod ui;
mod users;

use crate::control::Engine;
use axum::extract::{ConnectInfo, FromRequestParts, State};
use axum::http::request::Parts;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use ductus_store::{Audit, By, Principal, Role};
use serde::Serialize;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use utoipa::openapi::security::{ApiKey, ApiKeyValue, HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi, ToSchema};
use utoipa_axum::router::OpenApiRouter;

pub use audit::AuditEntry;

pub type Shared = Arc<Engine>;

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Ductus API",
        version = "1",
        description = "Status, messages, drain mode and reports of a running Ductus engine. Every call but `health` needs `Authorization: Bearer <token>` (create one with `ductus token create`). Browsers sign in instead (`POST /session`) and send the session cookie; their changes also carry `X-Ductus: 1`."
    ),
    servers((url = "/api/v1")),
    modifiers(&BearerToken),
    security(("token" = []), ("session" = [])),
    tags(
        (name = "engine", description = "The engine, its interfaces and live updates"),
        (name = "session", description = "Signing a browser in and out"),
        (name = "me", description = "The signed-in user's own account: password, sessions, tokens, appearance"),
        (name = "users", description = "People who sign in (admin)"),
        (name = "messages", description = "The message browser, the quarantine, and resending or correcting messages"),
        (name = "drain", description = "Stop taking messages and deliver what's stored"),
        (name = "reports", description = "Impact, owners and connection checks"),
        (name = "alerts", description = "Alerts: what is wrong, acknowledging and silencing, and where notifications go"),
        (name = "audit", description = "Who did what: the audit log (the `audit` permission) and your own activity"),
        (name = "retention", description = "How long messages are kept, and changes to it"),
        (name = "tokens", description = "API tokens (admin)")
    )
)]
struct ApiDoc;

struct BearerToken;

impl Modify for BearerToken {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi.components.get_or_insert_default();
        let scheme = HttpBuilder::new().scheme(HttpAuthScheme::Bearer).bearer_format("dux_...").build();
        components.add_security_scheme("token", SecurityScheme::Http(scheme));
        let cookie = ApiKey::Cookie(ApiKeyValue::new(session::COOKIE));
        components.add_security_scheme("session", SecurityScheme::ApiKey(cookie));
    }
}

fn parts() -> (axum::Router<Shared>, utoipa::openapi::OpenApi) {
    OpenApiRouter::with_openapi(ApiDoc::openapi())
        .merge(status::routes())
        .merge(session::routes())
        .merge(me::routes())
        .merge(users::routes())
        .merge(hotspots::routes())
        .merge(messages::routes())
        .merge(actions::routes())
        .merge(alerts::routes())
        .merge(audit::routes())
        .merge(certificates::routes())
        .merge(metrics::routes())
        .merge(reports::routes())
        .merge(retention::routes())
        .merge(tokens::routes())
        .split_for_parts()
}

/// The OpenAPI document, without an engine (`ductus api-spec`: the web UI's types are made
/// from it).
pub fn openapi() -> utoipa::openapi::OpenApi {
    parts().1
}

/// The router (`/api/v1`, and the web UI at every other path) and its OpenAPI document.
pub fn router(engine: Shared) -> (Router, utoipa::openapi::OpenApi) {
    let (api, doc) = parts();
    let spec = doc.clone();
    let api = api
        .route("/openapi.json", axum::routing::get(move || async move { Json(spec) }))
        .fallback(|| async { ApiError::not_found("no such API path (the reference is at /api/v1/openapi.json)") });
    (Router::new().nest("/api/v1", api.with_state(engine)).fallback(ui::serve), doc)
}

/// Serve the API at `host:port` (HTTPS with `tls`) until the engine shuts down. The task ends
/// once the requests in flight have been answered: the one that asked for the shutdown (a drain
/// with `stop`) gets its answer before the process exits.
pub async fn serve(
    engine: Shared,
    host: &str,
    port: u16,
    tls: Option<Arc<rustls::ServerConfig>>,
) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    use anyhow::Context;
    use axum::serve::ListenerExt;
    let listener = tokio::net::TcpListener::bind((host, port)).await.with_context(|| {
        format!("binding the REST API to {host}:{port} (`api.listen` in ductus.yaml; `off` to run without it)")
    })?;
    let https = tls.is_some();
    // `tap_io` gives the listener axum's connect info (the caller's address, for the audit log).
    let listener = crate::tls::Listener::new(listener, tls, "the REST API")?.tap_io(|_| {});
    tracing::info!(%host, port, https, "REST API listening (/api/v1)");
    let shutdown = engine.shutdown.clone();
    let (mut router, _) = router(engine);
    if https {
        // Browsers that have seen HTTPS here never try plain HTTP again (a year).
        router = router.layer(axum::middleware::map_response(|mut response: axum::response::Response| async {
            response.headers_mut().insert(
                axum::http::header::STRICT_TRANSPORT_SECURITY,
                axum::http::HeaderValue::from_static("max-age=31536000"),
            );
            response
        }));
    }
    Ok(tokio::spawn(async move {
        let app = router.into_make_service_with_connect_info::<SocketAddr>();
        if let Err(e) = axum::serve(listener, app).with_graceful_shutdown(shutdown.cancelled_owned()).await {
            tracing::error!(error = %e, "REST API stopped");
        }
    }))
}

/// An error answer: `{"error": "..."}` with a status.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    message: String,
}

#[derive(Serialize, ToSchema)]
pub struct ErrorBody {
    /// What went wrong, in plain words.
    error: String,
}

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self { status, message: message.into() }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, message)
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        let message = format!("{e:#}");
        tracing::error!(error = %message, "API call failed");
        // Callers are authenticated operators of this engine: the reason helps them more than it
        // could help anyone else.
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(ErrorBody { error: self.message })).into_response()
    }
}

pub type ApiResult<T> = Result<Json<T>, ApiError>;

/// Who is calling: a user (signed in with a password) or a token, directly or through a
/// browser's session (`session` is then when it ends, and `cookie` its secret).
pub struct Caller {
    pub who: Principal,
    pub session: Option<DateTime<Utc>>,
    pub cookie: Option<String>,
    /// How they came (`session`: the web UI; `token`: a bearer token), for the audit log.
    pub via: &'static str,
    /// The address the connection came from (a proxy's, when there is one).
    pub ip: Option<IpAddr>,
}

impl Caller {
    /// Who this is, for the audit log.
    pub fn by(&self) -> By<'_> {
        match (self.who.token_id, self.who.user_id) {
            (Some(token), owner) => By::token(token, owner, &self.who.name, self.via, self.ip),
            (None, Some(user)) => By::user(user, &self.who.name, self.via, self.ip),
            (None, None) => By::anonymous(self.ip),
        }
    }

    /// A row for the audit log about something this caller did.
    pub fn audit<'a>(&'a self, action: &'a str) -> Audit<'a> {
        Audit::new(self.by(), action)
    }

    /// Refuse unless the caller's role is at least `role`.
    pub fn require(&self, role: Role) -> Result<(), ApiError> {
        if self.who.role >= role {
            return Ok(());
        }
        Err(ApiError::new(
            StatusCode::FORBIDDEN,
            format!("`{}` is a {}; this needs {}", self.who.name, self.who.role.label(), role.label()),
        ))
    }

    /// Refuse unless the caller was given `permission` (admins have them all).
    pub fn require_permission(&self, permission: &str) -> Result<(), ApiError> {
        if self.who.can(permission) {
            return Ok(());
        }
        Err(ApiError::new(
            StatusCode::FORBIDDEN,
            format!("`{}` doesn't have the `{permission}` permission: ask an admin for it", self.who.name),
        ))
    }

    pub fn name(&self) -> &str {
        &self.who.name
    }

    /// The signed-in person, for what only a person has (a password, sessions, settings).
    pub fn user_id(&self) -> Result<i32, ApiError> {
        self.who.user_id.filter(|_| self.who.token_id.is_none()).ok_or_else(|| {
            ApiError::new(
                StatusCode::FORBIDDEN,
                "this is for people signed in with a username and password, not tokens",
            )
        })
    }
}

/// Write an audit row before doing what it says (for actions that have no database transaction of
/// their own: a pause, a drain). If the log can't be written the action is refused: nothing
/// happens that the log doesn't know about.
pub async fn record_first(engine: &Shared, audit: Audit<'_>) -> Result<(), ApiError> {
    engine.store.record(&audit).await.map(|_| ()).map_err(|e| {
        tracing::error!(action = audit.action, error = %format!("{e:#}"), "audit log write failed: refusing the action");
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "the audit log can't be written, so this was not done: check the database, then try again",
        )
    })
}

/// A caller whose password an admin chose and who hasn't changed it yet: only changing it (and
/// saying who they are, or signing out) works. Every other call answers 403 `password_change_required`.
pub struct MustChange(pub Caller);

/// A signed-in browser's requests that change something must carry this header. Other sites
/// can't send it without the engine agreeing first (CORS, which it never does), so a page
/// elsewhere can't use the session cookie to act. Scripts using a token don't need it.
pub const BROWSER_HEADER: &str = "x-ductus";

/// The address the connection came from.
fn peer(parts: &Parts) -> Option<IpAddr> {
    parts.extensions.get::<ConnectInfo<SocketAddr>>().map(|c| c.0.ip())
}

/// `Authorization: Bearer ...`, if given.
fn bearer(parts: &Parts) -> Option<&str> {
    let value = parts.headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    value.strip_prefix("Bearer ").or_else(|| value.strip_prefix("bearer ")).map(str::trim)
}

async fn identify(parts: &Parts, engine: &Shared) -> Result<Caller, ApiError> {
    let unauthorized = |message: &str| {
        ApiError::new(StatusCode::UNAUTHORIZED, format!("{message} (create a token with `ductus token create`)"))
    };
    let ip = peer(parts);
    if let Some(secret) = bearer(parts) {
        return match engine.store.authenticate(secret).await? {
            Some(who) => Ok(Caller { who, session: None, cookie: None, via: "token", ip }),
            None => Err(unauthorized("unknown or revoked token")),
        };
    }
    let Some(secret) = session::cookie(parts) else {
        return Err(unauthorized("missing `Authorization: Bearer <token>`"));
    };
    if !parts.method.is_safe() && !parts.headers.contains_key(BROWSER_HEADER) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "a change made with a session cookie needs the `X-Ductus: 1` header",
        ));
    }
    match engine.store.authenticate_session(secret, session::limits(engine)).await? {
        Some((who, ends)) => {
            Ok(Caller { who, session: Some(ends), cookie: Some(secret.to_string()), via: "session", ip })
        }
        None => Err(ApiError::new(StatusCode::UNAUTHORIZED, "the session has ended: sign in again")),
    }
}

impl FromRequestParts<Shared> for Caller {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, engine: &Shared) -> Result<Self, ApiError> {
        let caller = identify(parts, engine).await?;
        if caller.who.must_change_password {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "password_change_required: choose a new password first (POST /me/password)",
            ));
        }
        Ok(caller)
    }
}

impl FromRequestParts<Shared> for MustChange {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, engine: &Shared) -> Result<Self, ApiError> {
        identify(parts, engine).await.map(MustChange)
    }
}

/// `State` for handlers, shorter.
pub type Eng = State<Shared>;

#[cfg(test)]
mod tests {
    /// A parameter struct without `#[into_params(parameter_in = Query)]` is published as path
    /// parameters (the message browser's `status` once was): clients generated from the
    /// document then can't call it.
    #[test]
    fn the_document_has_path_parameters_only_where_the_path_has_them() {
        let doc = serde_json::to_value(super::openapi()).unwrap();
        let mut checked = 0;
        for (path, item) in doc["paths"].as_object().unwrap() {
            for op in item.as_object().unwrap().values() {
                for p in op["parameters"].as_array().into_iter().flatten() {
                    let name = p["name"].as_str().unwrap();
                    let in_path = path.contains(&format!("{{{name}}}"));
                    assert_eq!(p["in"] == "path", in_path, "{path}: parameter `{name}` is in {}", p["in"]);
                    checked += 1;
                }
            }
        }
        assert!(checked > 10, "only {checked} parameters found: did the document's shape change?");
    }

    /// Who may call what, in one place. Every operation in the API document has a row (and the
    /// other way round), so an endpoint can't ship without someone deciding its access:
    /// `public` needs nothing, `signed` any signed-in caller (user or token), `user` a person
    /// signed in with a password, `operator` and `admin` that role at least, `audit` the `audit`
    /// permission (admins have it). Behaviour is covered
    /// by the tests that call the engine (`cargo test -p ductus --test cli`, `demo-ui.sh`).
    const ACCESS: &[(&str, &str, &str)] = &[
        ("GET", "/health", "public"),
        ("POST", "/session", "public"),
        ("DELETE", "/session", "public"),
        ("GET", "/session", "signed"),
        ("GET", "/engine", "signed"),
        ("GET", "/events", "signed"),
        ("GET", "/metrics", "signed"),
        ("GET", "/interfaces", "signed"),
        ("GET", "/interfaces/{name}", "signed"),
        ("GET", "/interfaces/{name}/messages", "signed"),
        ("GET", "/messages/{id}", "signed"),
        ("GET", "/quarantine", "signed"),
        ("GET", "/hotspots", "signed"),
        ("GET", "/impact", "signed"),
        ("GET", "/owners", "signed"),
        ("GET", "/owners/people", "signed"),
        ("POST", "/owners/reassign/preview", "signed"),
        ("GET", "/docs", "signed"),
        ("GET", "/retention", "signed"),
        ("GET", "/drain", "signed"),
        ("POST", "/drain", "operator"),
        ("DELETE", "/drain", "operator"),
        ("POST", "/doctor", "operator"),
        ("GET", "/doctor/runs", "operator"),
        ("GET", "/doctor/runs/{id}", "operator"),
        ("GET", "/certificates", "signed"),
        ("POST", "/interfaces/{name}/destinations/{destination}/pause", "operator"),
        ("DELETE", "/interfaces/{name}/destinations/{destination}/pause", "operator"),
        ("POST", "/messages/{id}/resend", "operator"),
        ("POST", "/messages/resend", "operator"),
        ("POST", "/messages/{id}/edit", "operator"),
        ("POST", "/messages/{id}/dismiss", "operator"),
        ("POST", "/messages/dismiss", "operator"),
        ("GET", "/alerts", "signed"),
        ("GET", "/alerts/{id}", "signed"),
        ("POST", "/alerts/{id}/ack", "operator"),
        ("POST", "/alerts/{id}/silence", "operator"),
        ("DELETE", "/alerts/{id}/silence", "operator"),
        ("GET", "/alerts/channels", "operator"),
        ("POST", "/alerts/channels/{name}/test", "operator"),
        ("GET", "/audit", "audit"),
        ("GET", "/me/activity", "user"),
        ("POST", "/retention/{name}/confirm", "operator"),
        ("GET", "/me", "user"),
        ("PATCH", "/me", "user"),
        ("POST", "/me/password", "user"),
        ("GET", "/me/sessions", "user"),
        ("DELETE", "/me/sessions", "user"),
        ("DELETE", "/me/sessions/{id}", "user"),
        ("GET", "/me/settings", "user"),
        ("PUT", "/me/settings", "user"),
        ("GET", "/me/tokens", "user"),
        ("POST", "/me/tokens", "user"),
        ("DELETE", "/me/tokens/{name}", "user"),
        ("GET", "/tokens", "admin"),
        ("POST", "/tokens", "admin"),
        ("DELETE", "/tokens/{name}", "admin"),
        ("GET", "/users", "admin"),
        ("POST", "/users", "admin"),
        ("PATCH", "/users/{id}", "admin"),
        ("POST", "/users/{id}/password", "admin"),
        ("POST", "/users/{id}/unlock", "admin"),
    ];

    #[test]
    fn every_operation_has_a_decided_access() {
        let doc = serde_json::to_value(super::openapi()).unwrap();
        let mut in_doc = std::collections::BTreeSet::new();
        for (path, item) in doc["paths"].as_object().unwrap() {
            for (method, op) in item.as_object().unwrap() {
                let public = op["security"] == serde_json::json!([{}]);
                in_doc.insert((method.to_uppercase(), path.clone(), public));
            }
        }
        let in_table: std::collections::BTreeSet<_> =
            ACCESS.iter().map(|(m, p, a)| (m.to_string(), p.to_string(), *a == "public")).collect();
        let missing: Vec<_> = in_doc.difference(&in_table).collect();
        let stale: Vec<_> = in_table.difference(&in_doc).collect();
        assert!(
            missing.is_empty() && stale.is_empty(),
            "add a row to ACCESS (in api/mod.rs tests) for {missing:?}, and drop rows for {stale:?} \
             (a public row must be the operations the document marks `security: []`)"
        );
        for (method, path, access) in ACCESS {
            assert!(
                ["public", "signed", "user", "operator", "admin", "audit"].contains(access),
                "{method} {path}: {access}"
            );
        }
    }

    /// utoipa names an operation after its handler, and two modules' `list` once shared one
    /// id: the UI's generated types then gave `GET /tokens` the message browser's answer.
    /// Name a clashing handler's operation with `operation_id = "..."`.
    #[test]
    fn every_operation_has_its_own_id() {
        let doc = serde_json::to_value(super::openapi()).unwrap();
        let mut seen = std::collections::HashMap::new();
        for (path, item) in doc["paths"].as_object().unwrap() {
            for (method, op) in item.as_object().unwrap() {
                let id = op["operationId"].as_str().unwrap();
                if let Some(other) = seen.insert(id.to_string(), format!("{method} {path}")) {
                    panic!("operationId `{id}` is used by {other} and {method} {path}");
                }
            }
        }
        assert!(seen.len() > 20, "only {} operations found", seen.len());
    }
}
