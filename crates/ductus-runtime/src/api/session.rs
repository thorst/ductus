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
use ductus_store::{Audit, By, Lockout, Principal, SessionFor, SessionLimits};
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
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
async fn sign_in(
    engine: Eng,
    parts: Parts,
    headers: HeaderMap,
    Json(body): Json<SignIn>,
) -> Result<impl IntoResponse, ApiError> {
    if !headers.contains_key(BROWSER_HEADER) {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "signing in needs the `X-Ductus: 1` header"));
    }
    let ip = super::peer(&parts);
    let (who, owner, audit_for) = if let Some(token) = body.token.as_deref().filter(|t| !t.trim().is_empty()) {
        if !engine.auth.token_sign_in {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "signing in with a token is turned off (`auth.token_sign_in`): use your username and password",
            ));
        }
        let Some(who) = engine.store.authenticate(token.trim()).await? else {
            refuse(&engine, ip, serde_json::json!({ "with": "token", "reason": "unknown or revoked token" })).await?;
            return Err(ApiError::new(StatusCode::UNAUTHORIZED, "unknown or revoked token"));
        };
        let id = who.token_id.expect("a token's caller has a token id");
        (who, SessionFor::Token(id), "token")
    } else {
        let username = body.username.as_deref().unwrap_or("").trim().to_string();
        let password = body.password.unwrap_or_default();
        if username.is_empty() || password.is_empty() {
            return Err(ApiError::bad_request("give a username and password, or a token"));
        }
        let user = user_sign_in(&engine, &username, password, ip).await?;
        let with = if user.source == "ldap" { "ldap" } else { "password" };
        let who = Principal {
            user_id: Some(user.id),
            token_id: None,
            name: user.username,
            role: user.role,
            permissions: user.permissions,
            must_change_password: user.must_change_password,
        };
        (who, SessionFor::User(user.id), with)
    };
    let by = match owner {
        SessionFor::Token(id) => By::token(id, who.user_id, &who.name, "session", ip),
        SessionFor::User(id) => By::user(id, &who.name, "session", ip),
    };
    let audit = Audit::new(by, "session.sign_in").detail(serde_json::json!({ "with": audit_for }));
    let (secret, ends_at) = engine.store.create_session(owner, limits(&engine), &audit).await?;
    tracing::info!(who = %who.name, kind = if who.token_id.is_some() { "token" } else { "user" }, "signed in to the web UI");
    let max_age = (ends_at - Utc::now()).num_seconds();
    Ok((AppendHeaders([(SET_COOKIE, set_cookie(&secret, max_age))]), Json(describe(&who, Some(ends_at)))))
}

/// A sign-in that failed for someone nobody knows (what they typed isn't kept: it may be a
/// password in the wrong box).
async fn refuse(engine: &Shared, ip: Option<IpAddr>, detail: serde_json::Value) -> Result<(), ApiError> {
    let audit = Audit::new(By::anonymous(ip), "session.failed").failed().detail(detail);
    engine.store.record(&audit).await?;
    Ok(())
}

/// Check a username and password; the account on success. A name a local account holds is
/// checked here and only here (a directory account of the same name can never take it over);
/// any other name goes to the directory, when `auth.ldap` is configured. Every refusal but a
/// lockout, a directory outage or a directory account without a group says the same thing, and
/// takes about as long, so nobody learns which usernames exist. (With a directory, a name it
/// doesn't know skips the bind: a few milliseconds quicker than a wrong password, against network
/// times that vary more. The audit log does know which: its rows are for admins.)
async fn user_sign_in(
    engine: &Shared,
    username: &str,
    password: String,
    ip: Option<IpAddr>,
) -> Result<ductus_store::User, ApiError> {
    let wrong = || ApiError::new(StatusCode::UNAUTHORIZED, WRONG);
    let failed = |id: i32, reason: &'static str| {
        let audit = Audit::new(By::user(id, username, "session", ip), "session.failed")
            .failed()
            .detail(serde_json::json!({ "reason": reason }));
        async move { engine.store.record(&audit).await }
    };
    let candidate = engine.store.candidate(username).await?;
    if let Some(c) = candidate.as_ref().filter(|c| c.source != "local")
        && let Some(until) = c.locked_until.filter(|u| *u > Utc::now())
    {
        return Err(locked(engine, c.id, username, until, ip).await?);
    }
    let candidate = match (candidate, &engine.directory) {
        (Some(c), _) if c.source == "local" => c,
        (candidate, Some(directory)) => {
            return directory_sign_in(engine, directory, candidate, username, password, ip).await;
        }
        (Some(c), None) => {
            waste_time(password).await;
            failed(c.id, "a directory account, and auth.ldap isn't configured").await?;
            return Err(wrong());
        }
        (None, None) => {
            waste_time(password).await;
            refuse(engine, ip, serde_json::json!({ "with": "password", "reason": "unknown username" })).await?;
            return Err(wrong());
        }
    };
    if let Some(until) = candidate.locked_until.filter(|u| *u > Utc::now()) {
        return Err(locked(engine, candidate.id, username, until, ip).await?);
    }
    let Some(stored) = candidate.password_hash.clone() else {
        waste_time(password).await;
        failed(candidate.id, "no password kept here").await?;
        return Err(wrong());
    };
    let ok = check_password(password, stored).await;
    if !ok || candidate.disabled_at.is_some() {
        if !ok && candidate.disabled_at.is_none() {
            // Counted and logged in one transaction.
            let audit = Audit::new(By::user(candidate.id, username, "session", ip), "session.failed").failed();
            let locked = engine.store.record_failed_login(candidate.id, lockout(engine), &audit).await?;
            tracing::warn!(user = username, locked = locked.is_some(), "sign-in failed: wrong password");
        } else if ok {
            failed(candidate.id, "account disabled").await?;
        }
        return Err(wrong());
    }
    engine.store.record_sign_in(candidate.id).await?;
    engine.store.user(candidate.id).await?.ok_or_else(wrong)
}

