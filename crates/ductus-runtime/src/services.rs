//! Engine services for interface code: named database pools, HTTP requests, environment
//! variables, secrets.

use crate::mssql;
use crate::stats::ResourceCalls;
use anyhow::{Result, anyhow, bail};
use ductus_core::pgtls::SslMode;
use ductus_core::resources::{Api, Database, DbConnection};
use ductus_core::{Environment, InterfaceConfig, Resources};
use ductus_script::{BoxFuture, FetchRequest, FetchResponse, Services, TRANSIENT};
use serde_json::Value;
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions, PgSslMode};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

/// Bigger responses are refused: interface code holds them in memory.
const MAX_RESPONSE: usize = 16 * 1024 * 1024;

pub(crate) enum Pool {
    Postgres(PgPool),
    Sqlserver(Box<mssql::Pool>),
}

/// Database pools by resource name.
pub(crate) type Pools = Arc<HashMap<String, Pool>>;

/// A pool for a database resource; `size` overrides its `max_connections`. `tls`: what its CA
/// and client certificate name, loaded (Postgres).
fn make_pool(db: &Database, size: Option<u32>, tls: Option<&PgTls>) -> Pool {
    match db {
        Database::Postgres(c) => {
            let c = DbConnection { max_connections: size.unwrap_or(c.max_connections), ..c.clone() };
            Pool::Postgres(postgres_pool(&c, tls.cloned().unwrap_or_default()))
        }
        Database::Sqlserver(c) => {
            Pool::Sqlserver(Box::new(mssql::Pool::new(&ductus_core::resources::SqlServerConnection {
                max_connections: size.unwrap_or(c.max_connections),
                ..c.clone()
            })))
        }
    }
}

/// Shared by every worker thread: connection pools are created once per resource.
pub struct SharedServices {
    env: Environment,
    definitions: BTreeMap<String, Database>,
    /// Postgres resources' CA and client certificate, loaded once (dedicated pools use them too).
    postgres_tls: HashMap<String, PgTls>,
    databases: HashMap<String, Pool>,
    apis: BTreeMap<String, Api>,
    http: reqwest::Client,
    /// APIs that name a CA or client certificate have a client of their own.
    api_http: BTreeMap<String, reqwest::Client>,
    /// Each resource's calls: counted and timed.
    calls: Arc<ResourceCalls>,
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
    pub fn new(env: Environment, resources: &Resources, certificates: &crate::tls::Certificates) -> Result<Self> {
        let mut postgres_tls = HashMap::new();
        for (name, db) in &resources.databases {
            if let Database::Postgres(c) = db {
                let tls = PgTls::load(c, certificates).map_err(|e| anyhow!("database `{name}`: {e:#}"))?;
                postgres_tls.insert(name.clone(), tls);
            }
        }
        let databases = resources
            .databases
            .iter()
            .map(|(name, db)| (name.clone(), make_pool(db, None, postgres_tls.get(name))))
            .collect();
        let mut api_http = BTreeMap::new();
        for (name, api) in resources.apis.iter().filter(|(_, a)| a.tls().is_set()) {
            let client = api_client(api, certificates).map_err(|e| anyhow!("api `{name}`: {e:#}"))?;
            api_http.insert(name.clone(), client);
        }
        Ok(Self {
            env,
            definitions: resources.databases.clone(),
            postgres_tls,
            databases,
            apis: resources.apis.clone(),
            http: http_client(),
            api_http,
            calls: Arc::new(ResourceCalls::new(resources.databases.keys().cloned(), resources.apis.keys().cloned())),
            io: tokio::runtime::Handle::current(),
        })
    }

    /// Each resource's call counts and timings (for the API and `/metrics`).
    pub(crate) fn calls(&self) -> Arc<ResourceCalls> {
        self.calls.clone()
    }

