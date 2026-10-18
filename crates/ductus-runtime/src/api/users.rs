//! People who sign in: list, create, change, reset a password, unlock. Admin only. (The first
//! admin is made on the server with `ductus user create`, which needs the database, not the API.)

use super::session::{check_password, hash_password};
use super::{ApiError, ApiResult, Caller, Eng, Shared};
use axum::Json;
use axum::extract::Path;
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use ductus_store::{NewUser, Role, User, UserChange};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

pub fn routes() -> OpenApiRouter<Shared> {
    OpenApiRouter::new()
        .routes(routes!(list, create))
        .routes(routes!(update))
        .routes(routes!(reset_password))
        .routes(routes!(unlock))
}

#[derive(Serialize, ToSchema)]
pub struct UserInfo {
    pub id: i32,
    pub username: String,
    pub display_name: String,
    pub email: String,
    /// `viewer`, `operator` or `admin`.
    pub role: &'static str,
    /// `payloads`, `audit` (admins have both whatever is listed).
    pub permissions: Vec<String>,
    /// Where the password is checked: `local`, or a directory.
    pub source: String,
    /// An admin chose the password; the next sign-in can only change it.
    pub must_change_password: bool,
    pub disabled: bool,
    /// Refused after too many wrong passwords, until then.
    pub locked_until: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub last_sign_in_at: Option<DateTime<Utc>>,
    /// A directory account: when the directory was last asked about them (sign-in or refresh).
    pub directory_checked_at: Option<DateTime<Utc>>,
    /// A directory account the directory refused at its last check (no group, disabled or gone
    /// there): it can't sign in or act until the directory says yes again.
    pub directory_refused: bool,
}

impl From<User> for UserInfo {
    fn from(u: User) -> Self {
        Self {
            id: u.id,
            username: u.username,
            display_name: u.display_name,
            email: u.email,
            role: u.role.label(),
            permissions: u.permissions,
            source: u.source,
            must_change_password: u.must_change_password,
            disabled: u.disabled_at.is_some(),
            locked_until: u.locked_until.filter(|t| *t > Utc::now()),
            created_at: u.created_at,
            last_sign_in_at: u.last_sign_in_at,
            directory_checked_at: u.directory_checked_at,
            directory_refused: u.directory_refused_at.is_some(),
        }
    }
}

/// A directory account's name and email are the directory's: they would come back at the next
/// sign-in or refresh, so changing them here would only look like it worked. The same values (a
/// form that sends everything) are fine.
pub fn directory_details_kept(user: &User, name: &Option<String>, email: &Option<String>) -> Result<(), ApiError> {
    let differs = |new: &Option<String>, old: &str| new.as_deref().is_some_and(|n| n.trim() != old);
    if user.source != "local" && (differs(name, &user.display_name) || differs(email, &user.email)) {
        return Err(ApiError::bad_request("this account's name and email come from its directory: change them there"));
    }
    Ok(())
}

fn role_of(text: &str) -> Result<Role, ApiError> {
    Role::parse(text).ok_or_else(|| ApiError::bad_request(format!("unknown role `{text}` (viewer, operator or admin)")))
}

fn bad(e: anyhow::Error) -> ApiError {
    ApiError::bad_request(format!("{e:#}"))
}

/// All users. Admin.
#[utoipa::path(get, path = "/users", tag = "users", operation_id = "list_users", responses((status = 200, body = Vec<UserInfo>), (status = 403, body = super::ErrorBody)))]
async fn list(engine: Eng, caller: Caller) -> ApiResult<Vec<UserInfo>> {
    caller.require(Role::Admin)?;
    Ok(Json(engine.store.users().await?.into_iter().map(Into::into).collect()))
}

#[derive(Deserialize, ToSchema)]
pub struct NewUserRequest {
    pub username: String,
    pub display_name: Option<String>,
    pub email: Option<String>,
    /// `viewer` (default), `operator` or `admin`.
    pub role: Option<String>,
    /// `payloads` and/or `audit`.
    pub permissions: Option<Vec<String>>,
    /// A temporary password: the person must change it at their first sign-in.
    pub password: String,
}

/// Make an account with a temporary password (the person changes it at their first sign-in). Admin.
#[utoipa::path(
    post, path = "/users", tag = "users", operation_id = "create_user", request_body = NewUserRequest,
    responses((status = 200, body = UserInfo), (status = 400, body = super::ErrorBody), (status = 403, body = super::ErrorBody))
)]
async fn create(engine: Eng, caller: Caller, Json(new): Json<NewUserRequest>) -> ApiResult<UserInfo> {
    caller.require(Role::Admin)?;
    let role = role_of(new.role.as_deref().unwrap_or("viewer"))?;
    ductus_core::password::check_policy(&new.username, &new.password, engine.auth.password.min_length)
        .map_err(ApiError::bad_request)?;
    let password_hash = hash_password(new.password).await?;
    let permissions = new.permissions.unwrap_or_default();
    let user = engine
        .store
        .create_user(
            &NewUser {
                username: &new.username,
                display_name: new.display_name.as_deref().unwrap_or(""),
                email: new.email.as_deref().unwrap_or(""),
                role,
                permissions: &permissions,
                password_hash: Some(&password_hash),
                must_change_password: true,
                source: "local",
            },
            &caller.audit("user.create"),
        )
        .await
        .map_err(bad)?;
    tracing::warn!(by = caller.name(), user = %user.username, role = role.label(), "user created");
    Ok(Json(user.into()))
}

