//! Engine services for interface code: named database pools, environment variables, secrets.

use anyhow::{Result, anyhow};
use ductus_core::resources::{Database, DbConnection};
use ductus_core::{Environment, Resources};
use ductus_script::{BoxFuture, Services, TRANSIENT};
use serde_json::Value;
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use std::collections::HashMap;
use std::sync::Arc;

enum Pool {
    Postgres(PgPool),
    Unsupported(&'static str),
}

/// Shared by every worker thread: connection pools are created once per resource.
pub struct SharedServices {
    env: Environment,
    databases: HashMap<String, Pool>,
}

impl SharedServices {
    /// Pools connect lazily, so a database that's down at startup doesn't stop the engine;
    /// queries against it fail as temporary errors and the messages wait.
    pub fn new(env: Environment, resources: &Resources) -> Self {
        let databases = resources
            .databases
            .iter()
            .map(|(name, db)| {
                let pool = match db {
                    Database::Postgres(c) => Pool::Postgres(postgres_pool(c)),
                    Database::Sqlserver(_) => Pool::Unsupported("SQL Server support is coming later in v0.4"),
                };
                (name.clone(), pool)
            })
            .collect();
        Self { env, databases }
    }

    fn pool(&self, name: &str) -> Result<&PgPool> {
        match self.databases.get(name) {
            Some(Pool::Postgres(p)) => Ok(p),
            Some(Pool::Unsupported(why)) => Err(anyhow!("database `{name}`: {why}")),
            None => {
                let known: Vec<_> = self.databases.keys().map(String::as_str).collect();
                Err(anyhow!("no database named `{name}` in resources.yaml (defined: {})", known.join(", ")))
            }
        }
    }
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
/// worker retries the message instead of quarantining it.
fn classify(e: sqlx::Error) -> anyhow::Error {
    let transient = match &e {
        sqlx::Error::Io(_) | sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed | sqlx::Error::Tls(_) => true,
        sqlx::Error::Database(db) => db.code().is_some_and(|c| {
            c.starts_with("08") || c.starts_with("53") || c.starts_with("57P") || c == "40001" || c == "40P01"
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

/// Per-thread handle (an `Rc<dyn Services>` lives in each isolate).
pub struct EngineServices(pub Arc<SharedServices>);

impl Services for EngineServices {
    fn db_query(&self, resource: &str, sql: &str, params: Vec<Value>) -> BoxFuture<Vec<Value>> {
        let shared = self.0.clone();
        let resource = resource.to_string();
        // Postgres builds the JSON itself, so every column type converts the same way.
        let wrapped = format!(
            "WITH ductus_q AS ({}) SELECT coalesce(json_agg(ductus_q), '[]'::json) FROM ductus_q",
            sql.trim().trim_end_matches(';')
        );
        Box::pin(async move {
            let pool = shared.pool(&resource)?;
            let rows: Value = bind_json!(sqlx::query_scalar::<_, Value>(sqlx::AssertSqlSafe(wrapped)), params)
                .fetch_one(pool)
                .await
                .map_err(classify)?;
            match rows {
                Value::Array(rows) => Ok(rows),
                other => Err(anyhow!("unexpected query result: {other}")),
            }
        })
    }

    fn db_execute(&self, resource: &str, sql: &str, params: Vec<Value>) -> BoxFuture<u64> {
        let shared = self.0.clone();
        let resource = resource.to_string();
        let sql = sql.to_string();
        Box::pin(async move {
            let pool = shared.pool(&resource)?;
            let done =
                bind_json!(sqlx::query(sqlx::AssertSqlSafe(sql)), params).execute(pool).await.map_err(classify)?;
            Ok(done.rows_affected())
        })
    }

    fn variable(&self, name: &str) -> Option<String> {
        self.0.env.variable(name)
    }

    fn secret(&self, name: &str) -> Result<Option<String>> {
        self.0.env.secret(name)
    }
}
