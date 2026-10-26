//! SQL Server for `db("name")`: a small pool of tiberius connections; rows become JSON objects.
//!
//! Interface code writes SQL the same way as for Postgres: `$1, $2...` placeholders (translated
//! to SQL Server's `@P1, @P2...`) with values bound separately, never pasted into the SQL.

use anyhow::anyhow;
use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime};
use ductus_core::resources::{Encrypt, Login, SqlServerConnection};
use ductus_script::TRANSIENT;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tiberius::error::Error;
use tiberius::{AuthMethod, Client, ColumnData, Config, EncryptionLevel, FromSql, Query, SqlBrowser};
use tokio::net::TcpStream;
use tokio::sync::{Semaphore, SemaphorePermit};
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

type Connection = Client<Compat<TcpStream>>;

/// How long a call waits for a free or new connection (same as the Postgres pools).
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(10);
/// Idle connections older than this are closed instead of reused: firewalls between networks
/// often drop quiet connections without telling either side.
const MAX_IDLE: Duration = Duration::from_secs(300);

/// Server errors that mean "try again later", not "this message is wrong". Login and
/// cannot-open-database errors are here too: they're fixed by fixing the configuration or
/// the server, and the messages should wait for that instead of being quarantined.
const TRANSIENT_CODES: &[u32] = &[
    1205,  // chosen as deadlock victim
    1222,  // lock request timed out
    701,   // out of memory
    8645,  // timed out waiting for memory
    4060,  // cannot open database (offline, restoring, failing over, or no access)
    18456, // login failed
    18452, // Windows login refused by Windows (worded by `windows_refusal`, as 18456 for Windows logins)
    976, 978, 983, // availability group replica not readable / changing role
    40197, 40501, 40613, 49918, 49919, 49920, 4221, 10928, 10929, // Azure SQL: busy or reconfiguring
    233, 10053, 10054, 10060, 64, // connection broken
];

/// Who and how a login was made: one statement, any login may run it (`sys.dm_exec_connections`
/// needs VIEW SERVER PERFORMANCE STATE on 2022; `CONNECTIONPROPERTY` doesn't).
pub(crate) const WHO_AND_HOW: &str =
    "SELECT SUSER_SNAME() AS who, CAST(CONNECTIONPROPERTY('auth_scheme') AS nvarchar(40)) AS scheme";

/// Resources whose Windows login has been said in the log, with what was said: once per SQL
/// Server for the process, whichever of its pools (shared or dedicated) connects first.
static SAID: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

pub struct Pool {
    config: Config,
    /// Look the port up through the SQL Server Browser (named instance, no fixed port).
    browse: bool,
    /// What its TLS refusals are worded with.
    tls: TlsSettings,
    /// What its Windows login refusals and its log line are worded with.
    login: LoginSettings,
    /// The resource's name, when its first Windows login is to be said in the log.
    say_as: Option<String>,
    idle: Mutex<Vec<(Connection, Instant)>>,
    permits: Semaphore,
}

/// A connection's login, for words: who, where, and whether NTLM is all it can do.
#[derive(Clone)]
pub(crate) struct LoginSettings {
    /// `Login`'s words: "Windows login `HOSPITAL\svc-lab`".
    said: String,
    windows: bool,
    ntlm_only: bool,
    /// The SPN Kerberos would need: `MSSQLSvc/sql1.hospital.org:1433` (or `:INSTANCE`).
    spn: String,
    local: bool,
}

impl LoginSettings {
    pub(crate) fn new(c: &SqlServerConnection) -> Self {
        let at = match (&c.instance, c.port) {
            (Some(instance), None) => instance.clone(),
            (_, port) => port.unwrap_or(1433).to_string(),
        };
        Self {
            said: c.login().to_string(),
            windows: !matches!(c.login(), Login::Sql(_)),
            ntlm_only: c.ntlm_only(),
            spn: format!("MSSQLSvc/{}:{at}", c.host),
            local: ductus_core::pgtls::local(&c.host),
        }
    }

    /// How a login went, from `CONNECTIONPROPERTY('auth_scheme')`: `Windows, Kerberos`;
    /// `Windows, NTLM: ...` with why and what to do; `SQL login`.
    pub(crate) fn how(&self, scheme: &str) -> String {
        match scheme.trim().to_ascii_uppercase().as_str() {
            "KERBEROS" => "Windows, Kerberos".into(),
            "NTLM" if self.ntlm_only => "Windows, NTLM: from Linux it always is".into(),
            "NTLM" if self.local => "Windows, NTLM: usual for a SQL Server on this machine".into(),
            "NTLM" => format!(
                "Windows, NTLM: no Kerberos, most likely no SPN for {} (ask the DBA to register it; NTLM is being \
                 retired)",
                self.spn
            ),
            "SQL" => "SQL login".into(),
            other => other.to_string(),
        }
    }
}

