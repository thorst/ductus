//! PostgreSQL storage: the message log and the work queues.
//!
//! A message is inserted (status pending) before the sender is ACKed, together with one
//! delivery row per destination of its interface. Each destination works through its own
//! pending deliveries (its queue) and marks them done; the message stays pending until every
//! destination is done. Anything pending after a crash is simply picked up again:
//! at-least-once delivery.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use sqlx::postgres::{PgPool, PgPoolOptions};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i16)]
pub enum Status {
    Pending = 0,
    Sent = 1,
    Filtered = 2,
    Quarantined = 3,
    /// A repeat of a message received within the dedupe window. Stored for audit, never delivered.
    Duplicate = 4,
}

impl Status {
    pub fn from_i16(v: i16) -> Option<Self> {
        Some(match v {
            0 => Status::Pending,
            1 => Status::Sent,
            2 => Status::Filtered,
            3 => Status::Quarantined,
            4 => Status::Duplicate,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Status::Pending => "pending",
            Status::Sent => "sent",
            Status::Filtered => "filtered",
            Status::Quarantined => "quarantined",
            Status::Duplicate => "duplicate",
        }
    }
}

impl TryFrom<i16> for Status {
    type Error = String;
    fn try_from(v: i16) -> std::result::Result<Self, String> {
        Status::from_i16(v).ok_or_else(|| format!("unknown status {v}"))
    }
}

pub struct NewMessage<'a> {
    pub interface: &'a str,
    pub partition_key: i32,
    pub ordering_key: Option<&'a str>,
    pub control_id: Option<&'a str>,
    pub payload: &'a [u8],
    /// Don't wait for the disk flush before returning (see `durability: relaxed`).
    pub relaxed: bool,
    /// The interface's destinations (`Store::destination_ids`): each gets a pending delivery.
    pub destinations: &'a [DestinationId],
}

/// Duplicate detection for one insert: the message's identifying key and how long keys are remembered.
#[derive(Debug, Clone, Copy)]
pub struct DedupeKey<'a> {
    pub key: &'a str,
    pub window: std::time::Duration,
}

/// Result of an insert with duplicate detection.
#[derive(Debug, Clone, Copy)]
pub struct Stored {
    pub message: MessageRef,
    /// True when the key was seen within the window: stored with status `Duplicate`.
    pub duplicate: bool,
}

/// Many messages from one source, stored in one transaction (one commit) together with the
/// source's progress, e.g. the byte offset reached in a file.
pub struct Batch<'a> {
    pub interface: &'a str,
    pub messages: Vec<BatchMessage<'a>>,
    /// How long dedupe keys are remembered (when messages carry them).
    pub dedupe_window: std::time::Duration,
    pub relaxed: bool,
    /// Saved in the same transaction: `(key, JSON value)`.
    pub state: Option<(&'a str, &'a str)>,
    /// The interface's destinations: each gets a pending delivery of every message queued.
    pub destinations: &'a [DestinationId],
}

pub struct BatchMessage<'a> {
    pub partition_key: i32,
    pub ordering_key: Option<&'a str>,
    pub control_id: Option<&'a str>,
    pub payload: &'a [u8],
    pub dedupe_key: Option<&'a str>,
}

/// Identifies a stored message. `received_at` lets Postgres go straight to the right partition.
#[derive(Debug, Clone, Copy)]
pub struct MessageRef {
    pub id: i64,
    pub received_at: DateTime<Utc>,
}

/// A destination of an interface, as stored (see `Store::destination_ids`).
pub type DestinationId = i32;

#[derive(Debug, sqlx::FromRow)]
pub struct Pending {
    pub id: i64,
    pub received_at: DateTime<Utc>,
    pub payload: Vec<u8>,
    pub attempts: i32,
}

impl Pending {
    pub fn reference(&self) -> MessageRef {
        MessageRef { id: self.id, received_at: self.received_at }
    }
}

/// What a destination did with a message: its final status and why, and what was sent if
/// the destination keeps it.
#[derive(Debug, Clone)]
pub struct Done {
    pub message: MessageRef,
    pub status: Status,
    pub reason: Option<String>,
    pub output: Option<Vec<u8>>,
}

impl Done {
    pub fn sent(message: MessageRef, output: Option<Vec<u8>>) -> Self {
        Self { message, status: Status::Sent, reason: None, output }
    }