    /// Run a call, timed under `stats` when it's a known resource.
    async fn timed<T>(stats: Option<&crate::stats::CallStats>, call: impl Future<Output = Result<T>>) -> Result<T> {
        match stats {
            Some(stats) => stats.time(call).await,
            None => call.await,
        }
    }

    /// An interface's own pools (`dedicated_pools`), created once and shared by its worker
    /// threads. Names were checked against resources.yaml at startup.
    pub(crate) fn dedicated_pools(&self, config: &InterfaceConfig) -> Pools {
        Arc::new(
            config
                .dedicated_pools
                .iter()
                .filter_map(|(name, size)| {
                    let pool = make_pool(self.definitions.get(name)?, Some(*size), self.postgres_tls.get(name));
                    Some((name.clone(), pool))
                })
                .collect(),
        )
    }

    /// The interface's own pool for `name` if it has one, else the shared one.
    fn pool<'a>(&'a self, own: &'a HashMap<String, Pool>, name: &str) -> Result<&'a Pool> {
        own.get(name).or_else(|| self.databases.get(name)).ok_or_else(|| {
            let known: Vec<_> = self.databases.keys().map(String::as_str).collect();
            anyhow!("no database named `{name}` in resources.yaml (defined: {})", known.join(", "))
        })
    }

    fn spawn<T: Send + 'static>(&self, call: impl Future<Output = Result<T>> + Send + 'static) -> BoxFuture<T> {
        let task = self.io.spawn(call);
        Box::pin(async move { task.await.map_err(|e| anyhow!("engine service call failed: {e}"))? })
    }
}