/// The refusal of a locked account (logged).
async fn locked(
    engine: &Shared,
    id: i32,
    username: &str,
    until: DateTime<Utc>,
    ip: Option<IpAddr>,
) -> Result<ApiError, ApiError> {
    let minutes = ((until - Utc::now()).num_seconds() + 59) / 60;
    tracing::warn!(user = username, "sign-in refused: the account is locked after wrong passwords");
    let audit = Audit::new(By::user(id, username, "session", ip), "session.failed")
        .failed()
        .detail(serde_json::json!({ "reason": "account locked" }));
    engine.store.record(&audit).await?;
    Ok(ApiError::new(
        StatusCode::TOO_MANY_REQUESTS,
        format!("too many wrong passwords: try again in {minutes} minute(s), or ask an admin to unlock the account"),
    ))
}

/// A sign-in the directory decides (`candidate`: the account with this name, if there is one).
/// The account is made or brought up to date from the directory (role, permissions, name, email).
async fn directory_sign_in(
    engine: &Shared,
    directory: &crate::ldap::Directory,
    candidate: Option<ductus_store::Candidate>,
    username: &str,
    password: String,
    ip: Option<IpAddr>,
) -> Result<ductus_store::User, ApiError> {
    use crate::ldap::Verdict;
    let known = candidate.as_ref().map(|c| c.id);
    let wrong = || ApiError::new(StatusCode::UNAUTHORIZED, WRONG);
    let failed = |reason: &str| {
        let by = match known {
            Some(id) => By::user(id, username, "session", ip),
            None => By::anonymous(ip),
        };
        Audit::new(by, "session.failed").failed().detail(serde_json::json!({ "with": "ldap", "reason": reason }))
    };
    let (verdict, found) = match directory.sign_in_found(username, &password).await {
        Ok(s) => (s.verdict, s.id),
        Err(e) => {
            tracing::error!(directory = %directory.name, error = %format!("{e:#}"), "sign-in: the directory can't be asked");
            engine.store.record(&failed("the directory can't be reached")).await?;
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "the directory can't be reached right now: try again in a few minutes (local accounts still work)",
            ));
        }
    };
    match verdict {
        Verdict::Allowed(person) => {
            let user = crate::ldap::signed_in(&engine.store, username, &person, ip).await?;
            if user.disabled_at.is_some() {
                engine.store.record(&failed("account disabled")).await?;
                return Err(wrong());
            }
            engine.store.record_sign_in(user.id).await?;
            Ok(user)
        }
        Verdict::WrongPassword => {
            match known {
                // Counted and logged in one transaction, like a local account's.
                Some(id) => {
                    let audit = failed(Verdict::WrongPassword.reason());
                    let locked = engine.store.record_failed_login(id, lockout(engine), &audit).await?;
                    tracing::warn!(
                        user = username,
                        locked = locked.is_some(),
                        "sign-in failed: wrong password (directory)"
                    );
                }
                None => {
                    engine.store.record(&failed(Verdict::WrongPassword.reason())).await?;
                }
            }
            Err(wrong())
        }
        refused => {
            engine.store.record(&failed(refused.reason())).await?;
            // The account with this name is refused only when the entry found is its person: one
            // known by its directory id may be renamed there (the refresh finds it by id), and
            // the name may be someone else's now.
            let same_person = |c: &ductus_store::Candidate| match (&c.directory_id, &found) {
                (None, _) => true,
                (Some(theirs), Some(entry)) => *theirs == entry.to_string(),
                (Some(_), None) => false,
            };
            if let Some(id) = candidate.as_ref().filter(|c| same_person(c)).map(|c| c.id) {
                let by = By { kind: "system", id: None, user_id: None, name: "directory", via: "session", ip };
                engine
                    .store
                    .refuse_directory_user(id, refused.reason(), &Audit::new(by, "user.directory_refused"))
                    .await?;
            }
            if refused == Verdict::NoGroup {
                // The password was right: saying why helps them, and tells nobody anything new.
                return Err(ApiError::new(
                    StatusCode::FORBIDDEN,
                    "your directory account has no access to Ductus: ask to be added to one of its groups",
                ));
            }
            Err(wrong())
        }
    }
}

/// Who is calling, and as what. Works while a password change is still required.
#[utoipa::path(get, path = "/session", tag = "session", responses((status = 200, body = Session), (status = 401, body = super::ErrorBody)))]
async fn whoami(MustChange(caller): MustChange) -> ApiResult<Session> {
    Ok(Json(describe(&caller.who, caller.session)))
}

/// Sign out: the session ends at once and the cookie is cleared.
#[utoipa::path(delete, path = "/session", tag = "session", security(()), responses((status = 204)))]
async fn sign_out(engine: Eng, parts: Parts, headers: HeaderMap) -> Result<impl IntoResponse, ApiError> {
    if let Some(secret) = from_headers(&headers) {
        engine.store.end_session(secret, super::peer(&parts)).await?;
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
