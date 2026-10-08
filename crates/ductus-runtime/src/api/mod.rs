//! The REST API, `/api/v1`: everything the web UI shows and does goes through it, so anything
//! the UI can do, a script can too. The OpenAPI document (`/api/v1/openapi.json`) is generated
//! from the same handler declarations the router is built from (`utoipa_axum::routes!`), so the
//! spec can't drift from the code.
//!
//! Every call but `health` and `openapi.json` needs a token (`Authorization: Bearer dux_...`,
//! made with `ductus token create`). Roles: viewer reads, operator also drains and runs
//! connection checks, admin also manages tokens.

mod messages;
mod reports;
mod status;
mod tokens;

use crate::control::Engine;
use axum::extract::{FromRequestParts, State};
use axum::http::request::Parts;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use ductus_store::{Role, Token};
use serde::Serialize;
use std::sync::Arc;
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi, ToSchema};
use utoipa_axum::router::OpenApiRouter;

pub type Shared = Arc<Engine>;

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Ductus API",
        version = "1",
        description = "Status, messages, drain mode and reports of a running Ductus engine. Every call but `health` needs `Authorization: Bearer <token>` (create one with `ductus token create`)."
    ),
    servers((url = "/api/v1")),
    modifiers(&BearerToken),
    security(("token" = [])),
    tags(
        (name = "engine", description = "The engine, its interfaces and live updates"),
        (name = "messages", description = "The message browser"),
        (name = "drain", description = "Stop taking messages and deliver what's stored"),
        (name = "reports", description = "Impact, owners and connection checks"),
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
    }
}

/// The router (under `/api/v1`) and its OpenAPI document.
pub fn router(engine: Shared) -> (Router, utoipa::openapi::OpenApi) {
    let (api, doc) = OpenApiRouter::with_openapi(ApiDoc::openapi())
        .merge(status::routes())
        .merge(messages::routes())
        .merge(reports::routes())
        .merge(tokens::routes())
        .split_for_parts();
    let spec = doc.clone();
    let api = api.route("/openapi.json", axum::routing::get(move || async move { Json(spec) }));
    (Router::new().nest("/api/v1", api.with_state(engine)), doc)
}

/// Serve the API at `host:port` until the process ends.
pub async fn serve(engine: Shared, host: &str, port: u16) -> anyhow::Result<()> {
    use anyhow::Context;
    let listener = tokio::net::TcpListener::bind((host, port)).await.with_context(|| {
        format!("binding the REST API to {host}:{port} (`api.listen` in ductus.yaml; `off` to run without it)")
    })?;
    tracing::info!(%host, port, "REST API listening (/api/v1)");
    let (router, _) = router(engine);
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, router).await {
            tracing::error!(error = %e, "REST API stopped");
        }
    });
    Ok(())
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

/// Who is calling: the token that came with the request.
pub struct Caller(pub Token);

impl Caller {
    /// Refuse unless the token's role is at least `role`.
    pub fn require(&self, role: Role) -> Result<(), ApiError> {
        if self.0.role >= role {
            return Ok(());
        }
        Err(ApiError::new(
            StatusCode::FORBIDDEN,
            format!("token `{}` is a {}; this needs {}", self.0.name, self.0.role.label(), role.label()),
        ))
    }

    pub fn name(&self) -> &str {
        &self.0.name
    }
}

impl FromRequestParts<Shared> for Caller {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, engine: &Shared) -> Result<Self, ApiError> {
        let unauthorized = |message: &str| {
            ApiError::new(StatusCode::UNAUTHORIZED, format!("{message} (create a token with `ductus token create`)"))
        };
        let secret = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")))
            .map(str::trim)
            .ok_or_else(|| unauthorized("missing `Authorization: Bearer <token>`"))?;
        match engine.store.authenticate(secret).await? {
            Some(token) => Ok(Caller(token)),
            None => Err(unauthorized("unknown or revoked token")),
        }
    }
}

/// `State` for handlers, shorter.
pub type Eng = State<Shared>;
