//! Browser sign-in. A browser can't keep a token or a password safely (anything page scripts can
//! read, injected script can too), so it signs in once, with a username and password or with a
//! token, and gets a session cookie scripts can't read (`HttpOnly`), that's only sent to this
//! engine's API (`SameSite=Strict`, `Path=/api`) and that ends (12 hours idle, 7 days at most, or
//! as `auth.session` says). The session acts as its user or token, and disabling the user or
//! revoking the token ends it.

use super::{ApiError, ApiResult, BROWSER_HEADER, Eng, MustChange, Shared};
use axum::Json;
use axum::http::header::{self, SET_COOKIE};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{AppendHeaders, IntoResponse};
use chrono::{DateTime, Utc};
use ductus_store::{Lockout, Principal, SessionFor, SessionLimits};
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use tokio::sync::Semaphore;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

pub fn routes() -> OpenApiRouter<Shared> {
    OpenApiRouter::new().routes(routes!(sign_in, whoami, sign_out))
}

/// How long sessions last, from `auth.session`.
pub fn limits(engine: &Shared) -> SessionLimits {
    SessionLimits { idle: engine.auth.session.idle, max: engine.auth.session.max }
}

fn lockout(engine: &Shared) -> Lockout {
    let l = engine.auth.lockout;
    Lockout { attempts: l.attempts, window: l.window, lock_for: l.lock_for }
}

/// Hashing a password takes ~50 ms of CPU and 19 MiB: at most four at once, on blocking threads,
/// so a flood of sign-ins can't starve intake of cores.
fn hashing() -> &'static Semaphore {
    static SLOTS: OnceLock<Semaphore> = OnceLock::new();
    SLOTS.get_or_init(|| Semaphore::new(4))
}

/// Does `password` match the stored hash?
pub async fn check_password(password: String, stored: String) -> bool {
    let Ok(_slot) = hashing().acquire().await else { return false };
    tokio::task::spawn_blocking(move || ductus_core::password::verify(&password, &stored)).await.unwrap_or(false)
}

/// The time a check takes, for a username nobody has.
async fn waste_time(password: String) {
    if let Ok(_slot) = hashing().acquire().await {
        let _ = tokio::task::spawn_blocking(move || ductus_core::password::waste_time(&password)).await;
    }
}

/// A new hash of `password`.
pub async fn hash_password(password: String) -> anyhow::Result<String> {
    let _slot = hashing().acquire().await?;
    tokio::task::spawn_blocking(move || ductus_core::password::hash(&password)).await?
}

/// The session cookie's name.
pub const COOKIE: &str = "ductus_session";

/// The session cookie's value, if the request has one.
pub fn cookie(parts: &Parts) -> Option<&str> {
    from_headers(&parts.headers)
}

fn from_headers(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|pair| pair.trim().strip_prefix(COOKIE)?.strip_prefix('='))
        .filter(|v| !v.is_empty())
}

/// `Secure`: browsers keep it only over https, and over plain http to `localhost`/`127.0.0.1`
/// (both count as secure). Opening the engine to other machines means a TLS proxy in front of
/// it anyway (no TLS on our listeners until v0.6).
fn set_cookie(value: &str, max_age: i64) -> String {
    format!("{COOKIE}={value}; Path=/api; HttpOnly; Secure; SameSite=Strict; Max-Age={max_age}")
}

