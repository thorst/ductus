//! What people do to messages through the API: send a delivery again (back to pending, in place)
//! or queue an edited copy of a message. Every action is recorded in `message_actions` with what
//! the delivery was before, so nothing it changes is lost. And the quarantine across interfaces,
//! which is what these actions are mostly for.

use crate::{DestinationId, MessageRef, Status, Store, Tier, retrying, span};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};

/// Who acted, and why.
#[derive(Debug, Clone, Copy)]
pub struct Actor<'a> {
    /// Their API token (`api_tokens.id`).
    pub token_id: Option<i32>,
    pub name: &'a str,
    pub reason: Option<&'a str>,
}

/// Which deliveries to send again.
#[derive(Debug, Clone, Copy)]
pub struct Resend<'a> {
    pub messages: &'a [i64],
    /// Only these destinations' deliveries. Pass configured destinations only: a removed
    /// destination's delivery put back to pending would never be sent.
    pub destinations: &'a [DestinationId],
    /// Only quarantined deliveries; otherwise any finished one, sent and filtered too (a replay).
    pub quarantined_only: bool,
}

/// A delivery put back to pending, and what it was before.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Resent {
    pub message_id: i64,
    pub interface: String,
    pub destination_id: DestinationId,
    pub destination: String,
    /// The message's lane (whose worker to wake).
    pub partition_key: i32,
    #[sqlx(try_from = "i16")]
    pub previous_status: Status,
    pub previous_attempts: i32,
    pub previous_error: Option<String>,
}

/// A corrected copy of a message, to queue at the back of its lane.
#[derive(Debug, Clone, Copy)]
pub struct Copy<'a> {
    pub original: i64,
    /// The original's interface (checked).
    pub interface: &'a str,
    /// Lane, ordering key and control id, read from the edited payload.
    pub partition_key: i32,
    pub ordering_key: Option<&'a str>,
    pub control_id: Option<&'a str>,
    pub payload: &'a [u8],
    pub tier: Tier,
    /// Where the copy goes. The original's quarantined deliveries to these are marked replaced.
    pub destinations: &'a [DestinationId],
}

/// What editing a message did.
#[derive(Debug)]
pub enum Edited {
    NotFound,
    /// The original is still pending for these destinations: a worker may be sending or
    /// retrying it right now, so it can't be replaced (both would be sent).
    StillPending(Vec<DestinationId>),
    Copied {
        copy: MessageRef,
        /// Destinations whose quarantined delivery of the original is now filtered, replaced by
        /// the copy.
        replaced: Vec<DestinationId>,
    },
}

/// One recorded action (`message_actions`).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Action {
    pub id: i64,
    pub at: DateTime<Utc>,
    /// `resend` or `edit`.
    pub action: String,
    pub actor: String,
    pub reason: Option<String>,
    pub message_id: i64,
    pub destination: String,
    /// The delivery before (none: the message had none for that destination).
    pub previous_status: Option<i16>,
    pub previous_attempts: Option<i32>,
    pub previous_error: Option<String>,
    /// `edit`: the copy it queued.
    pub copy_id: Option<i64>,
}

/// One quarantined delivery, with what identifies its message.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Quarantined {
    pub message_id: i64,
    pub received_at: DateTime<Utc>,
    pub interface: String,
    pub destination_id: DestinationId,
    pub attempts: i32,
    pub last_error: Option<String>,
    /// When it was quarantined (its delivery's last change).
    pub quarantined_at: DateTime<Utc>,
    pub ordering_key: Option<String>,
    pub control_id: Option<String>,
    pub size: i32,
}

