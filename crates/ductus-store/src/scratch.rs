//! The schema a bench runs in: made in the customer's database, the engine's tables migrated into
//! it, dropped at the end. Nothing outside it is read or written (the engine sees only it, through
//! `search_path`), so a bench never touches the database's own engine data.
//!
//! Postgres keeps no creation time for a schema, so the name carries it
//! (`ductus_bench_<unix seconds>_<6 hex>`): a bench killed before its drop (`kill -9`, a lost
//! connection) leaves one behind, and the next bench drops those older than a day.

use anyhow::{Context, Result, bail};
use sqlx::{Connection, PgConnection};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Left-behind schemas older than this are dropped by the next bench (a day: a bench running
/// elsewhere on the same database right now is never one of them).
pub const SWEEP_AFTER: Duration = Duration::from_secs(24 * 3600);

const PREFIX: &str = "ductus_bench_";

/// The scratch schema, made and not yet dropped.
pub struct Scratch {
    pub name: String,
    url: String,
}

/// What [`prepare`] found on the database before the bench makes its schema.
pub struct Found {
    /// Schemas of earlier benches, older than [`SWEEP_AFTER`], now dropped (by name).
    pub swept: Vec<String>,
    /// Ones that couldn't be dropped (another account's), with why.
    pub not_swept: Vec<(String, String)>,
    /// Engines running on this database (their lock connections), by `pg_stat_activity`.
    pub live_engines: i64,
}

/// A new schema's name for a bench started at `now`.
pub fn name_at(now: SystemTime) -> String {
    let secs = now.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let mut bytes = [0u8; 3];
    getrandom::fill(&mut bytes).expect("OS random number generator");
    format!("{PREFIX}{secs}_{:02x}{:02x}{:02x}", bytes[0], bytes[1], bytes[2])
}

/// When a bench's schema was made, from its name; None for any other name (a schema someone
/// named `ductus_bench_x` themselves is never swept).
pub fn made_at(name: &str) -> Option<SystemTime> {
    let rest = name.strip_prefix(PREFIX)?;
    let (secs, random) = rest.split_once('_')?;
    let hex = random.len() == 6 && random.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if !hex || secs.is_empty() || !secs.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(UNIX_EPOCH + Duration::from_secs(secs.parse().ok()?))
}

/// `url` searching only `schema`: the database looks new to whatever connects with it. Refused
/// when the URL sets `options` itself (the bench would have to merge into it; say so instead).
pub fn searching(url: &str, schema: &str) -> Result<String> {
    if url.split(['?', '&']).skip(1).any(|p| p.starts_with("options=")) {
        bail!(
            "the database URL sets `options`, and the bench sets its own (`search_path`, to run in a schema of its own): give the URL without `options=`"
        );
    }
    let join = if url.contains('?') { '&' } else { '?' };
    Ok(format!("{url}{join}options=-c%20search_path%3D{schema}"))
}

/// Check the database can take a bench (a URL the bench can add its `search_path` to, a login
/// that may create schemas, no engine running there unless `live_engines_ok`), then drop
/// earlier benches' schemas left behind. A refusal changes nothing.
pub async fn prepare(url: &str, live_engines_ok: bool) -> Result<Found> {
    searching(url, "x")?;
    let mut conn = connect(url).await?;
    let may: bool = sqlx::query_scalar("SELECT has_database_privilege(current_database(), 'CREATE')")
        .fetch_one(&mut conn)
        .await
        .context("checking whether this login may create schemas")?;
    if !may {
        let (user, database): (String, String) = sqlx::query_as("SELECT current_user::text, current_database()::text")
            .fetch_one(&mut conn)
            .await
            .context("reading the login's name")?;
        bail!(
            "{user} may not create schemas in {database}, and the bench runs in a schema of its own (made and dropped by it, so the engine's own data is never touched). A DBA can allow it: GRANT CREATE ON DATABASE {database} TO {user}; or run the bench with a login that may"
        );
    }
    let live_engines: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_stat_activity WHERE datname = current_database() AND application_name = 'ductus engine lock'",
    )
    .fetch_one(&mut conn)
    .await
    .context("looking for engines running on this database")?;
    if live_engines > 0 && !live_engines_ok {
        let engines = match live_engines {
            1 => "an engine is".to_string(),
            n => format!("{n} engines are"),
        };
        bail!(
            "{engines} running on this database: its traffic and the bench's share the database's disk, so both are slower while the bench runs and its numbers come out lower than the machine can do. Bench when it's stopped, or give --yes to bench anyway (its data is never touched: the bench runs in a schema of its own)"
        );
    }
    let (swept, not_swept) = sweep(&mut conn, SystemTime::now()).await?;
    let _ = conn.close().await;
    Ok(Found { swept, not_swept, live_engines })
}

