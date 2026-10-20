//! PostgreSQL storage: the message log and the work queues.
//!
//! A message is inserted (status pending) before the sender is ACKed, together with one
//! delivery row per destination of its interface. Each destination works through its own
//! pending deliveries (its queue) and marks them done; the message stays pending until every
//! destination is done. Anything pending after a crash is simply picked up again:
//! at-least-once delivery.

mod actions;
mod activity;
mod alerts;
mod api;
mod audit;
mod certificates;
mod directory;
mod docs;
mod engines;
mod retention;
mod users;

pub use actions::{Action, Actor, Copy, Dismissed, Edited, Quarantined, Resend, Resent};
pub use activity::{BaselineRow, DestinationActivity, InterfaceActivity, Oldest, PauseRow, Received, Seen};
pub use alerts::{AlertChange, AlertEventRow, AlertRow, NewEvent, OutboxCount, OutboxItem};
pub use api::{
    Browse, LaneRow, MessageRow, PERMISSIONS, Principal, QueueCount, Role, SESSION_PREFIX, SessionFor, SessionLimits,
    TOKEN_PREFIX, Token, clean_permissions,
};
pub use audit::{Audit, AuditFilter, AuditRow, By, PreviousRun};
pub use certificates::{CertificateRow, DOCTOR_RUNS_KEPT, DoctorRunRow};
pub use directory::{DirectoryPerson, DirectoryUpdate};
pub use docs::DocsRow;
pub use engines::{EngineLock, EngineRow, NewEngine, Owned, Stranded};
pub use retention::{Expired, HELD, HeldArea, HeldInterface, HeldMonth, Kept, RetentionState, Step, TIERS, Tier};
pub use users::{Candidate, Lockout, NewUser, SessionRow, User, UserChange, UserSettings};

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

    pub const ALL: [Status; 5] =
        [Status::Pending, Status::Sent, Status::Filtered, Status::Quarantined, Status::Duplicate];

    /// From its label (`pending`, `sent`, ...), ignoring case.
    pub fn parse(label: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.label().eq_ignore_ascii_case(label.trim()))
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
    /// The retention tier to store it in (the interface's retention, in days).
    pub tier: Tier,
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
    /// The retention tier to store them in.
    pub tier: Tier,
}

