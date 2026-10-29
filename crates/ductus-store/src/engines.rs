//! Engines on this database and the interfaces each runs (migration 0023).
//!
//! **One owner per interface.** Each engine holds a session advisory lock for its whole life on
//! a connection of its own (`EngineLock`): Postgres frees it however the process ends, so whether
//! an engine is alive is whether its lock is held (`pg_locks`), with no heartbeat and no timeout
//! to wait out. Before an engine changes anything for its interfaces it claims them
//! (`Store::claim`): an interface with no owner, or whose owner's lock is gone, becomes its;
//! one owned by a live engine refuses the whole claim. Two engines with different interfaces may
//! share a database.
//!
//! **Lanes.** `interface_applied` keeps the lane count each interface last ran with, so a start
//! can tell that `ordering` changed and look for messages waiting in lanes no worker would own
//! (`Store::stranded`).

use super::Store;
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use sqlx::{Connection, PgConnection};
use std::time::Duration;

/// Whether the engine owning `e.lock_key` (an `engines` row aliased `e`) still holds its lock.
/// A bigint advisory lock shows in `pg_locks` as its high half in `classid` and its low half in
/// `objid` (keys are positive and below 2^62, so the high half never reaches the sign bit).
const ALIVE: &str = "EXISTS (SELECT 1 FROM pg_locks l
     WHERE l.locktype = 'advisory' AND l.granted AND l.objsubid = 1
       AND l.database = (SELECT oid FROM pg_database WHERE datname = current_database())
       AND ((l.classid::bigint << 32) | l.objid::bigint) = e.lock_key)";

/// The engine starting, as `engines` records it.
#[derive(Debug, Clone)]
pub struct NewEngine<'a> {
    pub instance: &'a str,
    pub host: &'a str,
    pub pid: u32,
    pub version: &'a str,
    pub environment: &'a str,
}

/// Another engine, alive on this database.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct EngineRow {
    pub instance: String,
    pub host: String,
    pub pid: i32,
    pub version: String,
    pub environment: String,
    pub started_at: DateTime<Utc>,
    /// The interfaces it owns, by name.
    pub interfaces: Vec<String>,
}

/// An interface a claim found owned by a live engine.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Owned {
    pub interface: String,
    pub instance: String,
    pub host: String,
    pub pid: i32,
    pub version: String,
    pub started_at: DateTime<Utc>,
}

/// Messages waiting in lanes a destination's interface no longer has.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Stranded {
    pub interface: String,
    pub destination: String,
    pub waiting: i64,
    pub first_lane: i32,
    pub last_lane: i32,
}

/// The lock that says an engine is alive, on a connection of its own (not the pool's: a pooled
/// connection is handed back and forth, and the lock would go with it). Dropping it closes the
/// connection, which frees the lock.
pub struct EngineLock {
    conn: PgConnection,
    pub instance: String,
    pub key: i64,
}

impl EngineLock {
    /// Is the lock still held: the connection answers within `timeout`. An error means the
    /// connection is gone, and with it the lock (Postgres frees it with the session).
    pub async fn check(&mut self, timeout: Duration) -> Result<()> {
        match tokio::time::timeout(timeout, sqlx::query("SELECT 1").execute(&mut self.conn)).await {
            Ok(r) => r.map(|_| ()).context("the engine's lock connection failed"),
            Err(_) => bail!("the engine's lock connection didn't answer within {timeout:?}"),
        }
    }

    /// Close the connection, freeing the lock now.
    pub async fn release(self) {
        let _ = self.conn.close().await;
    }
}

/// A random lock key, positive and below 2^62.
fn new_key() -> i64 {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).expect("OS random number generator");
    (u64::from_le_bytes(bytes) >> 2) as i64
}

