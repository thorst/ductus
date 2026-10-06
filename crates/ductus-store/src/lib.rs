//! PostgreSQL storage: the message log and the work queue in one table.
//!
//! A message is inserted (status pending) before the sender is ACKed, and stays
//! pending until the destination confirms delivery. Anything pending after a
//! crash is simply picked up again: at-least-once delivery.

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

pub struct NewMessage<'a> {
    pub interface: &'a str,
    pub partition_key: i32,
    pub ordering_key: Option<&'a str>,
    pub control_id: Option<&'a str>,
    pub payload: &'a [u8],
    /// Don't wait for the disk flush before returning (see `durability: relaxed`).
    pub relaxed: bool,
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

/// Identifies a stored message. `received_at` lets Postgres go straight to the right partition.
#[derive(Debug, Clone, Copy)]
pub struct MessageRef {
    pub id: i64,
    pub received_at: DateTime<Utc>,
}

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
        store.ensure_partitions(3).await?;
        Ok(store)
    }

    pub async fn ensure_partitions(&self, days_ahead: i32) -> Result<()> {
        sqlx::query("SELECT ductus_ensure_partitions($1)")
            .bind(days_ahead)
            .execute(&self.pool)
            .await
            .context("creating partitions")?;
        Ok(())
    }

    pub async fn insert(&self, m: NewMessage<'_>) -> Result<MessageRef> {
        let query = sqlx::query_as::<_, (i64, DateTime<Utc>)>(
            "INSERT INTO messages (interface, partition_key, ordering_key, control_id, payload)
             VALUES ($1, $2, $3, $4, $5) RETURNING id, received_at",
        )
        .bind(m.interface)
        .bind(m.partition_key)
        .bind(m.ordering_key)
        .bind(m.control_id)
        .bind(m.payload);
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

        let (id, received_at) = sqlx::query_as::<_, (i64, DateTime<Utc>)>(
            "INSERT INTO messages (interface, partition_key, ordering_key, control_id, payload, status)
             VALUES ($1, $2, $3, $4, $5, $6) RETURNING id, received_at",
        )
        .bind(m.interface)
        .bind(m.partition_key)
        .bind(m.ordering_key)
        .bind(m.control_id)
        .bind(m.payload)
        .bind(if duplicate { Status::Duplicate } else { Status::Pending } as i16)
        .fetch_one(&mut *tx)
        .await
        .context("inserting message")?;
        tx.commit().await.context("committing message")?;
        Ok(Stored { message: MessageRef { id, received_at }, duplicate })
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

    /// Oldest pending messages in the given lanes. The caller must be the only owner of
    /// these lanes; that's what makes per-key ordering hold without row locks.
    pub async fn pending(&self, interface: &str, partitions: &[i32], limit: i64) -> Result<Vec<Pending>> {
        sqlx::query_as::<_, Pending>(
            "SELECT id, received_at, payload, attempts FROM messages
             WHERE interface = $1 AND status = 0 AND partition_key = ANY($2)
             ORDER BY id LIMIT $3",
        )
        .bind(interface)
        .bind(partitions)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("reading pending messages")
    }

    pub async fn set_status(&self, m: MessageRef, status: Status, error: Option<&str>) -> Result<()> {
        sqlx::query(
            "UPDATE messages SET status = $3, last_error = COALESCE($4, last_error), updated_at = now()
             WHERE id = $1 AND received_at = $2",
        )
        .bind(m.id)
        .bind(m.received_at)
        .bind(status as i16)
        .bind(error)
        .execute(&self.pool)
        .await
        .context("updating message status")?;
        Ok(())
    }

    /// Set the same status on many messages in one statement.
    pub async fn set_status_many(&self, messages: &[MessageRef], status: Status) -> Result<()> {
        if messages.is_empty() {
            return Ok(());
        }
        let ids: Vec<i64> = messages.iter().map(|m| m.id).collect();
        let times: Vec<DateTime<Utc>> = messages.iter().map(|m| m.received_at).collect();
        sqlx::query(
            "UPDATE messages SET status = $1, updated_at = now()
             WHERE (id, received_at) IN (SELECT * FROM unnest($2::bigint[], $3::timestamptz[]))",
        )
        .bind(status as i16)
        .bind(&ids)
        .bind(&times)
        .execute(&self.pool)
        .await
        .context("updating message statuses")?;
        Ok(())
    }

    /// Record a failed delivery attempt and return the new attempt count.
    pub async fn record_failure(&self, m: MessageRef, error: &str) -> Result<i32> {
        let (attempts,) = sqlx::query_as::<_, (i32,)>(
            "UPDATE messages SET attempts = attempts + 1, last_error = $3, updated_at = now()
             WHERE id = $1 AND received_at = $2 RETURNING attempts",
        )
        .bind(m.id)
        .bind(m.received_at)
        .bind(error)
        .fetch_one(&self.pool)
        .await
        .context("recording failure")?;
        Ok(attempts)
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
}