/// A refused Windows login in words: 18452 (Windows didn't accept the account: SQL Server says
/// "untrusted domain" whatever the cause) or 18456 (accepted, but no way in on SQL Server's side).
pub(crate) fn windows_refusal(login: &LoginSettings, code: u32) -> String {
    let account = if login.said.contains("the account the engine runs as") {
        " (on the network a service's own account, `NT SERVICE\\Ductus`, is the computer account: \
         DOMAIN\\COMPUTER$)"
    } else {
        ""
    };
    if code == 18456 {
        return format!(
            "SQL Server refused the {}{account}: Windows accepted the account, but SQL Server has no login for it \
             (CREATE LOGIN ... FROM WINDOWS), or the login is disabled or can't open its default database \
             (SQL Server error 18456)",
            login.said
        );
    }
    let ntlm = if login.ntlm_only { "NTLM (all a login from Linux can use) blocked" } else { "NTLM blocked" };
    format!(
        "SQL Server refused the {}{account}: a wrong password, an account the server's Windows doesn't know, or \
         {ntlm} by the domain (SQL Server error 18452 says \"untrusted domain\" for all three)",
        login.said
    )
}

/// A connection's TLS settings, for wording its refusals.
#[derive(Clone)]
pub(crate) struct TlsSettings {
    name: String,
    named_ca: bool,
    strict: bool,
}

impl Pool {
    /// Connects lazily, like the Postgres pools: nothing is opened until the first query. `ca`:
    /// the PEM of its `ca` (trusted besides the system's certificates).
    pub fn new(c: &SqlServerConnection, ca: Option<&[u8]>) -> Self {
        let mut config = Config::new();
        config.host(&c.host);
        if let Some(port) = c.port {
            config.port(port);
        }
        if let Some(instance) = &c.instance {
            config.instance_name(instance);
        }
        config.database(&c.database);
        config.authentication(match c.login() {
            Login::Sql(user) => AuthMethod::sql_server(user, &c.password),
            Login::Windows(user) => AuthMethod::windows(user, &c.password),
            // resources.yaml refuses integrated anywhere but Windows.
            #[cfg(windows)]
            Login::Integrated => AuthMethod::Integrated,
            #[cfg(not(windows))]
            Login::Integrated => AuthMethod::None,
        });
        config.application_name("ductus");
        config.encryption(match c.encrypt {
            Encrypt::Mandatory => EncryptionLevel::Required,
            Encrypt::Strict => EncryptionLevel::Strict,
        });
        if let Some(name) = &c.name_in_certificate {
            config.hostname_in_certificate(name);
        }
        // resources.yaml refuses `ca` next to `trust_server_certificate` (tiberius panics on both).
        if c.trust_server_certificate {
            config.trust_cert();
        } else if let Some(pem) = ca {
            config.trust_cert_ca_bundle(pem.to_vec());
        }
        Self {
            config,
            browse: c.instance.is_some() && c.port.is_none(),
            tls: TlsSettings {
                name: c.certificate_name().to_string(),
                named_ca: ca.is_some(),
                strict: c.encrypt == Encrypt::Strict,
            },
            login: LoginSettings::new(c),
            say_as: None,
            idle: Mutex::new(Vec::new()),
            permits: Semaphore::new(c.max_connections.max(1) as usize),
        }
    }

    /// Its first Windows login is said in the log as the resource `name`'s (once per resource):
    /// who the server took it for and how (Kerberos or NTLM). SQL logins aren't: nothing to learn.
    pub fn said_as(mut self, name: &str) -> Self {
        if self.login.windows {
            self.say_as = Some(name.to_string());
        }
        self
    }

    pub async fn query(&self, sql: &str, params: Vec<Value>) -> anyhow::Result<Vec<Value>> {
        let mut lease = self.lease().await?;
        let result = async {
            let stream = prepare(sql, params).query(lease.connection()).await?;
            stream.into_first_result().await
        }
        .await;
        let rows = lease.finish(result)?;
        let Some(first) = rows.first() else { return Ok(Vec::new()) };
        // Unnamed columns (`SELECT COUNT(*)`) are named by position: column1, column2...
        let names: Vec<String> = first
            .columns()
            .iter()
            .enumerate()
            .map(|(i, c)| if c.name().is_empty() { format!("column{}", i + 1) } else { c.name().to_string() })
            .collect();
        rows.into_iter()
            .map(|row| {
                let mut object = Map::with_capacity(names.len());
                for (name, cell) in names.iter().zip(row) {
                    object.insert(name.clone(), to_json(&cell).map_err(|e| anyhow!("column `{name}`: {e}"))?);
                }
                Ok(Value::Object(object))
            })
            .collect()
    }

    pub async fn execute(&self, sql: &str, params: Vec<Value>) -> anyhow::Result<u64> {
        let mut lease = self.lease().await?;
        let result = prepare(sql, params).execute(lease.connection()).await;
        Ok(lease.finish(result)?.total())
    }

