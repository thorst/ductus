//! The signed-in person's own account: their details, password, sessions, tokens and appearance.
//! For people signed in with a username and password; a token has no account to look after.

use super::session::hash_password;
use super::tokens::{CreatedToken, TokenInfo};
use super::users::{UserInfo, current_password_is};
use super::{ApiError, ApiResult, Caller, Eng, MustChange, Shared};
use axum::Json;
use axum::extract::Path;
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use ductus_store::{Role, UserChange};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

pub fn routes() -> OpenApiRouter<Shared> {
    OpenApiRouter::new()
        .routes(routes!(profile, update_profile))
        .routes(routes!(change_password))
        .routes(routes!(sessions, end_other_sessions))
        .routes(routes!(end_session))
        .routes(routes!(settings, save_settings))
        .routes(routes!(my_tokens, make_token))
        .routes(routes!(revoke_token))
}

/// The signed-in user's account.
#[utoipa::path(get, path = "/me", tag = "me", operation_id = "get_me", responses((status = 200, body = UserInfo), (status = 403, body = super::ErrorBody)))]
async fn profile(engine: Eng, caller: Caller) -> ApiResult<UserInfo> {
    let id = caller.user_id()?;
    let user = engine.store.user(id).await?.ok_or_else(|| ApiError::not_found("your account is gone"))?;
    Ok(Json(user.into()))
}

#[derive(Deserialize, ToSchema)]
pub struct ProfileUpdate {
    pub display_name: Option<String>,
    pub email: Option<String>,
}

/// Change your own name and email.
#[utoipa::path(
    patch, path = "/me", tag = "me", operation_id = "update_me", request_body = ProfileUpdate,
    responses((status = 200, body = UserInfo), (status = 403, body = super::ErrorBody))
)]
async fn update_profile(engine: Eng, caller: Caller, Json(change): Json<ProfileUpdate>) -> ApiResult<UserInfo> {
    let id = caller.user_id()?;
    let change = UserChange { display_name: change.display_name, email: change.email, ..UserChange::default() };
    let user = engine
        .store
        .update_user(id, &change, &caller.audit("user.update"))
        .await
        .map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
    Ok(Json(user.into()))
}

#[derive(Deserialize, ToSchema)]
pub struct PasswordChange {
    /// The password you have now.
    pub current: String,
    pub new: String,
}

/// Change your own password. Your other sessions end. Works while a change is required.
#[utoipa::path(
    post, path = "/me/password", tag = "me", operation_id = "change_password", request_body = PasswordChange,
    responses((status = 204), (status = 400, body = super::ErrorBody), (status = 403, body = super::ErrorBody))
)]
async fn change_password(
    engine: Eng,
    MustChange(caller): MustChange,
    Json(change): Json<PasswordChange>,
) -> Result<StatusCode, ApiError> {
    let id = caller.user_id()?;
    if !current_password_is(&engine, id, &change.current).await? {
        return Err(ApiError::bad_request("the current password is wrong"));
    }
    if change.new == change.current {
        return Err(ApiError::bad_request("the new password is the same as the current one"));
    }
    ductus_core::password::check_policy(&caller.who.name, &change.new, engine.auth.password.min_length)
        .map_err(ApiError::bad_request)?;
    let hash = hash_password(change.new).await?;
    engine
        .store
        .set_password(id, &hash, false, caller.cookie.as_deref(), &caller.audit("password.change"))
        .await
        .map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
    tracing::info!(user = caller.name(), "password changed");
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize, ToSchema)]
pub struct SessionInfo {
    pub id: i32,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    /// The one making this request.
    pub current: bool,
}

/// Your signed-in browsers.
#[utoipa::path(get, path = "/me/sessions", tag = "me", operation_id = "my_sessions", responses((status = 200, body = Vec<SessionInfo>), (status = 403, body = super::ErrorBody)))]
async fn sessions(engine: Eng, caller: Caller) -> ApiResult<Vec<SessionInfo>> {
    let id = caller.user_id()?;
    let rows = engine.store.user_sessions(id, caller.cookie.as_deref(), engine.auth.session.idle).await?;
    Ok(Json(
        rows.into_iter()
            .map(|s| SessionInfo {
                id: s.id,
                created_at: s.created_at,
                last_seen_at: s.last_seen_at,
                expires_at: s.expires_at,
                current: s.current,
            })
            .collect(),
    ))
}

/// Sign out everywhere else: every session of yours but this one ends.
#[utoipa::path(delete, path = "/me/sessions", tag = "me", operation_id = "end_other_sessions", responses((status = 204), (status = 403, body = super::ErrorBody)))]
async fn end_other_sessions(engine: Eng, caller: Caller) -> Result<StatusCode, ApiError> {
    let id = caller.user_id()?;
    engine.store.end_other_sessions(id, caller.cookie.as_deref()).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// End one of your sessions.
#[utoipa::path(
    delete, path = "/me/sessions/{id}", tag = "me", operation_id = "end_my_session",
    params(("id" = i32, Path, description = "Session id")),
    responses((status = 204), (status = 403, body = super::ErrorBody), (status = 404, body = super::ErrorBody))
)]
async fn end_session(engine: Eng, caller: Caller, Path(session): Path<i32>) -> Result<StatusCode, ApiError> {
    let id = caller.user_id()?;
    if !engine.store.end_user_session(id, session).await? {
        return Err(ApiError::not_found("no such session of yours"));
    }
    Ok(StatusCode::NO_CONTENT)
}

const THEMES: [&str; 5] = ["system", "light", "dark", "contrast", "custom"];
/// The five colors and what is worked out from them are a few hundred bytes.
const MAX_COLORS: usize = 8 * 1024;