    pub fn with(message: MessageRef, status: Status, reason: Option<&str>) -> Self {
        Self { message, status, reason: reason.map(str::to_string), output: None }
    }
}

/// One destination's record of one message.
#[derive(Debug, sqlx::FromRow)]
pub struct Delivery {
    pub destination: String,
    #[sqlx(try_from = "i16")]
    pub status: Status,
    pub attempts: i32,
    pub last_error: Option<String>,
    pub output: Option<Vec<u8>>,
}

/// What `Store::reconcile` changed to make stored deliveries match the configured destinations.
#[derive(Debug, Default, PartialEq)]
pub struct Reconciled {
    /// Deliveries created, per interface: pending messages with no row for a destination
    /// (stored by an older version, or the destination is new).
    pub added: Vec<(String, i64)>,
    /// Pending deliveries to destinations no longer configured, now filtered:
    /// `(interface, destination, count)`.
    pub removed: Vec<(String, String, i64)>,
    /// Messages whose overall status was brought up to date.
    pub settled: u64,
}

/// How long partition maintenance waits for a table lock before giving up (see
/// `Store::ensure_partitions`).
const PARTITION_LOCK_TIMEOUT: &str = "SET LOCAL lock_timeout = '2s'";

#[derive(Clone)]
pub struct Store {
    pool: PgPool,
}