#[derive(Clone)]
pub struct BatchMessage<'a> {
    pub partition_key: i32,
    pub ordering_key: Option<&'a str>,
    pub control_id: Option<&'a str>,
    pub payload: &'a [u8],
    pub dedupe_key: Option<&'a str>,
    /// Routed from another interface: the id of the message there.
    pub origin: Option<i64>,
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
    /// Routed from another interface: the id of the message there.
    pub origin: Option<i64>,
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

    /// Connect without changing anything: no migrations, no partitions. For commands that only
    /// read (`ductus validate`, `ductus owners`), which may run with a newer binary than the
    /// engines on that database, before it is deployed.
    pub async fn connect_read_only(url: &str) -> Result<Self> {
        let pool = PgPoolOptions::new().max_connections(2).connect(url).await.context("connecting to PostgreSQL")?;
        Ok(Self { pool })
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
    /// - The last resort: when tomorrow's partitions are still missing (every try for days was
    ///   blocked), it waits for the locks however long it takes. New messages then wait behind
    ///   it for as long as the blocking query runs, which beats having no partition to store
    ///   them in at midnight.
    /// - Nothing to create: no statement takes a lock at all.
    pub async fn ensure_partitions(&self, days_ahead: i32) -> Result<()> {
        self.ensure_partitions_urgent_within(days_ahead, 1).await
    }

    /// `ensure_partitions`, waiting without limit when a missing day is `urgent_within` days
    /// ahead or nearer. Separate for tests, which can't drop tomorrow's partitions.
    #[doc(hidden)]
    pub async fn ensure_partitions_urgent_within(&self, days_ahead: i32, urgent_within: i32) -> Result<()> {
        let missing = self.first_missing_partition_day(days_ahead).await?;
        if missing > days_ahead {
            return Ok(());
        }
        let urgent = missing <= urgent_within;
        if urgent && missing > 0 {
            tracing::warn!(
                days_ahead = missing,
                "a partition needed soon is missing (earlier tries were blocked by a long-held lock): waiting for the lock; new messages wait until it's created"
            );
        }
        let lock = if urgent { "SET LOCAL lock_timeout = 0" } else { PARTITION_LOCK_TIMEOUT };
        // One tier per transaction: each locks only its own tier's table, briefly (holding two
        // at once can deadlock with queries, which lock them all: migration 0010).
        for tier in TIERS {
            let mut tx = self.pool.begin().await?;
            sqlx::query(lock).execute(&mut *tx).await?;
            sqlx::query("SELECT ductus_ensure_tier_partitions('messages', $1, $2)")
                .bind(i32::from(tier))
                .bind(days_ahead)
                .execute(&mut *tx)
                .await
                .with_context(|| format!("creating message partitions ({tier}d tier)"))?;
            tx.commit().await?;
        }
        for tier in TIERS {
            let mut tx = self.pool.begin().await?;
            sqlx::query(lock).execute(&mut *tx).await?;
            sqlx::query("SELECT ductus_ensure_delivery_partitions($1)")
                .bind(i32::from(tier))
                .execute(&mut *tx)
                .await
                .with_context(|| format!("creating delivery partitions ({tier}d tier)"))?;
            tx.commit().await?;
        }
        Ok(())
    }

    /// The first day from today (0) to `days_ahead` without its `messages` and `deliveries`
    /// partitions in every tier; `days_ahead + 1` if none is missing. Reads the catalog only.
    pub async fn first_missing_partition_day(&self, days_ahead: i32) -> Result<i32> {
        let day = sqlx::query_scalar(
            "SELECT coalesce(min(i), $1 + 1) FROM generate_series(0, $1) i, unnest(ductus_tiers()) t,
                 LATERAL (SELECT format('_t%s_%s', t, to_char(date_trunc(ductus_tier_unit(t), (current_date + i)::timestamp), 'YYYYMMDD')) AS leaf) l
             WHERE to_regclass('messages' || l.leaf) IS NULL OR to_regclass('deliveries' || l.leaf) IS NULL",
        )
        .bind(days_ahead)
        .fetch_one(&self.pool)
        .await
        .context("checking partitions")?;
        Ok(day)
    }

    pub async fn insert(&self, m: NewMessage<'_>) -> Result<MessageRef> {
        let query = sqlx::query_as::<_, (i64, DateTime<Utc>)>(INSERT_ONE)
            .bind(m.interface)
            .bind(m.partition_key)
            .bind(m.ordering_key)
            .bind(m.control_id)
            .bind(m.payload)
            .bind(Status::Pending as i16)
            .bind(m.destinations)
            .bind(m.tier);
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
            .bind(m.tier)
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
        let duplicate = insert_batch_in(&mut tx, batch).await?;
        tx.commit().await.context("committing messages")?;
        Ok(duplicate)
    }

    /// A destination that routes to another interface: store `done` (its statuses) and `routed`
    /// (the messages it sent, as the target interface's batch; each `origin` is the id of a
    /// message in `done`) in one transaction, so each routed message is queued in the target
    /// exactly once. Returns, per routed message, whether it was queued there: not if it was a
    /// duplicate (the target's dedupe), or if its delivery here wasn't pending any more (a retry
    /// of a call that committed: then it was routed the first time).
    ///
    /// The commit doesn't wait for the disk flush: nothing outside the database has been told
    /// anything, so if the database loses the commit, it loses all of it, and the deliveries
    /// are simply still pending and routed again. (Later durable commits flush it anyway.)
    pub async fn finish_routed(
        &self,
        destination: DestinationId,
        done: &[Done],
        routed: &Batch<'_>,
    ) -> Result<Vec<bool>> {
        let fresh = |finished: &std::collections::HashSet<i64>, m: &BatchMessage| {
            m.origin.is_some_and(|o| finished.contains(&o))
        };
        // The whole transaction runs again if a delivery moved under it (`retrying`).
        let (finished, duplicate) = retrying(|| async {
            let mut tx = self.pool.begin().await.context("starting transaction")?;
            sqlx::query("SET LOCAL synchronous_commit = off").execute(&mut *tx).await.context("relaxing commit")?;
            let finished: std::collections::HashSet<i64> =
                update_deliveries(&mut tx, destination, done).await?.into_iter().collect();
            let messages = routed.messages.iter().filter(|m| fresh(&finished, m)).cloned().collect();
            let duplicate = insert_batch_in(&mut tx, &Batch { messages, ..*routed }).await.context("routing")?;
            tx.commit().await.context("committing routed messages")?;
            anyhow::Ok((finished, duplicate))
        })
        .await?;
        let mut duplicate = duplicate.into_iter();
        let queued = routed.messages.iter().map(|m| fresh(&finished, m) && !duplicate.next().unwrap_or(true)).collect();
        let messages: Vec<MessageRef> = done.iter().map(|d| d.message).collect();
        self.settle_saved(&messages).await?;
        Ok(queued)
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
                .bind(m.tier)
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
    ///
    /// The queue rows come first, then each message is looked up by its key (`LATERAL ... LIMIT
    /// 1`, which Postgres can't turn into a join). As a join, Postgres priced looking a message up
    /// in ~100 partitions above scanning them, and hashed every message stored (it grows with
    /// the day's volume: one destination drained at 429/s instead of ~1,500/s).
    ///
    /// The deliveries come from `deliveries_queue` in message order (migration 0011), which stops
    /// after `limit` rows however many are waiting or were delivered before them.
    pub async fn pending(&self, destination: DestinationId, partitions: &[i32], limit: i64) -> Result<Vec<Pending>> {
        sqlx::query_as::<_, Pending>(QUEUE_READ)
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
        retrying(|| async {
            let mut connection = self.pool.acquire().await.context("connecting")?;
            update_deliveries(&mut connection, destination, done).await
        })
        .await?;
        let messages: Vec<MessageRef> = done.iter().map(|d| d.message).collect();
        self.settle_saved(&messages).await
    }

    /// Bring the overall status of `messages` up to date with their deliveries: still pending
    /// while any destination is; then quarantined if any destination quarantined it (someone
    /// must look), else sent if any destination got it, else filtered.
    ///
    /// Must run after the delivery updates are committed, as a statement of its own: then of
    /// two destinations finishing the same message at once, the later one always sees both done.
    /// (In one statement or transaction, each could see the other still pending, and the
    /// message would stay pending forever.)
    ///
    /// The pending messages are locked first, in id order, in a statement of their own; the
    /// deliveries are read after, by a second statement (a fresh snapshot). Both matter:
    /// - Several destinations settle overlapping sets of messages at once: left to the plan, the
    ///   lock order followed a hash join or a scan, and two settles could lock the same rows in
    ///   opposite orders (a deadlock, seen with routes, which settle the same messages within
    ///   milliseconds of each other).
    /// - A resend puts a finished delivery back to pending (`Store::resend`, holding the
    ///   message's lock). A statement reads as of when it started, so one that read the
    ///   deliveries and then waited for the lock would settle from the delivery as it was before
    ///   the resend: the message quarantined with a delivery pending again, and startup's
    ///   `reconcile` would then undo the resend. Reading after the lock sees it.
    ///
    /// A retention change (or the held area) can move a message another destination just
    /// finished to another tier while this statement has it in view. Postgres then can't
    /// re-check the row (as it does after an ordinary update) and fails the statement ("already
    /// moved to another partition", a serialization failure). Settling again is safe and finds
    /// that message done (`retrying`).
    async fn settle(&self, messages: &[MessageRef]) -> Result<()> {
        retrying(|| self.settle_once(messages)).await
    }

    /// `settle` for statuses a destination has just saved, trying until it works. They're
    /// committed: returning an error would make the worker save them again, and by then an
    /// operator may have resent one of those deliveries (pending again), which saving again
    /// would mark done without sending it. Until the database answers, the worker waits here
    /// instead of in its own retry loop.
    async fn settle_saved(&self, messages: &[MessageRef]) -> Result<()> {
        let mut wait = std::time::Duration::from_millis(100);
        loop {
            match self.settle(messages).await {
                Ok(()) => return Ok(()),
                Err(e) => {
                    tracing::warn!(error = %format!("{e:#}"), "statuses saved, settling their messages failed; trying again");
                    tokio::time::sleep(wait).await;
                    wait = (wait * 2).min(std::time::Duration::from_secs(5));
                }
            }
        }
    }

    async fn settle_once(&self, messages: &[MessageRef]) -> Result<()> {
        let ids: Vec<i64> = messages.iter().map(|m| m.id).collect();
        let times: Vec<DateTime<Utc>> = messages.iter().map(|m| m.received_at).collect();
        let Some((first, last)) = span(&times) else { return Ok(()) };
        let mut tx = self.pool.begin().await.context("starting transaction")?;
        sqlx::query(
            "SELECT 1 FROM messages
             WHERE (id, received_at) IN (SELECT * FROM unnest($1::bigint[], $2::timestamptz[]))
               AND received_at BETWEEN $3 AND $4 AND status = 0
             ORDER BY id FOR UPDATE",
        )
        .bind(&ids)
        .bind(&times)
        .bind(first)
        .bind(last)
        .execute(&mut *tx)
        .await
        .context("locking messages to settle")?;
        sqlx::query(
            "WITH done AS (
                 SELECT message_id, received_at, count(*) AS n,
                        CASE WHEN bool_or(status = 3) THEN 3 WHEN bool_or(status = 1) THEN 1 ELSE 2 END AS status
                 FROM deliveries
                 WHERE (message_id, received_at) IN (SELECT * FROM unnest($1::bigint[], $2::timestamptz[]))
                   AND received_at BETWEEN $3 AND $4
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
             WHERE m.id = done.message_id AND m.received_at = done.received_at AND m.status = 0
               AND m.received_at BETWEEN $3 AND $4",
        )
        .bind(&ids)
        .bind(&times)
        .bind(first)
        .bind(last)
        .execute(&mut *tx)
        .await
        .context("updating message status")?;
        tx.commit().await.context("committing message status")?;
        Ok(())
    }

    /// Record a failed attempt to deliver to a destination; returns its new attempt count.
    pub async fn record_failure(&self, m: MessageRef, destination: DestinationId, error: &str) -> Result<i32> {
        let (attempts,) = retrying(|| {
            sqlx::query_as::<_, (i32,)>(
                "UPDATE deliveries SET attempts = attempts + 1, last_error = $4, updated_at = now()
                 WHERE message_id = $1 AND received_at = $2 AND destination_id = $3 RETURNING attempts",
            )
            .bind(m.id)
            .bind(m.received_at)
            .bind(destination)
            .bind(error)
            .fetch_one(&self.pool)
        })
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
    ///   destination moves its backlog to the new name, through the step above).
    ///
    /// `ids`: the destinations' ids (`destination_ids`). Then `settle_finished`, which workers
    /// don't wait for.
    pub async fn reconcile(&self, destinations: &[(String, String)], ids: &[DestinationId]) -> Result<Reconciled> {
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
        // Only the missing ones are inserted: letting every pending message's insert run into its
        // existing row (ON CONFLICT) took 0.9 s with 300,000 waiting; looking first is cheaper.
        let added: Vec<(String, i64)> = sqlx::query_as(
            "WITH added AS (
                 INSERT INTO deliveries (message_id, received_at, destination_id, partition_key, tier)
                 SELECT m.id, m.received_at, c.id, m.partition_key, m.tier
                 FROM messages m JOIN unnest($1::text[], $2::int[]) AS c(interface, id) ON m.interface = c.interface
                 WHERE m.interface = ANY($1) AND m.status = 0
                   AND NOT EXISTS (SELECT 1 FROM deliveries d
                                   WHERE d.message_id = m.id AND d.received_at = m.received_at AND d.destination_id = c.id)
                 ON CONFLICT DO NOTHING
                 RETURNING destination_id
             )
             SELECT t.interface, count(*) FROM added JOIN destinations t ON t.id = added.destination_id
             GROUP BY 1 ORDER BY 1",
        )
        .bind(&interfaces)
        .bind(ids)
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
        .bind(ids)
        .fetch_all(&self.pool)
        .await
        .context("filtering deliveries to removed destinations")?;
        Ok(Reconciled { added, removed })
    }

    /// Which of `interfaces` have anything waiting: a pending message, or a pending delivery to
    /// any destination they ever had (removed ones too). The others have nothing for `reconcile`
    /// to change, and their workers nothing to read at start.
    ///
    /// Two loose index scans (the next distinct value after the last, from the pending indexes),
    /// so the cost is the number of interfaces with a backlog, not the backlog: ~20 ms for 1,001
    /// interfaces whether one has 300,000 waiting or none has any. Reading every queue at start
    /// instead kept a 32-connection pool busy for ~0.6 s at 1,001 interfaces.
    pub async fn with_waiting(&self, interfaces: &[String]) -> Result<std::collections::HashSet<String>> {
        let mut waiting: std::collections::HashSet<String> = sqlx::query_scalar(
            "WITH RECURSIVE w AS (
                 SELECT (SELECT min(interface) FROM messages WHERE status = 0 AND interface = ANY($1)) AS i
                 UNION ALL
                 SELECT (SELECT min(interface) FROM messages WHERE status = 0 AND interface = ANY($1) AND interface > w.i)
                 FROM w WHERE w.i IS NOT NULL)
             SELECT i FROM w WHERE i IS NOT NULL",
        )
        .bind(interfaces)
        .fetch_all(&self.pool)
        .await
        .context("finding interfaces with pending messages")?
        .into_iter()
        .collect();
        let destinations: Vec<(DestinationId, String)> =
            sqlx::query_as("SELECT id, interface FROM destinations WHERE interface = ANY($1)")
                .bind(interfaces)
                .fetch_all(&self.pool)
                .await
                .context("reading destinations")?;
        let ids: Vec<DestinationId> = destinations.iter().map(|(id, _)| *id).collect();
        let delivering: Vec<DestinationId> = sqlx::query_scalar(
            "WITH RECURSIVE w AS (
                 SELECT (SELECT min(destination_id) FROM deliveries WHERE status = 0 AND destination_id = ANY($1)) AS id
                 UNION ALL
                 SELECT (SELECT min(destination_id) FROM deliveries
                         WHERE status = 0 AND destination_id = ANY($1) AND destination_id > w.id)
                 FROM w WHERE w.id IS NOT NULL)
             SELECT id FROM w WHERE id IS NOT NULL",
        )
        .bind(&ids)
        .fetch_all(&self.pool)
        .await
        .context("finding destinations with pending deliveries")?;
        let by_id: std::collections::HashMap<DestinationId, String> = destinations.into_iter().collect();
        waiting.extend(delivering.into_iter().filter_map(|id| by_id.get(&id).cloned()));
        Ok(waiting)
    }

    /// Messages of `interfaces` still pending though none of their deliveries is (a crash
    /// between a delivery's update and the settle after it; `reconcile`'s changes) get their
    /// overall status. Returns how many. Workers don't read `messages.status`, so this can run
    /// after they start (it locks like any settle).
    pub async fn settle_finished(&self, interfaces: &[String]) -> Result<u64> {
        let done: Vec<(i64, DateTime<Utc>)> = sqlx::query_as(
            "SELECT id, received_at FROM messages m
             WHERE interface = ANY($1) AND status = 0 AND NOT EXISTS (
                 SELECT 1 FROM deliveries d
                 WHERE d.message_id = m.id AND d.received_at = m.received_at AND d.status = 0)",
        )
        .bind(interfaces)
        .fetch_all(&self.pool)
        .await
        .context("finding finished messages")?;
        let done: Vec<MessageRef> = done.into_iter().map(|(id, received_at)| MessageRef { id, received_at }).collect();
        self.settle(&done).await?;
        Ok(done.len() as u64)
    }

    /// The ids of destinations (pairs of interface and destination name), in the same order,
    /// registering the ones not seen before.
    pub async fn destination_ids(&self, destinations: &[(String, String)]) -> Result<Vec<DestinationId>> {
        let (interfaces, names): (Vec<&str>, Vec<&str>) =
            destinations.iter().map(|(i, d)| (i.as_str(), d.as_str())).unzip();
        let read = async || -> Result<Vec<DestinationId>> {
            sqlx::query_scalar(
                "SELECT t.id FROM unnest($1::text[], $2::text[]) WITH ORDINALITY AS c(interface, name, n)
                 JOIN destinations t ON t.interface = c.interface AND t.name = c.name ORDER BY c.n",
            )
            .bind(&interfaces)
            .bind(&names)
            .fetch_all(&self.pool)
            .await
            .context("reading destination ids")
        };
        // Usually all are known: inserting them anyway costs a conflict check each.
        let ids = read().await?;
        if ids.len() == destinations.len() {
            return Ok(ids);
        }
        sqlx::query(
            "INSERT INTO destinations (interface, name) SELECT * FROM unnest($1::text[], $2::text[])
             ON CONFLICT DO NOTHING",
        )
        .bind(&interfaces)
        .bind(&names)
        .execute(&self.pool)
        .await
        .context("registering destinations")?;
        read().await
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

/// The earliest and latest of some messages' `received_at`, for statements that find messages by
/// `(id, received_at)` pairs. With the range written out, Postgres leaves out the partitions
/// outside it when it plans; without it, it priced looking each message up in all ~100
/// partitions above scanning them, and settling 50 messages read every `messages` partition
/// (84 ms instead of 1.8 ms on 375k rows, and growing with the table).
fn span(times: &[DateTime<Utc>]) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    Some((*times.iter().min()?, *times.iter().max()?))
}

