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
}

impl Status {
    pub fn from_i16(v: i16) -> Option<Self> {
        Some(match v {
            0 => Status::Pending,
            1 => Status::Sent,
            2 => Status::Filtered,
            3 => Status::Quarantined,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Status::Pending => "pending",
            Status::Sent => "sent",
            Status::Filtered => "filtered",
            Status::Quarantined => "quarantined",
        }
    }
}

pub struct NewMessage<'a> {
    pub interface: &'a str,
    pub partition_key: i32,
    pub ordering_key: Option<&'a str>,
    pub control_id: Option<&'a str>,
    pub payload: &'a [u8],
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
        sqlx::migrate!("./migrations").run(&pool).await.context("running migrations")?;
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
        let (id, received_at) = sqlx::query_as::<_, (i64, DateTime<Utc>)>(
            "INSERT INTO messages (interface, partition_key, ordering_key, control_id, payload)
             VALUES ($1, $2, $3, $4, $5) RETURNING id, received_at",
        )
        .bind(m.interface)
        .bind(m.partition_key)
        .bind(m.ordering_key)
        .bind(m.control_id)
        .bind(m.payload)
        .fetch_one(&self.pool)
        .await
        .context("inserting message")?;
        Ok(MessageRef { id, received_at })
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