#[derive(Deserialize, ToSchema)]
pub struct SignIn {
    /// A user's name (with `password`)...
    username: Option<String>,
    password: Option<String>,
    /// ...or an API token (`dux_...`), unless `auth.token_sign_in` is off.
    token: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct Session {
    /// `user` (signed in with a password) or `token`.
    kind: &'static str,
    /// The username, or the token's name.
    name: String,
    /// `viewer`, `operator` or `admin`.
    role: &'static str,
    /// What the role doesn't say: `payloads`, `audit`. Admins have both.
    permissions: Vec<String>,
    /// When the session ends at the latest (it also ends when unused for too long). None when
    /// the call came with a token instead of a session.
    ends_at: Option<DateTime<Utc>>,
    /// An admin chose this password: nothing else works until it is changed (`POST /me/password`).
    must_change_password: bool,
}

pub fn describe(who: &Principal, ends_at: Option<DateTime<Utc>>) -> Session {
    let mut permissions = who.permissions.clone();
    if who.role == ductus_store::Role::Admin {
        permissions = ductus_store::PERMISSIONS.iter().map(|p| p.to_string()).collect();
    }
    Session {
        kind: if who.token_id.is_some() { "token" } else { "user" },
        name: who.name.clone(),
        role: who.role.label(),
        permissions,
        ends_at,
        must_change_password: who.must_change_password,
    }
}

const WRONG: &str = "wrong username or password";

/// Sign a browser in with a username and password, or a token: answers with a session cookie
/// (`ductus_session`), which then stands in for them. Needs the `X-Ductus: 1` header. After too
/// many wrong passwords for one username the account refuses sign-in for a while (429).
#[utoipa::path(
    post, path = "/session", tag = "session", security(()), request_body = SignIn,
    responses((status = 200, body = Session), (status = 401, body = super::ErrorBody), (status = 403, body = super::ErrorBody), (status = 429, body = super::ErrorBody))
)]
async fn sign_in(engine: Eng, headers: HeaderMap, Json(body): Json<SignIn>) -> Result<impl IntoResponse, ApiError> {
    if !headers.contains_key(BROWSER_HEADER) {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "signing in needs the `X-Ductus: 1` header"));
    }
    let (who, owner) = if let Some(token) = body.token.as_deref().filter(|t| !t.trim().is_empty()) {
        if !engine.auth.token_sign_in {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "signing in with a token is turned off (`auth.token_sign_in`): use your username and password",
            ));
        }
        let Some(who) = engine.store.authenticate(token.trim()).await? else {
            return Err(ApiError::new(StatusCode::UNAUTHORIZED, "unknown or revoked token"));
        };
        let id = who.token_id.expect("a token's caller has a token id");
        (who, SessionFor::Token(id))
    } else {
        let username = body.username.as_deref().unwrap_or("").trim().to_string();
        let password = body.password.unwrap_or_default();
        if username.is_empty() || password.is_empty() {
            return Err(ApiError::bad_request("give a username and password, or a token"));
        }
        let user = user_sign_in(&engine, &username, password).await?;
        let who = Principal {
            user_id: Some(user.id),
            token_id: None,
            name: user.username,
            role: user.role,
            permissions: user.permissions,
            must_change_password: user.must_change_password,
        };
        (who, SessionFor::User(user.id))
    };
    let (secret, ends_at) = engine.store.create_session(owner, limits(&engine)).await?;
    tracing::info!(who = %who.name, kind = if who.token_id.is_some() { "token" } else { "user" }, "signed in to the web UI");
    let max_age = (ends_at - Utc::now()).num_seconds();
    Ok((AppendHeaders([(SET_COOKIE, set_cookie(&secret, max_age))]), Json(describe(&who, Some(ends_at)))))
}

/// Check a username and password; the account on success. Every refusal but a lockout says the
/// same thing, and takes about as long, so nobody learns which usernames exist.
async fn user_sign_in(engine: &Shared, username: &str, password: String) -> Result<ductus_store::User, ApiError> {
    let wrong = || ApiError::new(StatusCode::UNAUTHORIZED, WRONG);
    let Some(candidate) = engine.store.candidate(username).await? else {
        waste_time(password).await;
        return Err(wrong());
    };
    if let Some(until) = candidate.locked_until.filter(|u| *u > Utc::now()) {
        let minutes = ((until - Utc::now()).num_seconds() + 59) / 60;
        tracing::warn!(user = username, "sign-in refused: the account is locked after wrong passwords");
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            format!(
                "too many wrong passwords: try again in {minutes} minute(s), or ask an admin to unlock the account"
            ),
        ));
    }
    let stored = candidate.password_hash.clone().filter(|_| candidate.source == "local");
    let Some(stored) = stored else {
        waste_time(password).await;
        return Err(wrong());
    };
    let ok = check_password(password, stored).await;
    if !ok || candidate.disabled_at.is_some() {
        if !ok && candidate.disabled_at.is_none() {
            let locked = engine.store.record_failed_login(candidate.id, lockout(engine)).await?;
            tracing::warn!(user = username, locked = locked.is_some(), "sign-in failed: wrong password");
        }
        return Err(wrong());
    }
    engine.store.record_sign_in(candidate.id).await?;
    engine.store.user(candidate.id).await?.ok_or_else(wrong)
}

/// Who is calling, and as what. Works while a password change is still required.
#[utoipa::path(get, path = "/session", tag = "session", responses((status = 200, body = Session), (status = 401, body = super::ErrorBody)))]
async fn whoami(MustChange(caller): MustChange) -> ApiResult<Session> {
    Ok(Json(describe(&caller.who, caller.session)))
}

/// Sign out: the session ends at once and the cookie is cleared.
#[utoipa::path(delete, path = "/session", tag = "session", security(()), responses((status = 204)))]
async fn sign_out(engine: Eng, headers: HeaderMap) -> Result<impl IntoResponse, ApiError> {
    if let Some(secret) = from_headers(&headers) {
        engine.store.end_session(secret).await?;
    }
    Ok((StatusCode::NO_CONTENT, AppendHeaders([(SET_COOKIE, set_cookie("", 0))])))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn the_cookie_is_found_among_others() {
        let mut headers = HeaderMap::new();
        headers.append(header::COOKIE, HeaderValue::from_static("theme=dark; ductus_session_old=x"));
        assert_eq!(from_headers(&headers), None, "a longer name isn't ours");
        headers.append(header::COOKIE, HeaderValue::from_static("a=1; ductus_session=dus_abc; b=2"));
        assert_eq!(from_headers(&headers), Some("dus_abc"));
        let mut empty = HeaderMap::new();
        empty.append(header::COOKIE, HeaderValue::from_static("ductus_session="));
        assert_eq!(from_headers(&empty), None);
    }
}