    async fn lease(&self) -> anyhow::Result<Lease<'_>> {
        let acquire = async {
            let permit = self.permits.acquire().await.expect("the pool's semaphore is never closed");
            let reused = {
                let mut idle = self.idle.lock().expect("pool lock");
                idle.retain(|(_, since)| since.elapsed() < MAX_IDLE);
                idle.pop()
            };
            let connection = match reused {
                Some((c, _)) => c,
                None => {
                    let mut c = self.connect().await.map_err(|e| self.classify(e))?;
                    self.say(&mut c).await;
                    c
                }
            };
            Ok(Lease { pool: self, connection: Some(connection), _permit: permit })
        };
        tokio::time::timeout(ACQUIRE_TIMEOUT, acquire).await.unwrap_or_else(|_| {
            Err(anyhow!("{TRANSIENT} database unavailable: no SQL Server connection within {ACQUIRE_TIMEOUT:?}"))
        })
    }

    async fn connect(&self) -> Result<Connection, Error> {
        let mut config = self.config.clone();
        let mut redirected = false;
        loop {
            let tcp = if self.browse && !redirected {
                TcpStream::connect_named(&config).await?
            } else {
                ductus_mllp::net::connect(config.get_addr()).await?
            };
            tcp.set_nodelay(true)?;
            match Client::connect(config.clone(), tcp.compat_write()).await {
                // Azure SQL and availability groups can send the client to another server.
                Err(Error::Routing { host, port }) if !redirected => {
                    config.host(host);
                    config.port(port);
                    redirected = true;
                }
                other => return other,
            }
        }
    }

    /// The first Windows login of this resource, said once: who and how. One statement on the
    /// new connection (only for Windows logins, once per process); a failure says nothing.
    async fn say(&self, connection: &mut Connection) {
        let Some(name) = &self.say_as else { return };
        {
            let mut said = SAID.lock().expect("said lock");
            let said = said.get_or_insert_default();
            if said.contains_key(name) {
                return;
            }
            said.insert(name.clone(), String::new());
        }
        let row = async { connection.simple_query(WHO_AND_HOW).await?.into_row().await }.await;
        if let Ok(Some(row)) = row {
            let who: Option<&str> = row.get("who");
            let scheme: Option<&str> = row.get("scheme");
            let line = format!(
                "SQL Server logged us in as {} ({})",
                who.unwrap_or("?"),
                self.login.how(scheme.unwrap_or("?"))
            );
            tracing::info!(database = name.as_str(), "{line}");
            SAID.lock().expect("said lock").get_or_insert_default().insert(name.clone(), line);
        }
    }

    /// What was said of resource `name`'s first Windows login, if anything (for tests).
    #[cfg(test)]
    fn said(name: &str) -> Option<String> {
        SAID.lock().expect("said lock").as_ref()?.get(name).cloned()
    }

    /// Temporary failures are marked so the worker retries the message instead of
    /// quarantining it (see `TRANSIENT_CODES`).
    fn classify(&self, e: Error) -> anyhow::Error {
        if let Some(words) = refusal(&e, &self.tls) {
            return anyhow!("{TRANSIENT} database unavailable: TLS: {words}");
        }
        match &e {
            Error::Server(t) if t.code() == 18452 || (t.code() == 18456 && self.login.windows) => {
                anyhow!("{TRANSIENT} database unavailable: {}", windows_refusal(&self.login, t.code()))
            }
            Error::Io { .. } | Error::Tls(_) | Error::Routing { .. } => {
                anyhow!("{TRANSIENT} database unavailable: {e}")
            }
            // sspi-rs couldn't make the NTLM login (a malformed account name, an answer it can't read).
            #[cfg(unix)]
            Error::SspiRs(_) => {
                anyhow!("{TRANSIENT} database unavailable: the {} couldn't be made: {e}", self.login.said)
            }
            Error::Server(t) if TRANSIENT_CODES.contains(&t.code()) => {
                anyhow!("{TRANSIENT} database unavailable: {}", t.message())
            }
            Error::Server(t) => anyhow!("database error: {} (SQL Server error {})", t.message(), t.code()),
            _ => anyhow!("database error: {e}"),
        }
    }
}

/// The address a named instance listens on, from the SQL Server Browser (UDP 1434).
pub(crate) async fn instance_address(c: &SqlServerConnection) -> Result<std::net::SocketAddr, Error> {
    let mut config = Config::new();
    config.host(&c.host);
    if let Some(instance) = &c.instance {
        config.instance_name(instance);
    }
    Ok(TcpStream::connect_named(&config).await?.peer_addr()?)
}

/// A refused TLS handshake in plain words, with what to do; None for anything else. tiberius
/// flattens rustls's error into a string with advice in its own API's terms (`Error::Tls`), so the
/// verdict is read from rustls's rendered name (tests against a real SQL Server guard each one).
pub(crate) fn refusal(e: &Error, tls: &TlsSettings) -> Option<String> {
    let name = &tls.name;
    let text = match e {
        Error::Tls(text) => text.as_str(),
        // A server that doesn't accept TDS 8 strict closes the connection during the handshake.
        Error::Io { message, .. } if tls.strict && (message.contains("eof") || message.contains("reset")) => {
            return Some(format!(
                "the server closed the connection during the TDS 8 strict handshake ({message}): it doesn't accept \
                 strict, or has no certificate configured for it (SQL Server 2022 on Linux never does). Ask its DBA, \
                 or leave out `encrypt: strict`"
            ));
        }
        _ => return None,
    };
    let ca = if tls.named_ca {
        "the CA named in `ca` didn't issue it either: ask the DBA which CA did"
    } else {
        "name the CA that issued it in `ca` (a `certificates:` entry), or install that CA on this server"
    };
    Some(if text.contains("UnsupportedCertVersion") {
        "the server's certificate is an old X.509 v1 one (as SQL Server's own generated certificate is), which \
         can't be verified: ask the DBA for a certificate from your CA (mssql-conf network.tlscert, or SQL Server \
         Configuration Manager)"
            .to_string()
    } else if text.contains("UnknownIssuer") {
        format!(
            "not trusted: its issuer isn't one this server trusts (self-signed, or a private CA): {ca} (SQL Server's \
             own generated certificate can't be verified by anyone: it needs one from a CA)"
        )
    } else if text.contains("not valid for name") || text.contains("NotValidForName") {
        format!(
            "the certificate isn't for {name}: connect by a name it lists, or set `name_in_certificate` to one (an \
             availability group listener or alias whose name isn't on it)"
        )
    } else if text.contains("expired") || text.contains("Expired") {
        "the server's certificate has expired: its owner must renew it".to_string()
    } else if text.contains("not valid yet") || text.contains("NotValidYet") {
        "the server's certificate isn't valid yet: check this server's clock".to_string()
    } else if text.contains("CaUsedAsEndEntity") {
        "the server presents a CA's certificate as its own: it needs a server certificate issued by that CA".to_string()
    } else {
        // Not a verdict on the certificate (no shared protocol version or cipher, a broken connection):
        // rustls's words, without tiberius's advice.
        let first = text.split(". To connect anyway").next().unwrap_or(text);
        format!("the handshake failed: {first}")
    })
}