/// Run a statement (or transaction) again, up to 5 times, when Postgres failed it in a way that
/// running it again fixes: a serialization failure (40001) or a deadlock (40P01). Every
/// statement that updates messages or deliveries goes through it.
///
/// Rows move between partitions: retention changes move finished messages to another tier, and
/// expired partitions' unfinished messages move to the held area while their destinations'
/// workers are updating them. A statement that saw a row before it moved can't re-check it (as
/// it would after an ordinary update) and fails with 40001; run again, it finds the row where it
/// is now (nothing reads or writes by tier). Moving locks rows in id order, as every statement
/// that locks many does; a deadlock with a statement that doesn't is broken by Postgres, and the
/// loser runs again here.
pub(crate) async fn retrying<T, E, F, Fut>(mut statement: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = std::result::Result<T, E>>,
    E: Into<anyhow::Error>,
{
    let mut tries = 0;
    loop {
        match statement().await.map_err(Into::into) {
            Err(e) if tries < 5 && can_run_again(&e) => tries += 1,
            result => return result,
        }
    }
}

/// Postgres's serialization failure (40001) or deadlock (40P01): the transaction was rolled
/// back and can simply run again.
fn can_run_again(e: &anyhow::Error) -> bool {
    e.downcast_ref::<sqlx::Error>()
        .and_then(|e| e.as_database_error())
        .is_some_and(|e| matches!(e.code().as_deref(), Some("40001" | "40P01")))
}

/// `Store::pending`: destination `$1`'s oldest `$3` pending deliveries in lanes `$2`, with their
/// messages. Public so a test can check its plan.
#[doc(hidden)]
pub const QUEUE_READ: &str = "SELECT d.message_id AS id, d.received_at, m.payload, d.attempts, m.origin_id AS origin
     FROM (SELECT message_id, received_at, attempts FROM deliveries
           WHERE destination_id = $1 AND status = 0 AND partition_key = ANY($2)
           ORDER BY message_id LIMIT $3) d
     CROSS JOIN LATERAL (SELECT payload, origin_id FROM messages m
                         WHERE m.id = d.message_id AND m.received_at = d.received_at LIMIT 1) m
     ORDER BY d.message_id";

/// Insert one message (`$6` its status, `$8` its tier) and a pending delivery for each
/// destination in `$7` (none for a duplicate), in one statement.
const INSERT_ONE: &str = "
    WITH m AS (
        INSERT INTO messages (interface, partition_key, ordering_key, control_id, payload, status, tier)
        VALUES ($1, $2, $3, $4, $5, $6, $8) RETURNING id, received_at
    ), queued AS (
        INSERT INTO deliveries (message_id, received_at, destination_id, partition_key, tier)
        SELECT m.id, m.received_at, d, $2, $8 FROM m, unnest($7::int[]) d
    )
    SELECT id, received_at FROM m";

/// `Store::insert_batch` inside a transaction the caller commits.
async fn insert_batch_in(tx: &mut sqlx::PgConnection, batch: &Batch<'_>) -> Result<Vec<bool>> {
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
        // Sorted: several batches can claim overlapping keys at once (routes from several
        // interfaces into one), and must take them in the same order.
        let mut keys: Vec<&str> = first_seen.keys().copied().collect();
        keys.sort_unstable();
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
                 INSERT INTO messages (interface, partition_key, ordering_key, control_id, payload, status, origin_id, tier)
                 SELECT $1, p, o, c, b, s, r, $9
                 FROM unnest($2::int[], $3::text[], $4::text[], $5::bytea[], $6::smallint[], $8::bigint[])
                      WITH ORDINALITY AS t(p, o, c, b, s, r, n)
                 ORDER BY n
                 RETURNING id, received_at, partition_key, status
             )
             INSERT INTO deliveries (message_id, received_at, destination_id, partition_key, tier)
             SELECT m.id, m.received_at, d, m.partition_key, $9 FROM m, unnest($7::int[]) d WHERE m.status = 0",
        )
        .bind(batch.interface)
        .bind(m.iter().map(|m| m.partition_key).collect::<Vec<_>>())
        .bind(m.iter().map(|m| m.ordering_key).collect::<Vec<_>>())
        .bind(m.iter().map(|m| m.control_id).collect::<Vec<_>>())
        .bind(m.iter().map(|m| m.payload.to_vec()).collect::<Vec<_>>())
        .bind(statuses)
        .bind(batch.destinations)
        .bind(m.iter().map(|m| m.origin).collect::<Vec<_>>())
        .bind(batch.tier)
        .execute(&mut *tx)
        .await
        .context("inserting messages")?;
    }
    if let Some((key, value)) = batch.state {
        save_state(tx, batch.interface, key, value).await?;
    }
    Ok(duplicate)
}

