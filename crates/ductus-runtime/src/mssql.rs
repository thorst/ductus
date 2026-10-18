//! SQL Server for `db("name")`: a small pool of tiberius connections; rows become JSON objects.
//!
//! Interface code writes SQL the same way as for Postgres: `$1, $2...` placeholders (translated
//! to SQL Server's `@P1, @P2...`) with values bound separately, never pasted into the SQL.

use anyhow::anyhow;
use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime};
use ductus_core::resources::SqlServerConnection;
use ductus_script::TRANSIENT;
use serde_json::{Map, Value};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tiberius::error::Error;
use tiberius::{AuthMethod, Client, ColumnData, Config, FromSql, Query, SqlBrowser};
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
    976, 978, 983, // availability group replica not readable / changing role
    40197, 40501, 40613, 49918, 49919, 49920, 4221, 10928, 10929, // Azure SQL: busy or reconfiguring
    233, 10053, 10054, 10060, 64, // connection broken
];

pub struct Pool {
    config: Config,
    /// Look the port up through the SQL Server Browser (named instance, no fixed port).
    browse: bool,
    trusts_any_certificate: bool,
    idle: Mutex<Vec<(Connection, Instant)>>,
    permits: Semaphore,
}

impl Pool {
    /// Connects lazily, like the Postgres pools: nothing is opened until the first query.
    pub fn new(c: &SqlServerConnection) -> Self {
        let mut config = Config::new();
        config.host(&c.host);
        if let Some(port) = c.port {
            config.port(port);
        }
        if let Some(instance) = &c.instance {
            config.instance_name(instance);
        }
        config.database(&c.database);
        config.authentication(AuthMethod::sql_server(&c.user, &c.password));
        config.application_name("ductus");
        if c.trust_server_certificate {
            config.trust_cert();
        }
        Self {
            config,
            browse: c.instance.is_some() && c.port.is_none(),
            trusts_any_certificate: c.trust_server_certificate,
            idle: Mutex::new(Vec::new()),
            permits: Semaphore::new(c.max_connections.max(1) as usize),
        }
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
                None => self.connect().await.map_err(|e| self.classify(e))?,
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
                TcpStream::connect(config.get_addr()).await?
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

    /// Temporary failures are marked so the worker retries the message instead of
    /// quarantining it (see `TRANSIENT_CODES`).
    fn classify(&self, e: Error) -> anyhow::Error {
        match &e {
            Error::Tls(_) if !self.trusts_any_certificate => anyhow!(
                "{TRANSIENT} database unavailable: {e} (if the server uses a self-signed certificate, set \
                 `trust_server_certificate: true` for it in resources.yaml)"
            ),
            Error::Io { .. } | Error::Tls(_) | Error::Routing { .. } => {
                anyhow!("{TRANSIENT} database unavailable: {e}")
            }
            Error::Server(t) if TRANSIENT_CODES.contains(&t.code()) => {
                anyhow!("{TRANSIENT} database unavailable: {}", t.message())
            }
            Error::Server(t) => anyhow!("database error: {} (SQL Server error {})", t.message(), t.code()),
            _ => anyhow!("database error: {e}"),
        }
    }
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
        let spec = std::env::var("DUCTUS_TEST_SQLSERVER").ok()?;
        let (credentials, address) = spec.rsplit_once('@').expect("DUCTUS_TEST_SQLSERVER = user:password@host:port");
        let (user, password) = credentials.split_once(':').expect("user:password");
        let (host, port) = address.split_once(':').unwrap_or((address, "1433"));
        Some(SqlServerConnection {
            host: host.into(),
            port: Some(port.parse().unwrap()),
            instance: None,
            database: "master".into(),
            user: user.into(),
            password: password.into(),
            max_connections: 1,
            trust_server_certificate: true,
            application: None,
            vendor: None,
            owner: None,
            custom: Default::default(),
        })
    }

    fn test_pool(max_connections: u32) -> Option<Pool> {
        test_connection().map(|c| Pool::new(&SqlServerConnection { max_connections, ..c }))
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
        let pool = Pool::new(&c);
        let err = pool.query("SELECT * FROM no_such_table", vec![]).await.unwrap_err().to_string();
        assert!(err.starts_with("database error: Invalid object name") && !err.contains(TRANSIENT), "{err}");
        // The failed call's connection was closed; the next call gets a fresh one.
        assert_eq!(pool.query("SELECT 1 AS ok", vec![]).await.unwrap(), [serde_json::json!({"ok": 1})]);

        let wrong = SqlServerConnection { password: "wrong".into(), ..c.clone() };
        let err = Pool::new(&wrong).query("SELECT 1", vec![]).await.unwrap_err().to_string();
        assert!(err.contains(TRANSIENT) && err.contains("Login failed"), "{err}");

        let down = SqlServerConnection { port: Some(1), ..c };
        let err = Pool::new(&down).query("SELECT 1", vec![]).await.unwrap_err().to_string();
        assert!(err.contains(TRANSIENT), "{err}");
    }
}