/// The benches' schemas on the database now (made by benches running, or left behind), by name.
pub async fn left(url: &str) -> Result<Vec<String>> {
    let mut conn = connect(url).await?;
    let names = sqlx::query_scalar("SELECT nspname::text FROM pg_namespace WHERE starts_with(nspname, $1) ORDER BY 1")
        .bind(PREFIX)
        .fetch_all(&mut conn)
        .await
        .context("listing the benches' schemas")?;
    let _ = conn.close().await;
    Ok(names)
}

impl Scratch {
    /// Make a new schema (after [`prepare`]).
    pub async fn create(url: &str) -> Result<Self> {
        let mut conn = connect(url).await?;
        let name = name_at(SystemTime::now());
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {name}")))
            .execute(&mut conn)
            .await
            .with_context(|| format!("creating the bench's schema {name}"))?;
        let _ = conn.close().await;
        Ok(Self { name, url: url.to_string() })
    }

    /// The URL the bench's engine and queries use: this schema alone.
    pub fn url(&self) -> Result<String> {
        searching(&self.url, &self.name)
    }

    /// Drop the schema with everything in it. Waits for the bench's engine's last statements
    /// (its locks) rather than failing: the engine has been stopped first.
    pub async fn drop(self) -> Result<()> {
        let mut conn = connect(&self.url).await?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP SCHEMA IF EXISTS {} CASCADE", self.name)))
            .execute(&mut conn)
            .await
            .with_context(|| {
                format!("dropping the bench's schema {} (drop it by hand: DROP SCHEMA {0} CASCADE)", self.name)
            })?;
        let _ = conn.close().await;
        Ok(())
    }
}

/// Counts and timings in the bench's schema, on a connection of its own, read while its engine
/// runs.
pub struct Counter {
    conn: PgConnection,
}

impl Counter {
    /// Connect with `url` (the scratch schema's: [`Scratch::url`]).
    pub async fn connect(url: &str) -> Result<Self> {
        Ok(Self { conn: connect(url).await? })
    }

    /// One interface's messages: stored, and still pending (not yet at every destination).
    pub async fn messages(&mut self, interface: &str) -> Result<(i64, i64)> {
        sqlx::query_as("SELECT count(*), count(*) FILTER (WHERE status = 0) FROM messages WHERE interface = $1")
            .bind(interface)
            .fetch_one(&mut self.conn)
            .await
            .context("counting the bench's messages")
    }

    /// The median of `n` round trips (`SELECT 1`): how far away the database is.
    pub async fn round_trip(&mut self, n: usize) -> Result<Duration> {
        let mut times = Vec::with_capacity(n);
        for _ in 0..n.max(1) {
            let started = std::time::Instant::now();
            sqlx::query("SELECT 1").execute(&mut self.conn).await.context("timing a round trip to the database")?;
            times.push(started.elapsed());
        }
        times.sort();
        Ok(times[times.len() / 2])
    }

    /// The server's version (`18.1`) and `synchronous_commit`.
    pub async fn server(&mut self) -> Result<(String, String)> {
        let version: String =
            sqlx::query_scalar("SHOW server_version").fetch_one(&mut self.conn).await.context("reading the version")?;
        let synchronous: String = sqlx::query_scalar("SHOW synchronous_commit")
            .fetch_one(&mut self.conn)
            .await
            .context("reading synchronous_commit")?;
        // "18.1 (Ubuntu 18.1-1.pgdg24.04+1)": the number is what's compared.
        Ok((version.split_whitespace().next().unwrap_or_default().to_string(), synchronous))
    }

    /// `n` durable commits, each timed, sorted ([`time_commits`]).
    pub async fn commits(&mut self, n: usize) -> Result<Vec<Duration>> {
        time_commits(&mut self.conn, n, "ductus bench").await.context("timing durable commits")
    }

    pub async fn close(self) {
        let _ = self.conn.close().await;
    }
}