#[derive(Deserialize, ToSchema)]
pub struct UserUpdate {
    pub display_name: Option<String>,
    pub email: Option<String>,
    pub role: Option<String>,
    pub permissions: Option<Vec<String>>,
    /// Disabled users can't sign in, and their sessions and tokens stop at once.
    pub disabled: Option<bool>,
}

/// Change a user's details, role, permissions, or disable them. Refused when it would leave no
/// admin who can sign in. Admin.
#[utoipa::path(
    patch, path = "/users/{id}", tag = "users", operation_id = "update_user",
    params(("id" = i32, Path, description = "User id")), request_body = UserUpdate,
    responses((status = 200, body = UserInfo), (status = 400, body = super::ErrorBody), (status = 403, body = super::ErrorBody), (status = 404, body = super::ErrorBody))
)]
async fn update(
    engine: Eng,
    caller: Caller,
    Path(id): Path<i32>,
    Json(change): Json<UserUpdate>,
) -> ApiResult<UserInfo> {
    caller.require(Role::Admin)?;
    let existing = engine.store.user(id).await?.ok_or_else(|| ApiError::not_found(format!("no user {id}")))?;
    if existing.source != "local" && (change.role.is_some() || change.permissions.is_some()) {
        return Err(ApiError::bad_request("this account's role is decided by its directory groups"));
    }
    directory_details_kept(&existing, &change.display_name, &change.email)?;
    let user = engine
        .store
        .update_user(
            id,
            &UserChange {
                display_name: change.display_name,
                email: change.email,
                role: change.role.as_deref().map(role_of).transpose()?,
                permissions: change.permissions,
                disabled: change.disabled,
            },
            &caller.audit(if change.disabled == Some(true) { "user.disable" } else { "user.update" }),
        )
        .await
        .map_err(bad)?;
    tracing::warn!(by = caller.name(), user = %user.username, role = user.role.label(), disabled = user.disabled_at.is_some(), "user changed");
    Ok(Json(user.into()))
}

#[derive(Deserialize, ToSchema)]
pub struct PasswordReset {
    /// The temporary password; the person must change it at their next sign-in.
    pub password: String,
}

/// Give a user a new temporary password (their sessions end; they must change it when they sign
/// in). Admin.
#[utoipa::path(
    post, path = "/users/{id}/password", tag = "users", operation_id = "reset_password",
    params(("id" = i32, Path, description = "User id")), request_body = PasswordReset,
    responses((status = 204), (status = 400, body = super::ErrorBody), (status = 403, body = super::ErrorBody), (status = 404, body = super::ErrorBody))
)]
async fn reset_password(
    engine: Eng,
    caller: Caller,
    Path(id): Path<i32>,
    Json(reset): Json<PasswordReset>,
) -> Result<StatusCode, ApiError> {
    caller.require(Role::Admin)?;
    let user = engine.store.user(id).await?.ok_or_else(|| ApiError::not_found(format!("no user {id}")))?;
    ductus_core::password::check_policy(&user.username, &reset.password, engine.auth.password.min_length)
        .map_err(ApiError::bad_request)?;
    let hash = hash_password(reset.password).await?;
    engine.store.set_password(id, &hash, true, None, &caller.audit("password.reset")).await.map_err(bad)?;
    tracing::warn!(by = caller.name(), user = %user.username, "password reset");
    Ok(StatusCode::NO_CONTENT)
}

/// Let a user who was locked out after wrong passwords try again. Admin.
#[utoipa::path(
    post, path = "/users/{id}/unlock", tag = "users", operation_id = "unlock_user",
    params(("id" = i32, Path, description = "User id")),
    responses((status = 204), (status = 403, body = super::ErrorBody), (status = 404, body = super::ErrorBody))
)]
async fn unlock(engine: Eng, caller: Caller, Path(id): Path<i32>) -> Result<StatusCode, ApiError> {
    caller.require(Role::Admin)?;
    let user = engine.store.user(id).await?.ok_or_else(|| ApiError::not_found(format!("no user {id}")))?;
    engine.store.unlock_user(id, &caller.audit("user.unlock")).await?;
    tracing::warn!(by = caller.name(), user = %user.username, "user unlocked");
    Ok(StatusCode::NO_CONTENT)
}

/// Is `password` this user's current one? (Used by `/me/password`.)
pub async fn current_password_is(engine: &Shared, user_id: i32, password: &str) -> Result<bool, ApiError> {
    let Some(stored) = engine.store.password_hash(user_id).await? else { return Ok(false) };
    Ok(check_password(password.to_string(), stored).await)
}