/// A connection borrowed from the pool. It goes back only through `finish` after a call that
/// succeeded: a failed (or cancelled) call can leave a connection mid-response or inside an
/// open transaction, so it's closed instead.
struct Lease<'a> {
    pool: &'a Pool,
    connection: Option<Connection>,
    _permit: SemaphorePermit<'a>,
}

impl Lease<'_> {
    fn connection(&mut self) -> &mut Connection {
        self.connection.as_mut().expect("leased connection")
    }

    fn finish<T>(mut self, result: Result<T, Error>) -> anyhow::Result<T> {
        match result {
            Ok(value) => {
                let connection = self.connection.take().expect("leased connection");
                self.pool.idle.lock().expect("pool lock").push((connection, Instant::now()));
                Ok(value)
            }
            Err(e) => Err(self.pool.classify(e)),
        }
    }
}

fn prepare(sql: &str, params: Vec<Value>) -> Query<'static> {
    let mut query = Query::new(placeholders(sql));
    for p in params {
        match p {
            Value::Null => query.bind(None::<String>),
            Value::Bool(b) => query.bind(b),
            Value::Number(n) => match n.as_i64() {
                Some(i) => query.bind(i),
                None => query.bind(n.as_f64()),
            },
            Value::String(s) => query.bind(s),
            // SQL Server has no JSON type: objects and arrays go as JSON text (use OPENJSON).
            other => query.bind(other.to_string()),
        }
    }
    query
}