impl Store {
    /// Take this engine's lock on a connection of its own and record the engine. `key`: the lock
    /// key to take again after the connection was lost (`None`: a new engine, a new key).
    ///
    /// The connection asks the server to notice a vanished client within ~25 s (TCP keepalives;
    /// without them, a powered-off host's session and lock can last the OS default of about two
    /// hours), and turns off `idle_session_timeout` for itself (it is idle on purpose).
    pub async fn lock_engine(&self, engine: &NewEngine<'_>, key: Option<i64>) -> Result<EngineLock> {
        let options = self.pool.connect_options().as_ref().clone().application_name("ductus engine lock");
        let mut conn = PgConnection::connect_with(&options).await.context("connecting for the engine's lock")?;
        for setting in [
            "SET tcp_keepalives_idle = 10",
            "SET tcp_keepalives_interval = 5",
            "SET tcp_keepalives_count = 3",
            "SET idle_session_timeout = 0",
        ] {
            sqlx::query(setting)
                .execute(&mut conn)
                .await
                .with_context(|| format!("setting up the lock connection: {setting}"))?;
        }
        let mut tries = 0;
        let key = loop {
            let key = match key {
                Some(k) => k,
                None => new_key(),
            };
            let taken: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
                .bind(key)
                .fetch_one(&mut conn)
                .await
                .context("taking the engine's lock")?;
            if taken {
                break key;
            }
            tries += 1;
            // Our own key after a lost connection: the server hasn't noticed the old session is
            // gone yet (up to the keepalive window). A new key taken by someone else: pick again.
            if tries >= 30 {
                bail!("the engine's lock (key {key}) is still held by another session");
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        };
        sqlx::query(
            "INSERT INTO engines (instance, lock_key, host, pid, version, environment) VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (instance) DO UPDATE SET lock_key = excluded.lock_key, stopped_at = NULL",
        )
        .bind(engine.instance)
        .bind(key)
        .bind(engine.host)
        .bind(engine.pid as i32)
        .bind(engine.version)
        .bind(engine.environment)
        .execute(&mut conn)
        .await
        .context("recording the engine")?;
        Ok(EngineLock { conn, instance: engine.instance.to_string(), key })
    }

    /// Claim `interfaces` for engine `instance`, all or none: each must have no owner, be this
    /// engine's already, or be owned by an engine that no longer holds its lock. Returns the
    /// interfaces owned by live engines (empty: all claimed).
    ///
    /// Rows are written in name order, so two engines claiming overlapping sets at once wait for
    /// each other instead of deadlocking; the later one then sees the earlier one's rows.
    pub async fn claim(&self, instance: &str, interfaces: &[String]) -> Result<Vec<Owned>> {
        let mut tx = self.pool.begin().await.context("claiming interfaces")?;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DELETE FROM interface_owners o USING engines e
             WHERE o.instance = e.instance AND o.interface = ANY($1) AND o.instance <> $2 AND NOT {ALIVE}"
        )))
        .bind(interfaces)
        .bind(instance)
        .execute(&mut *tx)
        .await
        .context("taking interfaces over from engines that stopped")?;
        sqlx::query(
            "INSERT INTO interface_owners (interface, instance)
             SELECT n, $2 FROM unnest($1::text[]) n ORDER BY n
             ON CONFLICT (interface) DO NOTHING",
        )
        .bind(interfaces)
        .bind(instance)
        .execute(&mut *tx)
        .await
        .context("claiming interfaces")?;
        let owned: Vec<Owned> = sqlx::query_as(
            "SELECT o.interface, e.instance, e.host, e.pid, e.version, e.started_at
             FROM interface_owners o JOIN engines e ON e.instance = o.instance
             WHERE o.interface = ANY($1) AND o.instance <> $2
             ORDER BY e.started_at, e.instance, o.interface",
        )
        .bind(interfaces)
        .bind(instance)
        .fetch_all(&mut *tx)
        .await
        .context("reading who owns the interfaces")?;
        if owned.is_empty() {
            tx.commit().await.context("claiming interfaces")?;
        } else {
            tx.rollback().await.context("claiming interfaces")?;
        }
        Ok(owned)
    }

    /// Does engine `instance` still own every one of `interfaces` (after its lock connection came
    /// back)? Returns those owned by another engine now.
    pub async fn lost_claims(&self, instance: &str, interfaces: &[String]) -> Result<Vec<Owned>> {
        sqlx::query_as(
            "SELECT o.interface, e.instance, e.host, e.pid, e.version, e.started_at
             FROM interface_owners o JOIN engines e ON e.instance = o.instance
             WHERE o.interface = ANY($1) AND o.instance <> $2
             ORDER BY e.started_at, e.instance, o.interface",
        )
        .bind(interfaces)
        .bind(instance)
        .fetch_all(&self.pool)
        .await
        .context("reading who owns the interfaces")
    }

    /// Which of `interfaces` a live engine other than those in `except` owns (a takeover's check
    /// before it claims: the engine it takes over from owns them, and may).
    pub async fn live_owners(&self, interfaces: &[String], except: &[&str]) -> Result<Vec<Owned>> {
        let except: Vec<String> = except.iter().map(|s| s.to_string()).collect();
        sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT o.interface, e.instance, e.host, e.pid, e.version, e.started_at
             FROM interface_owners o JOIN engines e ON e.instance = o.instance
             WHERE o.interface = ANY($1) AND o.instance <> ALL($2) AND {ALIVE}
             ORDER BY e.started_at, e.instance, o.interface"
        )))
        .bind(interfaces)
        .bind(&except)
        .fetch_all(&self.pool)
        .await
        .context("reading who owns the interfaces")
    }

    /// A clean stop: let go of the interfaces and record when.
    pub async fn release_engine(&self, instance: &str) -> Result<()> {
        let mut tx = self.pool.begin().await.context("releasing interfaces")?;
        sqlx::query("DELETE FROM interface_owners WHERE instance = $1")
            .bind(instance)
            .execute(&mut *tx)
            .await
            .context("releasing interfaces")?;
        sqlx::query("UPDATE engines SET stopped_at = now() WHERE instance = $1")
            .bind(instance)
            .execute(&mut *tx)
            .await
            .context("recording the stop")?;
        tx.commit().await.context("releasing interfaces")
    }

    /// A refused start: the engine never ran, so it leaves no row (nor claims).
    pub async fn forget_engine(&self, instance: &str) -> Result<()> {
        sqlx::query("DELETE FROM engines WHERE instance = $1")
            .bind(instance)
            .execute(&self.pool)
            .await
            .context("removing the refused engine's record")?;
        Ok(())
    }

    /// The live engines on this database other than `instance`, oldest first, with what they own.
    pub async fn other_engines(&self, instance: &str) -> Result<Vec<EngineRow>> {
        sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT e.instance, e.host, e.pid, e.version, e.environment, e.started_at,
                    coalesce(array_agg(o.interface ORDER BY o.interface) FILTER (WHERE o.interface IS NOT NULL), '{{}}') AS interfaces
             FROM engines e LEFT JOIN interface_owners o ON o.instance = e.instance
             WHERE e.instance <> $1 AND e.stopped_at IS NULL AND {ALIVE}
             GROUP BY e.instance ORDER BY e.started_at"
        )))
        .bind(instance)
        .fetch_all(&self.pool)
        .await
        .context("reading the other engines")
    }

    /// Every live engine on this database, oldest first, with what it owns.
    pub async fn live_engines(&self) -> Result<Vec<EngineRow>> {
        self.other_engines("").await
    }

    /// Delete the records of engines that started more than `keep` ago, stopped or dead, and own
    /// nothing. Returns how many.
    pub async fn trim_engines(&self, keep: Duration) -> Result<u64> {
        let done = sqlx::query(sqlx::AssertSqlSafe(format!(
            "DELETE FROM engines e
             WHERE e.started_at < now() - make_interval(secs => $1)
               AND NOT EXISTS (SELECT 1 FROM interface_owners o WHERE o.instance = e.instance)
               AND NOT {ALIVE}"
        )))
        .bind(keep.as_secs_f64())
        .execute(&self.pool)
        .await
        .context("trimming the engines")?;
        Ok(done.rows_affected())
    }

    /// The lanes each of `interfaces` last ran with (absent: never recorded).
    pub async fn applied_lanes(&self, interfaces: &[String]) -> Result<Vec<(String, i32)>> {
        sqlx::query_as("SELECT interface, lanes FROM interface_applied WHERE interface = ANY($1)")
            .bind(interfaces)
            .fetch_all(&self.pool)
            .await
            .context("reading the lanes interfaces last ran with")
    }

    /// Record the lanes each interface runs with now.
    pub async fn save_lanes(&self, lanes: &[(String, i32)]) -> Result<()> {
        let (names, counts): (Vec<&str>, Vec<i32>) = lanes.iter().map(|(n, l)| (n.as_str(), *l)).unzip();
        sqlx::query(
            "INSERT INTO interface_applied (interface, lanes)
             SELECT * FROM unnest($1::text[], $2::int[]) ORDER BY 1
             ON CONFLICT (interface) DO UPDATE SET lanes = excluded.lanes, updated_at = now()
             WHERE interface_applied.lanes <> excluded.lanes",
        )
        .bind(&names)
        .bind(&counts)
        .execute(&self.pool)
        .await
        .context("recording the lanes interfaces run with")?;
        Ok(())
    }

    /// Messages waiting for each `(interface, destination)` in lanes at or past its `lanes`, the
    /// ones a worker would never read (only destinations with such messages are returned).
    ///
    /// The waiting deliveries of these destinations are read once from the queue index (a
    /// materialized CTE; only lanes past the smallest limit), then matched to their limits. A
    /// count per destination (`LATERAL`) let the plan scan a partition holding one destination's
    /// backlog once for every destination: 18 s for 1,001 destinations with 300,000 waiting,
    /// against 18–55 ms this way (150–190 ms with 150,000 of them stranded). Its cost is the
    /// backlog's, and it only runs when lanes were lowered or never recorded.
    pub async fn stranded(&self, checks: &[(String, String, i32)]) -> Result<Vec<Stranded>> {
        let mut interfaces = Vec::with_capacity(checks.len());
        let mut destinations = Vec::with_capacity(checks.len());
        let mut lanes = Vec::with_capacity(checks.len());
        for (i, d, l) in checks {
            interfaces.push(i.as_str());
            destinations.push(d.as_str());
            lanes.push(*l);
        }
        sqlx::query_as(
            "WITH c AS MATERIALIZED (
                 SELECT t.id, c.interface, c.destination, c.lanes
                 FROM unnest($1::text[], $2::text[], $3::int[]) AS c(interface, destination, lanes)
                 JOIN destinations t ON t.interface = c.interface AND t.name = c.destination),
             waiting AS MATERIALIZED (
                 SELECT d.destination_id, d.partition_key FROM deliveries d
                 WHERE d.status = 0 AND d.destination_id = ANY (SELECT id FROM c)
                   AND d.partition_key >= (SELECT min(lanes) FROM c))
             SELECT c.interface, c.destination, count(*) AS waiting,
                    min(w.partition_key) AS first_lane, max(w.partition_key) AS last_lane
             FROM waiting w JOIN c ON c.id = w.destination_id
             WHERE w.partition_key >= c.lanes
             GROUP BY c.interface, c.destination
             ORDER BY c.interface, c.destination",
        )
        .bind(&interfaces)
        .bind(&destinations)
        .bind(&lanes)
        .fetch_all(&self.pool)
        .await
        .context("looking for messages in lanes no worker would read")
    }

    /// How many messages wait for each of `interfaces` (all destinations), for those with any.
    /// Read like `stranded`: once, from the queue index.
    pub async fn waiting_by_interface(&self, interfaces: &[String]) -> Result<Vec<(String, i64)>> {
        sqlx::query_as(
            "WITH t AS MATERIALIZED (SELECT id, interface FROM destinations WHERE interface = ANY($1)),
             waiting AS MATERIALIZED (
                 SELECT d.destination_id FROM deliveries d
                 WHERE d.status = 0 AND d.destination_id = ANY (SELECT id FROM t))
             SELECT t.interface, count(*) FROM waiting w JOIN t ON t.id = w.destination_id
             GROUP BY t.interface ORDER BY t.interface",
        )
        .bind(interfaces)
        .fetch_all(&self.pool)
        .await
        .context("counting waiting messages")
    }

    /// Whether this database has a table (a read-only connection to a database an older version
    /// migrated may lack the newest ones).
    pub async fn has_table(&self, name: &str) -> Result<bool> {
        sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
            .bind(name)
            .fetch_one(&self.pool)
            .await
            .context("looking for a table")
    }
}
