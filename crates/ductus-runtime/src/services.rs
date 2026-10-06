//! Engine services for interface code: named database pools, HTTP requests, environment
//! variables, secrets.

use crate::mssql;
use anyhow::{Result, anyhow, bail};
use ductus_core::resources::{Database, DbConnection};
use ductus_core::{Environment, Resources};
use ductus_script::{BoxFuture, FetchRequest, FetchResponse, Services, TRANSIENT};
use serde_json::Value;
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

/// Bigger responses are refused: interface code holds them in memory.
const MAX_RESPONSE: usize = 16 * 1024 * 1024;

enum Pool {
    Postgres(PgPool),
    Sqlserver(Box<mssql::Pool>),
}

/// Shared by every worker thread: connection pools are created once per resource.
pub struct SharedServices {
    env: Environment,
    databases: HashMap<String, Pool>,
    http: reqwest::Client,
    /// Database and HTTP calls run on the engine's main (multi-threaded) runtime. A socket is served by
    /// the runtime that opened it; if a worker thread's runtime opened it, its replies would
    /// wait while that thread is busy running JavaScript.
    io: tokio::runtime::Handle,
}

impl SharedServices {
    /// Pools connect lazily, so a database that's down at startup doesn't stop the engine;
    /// queries against it fail as temporary errors and the messages wait.
    ///
    /// Must be called on the engine's main runtime.
    pub fn new(env: Environment, resources: &Resources) -> Self {
        let databases = resources
            .databases
            .iter()
            .map(|(name, db)| {
                let pool = match db {
                    Database::Postgres(c) => Pool::Postgres(postgres_pool(c)),
                    Database::Sqlserver(c) => Pool::Sqlserver(Box::new(mssql::Pool::new(c))),
                };
                (name.clone(), pool)
            })
            .collect();
        Self { env, databases, http: http_client(), io: tokio::runtime::Handle::current() }
    }

    fn pool(&self, name: &str) -> Result<&Pool> {
        self.databases.get(name).ok_or_else(|| {
            let known: Vec<_> = self.databases.keys().map(String::as_str).collect();
            anyhow!("no database named `{name}` in resources.yaml (defined: {})", known.join(", "))
        })
    }

    fn spawn<T: Send + 'static>(&self, call: impl Future<Output = Result<T>> + Send + 'static) -> BoxFuture<T> {
        let task = self.io.spawn(call);
        Box::pin(async move { task.await.map_err(|e| anyhow!("engine service call failed: {e}"))? })
    }
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("ductus/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        .build()
        .expect("building the HTTP client")
}

/// The URL for messages: without query string, fragment or password (they may carry secrets).
fn shown(url: &reqwest::Url) -> String {
    let mut url = url.clone();
    url.set_query(None);
    url.set_fragment(None);
    let _ = url.set_password(None);
    url.to_string()
}