impl Store {
    /// Connect, apply migrations, and make sure upcoming partitions exist.
    pub async fn connect(url: &str, max_connections: u32) -> Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .connect(url)
            .await
            .context("connecting to PostgreSQL")?;
        let mut migrator = sqlx::migrate!("./migrations");
        // A newer engine may have applied migrations this binary doesn't know. Migrations are
        // expand-only, so older binaries keep working: that's what makes rollback safe.
        migrator.set_ignore_missing(true);
        migrator.run(&pool).await.context("running migrations")?;
        let store = Self { pool };
        // Today's partition must exist before anything is received, so keep trying for a while
        // if a long query holds the tables (each try gives up quickly; see ensure_partitions).
        let mut tries = 0;
        while let Err(e) = store.ensure_partitions(3).await {
            tries += 1;
            if tries == 20 {
                return Err(e);
            }
            tracing::warn!(error = %format!("{e:#}"), "creating partitions; trying again");
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        }
        Ok(store)
    }

    /// Day partitions from yesterday to `days_ahead` days ahead, for `messages` and then
    /// `deliveries`.
    ///
    /// - Two transactions, so the two tables are never locked together: settling a status
    ///   locks them in the other order, and the two could deadlock (migration 0006).
    /// - Each waits at most `PARTITION_LOCK_TIMEOUT` for its table lock, then fails. Creating a
    ///   partition must wait for every open query on the table (a long report, say), and while
    ///   it waits, every later statement on the table queues behind it: inserts, so ACKs, would
    ///   stall for as long as the report runs. Partitions are made days ahead, so failing now
    ///   and trying again in an hour costs nothing.
    pub async fn ensure_partitions(&self, days_ahead: i32) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(PARTITION_LOCK_TIMEOUT).execute(&mut *tx).await?;
        sqlx::query("SELECT ductus_ensure_message_partitions($1)")
            .bind(days_ahead)
            .execute(&mut *tx)
            .await
            .context("creating message partitions")?;
        tx.commit().await?;
        let mut tx = self.pool.begin().await?;
        sqlx::query(PARTITION_LOCK_TIMEOUT).execute(&mut *tx).await?;
        sqlx::query("SELECT ductus_ensure_delivery_partitions()")
            .execute(&mut *tx)
            .await
            .context("creating delivery partitions")?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn insert(&self, m: NewMessage<'_>) -> Result<MessageRef> {
        let query = sqlx::query_as::<_, (i64, DateTime<Utc>)>(INSERT_ONE)
            .bind(m.interface)
            .bind(m.partition_key)
            .bind(m.ordering_key)
            .bind(m.control_id)
            .bind(m.payload)
            .bind(Status::Pending as i16)
            .bind(m.destinations);
        let (id, received_at) = if m.relaxed {
            let mut tx = self.pool.begin().await.context("starting transaction")?;
            sqlx::query("SET LOCAL synchronous_commit = off").execute(&mut *tx).await.context("relaxing commit")?;
            let row = query.fetch_one(&mut *tx).await.context("inserting message")?;
            tx.commit().await.context("committing message")?;
            row
        } else {
            query.fetch_one(&self.pool).await.context("inserting message")?
        };
        Ok(MessageRef { id, received_at })
    }

    /// Insert with duplicate detection, in one transaction (one commit, same durability cost
    /// as a plain insert). Duplicates are stored with status `Duplicate` for audit.
    pub async fn insert_deduped(&self, m: NewMessage<'_>, dedupe: DedupeKey<'_>) -> Result<Stored> {
        let mut tx = self.pool.begin().await.context("starting transaction")?;
        if m.relaxed {
            sqlx::query("SET LOCAL synchronous_commit = off").execute(&mut *tx).await.context("relaxing commit")?;
        }
        // New key, or an old one outside the window: claim it. Otherwise nothing is returned.
        let claimed = sqlx::query(
            "INSERT INTO dedupe_keys (interface, key) VALUES ($1, $2)
             ON CONFLICT (interface, key) DO UPDATE SET seen_at = now()
                 WHERE dedupe_keys.seen_at < now() - make_interval(secs => $3)
             RETURNING 1",
        )
        .bind(m.interface)
        .bind(dedupe.key)
        .bind(dedupe.window.as_secs_f64())
        .fetch_optional(&mut *tx)
        .await
        .context("checking for duplicate")?;
        let duplicate = claimed.is_none();

        let (id, received_at) = sqlx::query_as::<_, (i64, DateTime<Utc>)>(INSERT_ONE)
            .bind(m.interface)
            .bind(m.partition_key)
            .bind(m.ordering_key)
            .bind(m.control_id)
            .bind(m.payload)
            .bind(if duplicate { Status::Duplicate } else { Status::Pending } as i16)
            .bind(if duplicate { &[][..] } else { m.destinations }) // duplicates aren't delivered
            .fetch_one(&mut *tx)
            .await
            .context("inserting message")?;
        tx.commit().await.context("committing message")?;
        Ok(Stored { message: MessageRef { id, received_at }, duplicate })
    }

    /// Store a batch in one transaction. Returns, per message, whether it was a duplicate
    /// (stored with status `Duplicate`, not queued). A key repeated within the batch counts
    /// as a duplicate after its first occurrence.
    pub async fn insert_batch(&self, batch: &Batch<'_>) -> Result<Vec<bool>> {
        let mut tx = self.pool.begin().await.context("starting transaction")?;
        if batch.relaxed {
            sqlx::query("SET LOCAL synchronous_commit = off").execute(&mut *tx).await.context("relaxing commit")?;
        }

        let mut duplicate = vec![false; batch.messages.len()];
        let mut first_seen: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        for (i, m) in batch.messages.iter().enumerate() {
            if let Some(key) = m.dedupe_key {
                if first_seen.contains_key(key) {
                    duplicate[i] = true;
                } else {
                    first_seen.insert(key, i);
                }
            }
        }
        if !first_seen.is_empty() {
            let keys: Vec<&str> = first_seen.keys().copied().collect();
            // Claims keys that are new or outside the window; the rest are duplicates.
            let claimed: Vec<String> = sqlx::query_scalar(
                "INSERT INTO dedupe_keys (interface, key) SELECT $1, unnest($2::text[])
                 ON CONFLICT (interface, key) DO UPDATE SET seen_at = now()
                     WHERE dedupe_keys.seen_at < now() - make_interval(secs => $3)
                 RETURNING key",
            )
            .bind(batch.interface)
            .bind(&keys)
            .bind(batch.dedupe_window.as_secs_f64())
            .fetch_all(&mut *tx)
            .await
            .context("checking for duplicates")?;
            let claimed: std::collections::HashSet<&str> = claimed.iter().map(String::as_str).collect();
            for (key, i) in &first_seen {
                duplicate[*i] = !claimed.contains(key);
            }
        }

        if !batch.messages.is_empty() {
            let m = &batch.messages;
            let statuses: Vec<i16> =
                duplicate.iter().map(|&d| if d { Status::Duplicate } else { Status::Pending } as i16).collect();
            // ORDER BY ordinality: ids follow the order of the batch, which is the lanes' order.
            sqlx::query(
                "WITH m AS (
                     INSERT INTO messages (interface, partition_key, ordering_key, control_id, payload, status)
                     SELECT $1, p, o, c, b, s
                     FROM unnest($2::int[], $3::text[], $4::text[], $5::bytea[], $6::smallint[])
                          WITH ORDINALITY AS t(p, o, c, b, s, n)
                     ORDER BY n
                     RETURNING id, received_at, partition_key, status
                 )
                 INSERT INTO deliveries (message_id, received_at, destination_id, partition_key)
                 SELECT m.id, m.received_at, d, m.partition_key FROM m, unnest($7::int[]) d WHERE m.status = 0",
            )
            .bind(batch.interface)
            .bind(m.iter().map(|m| m.partition_key).collect::<Vec<_>>())
            .bind(m.iter().map(|m| m.ordering_key).collect::<Vec<_>>())
            .bind(m.iter().map(|m| m.control_id).collect::<Vec<_>>())
            .bind(m.iter().map(|m| m.payload.to_vec()).collect::<Vec<_>>())
            .bind(statuses)
            .bind(batch.destinations)
            .execute(&mut *tx)
            .await
            .context("inserting messages")?;
        }
        if let Some((key, value)) = batch.state {
            save_state(&mut tx, batch.interface, key, value).await?;
        }
        tx.commit().await.context("committing messages")?;
        Ok(duplicate)
    }

    /// Store a scheduled run's message and the schedule's `state` (JSON, under `key`) together,
    /// unless the previous run is still pending (runs are one at a time): then `skipped_state`
    /// is saved instead and `None` is returned.
    pub async fn insert_scheduled(
        &self,
        m: NewMessage<'_>,
        key: &str,
        state: &str,
        skipped_state: &str,
    ) -> Result<Option<MessageRef>> {
        let mut tx = self.pool.begin().await.context("starting transaction")?;
        let busy: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM messages WHERE interface = $1 AND status = 0)")
                .bind(m.interface)
                .fetch_one(&mut *tx)
                .await
                .context("checking for a pending run")?;
        let stored = if busy {
            None
        } else {
            let (id, received_at) = sqlx::query_as::<_, (i64, DateTime<Utc>)>(INSERT_ONE)
                .bind(m.interface)
                .bind(m.partition_key)
                .bind(m.ordering_key)
                .bind(m.control_id)
                .bind(m.payload)
                .bind(Status::Pending as i16)
                .bind(m.destinations)
                .fetch_one(&mut *tx)
                .await
                .context("inserting message")?;
            Some(MessageRef { id, received_at })
        };
        save_state(&mut tx, m.interface, key, if stored.is_some() { state } else { skipped_state }).await?;
        tx.commit().await.context("committing scheduled run")?;
        Ok(stored)
    }

    /// A source's saved state (JSON), if any.
    pub async fn source_state(&self, interface: &str, key: &str) -> Result<Option<String>> {
        sqlx::query_scalar("SELECT value::text FROM source_state WHERE interface = $1 AND key = $2")
            .bind(interface)
            .bind(key)
            .fetch_optional(&self.pool)
            .await
            .context("reading source state")
    }

    pub async fn delete_source_state(&self, interface: &str, key: &str) -> Result<()> {
        sqlx::query("DELETE FROM source_state WHERE interface = $1 AND key = $2")
            .bind(interface)
            .bind(key)
            .execute(&self.pool)
            .await
            .context("deleting source state")?;
        Ok(())
    }

    /// Forget dedupe keys older than `age` (keeps the table small).
    pub async fn purge_dedupe_keys(&self, age: std::time::Duration) -> Result<u64> {
        let result = sqlx::query("DELETE FROM dedupe_keys WHERE seen_at < now() - make_interval(secs => $1)")
            .bind(age.as_secs_f64())
            .execute(&self.pool)
            .await
            .context("purging dedupe keys")?;
        Ok(result.rows_affected())
    }

    /// A destination's oldest pending messages in the given lanes. The caller must be the only
    /// owner of these lanes for this destination; that's what makes per-key ordering hold
    /// without row locks.
    pub async fn pending(&self, destination: DestinationId, partitions: &[i32], limit: i64) -> Result<Vec<Pending>> {
        sqlx::query_as::<_, Pending>(
            "SELECT d.message_id AS id, d.received_at, m.payload, d.attempts
             FROM deliveries d JOIN messages m ON m.id = d.message_id AND m.received_at = d.received_at
             WHERE d.destination_id = $1 AND d.status = 0 AND d.partition_key = ANY($2)
             ORDER BY d.message_id LIMIT $3",
        )
        .bind(destination)
        .bind(partitions)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("reading pending messages")
    }

    /// Record what a destination did with messages (sent, filtered, quarantined), in one
    /// statement, then their overall status.
    pub async fn finish(&self, destination: DestinationId, done: &[Done]) -> Result<()> {
        if done.is_empty() {
            return Ok(());
        }
        let ids: Vec<i64> = done.iter().map(|d| d.message.id).collect();
        let times: Vec<DateTime<Utc>> = done.iter().map(|d| d.message.received_at).collect();
        let statuses: Vec<i16> = done.iter().map(|d| d.status as i16).collect();
        let reasons: Vec<Option<&str>> = done.iter().map(|d| d.reason.as_deref()).collect();
        let outputs: Vec<Option<&[u8]>> = done.iter().map(|d| d.output.as_deref()).collect();
        sqlx::query(
            "UPDATE deliveries d
             SET status = u.status, last_error = COALESCE(u.reason, d.last_error), output = u.output, updated_at = now()
             FROM unnest($2::bigint[], $3::timestamptz[], $4::smallint[], $5::text[], $6::bytea[])
                  AS u(id, received_at, status, reason, output)
             WHERE d.message_id = u.id AND d.received_at = u.received_at AND d.destination_id = $1",
        )
        .bind(destination)
        .bind(&ids)
        .bind(&times)
        .bind(&statuses)
        .bind(&reasons)
        .bind(&outputs)
        .execute(&self.pool)
        .await
        .context("updating delivery statuses")?;
        let messages: Vec<MessageRef> = done.iter().map(|d| d.message).collect();
        self.settle(&messages).await
    }

    /// Bring the overall status of `messages` up to date with their deliveries: still pending
    /// while any destination is; then quarantined if any destination quarantined it (someone
    /// must look), else sent if any destination got it, else filtered.
    ///
    /// Must run after the delivery updates are committed, as a statement of its own: then of
    /// two destinations finishing the same message at once, the later one always sees both done.
    /// (In one statement or transaction, each could see the other still pending, and the
    /// message would stay pending forever.)
    async fn settle(&self, messages: &[MessageRef]) -> Result<()> {
        let ids: Vec<i64> = messages.iter().map(|m| m.id).collect();
        let times: Vec<DateTime<Utc>> = messages.iter().map(|m| m.received_at).collect();
        sqlx::query(
            "WITH done AS (
                 SELECT message_id, received_at, count(*) AS n,
                        CASE WHEN bool_or(status = 3) THEN 3 WHEN bool_or(status = 1) THEN 1 ELSE 2 END AS status
                 FROM deliveries
                 WHERE (message_id, received_at) IN (SELECT * FROM unnest($1::bigint[], $2::timestamptz[]))
                 GROUP BY message_id, received_at
                 HAVING NOT bool_or(status = 0)
             )
             UPDATE messages m
             SET status = done.status, updated_at = now(),
                 -- why it was quarantined or filtered (per destination, when there are several)
                 last_error = COALESCE((
                     SELECT CASE WHEN done.n = 1 THEN max(d.last_error)
                                 ELSE string_agg(t.name || ': ' || d.last_error, '; ' ORDER BY t.name) END
                     FROM deliveries d JOIN destinations t ON t.id = d.destination_id
                     WHERE d.message_id = done.message_id AND d.received_at = done.received_at
                       AND d.status = done.status AND done.status <> 1
                 ), m.last_error)
             FROM done
             WHERE m.id = done.message_id AND m.received_at = done.received_at AND m.status = 0",
        )
        .bind(&ids)
        .bind(&times)
        .execute(&self.pool)
        .await
        .context("updating message status")?;
        Ok(())
    }

    /// Record a failed attempt to deliver to a destination; returns its new attempt count.
    pub async fn record_failure(&self, m: MessageRef, destination: DestinationId, error: &str) -> Result<i32> {
        let (attempts,) = sqlx::query_as::<_, (i32,)>(
            "UPDATE deliveries SET attempts = attempts + 1, last_error = $4, updated_at = now()
             WHERE message_id = $1 AND received_at = $2 AND destination_id = $3 RETURNING attempts",
        )
        .bind(m.id)
        .bind(m.received_at)
        .bind(destination)
        .bind(error)
        .fetch_one(&self.pool)
        .await
        .context("recording failure")?;
        Ok(attempts)
    }

    /// Make the stored deliveries of the interfaces in `destinations` (pairs of interface and
    /// destination name: every destination of every queued interface) match them, before any
    /// worker starts. All interfaces at once, so startup costs the same few queries for 1,000
    /// interfaces as for one. For each interface:
    /// - a pending delivery whose message isn't pending any more takes the message's status
    ///   (an older version, which knows only `messages.status`, finished it);
    /// - pending messages get a delivery for every destination they lack one for (stored by an
    ///   older version, or the destination was added: messages still in flight go to it too);
    /// - pending deliveries to destinations no longer configured are filtered (renaming a
    ///   destination moves its backlog to the new name, through the step above);
    /// - messages whose deliveries are now all done get their overall status.
    pub async fn reconcile(&self, destinations: &[(String, String)]) -> Result<Reconciled> {
        let ids = self.destination_ids(destinations).await?;
        let interfaces: Vec<&str> = destinations.iter().map(|(i, _)| i.as_str()).collect();
        sqlx::query(
            "UPDATE deliveries d SET status = m.status, updated_at = now()
             FROM messages m
             WHERE d.destination_id IN (SELECT id FROM destinations WHERE interface = ANY($1)) AND d.status = 0
               AND m.id = d.message_id AND m.received_at = d.received_at AND m.status <> 0",
        )
        .bind(&interfaces)
        .execute(&self.pool)
        .await
        .context("syncing deliveries finished elsewhere")?;
        let added: Vec<(String, i64)> = sqlx::query_as(
            "WITH added AS (
                 INSERT INTO deliveries (message_id, received_at, destination_id, partition_key)
                 SELECT m.id, m.received_at, c.id, m.partition_key
                 FROM messages m JOIN unnest($1::text[], $2::int[]) AS c(interface, id) ON m.interface = c.interface
                 WHERE m.interface = ANY($1) AND m.status = 0
                 ON CONFLICT DO NOTHING
                 RETURNING destination_id
             )
             SELECT t.interface, count(*) FROM added JOIN destinations t ON t.id = added.destination_id
             GROUP BY 1 ORDER BY 1",
        )
        .bind(&interfaces)
        .bind(&ids)
        .fetch_all(&self.pool)
        .await
        .context("adding deliveries")?;
        let removed: Vec<(String, String, i64)> = sqlx::query_as(
            "WITH removed AS (
                 UPDATE deliveries d
                 SET status = 2, updated_at = now(),
                     last_error = 'destination `' || t.name || '` was removed from interface.yaml'
                 FROM destinations t
                 WHERE t.id = d.destination_id AND t.interface = ANY($1)
                   AND d.destination_id <> ALL($2) AND d.status = 0
                 RETURNING t.interface, t.name
             )
             SELECT interface, name, count(*) FROM removed GROUP BY 1, 2 ORDER BY 1, 2",
        )
        .bind(&interfaces)
        .bind(&ids)
        .fetch_all(&self.pool)
        .await
        .context("filtering deliveries to removed destinations")?;
        let done: Vec<(i64, DateTime<Utc>)> = sqlx::query_as(
            "SELECT id, received_at FROM messages m
             WHERE interface = ANY($1) AND status = 0 AND NOT EXISTS (
                 SELECT 1 FROM deliveries d
                 WHERE d.message_id = m.id AND d.received_at = m.received_at AND d.status = 0)",
        )
        .bind(&interfaces)
        .fetch_all(&self.pool)
        .await
        .context("finding finished messages")?;
        let done: Vec<MessageRef> = done.into_iter().map(|(id, received_at)| MessageRef { id, received_at }).collect();
        self.settle(&done).await?;
        Ok(Reconciled { added, removed, settled: done.len() as u64 })
    }

    /// The ids of destinations (pairs of interface and destination name), in the same order,
    /// registering the ones not seen before.
    pub async fn destination_ids(&self, destinations: &[(String, String)]) -> Result<Vec<DestinationId>> {
        let (interfaces, names): (Vec<&str>, Vec<&str>) =
            destinations.iter().map(|(i, d)| (i.as_str(), d.as_str())).unzip();
        sqlx::query(
            "INSERT INTO destinations (interface, name) SELECT * FROM unnest($1::text[], $2::text[])
             ON CONFLICT DO NOTHING",
        )
        .bind(&interfaces)
        .bind(&names)
        .execute(&self.pool)
        .await
        .context("registering destinations")?;
        sqlx::query_scalar(
            "SELECT t.id FROM unnest($1::text[], $2::text[]) WITH ORDINALITY AS c(interface, name, n)
             JOIN destinations t ON t.interface = c.interface AND t.name = c.name ORDER BY c.n",
        )
        .bind(&interfaces)
        .bind(&names)
        .fetch_all(&self.pool)
        .await
        .context("reading destination ids")
    }

    /// Every destination's record of a message, by destination name.
    pub async fn deliveries(&self, m: MessageRef) -> Result<Vec<Delivery>> {
        sqlx::query_as::<_, Delivery>(
            "SELECT t.name AS destination, d.status, d.attempts, d.last_error, d.output
             FROM deliveries d JOIN destinations t ON t.id = d.destination_id
             WHERE d.message_id = $1 AND d.received_at = $2 ORDER BY t.name",
        )
        .bind(m.id)
        .bind(m.received_at)
        .fetch_all(&self.pool)
        .await
        .context("reading deliveries")
    }

    /// Payloads of an interface's messages with a status, oldest first (for tests and tools).
    pub async fn payloads(&self, interface: &str, status: Status) -> Result<Vec<Vec<u8>>> {
        sqlx::query_scalar("SELECT payload FROM messages WHERE interface = $1 AND status = $2 ORDER BY id")
            .bind(interface)
            .bind(status as i16)
            .fetch_all(&self.pool)
            .await
            .context("reading messages")
    }

    /// Message counts by interface and status.
    pub async fn counts(&self) -> Result<Vec<(String, Status, i64)>> {
        let rows = sqlx::query_as::<_, (String, i16, i64)>(
            "SELECT interface, status, count(*) FROM messages GROUP BY 1, 2 ORDER BY 1, 2",
        )
        .fetch_all(&self.pool)
        .await
        .context("counting messages")?;
        Ok(rows.into_iter().filter_map(|(i, s, n)| Status::from_i16(s).map(|s| (i, s, n))).collect())
    }

    /// Delivery counts by interface, destination and status.
    pub async fn delivery_counts(&self) -> Result<Vec<(String, String, Status, i64)>> {
        let rows = sqlx::query_as::<_, (String, String, i16, i64)>(
            "SELECT t.interface, t.name, d.status, count(*)
             FROM deliveries d JOIN destinations t ON t.id = d.destination_id
             GROUP BY 1, 2, 3 ORDER BY 1, 2, 3",
        )
        .fetch_all(&self.pool)
        .await
        .context("counting deliveries")?;
        Ok(rows.into_iter().filter_map(|(i, d, s, n)| Status::from_i16(s).map(|s| (i, d, s, n))).collect())
    }
}

/// Insert one message (`$6` its status) and a pending delivery for each destination in `$7`
/// (none for a duplicate), in one statement.
const INSERT_ONE: &str = "
    WITH m AS (
        INSERT INTO messages (interface, partition_key, ordering_key, control_id, payload, status)
        VALUES ($1, $2, $3, $4, $5, $6) RETURNING id, received_at
    ), queued AS (
        INSERT INTO deliveries (message_id, received_at, destination_id, partition_key)
        SELECT m.id, m.received_at, d, $2 FROM m, unnest($7::int[]) d
    )
    SELECT id, received_at FROM m";

async fn save_state(tx: &mut sqlx::PgConnection, interface: &str, key: &str, value: &str) -> Result<()> {
    sqlx::query(
        "INSERT INTO source_state (interface, key, value) VALUES ($1, $2, $3::jsonb)
         ON CONFLICT (interface, key) DO UPDATE SET value = EXCLUDED.value, updated_at = now()",
    )
    .bind(interface)
    .bind(key)
    .bind(value)
    .execute(tx)
    .await
    .context("saving source state")?;
    Ok(())
}