impl Store {
    /// Put deliveries back to pending (and their messages), recording each. A message keeps its
    /// id, so its worker takes it next in its lane, ahead of what's waiting. Pending deliveries
    /// are left alone (one is being sent or retried right now), and so are duplicates (never
    /// queued). Returns what was reset, by message and destination.
    ///
    /// The messages are locked first, in id order, as settling and the held area lock them;
    /// settling reads deliveries only once it holds the lock, so it can't settle a message from
    /// the delivery as it was before this (see `settle`).
    pub async fn resend(&self, r: Resend<'_>, by: Actor<'_>) -> Result<Vec<Resent>> {
        if r.messages.is_empty() || r.destinations.is_empty() {
            return Ok(Vec::new());
        }
        retrying(|| async {
            let mut tx = self.pool.begin().await.context("starting transaction")?;
            let locked: Vec<(i64, DateTime<Utc>)> = sqlx::query_as(
                "SELECT id, received_at FROM messages WHERE id = ANY($1) AND status <> 4 ORDER BY id FOR UPDATE",
            )
            .bind(r.messages)
            .fetch_all(&mut *tx)
            .await
            .context("locking messages")?;
            let (ids, times): (Vec<i64>, Vec<DateTime<Utc>>) = locked.into_iter().unzip();
            let Some((first, last)) = span(&times) else { return Ok(Vec::new()) };
            let resent: Vec<Resent> = sqlx::query_as(
                "WITH old AS (
                     SELECT d.message_id, d.received_at, d.destination_id, d.partition_key,
                            d.status, d.attempts, d.last_error, d.output
                     FROM deliveries d
                     WHERE (d.message_id, d.received_at) IN (SELECT * FROM unnest($1::bigint[], $2::timestamptz[]))
                       AND d.received_at BETWEEN $3 AND $4 AND d.destination_id = ANY($5)
                       AND d.status <> 0 AND (d.status = 3 OR NOT $6)
                     ORDER BY d.message_id, d.destination_id
                     FOR UPDATE
                 ), reset AS (
                     UPDATE deliveries d
                     SET status = 0, attempts = 0, last_error = NULL, output = NULL, updated_at = now()
                     FROM old
                     WHERE d.message_id = old.message_id AND d.received_at = old.received_at
                       AND d.destination_id = old.destination_id AND d.received_at BETWEEN $3 AND $4
                 ), recorded AS (
                     INSERT INTO message_actions (action, token_id, actor, reason, message_id, received_at, interface,
                                                  destination_id, previous_status, previous_attempts, previous_error,
                                                  previous_output)
                     SELECT 'resend', $7, $8, $9, old.message_id, old.received_at, t.interface, old.destination_id,
                            old.status, old.attempts, old.last_error, old.output
                     FROM old JOIN destinations t ON t.id = old.destination_id
                 )
                 SELECT old.message_id, t.interface, old.destination_id, t.name AS destination, old.partition_key,
                        old.status AS previous_status, old.attempts AS previous_attempts,
                        old.last_error AS previous_error
                 FROM old JOIN destinations t ON t.id = old.destination_id
                 ORDER BY old.message_id, t.name",
            )
            .bind(&ids)
            .bind(&times)
            .bind(first)
            .bind(last)
            .bind(r.destinations)
            .bind(r.quarantined_only)
            .bind(by.token_id)
            .bind(by.name)
            .bind(by.reason)
            .fetch_all(&mut *tx)
            .await
            .context("resetting deliveries")?;
            // Pending again until its destinations are done; settled then like any other.
            let mut again: Vec<i64> = resent.iter().map(|r| r.message_id).collect();
            again.dedup();
            sqlx::query(
                "UPDATE messages SET status = 0, last_error = NULL, updated_at = now()
                 WHERE id = ANY($1) AND received_at BETWEEN $2 AND $3 AND status <> 0",
            )
            .bind(&again)
            .bind(first)
            .bind(last)
            .execute(&mut *tx)
            .await
            .context("marking messages pending again")?;
            tx.commit().await.context("committing resend")?;
            anyhow::Ok(resent)
        })
        .await
    }

    /// Queue a corrected copy of a message: a new message (new id, at the back of its lane)
    /// with the original's `origin`, for `c.destinations` only, never deduplicated. The
    /// original's quarantined deliveries to those destinations become filtered ("replaced by
    /// edited copy N"); its sent ones stay as they are. Refused for destinations where the
    /// original is still pending (`Edited::StillPending`). Recorded per destination with the copy.
    pub async fn copy_edited(&self, c: Copy<'_>, by: Actor<'_>) -> Result<Edited> {
        let edited = retrying(|| async {
            let mut tx = self.pool.begin().await.context("starting transaction")?;
            let original: Option<(DateTime<Utc>, String, Option<i64>)> = sqlx::query_as(
                "SELECT received_at, interface, origin_id FROM messages WHERE id = $1 AND status <> 4 FOR UPDATE",
            )
            .bind(c.original)
            .fetch_optional(&mut *tx)
            .await
            .context("locking the original")?;
            let Some((received_at, interface, origin)) = original else { return anyhow::Ok((Edited::NotFound, None)) };
            anyhow::ensure!(interface == c.interface, "message {} belongs to `{interface}`", c.original);
            let pending: Vec<DestinationId> = sqlx::query_scalar(
                "SELECT destination_id FROM deliveries
                 WHERE message_id = $1 AND received_at = $2 AND destination_id = ANY($3) AND status = 0
                 ORDER BY destination_id",
            )
            .bind(c.original)
            .bind(received_at)
            .bind(c.destinations)
            .fetch_all(&mut *tx)
            .await
            .context("reading the original's deliveries")?;
            if !pending.is_empty() {
                return Ok((Edited::StillPending(pending), None));
            }
            let (id, copy_received_at): (i64, DateTime<Utc>) = sqlx::query_as(
                "WITH m AS (
                     INSERT INTO messages (interface, partition_key, ordering_key, control_id, payload, status, origin_id, tier)
                     VALUES ($1, $2, $3, $4, $5, 0, $6, $7) RETURNING id, received_at
                 ), queued AS (
                     INSERT INTO deliveries (message_id, received_at, destination_id, partition_key, tier)
                     SELECT m.id, m.received_at, d, $2, $7 FROM m, unnest($8::int[]) d
                 )
                 SELECT id, received_at FROM m",
            )
            .bind(c.interface)
            .bind(c.partition_key)
            .bind(c.ordering_key)
            .bind(c.control_id)
            .bind(c.payload)
            .bind(origin)
            .bind(c.tier)
            .bind(c.destinations)
            .fetch_one(&mut *tx)
            .await
            .context("inserting the copy")?;
            let replaced: Vec<DestinationId> = sqlx::query_scalar(
                "WITH old AS (
                     SELECT destination_id, status, attempts, last_error, output FROM deliveries
                     WHERE message_id = $1 AND received_at = $2 AND destination_id = ANY($3)
                     ORDER BY destination_id FOR UPDATE
                 ), replaced AS (
                     UPDATE deliveries d SET status = 2, last_error = $4, updated_at = now()
                     FROM old
                     WHERE d.message_id = $1 AND d.received_at = $2 AND d.destination_id = old.destination_id
                       AND old.status = 3
                     RETURNING d.destination_id
                 ), recorded AS (
                     INSERT INTO message_actions (action, token_id, actor, reason, message_id, received_at, interface,
                                                  destination_id, previous_status, previous_attempts, previous_error,
                                                  previous_output, copy_id)
                     SELECT 'edit', $5, $6, $7, $1, $2, $8, c.id, old.status, old.attempts, old.last_error, old.output, $9
                     FROM unnest($3::int[]) AS c(id) LEFT JOIN old ON old.destination_id = c.id
                 )
                 SELECT destination_id FROM replaced ORDER BY 1",
            )
            .bind(c.original)
            .bind(received_at)
            .bind(c.destinations)
            .bind(format!("replaced by edited copy {id}"))
            .bind(by.token_id)
            .bind(by.name)
            .bind(by.reason)
            .bind(c.interface)
            .bind(id)
            .fetch_all(&mut *tx)
            .await
            .context("replacing the original's deliveries")?;
            if !replaced.is_empty() {
                // Its status follows from its deliveries again: settled below.
                sqlx::query(
                    "UPDATE messages SET status = 0, last_error = NULL, updated_at = now()
                     WHERE id = $1 AND received_at = $2 AND status <> 0",
                )
                .bind(c.original)
                .bind(received_at)
                .execute(&mut *tx)
                .await
                .context("updating the original")?;
            }
            tx.commit().await.context("committing the copy")?;
            let original = MessageRef { id: c.original, received_at };
            Ok((Edited::Copied { copy: MessageRef { id, received_at: copy_received_at }, replaced }, Some(original)))
        })
        .await?;
        // After the commit and outside `retrying` (running the transaction again would queue a
        // second copy).
        if let (Edited::Copied { replaced, .. }, Some(original)) = &edited
            && !replaced.is_empty()
        {
            self.settle(&[*original]).await?;
        }
        Ok(edited.0)
    }

    /// What was done to a message, and the edit that made it if it's a copy: oldest first.
    pub async fn actions(&self, message: i64) -> Result<Vec<Action>> {
        sqlx::query_as::<_, Action>(
            "SELECT a.id, a.at, a.action, a.actor, a.reason, a.message_id, t.name AS destination,
                    a.previous_status, a.previous_attempts, a.previous_error, a.copy_id
             FROM message_actions a JOIN destinations t ON t.id = a.destination_id
             WHERE a.message_id = $1 OR a.copy_id = $1 ORDER BY a.id",
        )
        .bind(message)
        .fetch_all(&self.pool)
        .await
        .context("reading message actions")
    }

    /// Quarantined deliveries of `destinations`, newest message first, a page at a time:
    /// `before` is the previous page's last `(message, destination)`. From the partial index of
    /// quarantined deliveries, so it costs what's quarantined, not what's kept.
    pub async fn quarantined(
        &self,
        destinations: &[DestinationId],
        before: Option<(i64, DestinationId)>,
        limit: i64,
    ) -> Result<Vec<Quarantined>> {
        sqlx::query_as::<_, Quarantined>(
            "SELECT d.message_id, d.received_at, m.interface, d.destination_id, d.attempts, d.last_error,
                    d.updated_at AS quarantined_at, m.ordering_key, m.control_id, m.size
             FROM (SELECT message_id, received_at, destination_id, attempts, last_error, updated_at FROM deliveries
                   WHERE status = 3 AND destination_id = ANY($1)
                     AND ($2::bigint IS NULL OR (message_id, destination_id) < ($2, $3))
                   ORDER BY message_id DESC, destination_id DESC LIMIT $4) d
             CROSS JOIN LATERAL (
                 SELECT interface, ordering_key, control_id, octet_length(payload) AS size FROM messages m
                 WHERE m.id = d.message_id AND m.received_at = d.received_at LIMIT 1
             ) m
             ORDER BY d.message_id DESC, d.destination_id DESC",
        )
        .bind(destinations)
        .bind(before.map(|b| b.0))
        .bind(before.map(|b| b.1))
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("listing quarantined messages")
    }
}