/// `$1` → `@P1`, outside string literals, quoted names and comments.
fn placeholders(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len() + 8);
    let mut chars = sql.chars().peekable();
    let mut previous = ' ';
    while let Some(c) = chars.next() {
        out.push(c);
        match c {
            '\'' | '"' | '[' => {
                let close = if c == '[' { ']' } else { c };
                // A doubled closing character is an escaped one and stays inside.
                while let Some(d) = chars.next() {
                    out.push(d);
                    if d == close {
                        if chars.peek() == Some(&close) {
                            out.push(chars.next().unwrap());
                        } else {
                            break;
                        }
                    }
                }
            }
            '-' if chars.peek() == Some(&'-') => {
                for d in chars.by_ref() {
                    out.push(d);
                    if d == '\n' {
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                out.push(chars.next().unwrap());
                let mut depth = 1; // SQL Server block comments nest
                while depth > 0 {
                    let Some(d) = chars.next() else { break };
                    out.push(d);
                    if d == '*' && chars.peek() == Some(&'/') {
                        out.push(chars.next().unwrap());
                        depth -= 1;
                    } else if d == '/' && chars.peek() == Some(&'*') {
                        out.push(chars.next().unwrap());
                        depth += 1;
                    }
                }
            }
            // `abc$1` is an identifier, not a placeholder.
            '$' if chars.peek().is_some_and(char::is_ascii_digit)
                && !(previous.is_alphanumeric() || matches!(previous, '_' | '@' | '#' | '$')) =>
            {
                out.pop();
                out.push_str("@P");
            }
            _ => {}
        }
        previous = c;
    }
    out
}

/// Values convert like Postgres's `json_agg` does, so a lookup reads the same from either
/// database: numbers stay numbers, times are ISO 8601 text, binary is `\x` + hex.
fn to_json(cell: &ColumnData<'static>) -> Result<Value, Error> {
    fn opt<T: Into<Value>>(v: Option<T>) -> Value {
        v.map_or(Value::Null, Into::into)
    }
    Ok(match cell {
        ColumnData::U8(v) => opt(*v),
        ColumnData::I16(v) => opt(*v),
        ColumnData::I32(v) => opt(*v),
        ColumnData::I64(v) => opt(*v),
        ColumnData::F32(v) => opt(v.map(f64::from)),
        ColumnData::F64(v) => opt(*v),
        ColumnData::Bit(v) => opt(*v),
        ColumnData::String(v) => opt(v.as_deref()),
        // Upper case, as SQL Server prints them.
        ColumnData::Guid(v) => opt(v.map(|g| g.to_string().to_uppercase())),
        ColumnData::Binary(v) => opt(v.as_deref().map(|b| {
            let hex: String = b.iter().map(|byte| format!("{byte:02x}")).collect();
            format!("\\x{hex}")
        })),
        ColumnData::Numeric(v) => match v {
            None => Value::Null,
            Some(n) if n.scale() == 0 => {
                i64::try_from(n.value()).map_or_else(|_| Value::from(n.value() as f64), Value::from)
            }
            Some(n) => n.to_string().parse::<f64>().map_or(Value::Null, Value::from),
        },
        ColumnData::Xml(v) => opt(v.as_deref().map(ToString::to_string)),
        ColumnData::DateTime(_) | ColumnData::SmallDateTime(_) | ColumnData::DateTime2(_) => {
            opt(NaiveDateTime::from_sql(cell)?.map(|t| t.format("%Y-%m-%dT%H:%M:%S%.f").to_string()))
        }
        ColumnData::Date(_) => opt(NaiveDate::from_sql(cell)?.map(|d| d.format("%Y-%m-%d").to_string())),
        ColumnData::Time(_) => opt(NaiveTime::from_sql(cell)?.map(|t| t.format("%H:%M:%S%.f").to_string())),
        ColumnData::DateTimeOffset(_) => {
            opt(DateTime::<FixedOffset>::from_sql(cell)?.map(|t| t.format("%Y-%m-%dT%H:%M:%S%.f%:z").to_string()))
        }
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn translates_placeholders_outside_literals_names_and_comments() {
        assert_eq!(
            placeholders("SELECT * FROM t WHERE a = $1 AND b = $12"),
            "SELECT * FROM t WHERE a = @P1 AND b = @P12"
        );
        assert_eq!(
            placeholders("SELECT '$1', 'it''s $2', N'$3' WHERE x = $4"),
            "SELECT '$1', 'it''s $2', N'$3' WHERE x = @P4"
        );
        assert_eq!(
            placeholders("SELECT [col $1], \"q $2\" FROM [a]]$3] WHERE y=$1"),
            "SELECT [col $1], \"q $2\" FROM [a]]$3] WHERE y=@P1"
        );
        assert_eq!(placeholders("SELECT 1 -- $1\nWHERE z = ($2)"), "SELECT 1 -- $1\nWHERE z = (@P2)");
        assert_eq!(placeholders("/* $1 /* $2 */ $3 */ SELECT $4"), "/* $1 /* $2 */ $3 */ SELECT @P4");
        assert_eq!(placeholders("SELECT price$1, $ FROM t WHERE x=$1"), "SELECT price$1, $ FROM t WHERE x=@P1");
        assert_eq!(placeholders("SELECT 'unterminated $1"), "SELECT 'unterminated $1");
    }

    /// Runs against a real SQL Server when DUCTUS_TEST_SQLSERVER is set (`user:password@host:port`;
    /// CI sets it, `scripts/dev/sqlserver.sh` starts one locally).
    pub(crate) fn test_connection() -> Option<SqlServerConnection> {
        std::env::var("DUCTUS_TEST_SQLSERVER").ok().map(|spec| test_connection_to(&spec))
    }

    /// `user:password@host:port` as a connection: master, one connection, not verified.
    fn test_connection_to(spec: &str) -> SqlServerConnection {
        let (credentials, address) = spec.rsplit_once('@').expect("DUCTUS_TEST_SQLSERVER = user:password@host:port");
        let (user, password) = credentials.split_once(':').expect("user:password");
        let (host, port) = address.split_once(':').unwrap_or((address, "1433"));
        SqlServerConnection {
            host: host.into(),
            port: Some(port.parse().unwrap()),
            instance: None,
            database: "master".into(),
            auth: Default::default(),
            user: user.into(),
            password: password.into(),
            max_connections: 1,
            ca: None,
            encrypt: Encrypt::Mandatory,
            name_in_certificate: None,
            trust_server_certificate: true,
            application: None,
            vendor: None,
            owner: None,
            custom: Default::default(),
        }
    }

    fn test_pool(max_connections: u32) -> Option<Pool> {
        test_connection().map(|c| Pool::new(&SqlServerConnection { max_connections, ..c }, None))
    }

    #[tokio::test]
    async fn queries_return_json_objects() {
        let Some(pool) = test_pool(2) else { return };
        let rows = pool
            .query(
                "SELECT CAST($1 AS int) AS n, $2 AS s, CAST(1 AS bit) AS yes, CAST(NULL AS int) AS nothing,
                        CAST(12.50 AS decimal(9,2)) AS amount, CAST(42 AS decimal(9,0)) AS whole,
                        CAST('2026-10-06T12:34:56.789' AS datetime2(3)) AS at, CAST('2026-10-06' AS date) AS day,
                        CAST('2026-10-06T12:00:00+02:00' AS datetimeoffset(0)) AS zoned,
                        CAST('6F9619FF-8B86-D011-B42D-00C04FC964FF' AS uniqueidentifier) AS id,
                        CAST(0x0102FF AS varbinary(3)) AS raw, COUNT(*) OVER () , N'Zoë' AS name",
                vec![Value::from(7), Value::from("it's")],
            )
            .await
            .unwrap();
        assert_eq!(
            rows,
            [serde_json::json!({
                "n": 7, "s": "it's", "yes": true, "nothing": null, "amount": 12.5, "whole": 42,
                "at": "2026-10-06T12:34:56.789", "day": "2026-10-06", "zoned": "2026-10-06T12:00:00+02:00",
                "id": "6F9619FF-8B86-D011-B42D-00C04FC964FF", "raw": "\\x0102ff", "column12": 1, "name": "Zoë"
            })]
        );
        assert_eq!(pool.query("SELECT 1 AS x WHERE 1 = 0", vec![]).await.unwrap(), Vec::<Value>::new());
    }

    #[tokio::test]
    async fn executes_and_binds_every_json_type() {
        let Some(pool) = test_pool(1) else { return };
        // One connection, so the temp table (connection-scoped) is seen by every call.
        let table = format!("##ductus_test_{}", std::process::id());
        pool.execute(
            &format!("CREATE TABLE {table} (i bigint, f float, b bit, s nvarchar(50), j nvarchar(max), z int)"),
            vec![],
        )
        .await
        .unwrap();
        let inserted = pool
            .execute(
                &format!("INSERT INTO {table} VALUES ($1, $2, $3, $4, $5, $6), ($1, $2, $3, $4, $5, $6)"),
                vec![
                    Value::from(9_007_199_254_740_993_i64),
                    Value::from(2.5),
                    Value::from(false),
                    Value::from("x"),
                    serde_json::json!({"a": [1]}),
                    Value::Null,
                ],
            )
            .await
            .unwrap();
        assert_eq!(inserted, 2);
        let rows = pool.query(&format!("SELECT TOP 1 * FROM {table}"), vec![]).await.unwrap();
        assert_eq!(
            rows,
            [
                serde_json::json!({"i": 9_007_199_254_740_993_i64, "f": 2.5, "b": false, "s": "x", "j": "{\"a\":[1]}", "z": null})
            ]
        );
        assert_eq!(pool.execute(&format!("DELETE FROM {table}"), vec![]).await.unwrap(), 2);
        pool.execute(&format!("DROP TABLE {table}"), vec![]).await.unwrap();
    }

    #[tokio::test]
    async fn sql_errors_are_permanent_and_outages_are_temporary() {
        let Some(c) = test_connection() else { return };
        let pool = Pool::new(&c, None);
        let err = pool.query("SELECT * FROM no_such_table", vec![]).await.unwrap_err().to_string();
        assert!(err.starts_with("database error: Invalid object name") && !err.contains(TRANSIENT), "{err}");
        // The failed call's connection was closed; the next call gets a fresh one.
        assert_eq!(pool.query("SELECT 1 AS ok", vec![]).await.unwrap(), [serde_json::json!({"ok": 1})]);

        let wrong = SqlServerConnection { password: "wrong".into(), ..c.clone() };
        let err = Pool::new(&wrong, None).query("SELECT 1", vec![]).await.unwrap_err().to_string();
        assert!(err.contains(TRANSIENT) && err.contains("Login failed"), "{err}");

        let down = SqlServerConnection { port: Some(1), ..c };
        let err = Pool::new(&down, None).query("SELECT 1", vec![]).await.unwrap_err().to_string();
        assert!(err.contains(TRANSIENT), "{err}");
    }

    /// A connection's TLS settings for wording refusals: verified as `name`, no `ca:`, not strict.
    pub(crate) fn settings(name: &str) -> TlsSettings {
        TlsSettings { name: name.into(), named_ca: false, strict: false }
    }

    /// The test server's TLS folder (`scripts/dev/sqlserver.sh`: its CA, another CA). check.sh and CI
    /// set DUCTUS_TEST_SQLSERVER_TLS with DUCTUS_TEST_SQLSERVER, so these tests can't be skipped
    /// silently where the server runs.
    pub(crate) fn test_tls(file: &str) -> Vec<u8> {
        let dir = std::env::var("DUCTUS_TEST_SQLSERVER_TLS")
            .expect("DUCTUS_TEST_SQLSERVER_TLS (scripts/dev/sqlserver.sh dir) goes with DUCTUS_TEST_SQLSERVER");
        std::fs::read(std::path::Path::new(&dir).join(file)).unwrap_or_else(|e| panic!("{dir}/{file}: {e}"))
    }

    /// `c` verified against `ca` (a file in the TLS folder), as `name`.
    fn verified(c: &SqlServerConnection, host: &str, name: Option<&str>, encrypt: Encrypt) -> SqlServerConnection {
        SqlServerConnection {
            host: host.into(),
            trust_server_certificate: false,
            name_in_certificate: name.map(str::to_string),
            encrypt,
            ..c.clone()
        }
    }

    async fn login(c: &SqlServerConnection, ca: Option<&str>) -> Result<Vec<Value>, String> {
        let ca = ca.map(test_tls);
        Pool::new(c, ca.as_deref()).query("SELECT 1 AS ok", vec![]).await.map_err(|e| e.to_string())
    }

    #[tokio::test]
    async fn logs_in_verified_with_its_ca_and_says_why_otherwise() {
        let Some(c) = test_connection() else { return };
        let ok = Ok(vec![serde_json::json!({"ok": 1})]);
        // Verified against the test CA, by the name connected to, or by `name_in_certificate`.
        assert_eq!(login(&verified(&c, "localhost", None, Encrypt::Mandatory), Some("ca.pem")).await, ok);
        assert_eq!(login(&verified(&c, "127.0.0.1", None, Encrypt::Mandatory), Some("ca.pem")).await, ok);
        #[cfg(target_os = "linux")]
        assert_eq!(
            login(&verified(&c, "127.0.0.2", Some("sql1.ductus.test"), Encrypt::Mandatory), Some("ca.pem")).await,
            ok,
            "an address the certificate doesn't list, checked for the name it does"
        );

        // Without its CA: not trusted, in words, and temporary (the messages wait).
        let err = login(&verified(&c, "localhost", None, Encrypt::Mandatory), None).await.unwrap_err();
        assert!(
            err.contains(TRANSIENT) && err.contains("TLS: not trusted: its issuer isn't one this server trusts"),
            "{err}"
        );
        assert!(err.contains("name the CA that issued it in `ca`") && !err.contains("Config::"), "{err}");
        let err = login(&verified(&c, "localhost", None, Encrypt::Mandatory), Some("other-ca.pem")).await.unwrap_err();
        assert!(err.contains("the CA named in `ca` didn't issue it either"), "{err}");

        // A name the certificate doesn't list.
        #[cfg(target_os = "linux")]
        {
            let err = login(&verified(&c, "127.0.0.2", None, Encrypt::Mandatory), Some("ca.pem")).await.unwrap_err();
            assert!(err.contains("TLS: the certificate isn't for 127.0.0.2: connect by a name it lists"), "{err}");
        }
        let err = login(&verified(&c, "localhost", Some("billing.hospital.org"), Encrypt::Mandatory), Some("ca.pem"))
            .await
            .unwrap_err();
        assert!(err.contains("the certificate isn't for billing.hospital.org"), "{err}");
    }

    #[test]
    fn windows_logins_are_said_with_who_and_how() {
        let c = |host: &str, user: &str| SqlServerConnection {
            host: host.into(),
            auth: ductus_core::resources::SqlAuth::Windows,
            user: user.into(),
            password: if user.is_empty() { String::new() } else { "p".into() },
            ..test_connection_to("u:p@x:1433")
        };
        let named = LoginSettings::new(&c("sql1.hospital.org", "HOSPITAL\\svc-lab"));
        assert_eq!(named.how("KERBEROS"), "Windows, Kerberos");
        assert_eq!(named.how("SQL"), "SQL login");
        if cfg!(windows) {
            assert_eq!(
                named.how("NTLM"),
                "Windows, NTLM: no Kerberos, most likely no SPN for MSSQLSvc/sql1.hospital.org:1433 (ask the DBA to \
                 register it; NTLM is being retired)"
            );
        } else {
            assert_eq!(named.how("NTLM"), "Windows, NTLM: from Linux it always is");
        }
        let instance = SqlServerConnection { instance: Some("PROD".into()), port: None, ..c("sql1", "H\\svc") };
        assert!(LoginSettings::new(&instance).spn.ends_with("MSSQLSvc/sql1:PROD"));

        let refused = windows_refusal(&named, 18452);
        assert!(
            refused.starts_with("SQL Server refused the Windows login `HOSPITAL\\svc-lab`: a wrong password, an account the server's Windows doesn't know, or NTLM"),
            "{refused}"
        );
        let refused = windows_refusal(&named, 18456);
        assert!(refused.contains("Windows accepted the account, but SQL Server has no login for it"), "{refused}");
        #[cfg(windows)]
        {
            let integrated = LoginSettings::new(&c("localhost", ""));
            assert_eq!(integrated.how("NTLM"), "Windows, NTLM: usual for a SQL Server on this machine");
            assert!(windows_refusal(&integrated, 18456).contains("the computer account: DOMAIN\\COMPUTER$"));
        }
    }

    /// A SQL Server with Windows logins (`scripts/dev/sqlexpress.ps1`) when DUCTUS_TEST_SQLSERVER_WINDOWS
    /// is set: `required` (this machine's, port 1433; CI) or `DOMAIN@host:port` (another machine's:
    /// the NTLM check from Linux). The script's password is DUCTUS_TEST_SQLSERVER_WINDOWS_PASSWORD.
    /// Returns its integrated connection (no user) and the accounts' domain.
    pub(crate) fn windows_test() -> Option<(SqlServerConnection, String)> {
        let spec = std::env::var("DUCTUS_TEST_SQLSERVER_WINDOWS").ok()?;
        let (domain, address) = match spec.split_once('@') {
            Some((domain, address)) => (domain.to_string(), address.to_string()),
            None => (std::env::var("COMPUTERNAME").expect("COMPUTERNAME (Windows)"), "localhost:1433".into()),
        };
        let c = SqlServerConnection {
            auth: ductus_core::resources::SqlAuth::Windows,
            user: String::new(),
            password: String::new(),
            ..test_connection_to(&format!("u:p@{address}"))
        };
        Some((c, domain))
    }

    /// The test accounts' password (`sqlexpress.ps1 -Password`).
    pub(crate) fn windows_password() -> String {
        std::env::var("DUCTUS_TEST_SQLSERVER_WINDOWS_PASSWORD")
            .expect("DUCTUS_TEST_SQLSERVER_WINDOWS_PASSWORD goes with DUCTUS_TEST_SQLSERVER_WINDOWS")
    }

    /// `c` as the local account `ductus-sql-<account>` of `domain`.
    pub(crate) fn as_account(
        c: &SqlServerConnection,
        domain: &str,
        account: &str,
        password: &str,
    ) -> SqlServerConnection {
        SqlServerConnection { user: format!("{domain}\\ductus-sql-{account}"), password: password.into(), ..c.clone() }
    }

    async fn who_and_how(c: &SqlServerConnection) -> Result<(String, String), String> {
        let rows = Pool::new(c, None).query(WHO_AND_HOW, vec![]).await.map_err(|e| e.to_string())?;
        let field = |name: &str| rows[0][name].as_str().unwrap_or_default().to_string();
        Ok((field("who"), field("scheme")))
    }

    #[tokio::test]
    async fn logs_in_as_a_named_windows_account() {
        let Some((c, domain)) = windows_test() else { return };
        let named = as_account(&c, &domain, "named", &windows_password());
        let (who, scheme) = who_and_how(&named).await.unwrap();
        assert!(who.eq_ignore_ascii_case(&named.user), "{who}");
        // No domain on the test machines: NTLM, said as such.
        assert_eq!(scheme, "NTLM");
        assert!(LoginSettings::new(&named).how(&scheme).starts_with("Windows, NTLM"));

        // The start log says it once per resource, at its first connection (a SQL login: never).
        let name = format!("named-{}", std::process::id());
        let pool = Pool::new(&named, None).said_as(&name);
        pool.query("SELECT 1 AS ok", vec![]).await.unwrap();
        let said = Pool::said(&name).unwrap();
        assert!(
            said.to_lowercase()
                .starts_with(&format!("sql server logged us in as {} (windows, ntlm", named.user.to_lowercase())),
            "{said}"
        );
        let sql = format!("sql-{}", std::process::id());
        let sql_login = SqlServerConnection { auth: ductus_core::resources::SqlAuth::Sql, ..named };
        let _ = Pool::new(&sql_login, None).said_as(&sql).query("SELECT 1", vec![]).await;
        assert_eq!(Pool::said(&sql), None);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn logs_in_as_the_account_it_runs_as() {
        let Some((c, _)) = windows_test() else { return };
        let (who, scheme) = who_and_how(&c).await.unwrap();
        let me = format!("{}\\{}", std::env::var("USERDOMAIN").unwrap(), std::env::var("USERNAME").unwrap());
        assert!(who.eq_ignore_ascii_case(&me), "{who} vs {me}");
        assert_eq!(scheme, "NTLM");
    }

    /// A wrong password is 18452 (Windows refused it), an account with no login 18456 (Windows
    /// accepted it, SQL Server has no login): each worded, both temporary (the messages wait for the
    /// fix, as for a refused SQL login).
    #[tokio::test]
    async fn a_refused_windows_login_is_worded_and_temporary() {
        let Some((c, domain)) = windows_test() else { return };
        let password = windows_password();
        for (account, password, words) in [
            ("named", "wrong-password", "a wrong password, an account the server's Windows doesn't know"),
            ("nologin", password.as_str(), "Windows accepted the account, but SQL Server has no login for it"),
        ] {
            let refused = as_account(&c, &domain, account, password);
            let err = who_and_how(&refused).await.unwrap_err();
            assert!(err.contains(TRANSIENT), "{account}: {err}");
            assert!(
                err.contains(&format!(
                    "SQL Server refused the Windows login `{domain}\\ductus-sql-{account}`: {words}"
                )),
                "{account}: {err}"
            );
        }
    }

    /// How long a Windows login takes against a SQL login on the same server (run by hand or by
    /// CI with `--ignored --nocapture`): each the median of 10 new connections.
    #[tokio::test]
    #[ignore]
    async fn time_a_windows_login_against_a_sql_login() {
        let Some((c, domain)) = windows_test() else { return };
        let password = windows_password();
        let sql = SqlServerConnection {
            auth: ductus_core::resources::SqlAuth::Sql,
            user: "ductus_sql".into(),
            password: password.clone(),
            ..c.clone()
        };
        let mut logins =
            vec![("SQL login", sql), ("named Windows account", as_account(&c, &domain, "named", &password))];
        if cfg!(windows) {
            logins.push(("integrated", c.clone()));
        }
        for (what, login) in logins {
            let mut took = Vec::new();
            for _ in 0..10 {
                let started = Instant::now();
                who_and_how(&login).await.unwrap();
                took.push(started.elapsed());
            }
            took.sort();
            println!("{what}: median {:?} (min {:?}, max {:?})", took[5], took[0], took[9]);
        }
    }

    #[tokio::test]
    async fn logs_in_over_tds_8_strict() {
        let Some(c) = test_connection() else { return };
        let strict = verified(&c, "localhost", None, Encrypt::Strict);
        assert_eq!(login(&strict, Some("ca.pem")).await, Ok(vec![serde_json::json!({"ok": 1})]));
        // Strict verifies too.
        let err = login(&strict, None).await.unwrap_err();
        assert!(err.contains("TLS: not trusted"), "{err}");
    }
}