/// Record a destination's statuses for messages (not their overall status: see `Store::settle`).
/// Only deliveries still pending change (a final status is never overwritten, so a retried call
/// changes nothing twice); returns the ids of their messages.
async fn update_deliveries(tx: &mut sqlx::PgConnection, destination: DestinationId, done: &[Done]) -> Result<Vec<i64>> {
    let ids: Vec<i64> = done.iter().map(|d| d.message.id).collect();
    let times: Vec<DateTime<Utc>> = done.iter().map(|d| d.message.received_at).collect();
    let statuses: Vec<i16> = done.iter().map(|d| d.status as i16).collect();
    let reasons: Vec<Option<&str>> = done.iter().map(|d| d.reason.as_deref()).collect();
    let outputs: Vec<Option<&[u8]>> = done.iter().map(|d| d.output.as_deref()).collect();
    let Some((first, last)) = span(&times) else { return Ok(Vec::new()) };
    sqlx::query_scalar(
        "UPDATE deliveries d
         SET status = u.status, last_error = COALESCE(u.reason, d.last_error), output = u.output, updated_at = now()
         FROM unnest($2::bigint[], $3::timestamptz[], $4::smallint[], $5::text[], $6::bytea[])
              AS u(id, received_at, status, reason, output)
         WHERE d.message_id = u.id AND d.received_at = u.received_at AND d.destination_id = $1 AND d.status = 0
           AND d.received_at BETWEEN $7 AND $8
         RETURNING d.message_id",
    )
    .bind(destination)
    .bind(&ids)
    .bind(&times)
    .bind(&statuses)
    .bind(&reasons)
    .bind(&outputs)
    .bind(first)
    .bind(last)
    .fetch_all(tx)
    .await
    .context("updating delivery statuses")
}

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