/// One HTTP request for interface code. Network errors and timeouts are temporary (the message
/// retries); every status is returned as a response, and the SDK decides what to retry.
async fn http_fetch(client: &reqwest::Client, r: FetchRequest) -> Result<FetchResponse> {
    let url = reqwest::Url::parse(&r.url).map_err(|e| anyhow!("fetch: invalid URL: {e}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("fetch: only http and https URLs are supported, not `{}:`", url.scheme());
    }
    let method = reqwest::Method::from_bytes(r.method.as_bytes())
        .map_err(|_| anyhow!("fetch: invalid method `{}`", r.method))?;
    let label = format!("{method} {}", shown(&url));
    let mut request = client.request(method, url).timeout(Duration::from_millis(r.timeout_ms.max(1)));
    for (name, value) in &r.headers {
        request = request.header(name, value);
    }
    if let Some(body) = r.body {
        request = request.body(body);
    }
    let failed = |e: reqwest::Error| {
        if e.is_builder() {
            return anyhow!("{label}: {}", e.without_url());
        }
        let why = if e.is_timeout() {
            format!("timed out after {} ms", r.timeout_ms)
        } else {
            // The chain has the useful part ("connection refused", "certificate expired").
            let e = e.without_url();
            let mut why = e.to_string();
            let mut source = std::error::Error::source(&e);
            while let Some(s) = source {
                why = format!("{why}: {s}");
                source = s.source();
            }
            why
        };
        anyhow!("{TRANSIENT} {label} failed: {why}")
    };
    let mut response = request.send().await.map_err(failed)?;
    let mut headers = BTreeMap::<String, String>::new();
    for (name, value) in response.headers() {
        let value = String::from_utf8_lossy(value.as_bytes());
        headers
            .entry(name.as_str().to_string())
            .and_modify(|v| {
                v.push_str(", ");
                v.push_str(&value);
            })
            .or_insert_with(|| value.into_owned());
    }
    let status = response.status().as_u16();
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(failed)? {
        if body.len() + chunk.len() > MAX_RESPONSE {
            bail!("{label}: the response is larger than {} MB", MAX_RESPONSE / 1024 / 1024);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(FetchResponse { status, headers, body: String::from_utf8_lossy(&body).into_owned() })
}

fn postgres_pool(c: &DbConnection) -> PgPool {
    let options = PgConnectOptions::new()
        .host(&c.host)
        .port(c.port.unwrap_or(5432))
        .database(&c.database)
        .username(&c.user)
        .password(&c.password);
    PgPoolOptions::new()
        .max_connections(c.max_connections)
        .acquire_timeout(std::time::Duration::from_secs(10))
        .connect_lazy_with(options)
}

/// Temporary failures (can't connect, pool exhausted, server restarting) are marked so the
/// worker retries the message instead of quarantining it. So are login failures (28...) and a
/// missing database (3D000): fixing the configuration fixes them, and the messages should wait
/// for that rather than all be quarantined.
fn classify(e: sqlx::Error) -> anyhow::Error {
    let transient = match &e {
        sqlx::Error::Io(_) | sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed | sqlx::Error::Tls(_) => true,
        sqlx::Error::Database(db) => db.code().is_some_and(|c| {
            ["08", "28", "53", "57P"].iter().any(|class| c.starts_with(class))
                || ["40001", "40P01", "3D000"].contains(&&*c)
        }),
        _ => false,
    };
    if transient { anyhow!("{TRANSIENT} database unavailable: {e}") } else { anyhow!("database error: {e}") }
}

macro_rules! bind_json {
    ($query:expr, $params:expr) => {{
        let mut q = $query;
        for p in $params {
            q = match p {
                Value::Null => q.bind(None::<String>),
                Value::Bool(b) => q.bind(b),
                Value::Number(n) => match n.as_i64() {
                    Some(i) => q.bind(i),
                    None => q.bind(n.as_f64()),
                },
                Value::String(s) => q.bind(s),
                other => q.bind(sqlx::types::Json(other)),
            };
        }
        q
    }};
}

// Interface code supplies SQL text with $1, $2... placeholders and passes values separately as
// bound parameters, which is the safe pattern: values never become SQL. sqlx asks for dynamic
// SQL to be marked as reviewed (`AssertSqlSafe`); the SQL here is the interface author's own.

async fn postgres_query(pool: &PgPool, sql: &str, params: Vec<Value>) -> Result<Vec<Value>> {
    // Postgres builds the JSON itself, so every column type converts the same way.
    let wrapped = format!(
        "WITH ductus_q AS ({}) SELECT coalesce(json_agg(ductus_q), '[]'::json) FROM ductus_q",
        sql.trim().trim_end_matches(';')
    );
    let rows: Value = bind_json!(sqlx::query_scalar::<_, Value>(sqlx::AssertSqlSafe(wrapped)), params)
        .fetch_one(pool)
        .await
        .map_err(classify)?;
    match rows {
        Value::Array(rows) => Ok(rows),
        other => Err(anyhow!("unexpected query result: {other}")),
    }
}

async fn postgres_execute(pool: &PgPool, sql: String, params: Vec<Value>) -> Result<u64> {
    let done = bind_json!(sqlx::query(sqlx::AssertSqlSafe(sql)), params).execute(pool).await.map_err(classify)?;
    Ok(done.rows_affected())
}

/// Per-thread handle (an `Rc<dyn Services>` lives in each isolate).
pub struct EngineServices(pub Arc<SharedServices>);

impl Services for EngineServices {
    fn db_query(&self, resource: &str, sql: &str, params: Vec<Value>) -> BoxFuture<Vec<Value>> {
        let shared = self.0.clone();
        let (resource, sql) = (resource.to_string(), sql.to_string());
        self.0.spawn(async move {
            match shared.pool(&resource)? {
                Pool::Postgres(p) => postgres_query(p, &sql, params).await,
                Pool::Sqlserver(p) => p.query(&sql, params).await,
            }
        })
    }

    fn db_execute(&self, resource: &str, sql: &str, params: Vec<Value>) -> BoxFuture<u64> {
        let shared = self.0.clone();
        let (resource, sql) = (resource.to_string(), sql.to_string());
        self.0.spawn(async move {
            match shared.pool(&resource)? {
                Pool::Postgres(p) => postgres_execute(p, sql, params).await,
                Pool::Sqlserver(p) => p.execute(&sql, params).await,
            }
        })
    }

    fn fetch(&self, request: FetchRequest) -> BoxFuture<FetchResponse> {
        let shared = self.0.clone();
        self.0.spawn(async move { http_fetch(&shared.http, request).await })
    }

    fn variable(&self, name: &str) -> Option<String> {
        self.0.env.variable(name)
    }

    fn secret(&self, name: &str) -> Result<Option<String>> {
        self.0.env.secret(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(url: &str, timeout_ms: u64) -> FetchRequest {
        FetchRequest {
            url: url.into(),
            method: "POST".into(),
            headers: BTreeMap::from([("x-token".to_string(), "abc".to_string())]),
            body: Some("hello".into()),
            timeout_ms,
        }
    }

    /// A local HTTP server: /echo answers with what it got, /slow takes 2 s, /big is 17 MB.
    async fn server() -> String {
        use axum::routing::{get, post};
        let app = axum::Router::new()
            .route(
                "/echo",
                post(|headers: axum::http::HeaderMap, body: String| async move {
                    let token = headers.get("x-token").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
                    let seen =
                        axum::response::AppendHeaders([("x-seen", token.clone()), ("x-seen", "twice".to_string())]);
                    (seen, format!("{token}:{body}"))
                }),
            )
            .route("/slow", post(|| async { tokio::time::sleep(Duration::from_secs(2)).await }))
            .route("/big", get(|| async { "x".repeat(MAX_RESPONSE + 1) }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{address}")
    }

    #[tokio::test]
    async fn sends_requests_and_returns_any_status() {
        let base = server().await;
        let client = http_client();
        let r = http_fetch(&client, request(&format!("{base}/echo?q=1"), 5000)).await.unwrap();
        assert_eq!((r.status, r.body.as_str()), (200, "abc:hello"));
        assert_eq!(r.headers["x-seen"], "abc, twice");

        // A status is never an error here: the SDK decides what to retry.
        let r = http_fetch(&client, request(&format!("{base}/missing"), 5000)).await.unwrap();
        assert_eq!(r.status, 404);
    }

    #[tokio::test]
    async fn network_failures_are_temporary_and_never_show_the_query() {
        let base = server().await;
        let client = http_client();
        let err =
            http_fetch(&client, request(&format!("{base}/slow?api_key=SECRET"), 200)).await.unwrap_err().to_string();
        assert!(err.starts_with(TRANSIENT) && err.contains("/slow failed: timed out after 200 ms"), "{err}");
        assert!(!err.contains("SECRET"), "{err}");

        let err =
            http_fetch(&client, request("http://127.0.0.1:1/x?api_key=SECRET", 5000)).await.unwrap_err().to_string();
        assert!(err.starts_with(TRANSIENT) && err.contains("POST http://127.0.0.1:1/x failed"), "{err}");
        assert!(!err.contains("SECRET"), "{err}");

        let mut big = request(&format!("{base}/big"), 5000);
        big.method = "GET".into();
        let err = http_fetch(&client, big).await.unwrap_err().to_string();
        assert!(!err.contains(TRANSIENT) && err.contains("larger than 16 MB"), "{err}");

        let err = http_fetch(&client, request("file:///etc/passwd", 5000)).await.unwrap_err().to_string();
        assert!(err.contains("only http and https"), "{err}");
    }
}