#[derive(Serialize, Deserialize, ToSchema)]
pub struct Appearance {
    /// `system`, `light`, `dark`, `contrast` or `custom`.
    pub theme: String,
    /// The custom colors, as the UI saves them (a JSON object).
    #[schema(value_type = Option<Object>)]
    pub colors: Option<serde_json::Value>,
    /// Whether this was saved before: false means the UI may offer the browser's own choice.
    #[serde(default)]
    pub saved: bool,
}

/// Your saved appearance (theme and custom colors), which follows you to any browser.
#[utoipa::path(get, path = "/me/settings", tag = "me", operation_id = "get_my_settings", responses((status = 200, body = Appearance), (status = 403, body = super::ErrorBody)))]
async fn settings(engine: Eng, caller: Caller) -> ApiResult<Appearance> {
    let id = caller.user_id()?;
    Ok(Json(match engine.store.user_settings(id).await? {
        Some(s) => {
            Appearance { theme: s.theme, colors: s.colors.and_then(|c| serde_json::from_str(&c).ok()), saved: true }
        }
        None => Appearance { theme: "system".into(), colors: None, saved: false },
    }))
}

/// Save your appearance.
#[utoipa::path(
    put, path = "/me/settings", tag = "me", operation_id = "save_my_settings", request_body = Appearance,
    responses((status = 200, body = Appearance), (status = 400, body = super::ErrorBody), (status = 403, body = super::ErrorBody))
)]
async fn save_settings(engine: Eng, caller: Caller, Json(a): Json<Appearance>) -> ApiResult<Appearance> {
    let id = caller.user_id()?;
    if !THEMES.contains(&a.theme.as_str()) {
        return Err(ApiError::bad_request(format!("unknown theme `{}` (one of {})", a.theme, THEMES.join(", "))));
    }
    let colors = match &a.colors {
        None | Some(serde_json::Value::Null) => None,
        Some(v @ serde_json::Value::Object(_)) => Some(v.to_string()),
        Some(_) => return Err(ApiError::bad_request("`colors` is an object")),
    };
    if colors.as_ref().is_some_and(|c| c.len() > MAX_COLORS) {
        return Err(ApiError::bad_request("`colors` is too large"));
    }
    engine.store.save_user_settings(id, &a.theme, colors.as_deref()).await?;
    Ok(Json(Appearance { theme: a.theme, colors: a.colors.filter(|c| !c.is_null()), saved: true }))
}

/// Your own tokens (live ones).
#[utoipa::path(get, path = "/me/tokens", tag = "me", operation_id = "my_tokens", responses((status = 200, body = Vec<TokenInfo>), (status = 403, body = super::ErrorBody)))]
async fn my_tokens(engine: Eng, caller: Caller) -> ApiResult<Vec<TokenInfo>> {
    let id = caller.user_id()?;
    Ok(Json(engine.store.tokens(false, Some(id)).await?.into_iter().map(Into::into).collect()))
}

#[derive(Deserialize, ToSchema)]
pub struct NewMyToken {
    /// What it is for, e.g. `deploy-script`; it is listed as `<you>/<name>`.
    pub name: String,
    /// Defaults to your own role; never above it.
    pub role: Option<String>,
    /// Defaults to none; never more than you have.
    pub permissions: Option<Vec<String>>,
}

/// Make a token of your own, for scripts. It acts as you at most: never above your role or
/// permissions, and it stops working if you are disabled. Its secret is in this answer only.
#[utoipa::path(
    post, path = "/me/tokens", tag = "me", operation_id = "make_my_token", request_body = NewMyToken,
    responses((status = 200, body = CreatedToken), (status = 400, body = super::ErrorBody), (status = 403, body = super::ErrorBody))
)]
async fn make_token(engine: Eng, caller: Caller, Json(new): Json<NewMyToken>) -> ApiResult<CreatedToken> {
    let id = caller.user_id()?;
    if !engine.auth.user_tokens {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "users can't make their own tokens here (`auth.user_tokens: false`): ask an admin",
        ));
    }
    let role = match new.role.as_deref() {
        None => caller.who.role,
        Some(r) => Role::parse(r).ok_or_else(|| ApiError::bad_request(format!("unknown role `{r}`")))?,
    };
    if role > caller.who.role {
        return Err(ApiError::bad_request(format!(
            "a token can't be above your own role ({})",
            caller.who.role.label()
        )));
    }
    let permissions = new.permissions.unwrap_or_default();
    for p in &permissions {
        if !caller.who.can(p.trim()) {
            return Err(ApiError::bad_request(format!(
                "you don't have the `{p}` permission, so a token of yours can't"
            )));
        }
    }
    let name = format!("{}/{}", caller.name(), new.name.trim());
    let (token, secret) = engine
        .store
        .create_token(&name, role, &permissions, Some(id), &caller.audit("token.create"))
        .await
        .map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
    tracing::warn!(by = caller.name(), token = %token.name, role = role.label(), "API token created");
    Ok(Json(CreatedToken { token: token.into(), secret }))
}

/// Revoke one of your tokens (the name as listed).
#[utoipa::path(
    delete, path = "/me/tokens/{name}", tag = "me", operation_id = "revoke_my_token",
    params(("name" = String, Path, description = "Token name")),
    responses((status = 204), (status = 403, body = super::ErrorBody), (status = 404, body = super::ErrorBody))
)]
async fn revoke_token(engine: Eng, caller: Caller, Path(name): Path<String>) -> Result<StatusCode, ApiError> {
    let id = caller.user_id()?;
    if !engine.store.revoke_token(&name, Some(id), &caller.audit("token.revoke")).await? {
        return Err(ApiError::not_found(format!("you have no live token named `{name}`")));
    }
    tracing::warn!(by = caller.name(), token = %name, "API token revoked");
    Ok(StatusCode::NO_CONTENT)
}