fn http_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .user_agent(concat!("ductus/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
}

pub(crate) fn http_client() -> reqwest::Client {
    http_builder().build().expect("building the HTTP client")
}

/// The client for calls to `api` (one of its own when it names a CA or client certificate).
pub(crate) fn api_client(api: &Api, certificates: &crate::tls::Certificates) -> Result<reqwest::Client> {
    certificates.http(&api.tls(), http_builder())
}

/// The URL for messages: without query string, fragment or password (they may carry secrets).
fn shown(url: &reqwest::Url) -> String {
    let mut url = url.clone();
    url.set_query(None);
    url.set_fragment(None);
    let _ = url.set_password(None);
    url.to_string()
}

/// An `api(name)` request made concrete: its URL under the API's, the API's headers (code's own
/// win), and its timeout unless the code set one. Requests without `api` pass through.
fn resolve_api(apis: &BTreeMap<String, Api>, mut r: FetchRequest) -> Result<FetchRequest> {
    let Some(name) = r.api.take() else { return Ok(r) };
    let api = apis.get(&name).ok_or_else(|| {
        let known: Vec<_> = apis.keys().map(String::as_str).collect();
        let known = if known.is_empty() { "none".to_string() } else { known.join(", ") };
        anyhow!("no api named `{name}` in resources.yaml (defined: {known})")
    })?;
    let mut headers: BTreeMap<String, String> =
        api.headers.iter().map(|(k, v)| (k.to_ascii_lowercase(), v.clone())).collect();
    headers.extend(r.headers);
    Ok(FetchRequest {
        api: None,
        url: api.url_for(&r.url),
        headers,
        timeout_ms: r.timeout_ms.or(api.timeout.map(|t| t.as_millis() as u64)),
        ..r
    })
}

/// One HTTP request for interface code. Network errors and timeouts are temporary (the message
/// retries); every status is returned as a response, and the SDK decides what to retry.
async fn http_fetch(client: &reqwest::Client, r: FetchRequest) -> Result<FetchResponse> {
    let timeout_ms = r.timeout_ms.unwrap_or(30_000).max(1);
    let url = reqwest::Url::parse(&r.url).map_err(|e| anyhow!("fetch: invalid URL: {e}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("fetch: only http and https URLs are supported, not `{}:`", url.scheme());
    }
    let method = reqwest::Method::from_bytes(r.method.as_bytes())
        .map_err(|_| anyhow!("fetch: invalid method `{}`", r.method))?;
    let label = format!("{method} {}", shown(&url));
    let mut request = client.request(method, url).timeout(Duration::from_millis(timeout_ms));
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
            format!("timed out after {timeout_ms} ms")
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

/// A Postgres resource's `ca` and `certificate`, loaded: what its connections trust besides the
/// system's certificates, and present.
#[derive(Clone, Default)]
pub(crate) struct PgTls {
    ca: Option<Vec<u8>>,
    certificate: Option<Arc<crate::tls::Pair>>,
}

impl PgTls {
    /// The names were checked by `ductus_core::tls::check_outbound`; one that didn't load fails
    /// with why.
    pub(crate) fn load(c: &DbConnection, certificates: &crate::tls::Certificates) -> Result<Self> {
        let ca = c.ca.as_deref().map(|name| certificates.ca_pem(name).map(<[u8]>::to_vec)).transpose()?;
        let certificate = c.certificate.as_deref().map(|name| certificates.pair_or_why(name).cloned()).transpose()?;
        Ok(Self { ca, certificate })
    }
}

/// How the engine connects to a Postgres resource (also what `ductus doctor` checks): its
/// `tls:` mode, trusting the system's certificates plus its `ca`, presenting its `certificate`.
pub(crate) fn postgres_options(c: &DbConnection, tls: &PgTls) -> PgConnectOptions {
    let mode = match c.mode() {
        SslMode::Disable | SslMode::Allow => PgSslMode::Disable,
        SslMode::Prefer => PgSslMode::Prefer,
        SslMode::Require => PgSslMode::Require,
        SslMode::VerifyCa => PgSslMode::VerifyCa,
        SslMode::VerifyFull => PgSslMode::VerifyFull,
    };
    let mut options = PgConnectOptions::new()
        .host(&c.host)
        .port(c.port.unwrap_or(5432))
        .database(&c.database)
        .username(&c.user)
        .password(&c.password)
        .ssl_mode(mode);
    if let Some(ca) = &tls.ca {
        options = options.ssl_root_cert_from_pem(ca.clone());
    }
    match &tls.certificate {
        Some(pair) => with_client_certificate(options, &pair.pem().1),
        None => options,
    }
}

fn with_client_certificate(options: PgConnectOptions, pem: &crate::tls::PairPem) -> PgConnectOptions {
    options.ssl_client_cert_from_pem(&pem.0).ssl_client_key_from_pem(&pem.1)
}

/// The pool copies the client certificate into its options: a renewal from the pair's files is
/// put there too, so the next connection presents it (open ones keep theirs until they close).
fn postgres_pool(c: &DbConnection, tls: PgTls) -> PgPool {
    let pool = PgPoolOptions::new()
        .max_connections(c.max_connections)
        .acquire_timeout(std::time::Duration::from_secs(10))
        .connect_lazy_with(postgres_options(c, &tls));
    if let Some(pair) = &tls.certificate {
        let renewing = pool.clone();
        pair.on_renewal(move |pem| {
            let options = (*renewing.connect_options()).clone();
            renewing.set_connect_options(with_client_certificate(options, pem));
        });
    }
    pool
}

/// Temporary failures (can't connect, pool exhausted, server restarting) are marked so the
/// worker retries the message instead of quarantining it. So are login failures (28...) and a
/// missing database (3D000): fixing the configuration fixes them, and the messages should wait
/// for that rather than all be quarantined. A TLS refusal is said in words (the server's
/// certificate isn't from a CA we trust, or isn't for its host), and waits too.
fn classify(e: sqlx::Error, host: &str) -> anyhow::Error {
    if let Some((words, fix)) = ductus_store::tls::refusal(&e, host) {
        use ductus_store::tls::Fix;
        let advice = match fix {
            Fix::Ca => ": name its CA in `ca:` (resources.yaml), or install it on this server",
            Fix::Name => ": connect with a name it lists (`ductus doctor` shows them), or `tls: verify-ca`",
            Fix::ClientCertificate => ": is `certificate:` one the server's CA for clients issued?",
            _ => "",
        };
        return anyhow!("{TRANSIENT} database unavailable: TLS: {words}{advice}");
    }
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
        .map_err(|e| classify(e, pool.connect_options().get_host()))?;
    match rows {
        Value::Array(rows) => Ok(rows),
        other => Err(anyhow!("unexpected query result: {other}")),
    }
}

async fn postgres_execute(pool: &PgPool, sql: String, params: Vec<Value>) -> Result<u64> {
    let done = bind_json!(sqlx::query(sqlx::AssertSqlSafe(sql)), params)
        .execute(pool)
        .await
        .map_err(|e| classify(e, pool.connect_options().get_host()))?;
    Ok(done.rows_affected())
}

/// Per-thread handle (an `Rc<dyn Services>` lives in each isolate): the shared services plus,
/// on a dedicated interface's threads, that interface's own pools.
pub struct EngineServices {
    shared: Arc<SharedServices>,
    own: Pools,
}

impl EngineServices {
    pub(crate) fn new(shared: Arc<SharedServices>, own: Pools) -> Self {
        Self { shared, own }
    }
}

impl Services for EngineServices {
    fn db_query(&self, resource: &str, sql: &str, params: Vec<Value>) -> BoxFuture<Vec<Value>> {
        let (shared, own) = (self.shared.clone(), self.own.clone());
        let (resource, sql) = (resource.to_string(), sql.to_string());
        self.shared.spawn(async move {
            let call = async {
                match shared.pool(&own, &resource)? {
                    Pool::Postgres(p) => postgres_query(p, &sql, params).await,
                    Pool::Sqlserver(p) => p.query(&sql, params).await,
                }
            };
            SharedServices::timed(shared.calls.db(&resource), call).await
        })
    }

    fn db_execute(&self, resource: &str, sql: &str, params: Vec<Value>) -> BoxFuture<u64> {
        let (shared, own) = (self.shared.clone(), self.own.clone());
        let (resource, sql) = (resource.to_string(), sql.to_string());
        self.shared.spawn(async move {
            let call = async {
                match shared.pool(&own, &resource)? {
                    Pool::Postgres(p) => postgres_execute(p, sql, params).await,
                    Pool::Sqlserver(p) => p.execute(&sql, params).await,
                }
            };
            SharedServices::timed(shared.calls.db(&resource), call).await
        })
    }

    fn fetch(&self, request: FetchRequest) -> BoxFuture<FetchResponse> {
        let shared = self.shared.clone();
        self.shared.spawn(async move {
            let api = request.api.clone();
            let stats = shared.calls.http(api.as_deref());
            let client = api.as_ref().and_then(|a| shared.api_http.get(a)).unwrap_or(&shared.http);
            SharedServices::timed(stats, async { http_fetch(client, resolve_api(&shared.apis, request)?).await }).await
        })
    }

    fn variable(&self, name: &str) -> Option<String> {
        self.shared.env.variable(name)
    }

    fn secret(&self, name: &str) -> Result<Option<String>> {
        self.shared.env.secret(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Postgres test database as a resource with a one-connection shared pool.
    fn test_resources() -> Option<Resources> {
        let url = url::Url::parse(&std::env::var("DUCTUS_TEST_DATABASE_URL").ok()?).unwrap();
        let db = DbConnection {
            host: url.host_str()?.into(),
            port: url.port(),
            database: url.path().trim_start_matches('/').into(),
            user: url.username().into(),
            password: url.password().unwrap_or("").into(),
            max_connections: 1,
            tls: None,
            ca: None,
            certificate: None,
            application: None,
            vendor: None,
            owner: None,
            custom: Default::default(),
        };
        Some(Resources { databases: BTreeMap::from([("db".into(), Database::Postgres(db))]), ..Default::default() })
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_dedicated_pool_is_not_held_up_by_the_shared_one() {
        let Some(resources) = test_resources() else { return };
        let shared = Arc::new(
            SharedServices::new(Environment::with_variables("dev", &[]), &resources, &Default::default()).unwrap(),
        );
        let config: InterfaceConfig = serde_yaml_ng::from_str(
            "name: x\nsource: { mllp: { port: 1 } }\ndestination: { file: { dir: o } }\nisolation: dedicated\ndedicated_pools: { db: 1 }\n",
        )
        .unwrap();
        let busy = EngineServices::new(shared.clone(), Default::default());
        let own = EngineServices::new(shared.clone(), shared.dedicated_pools(&config));

        // Another interface holds the shared pool's only connection for a second.
        let slow = busy.db_query("db", "SELECT 1 AS slept FROM pg_sleep(1)", vec![]);
        tokio::time::sleep(Duration::from_millis(200)).await;

        let start = std::time::Instant::now();
        assert_eq!(own.db_query("db", "SELECT 1 AS one", vec![]).await.unwrap(), [serde_json::json!({"one": 1})]);
        assert!(start.elapsed() < Duration::from_millis(500), "own pool waited {:?}", start.elapsed());

        // The same query through the shared pool waits its turn.
        let start = std::time::Instant::now();
        busy.db_query("db", "SELECT 1 AS one", vec![]).await.unwrap();
        assert!(start.elapsed() >= Duration::from_millis(500), "shared pool took {:?}", start.elapsed());
        assert_eq!(slow.await.unwrap(), [serde_json::json!({"slept": 1})]);
    }

    /// `db()` over TLS from resources.yaml (a throwaway Postgres with its own CA): verify-full with
    /// the server's CA (`ca:`), refused in words with another CA, a client-certificate login, and a
    /// renewed client certificate presented by the pool's next connection.
    #[tokio::test(flavor = "multi_thread")]
    async fn postgres_resources_connect_over_verified_tls_with_a_renewable_client_certificate() {
        use ductus_core::resources::CertificateEntry;
        use sqlx::Connection;
        let Some(pg) = ductus_store::testing::PgTls::start() else { return };
        let dir = tempfile::tempdir().unwrap();
        let (cert, key) = (dir.path().join("client.pem"), dir.path().join("client.key"));
        std::fs::copy(pg.file("client.pem"), &cert).unwrap();
        std::fs::copy(pg.file("client.key"), &key).unwrap();
        let ca = |file: &str| CertificateEntry { ca_file: Some(pg.dir.join(file)), ..Default::default() };
        let entries = BTreeMap::from([
            ("pg-ca".to_string(), ca("ca.pem")),
            ("other-ca".to_string(), ca("other-ca.pem")),
            (
                "pg-client".to_string(),
                CertificateEntry { cert_file: Some(cert.clone()), key_file: Some(key.clone()), ..Default::default() },
            ),
        ]);
        let certificates = crate::tls::Certificates::load(&entries).unwrap();
        let p = pg.port;
        let resources: Resources = serde_yaml_ng::from_str(&format!(
            "databases:
  ok: {{ postgres: {{ host: 127.0.0.1, port: {p}, database: ductus, user: ductus, password: ductus, ca: pg-ca }} }}
  wrong: {{ postgres: {{ host: 127.0.0.1, port: {p}, database: ductus, user: ductus, password: ductus, ca: other-ca }} }}
  cert: {{ postgres: {{ host: 127.0.0.1, port: {p}, database: ductus, user: ductus_cert, ca: pg-ca, certificate: pg-client }} }}
"
        ))
        .unwrap();
        let shared =
            Arc::new(SharedServices::new(Environment::with_variables("dev", &[]), &resources, &certificates).unwrap());
        let services = EngineServices::new(shared.clone(), Default::default());
        const TLS: &str = "SELECT version, client_serial::text AS serial FROM pg_stat_ssl WHERE pid = pg_backend_pid()";

        let rows = services.db_query("ok", TLS, vec![]).await.unwrap();
        assert_eq!(rows[0]["version"], "TLSv1.3");
        let err = services.db_query("wrong", "SELECT 1", vec![]).await.unwrap_err().to_string();
        assert!(err.starts_with(TRANSIENT), "a refused handshake retries: {err}");
        assert!(err.contains("isn't from a CA this server trusts: name its CA in `ca:`"), "{err}");

        // The client certificate is the login (pg_hba `cert`): no password.
        let before = services.db_query("cert", TLS, vec![]).await.unwrap()[0]["serial"].clone();
        assert!(before.is_string(), "a client certificate was presented: {before}");
        // Renewed from the same CA, written over the files: the next connection presents it.
        let run = |args: &[&str]| {
            assert!(std::process::Command::new("openssl").args(args).output().unwrap().status.success())
        };
        let (new_key, csr, new_cert) =
            (dir.path().join("new.key"), dir.path().join("new.csr"), dir.path().join("new.pem"));
        let path = |p: &std::path::Path| p.to_str().unwrap().to_string();
        run(&[
            "req",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-subj",
            "/CN=ductus_cert",
            "-keyout",
            &path(&new_key),
            "-out",
            &path(&csr),
        ]);
        run(&[
            "x509",
            "-req",
            "-in",
            &path(&csr),
            "-CA",
            &pg.file("ca.pem"),
            "-CAkey",
            &pg.file("ca.key"),
            "-CAcreateserial",
            "-days",
            "30",
            "-out",
            &path(&new_cert),
        ]);
        std::fs::rename(&new_cert, &cert).unwrap();
        std::fs::rename(&new_key, &key).unwrap();
        assert_eq!(certificates.reload().await, ["pg-client"]);
        let Pool::Postgres(pool) = &shared.databases["cert"] else { unreachable!() };
        let mut next = sqlx::PgConnection::connect_with(&pool.connect_options()).await.unwrap();
        let after: Option<String> =
            sqlx::query_scalar("SELECT client_serial::text FROM pg_stat_ssl WHERE pid = pg_backend_pid()")
                .fetch_one(&mut next)
                .await
                .unwrap();
        assert_ne!(Some(before.as_str().unwrap().to_string()), after, "the renewed certificate is presented");
    }

    fn request(url: &str, timeout_ms: u64) -> FetchRequest {
        FetchRequest {
            api: None,
            url: url.into(),
            method: "POST".into(),
            headers: BTreeMap::from([("x-token".to_string(), "abc".to_string())]),
            body: Some("hello".into()),
            timeout_ms: Some(timeout_ms),
        }
    }

    #[tokio::test]
    async fn api_requests_get_the_api_url_headers_and_timeout() {
        let base = server().await;
        let resources: Resources = serde_yaml_ng::from_str(&format!(
            "apis:\n  echo: {{ url: '{base}/', headers: {{ X-Token: from-api, X-Other: kept }}, timeout: 300ms }}\n"
        ))
        .unwrap();
        let client = http_client();
        let mut r = request("/echo", 0);
        r.api = Some("echo".into());
        r.timeout_ms = None;
        let resolved = resolve_api(&resources.apis, r.clone()).unwrap();
        assert_eq!(resolved.url, format!("{base}/echo"));
        assert_eq!(resolved.timeout_ms, Some(300));
        assert_eq!(
            (resolved.headers["x-token"].as_str(), resolved.headers["x-other"].as_str()),
            ("abc", "kept"),
            "the code's header wins"
        );

        r.headers.clear();
        let response = http_fetch(&client, resolve_api(&resources.apis, r.clone()).unwrap()).await.unwrap();
        assert_eq!(response.body, "from-api:hello", "the API's header was sent");

        r.url = "/slow".into();
        let err = http_fetch(&client, resolve_api(&resources.apis, r.clone()).unwrap()).await.unwrap_err().to_string();
        assert!(err.contains("timed out after 300 ms"), "{err}");

        r.api = Some("nope".into());
        let err = resolve_api(&resources.apis, r).unwrap_err().to_string();
        assert_eq!(err, "no api named `nope` in resources.yaml (defined: echo)");
    }

    /// Calls are counted and timed per resource: a named API's under its name, plain URLs under
    /// `fetch`; failures count as errors, a name resources.yaml doesn't have counts nowhere.
    #[tokio::test(flavor = "multi_thread")]
    async fn calls_are_counted_per_resource() {
        let base = server().await;
        let resources: Resources =
            serde_yaml_ng::from_str(&format!("apis:\n  echo: {{ url: '{base}/', timeout: 300ms }}\n")).unwrap();
        let shared = Arc::new(
            SharedServices::new(Environment::with_variables("dev", &[]), &resources, &Default::default()).unwrap(),
        );
        let services = EngineServices::new(shared.clone(), Default::default());
        let via = |api: Option<&str>, url: &str| FetchRequest {
            api: api.map(Into::into),
            timeout_ms: None,
            ..request(url, 0)
        };

        services.fetch(via(Some("echo"), "/echo")).await.unwrap();
        services.fetch(via(Some("echo"), "/slow")).await.unwrap_err(); // times out
        services.fetch(via(Some("nope"), "/echo")).await.unwrap_err();
        services.fetch(request(&format!("{base}/echo"), 5000)).await.unwrap();

        let calls = shared.calls();
        let echo = calls.http(Some("echo")).unwrap();
        assert_eq!((echo.calls.count(), crate::stats::get(&echo.errors)), (2, 1));
        assert!(echo.calls.quantile(1.0).unwrap() >= Duration::from_millis(300), "the timeout was timed");
        let fetch = calls.http(None).unwrap();
        assert_eq!((fetch.calls.count(), crate::stats::get(&fetch.errors)), (1, 0));
        assert!(calls.http(Some("nope")).is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn api_calls_trust_the_api_s_ca_and_present_its_client_certificate() {
        use crate::tls::testing::{ca, receiver_and_client, receiving};
        let (server_ca, client_ca) = (ca("API's CA"), ca("clients' CA"));
        let certificates = receiver_and_client(&server_ca, &client_ca);
        let app = axum::Router::new().route("/patients/1", axum::routing::get(|| async { "Ada" }));
        let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = tcp.local_addr().unwrap().port();
        let tls = certificates.server(&receiving(true), &[b"http/1.1"]).unwrap();
        let listener = crate::tls::Listener::new(tcp, Some(tls), "test", None).unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let resources: Resources = serde_yaml_ng::from_str(&format!(
            "apis:\n  empi: {{ url: 'https://localhost:{port}/', ca: server-ca, certificate: client }}\n  empi-no-client: {{ url: 'https://localhost:{port}/', ca: server-ca }}\n  empi-platform: {{ url: 'https://localhost:{port}/' }}\n"
        ))
        .unwrap();
        let shared =
            Arc::new(SharedServices::new(Environment::with_variables("dev", &[]), &resources, &certificates).unwrap());
        let services = EngineServices::new(shared, Default::default());
        let get = |api: &str| FetchRequest {
            api: Some(api.into()),
            url: "/patients/1".into(),
            method: "GET".into(),
            headers: BTreeMap::new(),
            body: None,
            timeout_ms: Some(5000),
        };
        let r = services.fetch(get("empi")).await.unwrap();
        assert_eq!((r.status, r.body.as_str()), (200, "Ada"));
        for refused in ["empi-no-client", "empi-platform"] {
            let err = services.fetch(get(refused)).await.unwrap_err().to_string();
            assert!(err.starts_with(TRANSIENT), "{refused}: a refused handshake retries: {err}");
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
