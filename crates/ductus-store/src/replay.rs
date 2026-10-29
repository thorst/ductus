//! What `ductus replay` reads (0.6.6.4): a period's originals per interface in arrival order, and
//! what each destination did with them. Reading only: it may run against a replica.

use crate::{Status, Store};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};

/// A message as received, to run through the code again.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Original {
    pub id: i64,
    pub received_at: DateTime<Utc>,
    pub payload: Vec<u8>,
    /// Routed from another interface: the id of the message there.
    pub origin: Option<i64>,
}

/// One destination's record of one message: its status, why, and what was sent when it keeps that.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Recorded {
    pub id: i64,
    pub destination: String,
    #[sqlx(try_from = "i16")]
    pub status: Status,
    pub last_error: Option<String>,
    pub output: Option<Vec<u8>>,
}

impl Store {
    /// Up to `limit` of an interface's messages received in `[from, to)`, after id `after`, in id
    /// (arrival) order. Duplicates are left out: they were never delivered.
    pub async fn originals(
        &self,
        interface: &str,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        after: i64,
        limit: i64,
    ) -> Result<Vec<Original>> {
        sqlx::query_as::<_, Original>(
            "SELECT id, received_at, payload, origin_id AS origin FROM messages
             WHERE interface = $1 AND received_at >= $2 AND received_at < $3 AND id > $4 AND status <> 4
             ORDER BY id LIMIT $5",
        )
        .bind(interface)
        .bind(from)
        .bind(to)
        .bind(after)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("reading messages to replay")
    }

    /// What each of an interface's destinations recorded for `messages` (as `originals` returned
    /// them: their time range keeps the lookup in their partitions).
    pub async fn recorded(&self, interface: &str, messages: &[Original]) -> Result<Vec<Recorded>> {
        let (Some(first), Some(last)) =
            (messages.iter().map(|m| m.received_at).min(), messages.iter().map(|m| m.received_at).max())
        else {
            return Ok(Vec::new());
        };
        let ids: Vec<i64> = messages.iter().map(|m| m.id).collect();
        sqlx::query_as::<_, Recorded>(
            "SELECT d.message_id AS id, t.name AS destination, d.status, d.last_error, d.output
             FROM deliveries d JOIN destinations t ON t.id = d.destination_id
             WHERE t.interface = $1 AND d.message_id = ANY($2) AND d.received_at BETWEEN $3 AND $4",
        )
        .bind(interface)
        .bind(&ids)
        .bind(first)
        .bind(last)
        .fetch_all(&self.pool)
        .await
        .context("reading what the destinations recorded")
    }
}
