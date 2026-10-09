//! API tokens: list, create, revoke. Admin only. (The first admin token is made on the
//! server with `ductus token create`, which needs the database, not the API.)

use super::{ApiError, ApiResult, Caller, Eng, Shared};
use axum::Json;
use axum::extract::Path;
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use ductus_store::{Role, Token};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

pub fn routes() -> OpenApiRouter<Shared> {
    OpenApiRouter::new().routes(routes!(list, create)).routes(routes!(revoke))
}

#[derive(Serialize, ToSchema)]
pub struct TokenInfo {
    name: String,
    /// `viewer`, `operator` or `admin`.
    role: &'static str,
    created_at: DateTime<Utc>,
    /// To the minute.
    last_used_at: Option<DateTime<Utc>>,
}

impl From<Token> for TokenInfo {
    fn from(t: Token) -> Self {
        Self { name: t.name, role: t.role.label(), created_at: t.created_at, last_used_at: t.last_used_at }
    }
}

/// Live tokens (never their secrets). Admin.
#[utoipa::path(get, path = "/tokens", tag = "tokens", operation_id = "list_tokens", responses((status = 200, body = Vec<TokenInfo>), (status = 403, body = super::ErrorBody)))]
async fn list(engine: Eng, caller: Caller) -> ApiResult<Vec<TokenInfo>> {
    caller.require(Role::Admin)?;
    Ok(Json(engine.store.tokens(false).await?.into_iter().map(Into::into).collect()))
}

#[derive(Deserialize, ToSchema)]
pub struct NewToken {
    /// Who or what uses it, e.g. `ci` or `monitoring`.
    name: String,
    /// `viewer` (default), `operator` or `admin`.
    role: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct CreatedToken {
    token: TokenInfo,
    /// The token itself: shown this once, never stored (only its hash is).
    secret: String,
}

/// Create a token. Its secret is in this answer only. Admin.
#[utoipa::path(
    post, path = "/tokens", tag = "tokens", operation_id = "create_token", request_body = NewToken,
    responses((status = 200, body = CreatedToken), (status = 400, body = super::ErrorBody), (status = 403, body = super::ErrorBody))
)]
async fn create(engine: Eng, caller: Caller, Json(new): Json<NewToken>) -> ApiResult<CreatedToken> {
    caller.require(Role::Admin)?;
    let role = match new.role.as_deref() {
        None => Role::Viewer,
        Some(r) => Role::parse(r)
            .ok_or_else(|| ApiError::bad_request(format!("unknown role `{r}` (viewer, operator or admin)")))?,
    };
    let (token, secret) =
        engine.store.create_token(&new.name, role).await.map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
    tracing::warn!(by = caller.name(), token = %token.name, role = role.label(), "API token created");
    Ok(Json(CreatedToken { token: token.into(), secret }))
}

/// Revoke a token: it stops working at once. Admin.
#[utoipa::path(
    delete, path = "/tokens/{name}", tag = "tokens", operation_id = "revoke_token",
    params(("name" = String, Path, description = "Token name")),
    responses((status = 204, description = "Revoked"), (status = 403, body = super::ErrorBody), (status = 404, body = super::ErrorBody))
)]
async fn revoke(engine: Eng, caller: Caller, Path(name): Path<String>) -> Result<StatusCode, ApiError> {
    caller.require(Role::Admin)?;
    if !engine.store.revoke_token(&name).await? {
        return Err(ApiError::not_found(format!("no live token named `{name}`")));
    }
    tracing::warn!(by = caller.name(), token = %name, "API token revoked");
    Ok(StatusCode::NO_CONTENT)
}
