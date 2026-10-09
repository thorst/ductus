//! The web UI, served from the binary (embedded by `build.rs`) at every path outside `/api`.
//!
//! It's a single-page app: a path that isn't a file gets `index.html` and the app's router
//! shows the page. Vite puts a content hash in every file name under `/assets/`, so those are
//! cached for good; everything else is checked on each load (an upgrade shows at once).

use super::ApiError;
use axum::http::header::{ACCEPT_ENCODING, CACHE_CONTROL, CONTENT_ENCODING, CONTENT_TYPE, VARY};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};

pub struct Asset {
    pub path: &'static str,
    pub body: &'static [u8],
    pub gzip: Option<&'static [u8]>,
}

include!(concat!(env!("OUT_DIR"), "/ui/assets.rs"));

/// The page may load only its own files and talk only to this engine; no framing (clickjacking).
/// Styles allow inline `style` attributes, which the component library sets for positioning.
const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; \
                   font-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

fn find(path: &str) -> Option<&'static Asset> {
    ASSETS.binary_search_by(|a| a.path.cmp(path)).ok().map(|n| &ASSETS[n])
}

pub async fn serve(method: Method, uri: Uri, headers: HeaderMap) -> Response {
    let path = uri.path();
    if path == "/api" || path.starts_with("/api/") {
        return ApiError::not_found("no such API path (the API is at /api/v1)").into_response();
    }
    if method != Method::GET && method != Method::HEAD {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let path = if path == "/" { "/index.html" } else { path };
    let asset = match find(path) {
        Some(asset) => asset,
        // A hashed file that isn't here belongs to another version: never answer it with HTML.
        None if path.starts_with("/assets/") => return StatusCode::NOT_FOUND.into_response(),
        None => find("/index.html").expect("index.html is always embedded"),
    };
    let gzip = asset.gzip.filter(|_| accepts_gzip(&headers));
    let cache = if asset.path.starts_with("/assets/") { "public, max-age=31536000, immutable" } else { "no-cache" };
    let mut response = gzip.unwrap_or(asset.body).into_response();
    let h = response.headers_mut();
    h.insert(CONTENT_TYPE, HeaderValue::from_static(content_type(asset.path)));
    h.insert(CACHE_CONTROL, HeaderValue::from_static(cache));
    if asset.gzip.is_some() {
        h.insert(VARY, HeaderValue::from_static("accept-encoding"));
    }
    if gzip.is_some() {
        h.insert(CONTENT_ENCODING, HeaderValue::from_static("gzip"));
    }
    h.insert(HeaderName::from_static("content-security-policy"), HeaderValue::from_static(CSP));
    h.insert(HeaderName::from_static("x-content-type-options"), HeaderValue::from_static("nosniff"));
    h.insert(HeaderName::from_static("referrer-policy"), HeaderValue::from_static("no-referrer"));
    response
}

fn accepts_gzip(headers: &HeaderMap) -> bool {
    headers.get_all(ACCEPT_ENCODING).iter().filter_map(|v| v.to_str().ok()).flat_map(|v| v.split(',')).any(|e| {
        let mut parts = e.split(';');
        let name = parts.next().unwrap_or("").trim();
        let refused =
            parts.any(|p| matches!(p.trim().strip_prefix("q="), Some(q) if q.trim().parse::<f32>() == Ok(0.0)));
        (name.eq_ignore_ascii_case("gzip") || name == "*") && !refused
    })
}

fn content_type(path: &str) -> &'static str {
    let ext = path.rsplit_once('.').map_or("", |(_, e)| e);
    match ext {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "txt" => "text/plain; charset=utf-8",
        "webmanifest" => "application/manifest+json",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn accept(value: &'static str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(ACCEPT_ENCODING, HeaderValue::from_static(value));
        h
    }

    #[test]
    fn gzip_is_sent_only_to_those_who_take_it() {
        assert!(accepts_gzip(&accept("gzip, deflate, br")));
        assert!(accepts_gzip(&accept("br;q=1.0, gzip;q=0.8")));
        assert!(accepts_gzip(&accept("*")));
        assert!(!accepts_gzip(&accept("br")));
        assert!(!accepts_gzip(&accept("gzip;q=0")));
        assert!(!accepts_gzip(&HeaderMap::new()));
    }

    #[test]
    fn assets_are_sorted_and_have_an_index() {
        assert!(ASSETS.windows(2).all(|w| w[0].path < w[1].path), "binary search needs them sorted");
        assert!(find("/index.html").is_some());
        assert_eq!(content_type("/assets/index-abc.js"), "text/javascript; charset=utf-8");
    }
}
