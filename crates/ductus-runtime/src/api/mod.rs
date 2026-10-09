//! The REST API, `/api/v1`: everything the web UI shows and does goes through it, so anything
//! the UI can do, a script can too. The OpenAPI document (`/api/v1/openapi.json`) is generated
//! from the same handler declarations the router is built from (`utoipa_axum::routes!`), so the
//! spec can't drift from the code.
//!
//! Every call but `health` and `openapi.json` needs a token (`Authorization: Bearer dux_...`,
//! made with `ductus token create`) or a browser session made from one (`session.rs`). Roles:
//! viewer reads, operator also drains, pauses, resends and corrects messages and runs connection
//! checks, admin also manages tokens.
//!
//! The same listener serves the web UI (`ui.rs`) at every path outside `/api`.

mod actions;
mod hotspots;
mod messages;
mod metrics;
mod reports;
mod retention;
mod session;
mod status;
mod tokens;
mod ui;

use crate::control::Engine;
use axum::extract::{FromRequestParts, State};
use axum::http::request::Parts;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use ductus_store::{Role, Token};
use serde::Serialize;
use std::sync::Arc;
use utoipa::openapi::security::{ApiKey, ApiKeyValue, HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi, ToSchema};
use utoipa_axum::router::OpenApiRouter;

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
        (name = "messages", description = "The message browser, the quarantine, and resending or correcting messages"),
        (name = "drain", description = "Stop taking messages and deliver what's stored"),
        (name = "reports", description = "Impact, owners and connection checks"),
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
        .merge(hotspots::routes())
        .merge(messages::routes())
        .merge(actions::routes())
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

/// Serve the API at `host:port` until the engine shuts down. The task ends once the requests
/// in flight have been answered: the one that asked for the shutdown (a drain with `stop`) gets
/// its answer before the process exits.
pub async fn serve(engine: Shared, host: &str, port: u16) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    use anyhow::Context;
    let listener = tokio::net::TcpListener::bind((host, port)).await.with_context(|| {
        format!("binding the REST API to {host}:{port} (`api.listen` in ductus.yaml; `off` to run without it)")
    })?;
    tracing::info!(%host, port, "REST API listening (/api/v1)");
    let shutdown = engine.shutdown.clone();
    let (router, _) = router(engine);
    Ok(tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, router).with_graceful_shutdown(shutdown.cancelled_owned()).await {
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

/// Who is calling: the token that came with the request, directly or through a browser's
/// session (`session` is then when it ends).
pub struct Caller {
    pub token: Token,
    pub session: Option<DateTime<Utc>>,
}

impl Caller {
    /// Refuse unless the token's role is at least `role`.
    pub fn require(&self, role: Role) -> Result<(), ApiError> {
        if self.token.role >= role {
            return Ok(());
        }
        Err(ApiError::new(
            StatusCode::FORBIDDEN,
            format!("token `{}` is a {}; this needs {}", self.token.name, self.token.role.label(), role.label()),
        ))
    }

    pub fn name(&self) -> &str {
        &self.token.name
    }
}

/// A signed-in browser's requests that change something must carry this header. Other sites
/// can't send it without the engine agreeing first (CORS, which it never does), so a page
/// elsewhere can't use the session cookie to act. Scripts using a token don't need it.
pub const BROWSER_HEADER: &str = "x-ductus";

/// `Authorization: Bearer ...`, if given.
fn bearer(parts: &Parts) -> Option<&str> {
    let value = parts.headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    value.strip_prefix("Bearer ").or_else(|| value.strip_prefix("bearer ")).map(str::trim)
}

impl FromRequestParts<Shared> for Caller {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, engine: &Shared) -> Result<Self, ApiError> {
        let unauthorized = |message: &str| {
            ApiError::new(StatusCode::UNAUTHORIZED, format!("{message} (create a token with `ductus token create`)"))
        };
        if let Some(secret) = bearer(parts) {
            return match engine.store.authenticate(secret).await? {
                Some(token) => Ok(Caller { token, session: None }),
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
        match engine.store.authenticate_session(secret).await? {
            Some((token, ends)) => Ok(Caller { token, session: Some(ends) }),
            None => Err(ApiError::new(StatusCode::UNAUTHORIZED, "the session has ended: sign in again")),
        }
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
}