/// `n` durable commits on `connection`, each timed, sorted (`ductus doctor` and `ductus bench`).
/// Each commit writes a transactional WAL message (no table is touched), so it waits for the
/// flush like a message insert does. A transaction that only takes an id is not enough: without
/// WAL of its own Postgres doesn't wait for the flush (measured 0.28 ms vs 3.06 ms on the same
/// disk). Logical decoding consumers, if any, see the message under the prefix `prefix` and
/// ignore it.
pub async fn time_commits(connection: &mut PgConnection, n: usize, prefix: &str) -> Result<Vec<Duration>, sqlx::Error> {
    let mut times = Vec::with_capacity(n);
    for _ in 0..n.max(1) {
        let started = std::time::Instant::now();
        sqlx::query("SELECT pg_logical_emit_message(true, $1, '')").bind(prefix).execute(&mut *connection).await?;
        times.push(started.elapsed());
    }
    times.sort();
    Ok(times)
}

async fn connect(url: &str) -> Result<PgConnection> {
    let url = crate::tls::connect_url(url);
    let options: sqlx::postgres::PgConnectOptions = url.parse().context("reading the database URL")?;
    PgConnection::connect_with(&options.application_name("ductus bench")).await.context("connecting to the database")
}

/// Drop the schemas earlier benches left that are older than [`SWEEP_AFTER`] at `now`.
async fn sweep(conn: &mut PgConnection, now: SystemTime) -> Result<(Vec<String>, Vec<(String, String)>)> {
    let names: Vec<String> =
        sqlx::query_scalar("SELECT nspname::text FROM pg_namespace WHERE starts_with(nspname, $1)")
            .bind(PREFIX)
            .fetch_all(&mut *conn)
            .await
            .context("looking for schemas earlier benches left")?;
    let (mut swept, mut not_swept) = (Vec::new(), Vec::new());
    for name in names {
        let Some(made) = made_at(&name) else { continue };
        if now.duration_since(made).is_ok_and(|age| age > SWEEP_AFTER) {
            let drop = format!("DROP SCHEMA IF EXISTS {name} CASCADE");
            match sqlx::raw_sql(sqlx::AssertSqlSafe(drop)).execute(&mut *conn).await {
                Ok(_) => swept.push(name),
                Err(e) => not_swept.push((name, e.to_string())),
            }
        }
    }
    Ok((swept, not_swept))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_schemas_name_says_when_it_was_made_and_nothing_else_is_taken_for_one() {
        let at = UNIX_EPOCH + Duration::from_secs(1_790_000_000);
        let name = name_at(at);
        assert!(name.starts_with("ductus_bench_1790000000_") && name.len() == PREFIX.len() + 10 + 1 + 6, "{name}");
        assert_eq!(made_at(&name), Some(at));
        for other in [
            "ductus_bench_",
            "ductus_bench_x",
            "ductus_bench_1790000000",
            "ductus_bench_1790000000_abc",
            "ductus_bench_1790000000_ABCDEF",
            "ductus_bench__abcdef",
            "ductus_bench_17x0000000_abcdef",
            "public",
        ] {
            assert_eq!(made_at(other), None, "{other}");
        }
    }

    /// The sweep drops what benches left over a day ago and says which; a newer one (a bench
    /// running elsewhere now) and a schema of that prefix that isn't a bench's stay.
    #[tokio::test]
    async fn prepare_sweeps_what_benches_left_over_a_day_ago_and_nothing_else() {
        let Ok(url) = std::env::var("DUCTUS_TEST_DATABASE_URL") else { return };
        let old = name_at(SystemTime::now() - Duration::from_secs(2 * 86_400));
        let recent = name_at(SystemTime::now());
        let theirs = format!("{PREFIX}kept_{}", &recent[recent.len() - 6..]);
        let mut conn = connect(&url).await.unwrap();
        for name in [&old, &recent, &theirs] {
            sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {name}"))).execute(&mut conn).await.unwrap();
        }
        let found = prepare(&url, true).await.unwrap();
        let after = left(&url).await.unwrap();
        for name in [&recent, &theirs] {
            sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP SCHEMA {name}"))).execute(&mut conn).await.unwrap();
        }
        assert!(found.swept.contains(&old), "{:?}", found.swept);
        assert!(!after.contains(&old) && after.contains(&recent) && after.contains(&theirs), "{after:?}");
    }

    #[test]
    fn the_url_searches_the_schema_alone_and_one_with_options_is_refused() {
        assert_eq!(searching("postgres://u@h/db", "s").unwrap(), "postgres://u@h/db?options=-c%20search_path%3Ds");
        assert_eq!(
            searching("postgres://u@h/db?sslmode=require", "s").unwrap(),
            "postgres://u@h/db?sslmode=require&options=-c%20search_path%3Ds"
        );
        let e = searching("postgres://u@h/db?sslmode=require&options=-c%20x%3D1", "s").unwrap_err().to_string();
        assert!(e.contains("without `options=`"), "{e}");
    }
}
