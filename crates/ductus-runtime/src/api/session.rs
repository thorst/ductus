//! Browser sign-in. A browser can't keep a token safely (anything page scripts can read, injected
//! script can too), so it trades the token once for a session cookie scripts can't read
//! (`HttpOnly`), that's only sent to this engine's API (`SameSite=Strict`, `Path=/api`) and that
//! ends (12 hours idle, 7 days at most). The session acts with the token's role, and revoking
//! the token ends it. Step 8's user logins will make the same sessions.

use super::{ApiError, ApiResult, BROWSER_HEADER, Caller, Eng, Shared};
use axum::Json;
use axum::http::header::{self, SET_COOKIE};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{AppendHeaders, IntoResponse};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

pub fn routes() -> OpenApiRouter<Shared> {
    OpenApiRouter::new().routes(routes!(sign_in, whoami, sign_out))
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
    /// An API token (`dux_...`).
    token: String,
}

#[derive(Serialize, ToSchema)]
pub struct Session {
    /// The token's name.
    name: String,
    /// `viewer`, `operator` or `admin`.
    role: &'static str,
    /// When the session ends at the latest (it also ends after 12 hours unused). None when the
    /// call came with a token instead of a session.
    ends_at: Option<DateTime<Utc>>,
}

/// Sign a browser in with a token: answers with a session cookie (`ductus_session`), which then
/// stands in for the token. Needs the `X-Ductus: 1` header.
#[utoipa::path(
    post, path = "/session", tag = "session", security(()), request_body = SignIn,
    responses((status = 200, body = Session), (status = 401, body = super::ErrorBody), (status = 403, body = super::ErrorBody))
)]
async fn sign_in(engine: Eng, headers: HeaderMap, Json(body): Json<SignIn>) -> Result<impl IntoResponse, ApiError> {
    if !headers.contains_key(BROWSER_HEADER) {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "signing in needs the `X-Ductus: 1` header"));
    }
    let Some(token) = engine.store.authenticate(body.token.trim()).await? else {
        return Err(ApiError::new(StatusCode::UNAUTHORIZED, "unknown or revoked token"));
    };
    let (secret, ends_at) = engine.store.create_session(&token).await?;
    tracing::info!(token = %token.name, "signed in to the web UI");
    let max_age = (ends_at - Utc::now()).num_seconds();
    let session = Session { name: token.name, role: token.role.label(), ends_at: Some(ends_at) };
    Ok((AppendHeaders([(SET_COOKIE, set_cookie(&secret, max_age))]), Json(session)))
}

/// Who is calling: the token (directly or through a session) and its role.
#[utoipa::path(get, path = "/session", tag = "session", responses((status = 200, body = Session), (status = 401, body = super::ErrorBody)))]
async fn whoami(caller: Caller) -> ApiResult<Session> {
    Ok(Json(Session { name: caller.token.name, role: caller.token.role.label(), ends_at: caller.session }))
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
