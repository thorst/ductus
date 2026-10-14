//! API tokens: list, create, revoke. Admin only (users make their own under `/me/tokens`). The
//! first admin is made on the server with `ductus user create`, or a first token with
//! `ductus token create`, which need the database, not the API.

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
    pub name: String,
    /// `viewer`, `operator` or `admin`.
    pub role: &'static str,
    /// `payloads`, `audit` (admins' tokens have all).
    pub permissions: Vec<String>,
    /// The user it belongs to; none for a service token made by an admin.
    pub owner: Option<String>,
    pub created_at: DateTime<Utc>,
    /// To the minute.
    pub last_used_at: Option<DateTime<Utc>>,
}

impl From<Token> for TokenInfo {
    fn from(t: Token) -> Self {
        Self {
            name: t.name,
            role: t.role.label(),
            permissions: t.permissions,
            owner: t.owner,
            created_at: t.created_at,
            last_used_at: t.last_used_at,
        }
    }
}

/// Live tokens, everyone's (never their secrets). Admin.
#[utoipa::path(get, path = "/tokens", tag = "tokens", operation_id = "list_tokens", responses((status = 200, body = Vec<TokenInfo>), (status = 403, body = super::ErrorBody)))]
async fn list(engine: Eng, caller: Caller) -> ApiResult<Vec<TokenInfo>> {
    caller.require(Role::Admin)?;
    Ok(Json(engine.store.tokens(false, None).await?.into_iter().map(Into::into).collect()))
}

#[derive(Deserialize, ToSchema)]
pub struct NewToken {
    /// Who or what uses it, e.g. `ci` or `monitoring`.
    pub name: String,
    /// `viewer` (default), `operator` or `admin`.
    pub role: Option<String>,
    /// `payloads` and/or `audit`. None by default: a token can't read message payloads unless
    /// it is given the permission.
    pub permissions: Option<Vec<String>>,
}

#[derive(Serialize, ToSchema)]
pub struct CreatedToken {
    pub token: TokenInfo,
    /// The token itself: shown this once, never stored (only its hash is).
    pub secret: String,
}

pub fn role_of(text: Option<&str>) -> Result<Role, ApiError> {
    match text {
        None => Ok(Role::Viewer),
        Some(r) => Role::parse(r)
            .ok_or_else(|| ApiError::bad_request(format!("unknown role `{r}` (viewer, operator or admin)"))),
    }
}

/// Create a service token (it belongs to nobody). Its secret is in this answer only. Admin.
#[utoipa::path(
    post, path = "/tokens", tag = "tokens", operation_id = "create_token", request_body = NewToken,
    responses((status = 200, body = CreatedToken), (status = 400, body = super::ErrorBody), (status = 403, body = super::ErrorBody))
)]
async fn create(engine: Eng, caller: Caller, Json(new): Json<NewToken>) -> ApiResult<CreatedToken> {
    caller.require(Role::Admin)?;
    let role = role_of(new.role.as_deref())?;
    let permissions = new.permissions.unwrap_or_default();
    let (token, secret) = engine
        .store
        .create_token(&new.name, role, &permissions, None, &caller.audit("token.create"))
        .await
        .map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
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
    if !engine.store.revoke_token(&name, None, &caller.audit("token.revoke")).await? {
        return Err(ApiError::not_found(format!("no live token named `{name}`")));
    }
    tracing::warn!(by = caller.name(), token = %name, "API token revoked");
    Ok(StatusCode::NO_CONTENT)
}
