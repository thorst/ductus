//! Retention: each interface keeps its messages for one of a few fixed windows (its tier), and
//! old messages go by dropping whole partitions.
//!
//! `messages` and `deliveries` are partitioned by tier, then by time (migration 0010). A message
//! and its deliveries always share a tier. The tier is where a row lives; the interface's
//! current retention is when it goes: changing it moves the interface's rows into the new tier
//! (or deletes the ones past a shorter window), in small batches, in the background.
//!
//! A partition is never dropped while it holds pending or quarantined messages (a destination
//! stuck for a month keeps its backlog), or rows of an interface that now keeps them longer (a
//! change still moving them).

use super::Store;
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};

/// A retention tier: how many days messages are kept.
pub type Tier = i16;

/// The tiers, shortest first (`ductus_tiers()` in the database).
pub const TIERS: [Tier; 5] = [7, 30, 90, 180, 365];

/// The tier rows stored without one go to (older binaries; before migration 0010).
const LEGACY_TIER: Tier = 365;

/// An interface's retention as the engine applied it (the `retention` table).
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct RetentionState {
    pub interface: String,
    /// The tier its new messages go to, in days.
    pub days: Tier,
    /// Set while a change runs: the tier it's changing from.
    pub changing_from: Option<Tier>,
    /// Rows still to move or delete (an estimate).
    pub rows_left: Option<i64>,
    /// A configured value not applied yet: a change is running, or it's shorter and not confirmed.
    pub waiting: Option<Tier>,
    /// A shorter `waiting` value someone confirmed.
    pub confirmed: Option<Tier>,
    /// When the current value (or the running change) started.
    pub changed_at: DateTime<Utc>,
}

impl RetentionState {
    /// The waiting value is shorter (it would delete messages) and nobody has confirmed it.
    pub fn needs_confirmation(&self) -> bool {
        self.waiting.is_some_and(|w| w < self.days && self.confirmed != Some(w))
    }
}

/// What one batch of a retention change did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Step {
    /// Messages past the (new, shorter) window, deleted with their deliveries.
    pub deleted: u64,
    /// Messages moved into the interface's tier, with their deliveries.
    pub moved: u64,
}

impl Step {
    pub fn is_empty(&self) -> bool {
        self.deleted == 0 && self.moved == 0
    }
}

/// What `Store::drop_expired` did.
#[derive(Debug, Default)]
pub struct Expired {
    /// The `messages` partitions dropped (with their `deliveries` partitions).
    pub dropped: Vec<String>,
    /// Expired partitions kept, and why.
    pub held: Vec<Held>,
}

/// An expired partition that can't be dropped yet.
#[derive(Debug, Clone, PartialEq)]
pub struct Held {
    /// The `messages` partition, e.g. `messages_t30_20260901`.
    pub partition: String,
    pub tier: Tier,
    /// Messages still to be delivered: a destination that's down, or a stopped interface.
    pub pending: i64,
    /// Messages waiting for a person.
    pub quarantined: i64,
    /// Interfaces that keep their messages longer now, with rows still here (their change is
    /// moving them).
    pub kept_for: Vec<String>,
}

/// Is `days` a tier? (Config validation says the same to users; this guards the store.)
pub fn check_tier(days: Tier) -> Result<()> {
    if !TIERS.contains(&days) {
        bail!("retention must be one of {TIERS:?} days, not {days}");
    }
    Ok(())
}

const SELECT_STATE: &str =
    "SELECT interface, days, changing_from, rows_left, waiting, confirmed, changed_at FROM retention";

/// A partition's name as the migration makes them: `messages_t30_20261008`. Names are put into
/// SQL text (DDL can't take parameters), so anything else is refused.
fn leaf(name: &str) -> Result<&str> {
    let ok = ["messages_t", "deliveries_t"].iter().any(|prefix| {
        name.strip_prefix(prefix).and_then(|rest| rest.split_once('_')).is_some_and(|(tier, day)| {
            !tier.is_empty()
                && tier.bytes().all(|b| b.is_ascii_digit())
                && day.len() == 8
                && day.bytes().all(|b| b.is_ascii_digit())
        })
    });
    if !ok {
        bail!("not a partition name: {name}");
    }
    Ok(name)
}

/// `messages_t30_20261008` → `messages_t30`.
fn parent_of(leaf: &str) -> &str {
    &leaf[..leaf.rfind('_').unwrap_or(leaf.len())]
}

/// The `deliveries` partition with the same tier and period as a `messages` one.
fn deliveries_of(messages_leaf: &str) -> String {
    format!("deliveries{}", &messages_leaf["messages".len()..])
}

impl Store {
    /// Apply an interface's configured retention (at startup, after a confirmation, and when a
    /// change finishes), one change at a time:
    /// - first time: its new messages go to that tier (rows already stored in another tier, by a
    ///   version without tiers, are moved by a change);
    /// - longer: the change starts now (new messages go to the new tier; the old ones move);
    /// - shorter: it deletes messages, so it waits until someone confirms it;
    /// - while a change runs, a different value waits for it (values in between are skipped).
    pub async fn apply_retention(&self, interface: &str, configured: Tier) -> Result<RetentionState> {
        check_tier(configured)?;
        let mut tx = self.pool.begin().await.context("starting transaction")?;
        let current: Option<RetentionState> =
            sqlx::query_as(sqlx::AssertSqlSafe(format!("{SELECT_STATE} WHERE interface = $1 FOR UPDATE")))
                .bind(interface)
                .fetch_optional(&mut *tx)
                .await
                .context("reading retention")?;
        let statement = match &current {
            None => {
                let elsewhere: bool =
                    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM messages WHERE interface = $1 AND tier <> $2)")
                        .bind(interface)
                        .bind(configured)
                        .fetch_one(&mut *tx)
                        .await
                        .context("looking for messages stored without a retention")?;
                sqlx::query(
                    "INSERT INTO retention (interface, days, changing_from) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
                )
                .bind(interface)
                .bind(configured)
                .bind(elsewhere.then_some(LEGACY_TIER))
            }
            Some(s) if s.changing_from.is_some() || (configured < s.days && s.confirmed != Some(configured)) => {
                let waiting = (configured != s.days).then_some(configured);
                sqlx::query(
                    "UPDATE retention SET waiting = $2, confirmed = CASE WHEN confirmed = $2 THEN confirmed END,
                         updated_at = now()
                     WHERE interface = $1",
                )
                .bind(interface)
                .bind(waiting)
            }
            Some(s) if configured == s.days => sqlx::query(
                "UPDATE retention SET waiting = NULL, confirmed = NULL, updated_at = now() WHERE interface = $1",
            )
            .bind(interface),
            Some(_) => sqlx::query(
                "UPDATE retention SET changing_from = days, days = $2, rows_left = NULL, waiting = NULL,
                     confirmed = NULL, changed_at = now(), updated_at = now()
                 WHERE interface = $1",
            )
            .bind(interface)
            .bind(configured),
        };
        statement.execute(&mut *tx).await.context("saving retention")?;
        let state = sqlx::query_as(sqlx::AssertSqlSafe(format!("{SELECT_STATE} WHERE interface = $1")))
            .bind(interface)
            .fetch_one(&mut *tx)
            .await
            .context("reading retention")?;
        tx.commit().await.context("saving retention")?;
        Ok(state)
    }

    /// Confirm an interface's waiting, shorter retention. `None` if nothing is waiting for a
    /// confirmation. The engine then applies it (`apply_retention`), or does once the running
    /// change finishes.
    pub async fn confirm_retention(&self, interface: &str) -> Result<Option<RetentionState>> {
        sqlx::query_as(
            "UPDATE retention SET confirmed = waiting, updated_at = now()
             WHERE interface = $1 AND waiting < days
             RETURNING interface, days, changing_from, rows_left, waiting, confirmed, changed_at",
        )
        .bind(interface)
        .fetch_optional(&self.pool)
        .await
        .context("confirming retention")
    }

    /// Every interface's retention.
    pub async fn retention(&self) -> Result<Vec<RetentionState>> {
        sqlx::query_as(sqlx::AssertSqlSafe(format!("{SELECT_STATE} ORDER BY interface")))
            .fetch_all(&self.pool)
            .await
            .context("reading retention")
    }

    /// How many of an interface's messages a retention of `days` would delete now (finished
    /// ones older than the window, in longer tiers).
    pub async fn retention_would_delete(&self, interface: &str, days: Tier) -> Result<i64> {
        sqlx::query_scalar(
            "SELECT count(*) FROM messages
             WHERE interface = $1 AND tier > $2 AND received_at < current_date - $2::integer AND status IN (1, 2, 4)",
        )
        .bind(interface)
        .bind(days)
        .fetch_one(&self.pool)
        .await
        .context("counting messages past the window")
    }

    /// Before a change's first batch: partitions of the new tier for its whole window (rows may
    /// move into any day of it), and how many rows there are to move or delete (`rows_left`).
    pub async fn start_retention_change(&self, interface: &str, days: Tier) -> Result<i64> {
        check_tier(days)?;
        let mut tx = self.pool.begin().await?;
        sqlx::query(super::PARTITION_LOCK_TIMEOUT).execute(&mut *tx).await?;
        sqlx::query(
            "SELECT ductus_create_tier_partition('messages', $1, d::date)
             FROM generate_series(current_date - $1, current_date, interval '1 day') d",
        )
        .bind(i32::from(days))
        .execute(&mut *tx)
        .await
        .with_context(|| format!("creating message partitions of the {days}d tier"))?;
        tx.commit().await?;
        let mut tx = self.pool.begin().await?;
        sqlx::query(super::PARTITION_LOCK_TIMEOUT).execute(&mut *tx).await?;
        sqlx::query("SELECT ductus_ensure_delivery_partitions($1)")
            .bind(i32::from(days))
            .execute(&mut *tx)
            .await
            .context("creating delivery partitions")?;
        tx.commit().await?;
        let rows: i64 = sqlx::query_scalar(
            "WITH counted AS (
                 SELECT count(*) AS n FROM messages
                 WHERE interface = $1 AND tier <> $2 AND (status IN (1, 2, 4) OR received_at >= current_date - $2::integer)
             )
             UPDATE retention SET rows_left = counted.n, updated_at = now() FROM counted
             WHERE interface = $1 RETURNING rows_left",
        )
        .bind(interface)
        .bind(days)
        .fetch_optional(&self.pool)
        .await
        .context("counting rows to move")?
        .unwrap_or(0);
        Ok(rows)
    }

    /// One batch (at most `limit` messages) of an interface's retention change to `days`: first
    /// delete finished messages past the window that sit in other tiers (a shorter retention);
    /// when there are none, move finished messages from other tiers into `days`. Each with its
    /// deliveries, in one statement.
    ///
    /// Pending messages are left alone (a worker is updating them; moving a row under it would
    /// fail its update) and moved once delivered. Quarantined ones are moved but never deleted.
    pub async fn retention_step(&self, interface: &str, days: Tier, limit: i64) -> Result<Step> {
        check_tier(days)?;
        let deleted = sqlx::query(
            "WITH victims AS (
                 SELECT id, received_at, tier FROM messages
                 WHERE interface = $1 AND tier <> $2 AND received_at < current_date - $2::integer AND status IN (1, 2, 4)
                 LIMIT $3
             ), deliveries_gone AS (
                 DELETE FROM deliveries d USING victims v
                 WHERE d.message_id = v.id AND d.received_at = v.received_at AND d.tier = v.tier
             )
             DELETE FROM messages m USING victims v
             WHERE m.id = v.id AND m.received_at = v.received_at AND m.tier = v.tier",
        )
        .bind(interface)
        .bind(days)
        .bind(limit)
        .execute(&self.pool)
        .await
        .context("deleting messages past the retention window")?
        .rows_affected();
        let moved = if deleted > 0 {
            0
        } else {
            sqlx::query(
                "WITH moving AS (
                     SELECT id, received_at, tier FROM messages
                     WHERE interface = $1 AND tier <> $2 AND received_at >= current_date - $2::integer AND status <> 0
                     LIMIT $3
                 ), deliveries_moved AS (
                     UPDATE deliveries d SET tier = $2 FROM moving v
                     WHERE d.message_id = v.id AND d.received_at = v.received_at AND d.tier = v.tier
                 )
                 UPDATE messages m SET tier = $2 FROM moving v
                 WHERE m.id = v.id AND m.received_at = v.received_at AND m.tier = v.tier",
            )
            .bind(interface)
            .bind(days)
            .bind(limit)
            .execute(&self.pool)
            .await
            .context("moving messages to their retention tier")?
            .rows_affected()
        };
        let step = Step { deleted, moved };
        if !step.is_empty() {
            sqlx::query(
                "UPDATE retention SET rows_left = greatest(rows_left - $2, 0), updated_at = now() WHERE interface = $1",
            )
            .bind(interface)
            .bind((deleted + moved) as i64)
            .execute(&self.pool)
            .await
            .context("saving retention progress")?;
        }
        Ok(step)
    }

    /// An interface's messages in other tiers that a change to `days` must still move once
    /// they're delivered (pending, within the window). The change isn't over until there are none.
    pub async fn retention_waiting_for_delivery(&self, interface: &str, days: Tier) -> Result<i64> {
        sqlx::query_scalar(
            "SELECT count(*) FROM messages
             WHERE interface = $1 AND tier <> $2 AND status = 0 AND received_at >= current_date - $2::integer",
        )
        .bind(interface)
        .bind(days)
        .fetch_one(&self.pool)
        .await
        .context("counting pending messages in other tiers")
    }

    /// The running change is done. Returns the interface's messages left in other tiers: pending
    /// or quarantined ones past the new window, which stay where they are (and go with that
    /// tier's partitions).
    pub async fn finish_retention_change(&self, interface: &str) -> Result<i64> {
        let left: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM messages m JOIN retention r ON r.interface = m.interface
             WHERE m.interface = $1 AND m.tier <> r.days",
        )
        .bind(interface)
        .fetch_one(&self.pool)
        .await
        .context("counting messages left in other tiers")?;
        sqlx::query(
            "UPDATE retention SET changing_from = NULL, rows_left = NULL, changed_at = now(), updated_at = now()
             WHERE interface = $1",
        )
        .bind(interface)
        .execute(&self.pool)
        .await
        .context("finishing the retention change")?;
        Ok(left)
    }

    /// Drop every partition past its tier's window (all of its period older than the tier's
    /// days), `deliveries` first, unless it must be kept (`Held`).
    ///
    /// Each partition is detached with `DETACH PARTITION CONCURRENTLY`, which never blocks
    /// inserts or reads of the table (it waits for the queries that started before it instead),
    /// then checked again (nothing can change it any more), then dropped. A plain `DROP` would
    /// lock the whole tier while it waits for a long report, and every insert would queue behind
    /// it. A run cut short (a crash mid-detach) is finished by the next one.
    pub async fn drop_expired(&self) -> Result<Expired> {
        let mut out = Expired::default();
        self.finish_interrupted_drops().await?;
        let expired: Vec<(String, Tier)> = sqlx::query_as(
            "SELECT c.relname::text, substring(t.relname from 11)::smallint
             FROM pg_inherits i JOIN pg_class c ON c.oid = i.inhrelid JOIN pg_class t ON t.oid = i.inhparent
             WHERE t.relname ~ '^messages_t[0-9]+$' AND c.relname ~ '^messages_t[0-9]+_[0-9]{8}$' AND NOT i.inhdetachpending
               AND (to_date(right(c.relname, 8), 'YYYYMMDD')
                    + ('1 ' || ductus_tier_unit(substring(t.relname from 11)::integer))::interval)::date
                   <= current_date - substring(t.relname from 11)::integer
             ORDER BY 1",
        )
        .fetch_all(&self.pool)
        .await
        .context("listing expired partitions")?;
        for (messages, tier) in expired {
            let keep: Vec<String> = sqlx::query_scalar("SELECT interface FROM retention WHERE days > $1")
                .bind(tier)
                .fetch_all(&self.pool)
                .await
                .context("reading retention")?;
            if let Some(held) = self.held(&messages, tier, &keep).await? {
                out.held.push(held);
                continue;
            }
            let deliveries = deliveries_of(&messages);
            if self.is_partition(&deliveries).await? && !self.detach_and_drop(&deliveries).await? {
                continue;
            }
            if self.detach_and_drop(&messages).await? {
                out.dropped.push(messages);
            }
        }
        // `deliveries` partitions whose `messages` partition is gone (a run stopped between them).
        let orphans: Vec<String> = sqlx::query_scalar(
            "SELECT c.relname::text
             FROM pg_inherits i JOIN pg_class c ON c.oid = i.inhrelid JOIN pg_class t ON t.oid = i.inhparent
             WHERE t.relname ~ '^deliveries_t[0-9]+$' AND c.relname ~ '^deliveries_t[0-9]+_[0-9]{8}$' AND NOT i.inhdetachpending
               AND to_regclass('messages' || substr(c.relname, 11)) IS NULL
               AND (to_date(right(c.relname, 8), 'YYYYMMDD')
                    + ('1 ' || ductus_tier_unit(substring(t.relname from 13)::integer))::interval)::date
                   <= current_date - substring(t.relname from 13)::integer",
        )
        .fetch_all(&self.pool)
        .await
        .context("listing expired delivery partitions")?;
        for deliveries in orphans {
            self.detach_and_drop(&deliveries).await?;
        }
        Ok(out)
    }

    /// Why an expired `messages` partition (and its `deliveries` one) must stay, if it must.
    async fn held(&self, messages: &str, tier: Tier, keep: &[String]) -> Result<Option<Held>> {
        let (pending, quarantined) = self.unfinished(messages).await?;
        let deliveries = deliveries_of(messages);
        let (dp, dq) = if self.is_partition(&deliveries).await? { self.unfinished(&deliveries).await? } else { (0, 0) };
        let kept_for: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT k FROM unnest($1::text[]) k WHERE EXISTS (SELECT 1 FROM {} WHERE interface = k) ORDER BY k",
            leaf(messages)?
        )))
        .bind(keep)
        .fetch_all(&self.pool)
        .await
        .with_context(|| format!("checking {messages} for interfaces that keep messages longer"))?;
        let (pending, quarantined) = (pending.max(dp), quarantined.max(dq));
        if pending == 0 && quarantined == 0 && kept_for.is_empty() {
            return Ok(None);
        }
        Ok(Some(Held { partition: messages.to_string(), tier, pending, quarantined, kept_for }))
    }

    /// Pending and quarantined rows in a partition (`messages` or `deliveries`).
    async fn unfinished(&self, table: &str) -> Result<(i64, i64)> {
        sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT count(*) FILTER (WHERE status = 0), count(*) FILTER (WHERE status = 3) FROM {}
             WHERE status IN (0, 3)",
            leaf(table)?
        )))
        .fetch_one(&self.pool)
        .await
        .with_context(|| format!("counting unfinished messages in {table}"))
    }

    async fn is_partition(&self, name: &str) -> Result<bool> {
        sqlx::query_scalar("SELECT coalesce((SELECT relispartition FROM pg_class WHERE oid = to_regclass($1)), false)")
            .bind(name)
            .fetch_one(&self.pool)
            .await
            .context("reading the catalog")
    }

    /// Detach a partition without blocking its table, then drop it (see `drop_expired`).
    /// False if it was put back instead.
    async fn detach_and_drop(&self, name: &str) -> Result<bool> {
        let name = leaf(name)?;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "ALTER TABLE {} DETACH PARTITION {name} CONCURRENTLY",
            parent_of(name)
        )))
        .execute(&self.pool)
        .await
        .with_context(|| format!("detaching {name}"))?;
        self.drop_detached(name).await
    }

    /// Drop a detached partition, after checking it again: a message in it could have become
    /// pending since it was checked (then it's attached again, with its old bounds).
    async fn drop_detached(&self, name: &str) -> Result<bool> {
        let name = leaf(name)?;
        let (pending, quarantined) = self.unfinished(name).await?;
        if pending + quarantined > 0 {
            let tier: i32 = parent_of(name).rsplit('t').next().unwrap_or_default().parse()?;
            let bounds: String = sqlx::query_scalar(
                "SELECT format('FOR VALUES FROM (%L) TO (%L)', d::timestamp AT TIME ZONE 'UTC',
                               (d + ('1 ' || ductus_tier_unit($2))::interval) AT TIME ZONE 'UTC')
                 FROM to_date(right($1, 8), 'YYYYMMDD') d",
            )
            .bind(name)
            .bind(tier)
            .fetch_one(&self.pool)
            .await?;
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "ALTER TABLE {} ATTACH PARTITION {name} {bounds}",
                parent_of(name)
            )))
            .execute(&self.pool)
            .await
            .with_context(|| format!("attaching {name} again"))?;
            tracing::warn!(
                partition = name,
                pending,
                quarantined,
                "expired partition got unfinished messages while it was being dropped: kept"
            );
            return Ok(false);
        }
        sqlx::query(sqlx::AssertSqlSafe(format!("DROP TABLE {name}")))
            .execute(&self.pool)
            .await
            .with_context(|| format!("dropping {name}"))?;
        Ok(true)
    }

    /// Finish what an interrupted run left: detaches still pending (a detach cut off while it
    /// waited), and partitions detached but not dropped.
    async fn finish_interrupted_drops(&self) -> Result<()> {
        let pending: Vec<(String, String)> = sqlx::query_as(
            "SELECT t.relname::text, c.relname::text
             FROM pg_inherits i JOIN pg_class c ON c.oid = i.inhrelid JOIN pg_class t ON t.oid = i.inhparent
             WHERE i.inhdetachpending AND t.relname ~ '^(messages|deliveries)_t[0-9]+$'",
        )
        .fetch_all(&self.pool)
        .await
        .context("looking for unfinished detaches")?;
        for (parent, child) in &pending {
            let child = leaf(child)?;
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "ALTER TABLE {} DETACH PARTITION {child} FINALIZE",
                parent_of(child)
            )))
            .execute(&self.pool)
            .await
            .with_context(|| format!("finishing the detach of {child} from {parent}"))?;
        }
        // Deliveries first, as always.
        let detached: Vec<String> = sqlx::query_scalar(
            "SELECT relname::text FROM pg_class
             WHERE relkind = 'r' AND NOT relispartition AND relnamespace = current_schema()::regnamespace
               AND relname ~ '^(messages|deliveries)_t[0-9]+_[0-9]{8}$'
             ORDER BY relname LIKE 'messages%', relname",
        )
        .fetch_all(&self.pool)
        .await
        .context("looking for detached partitions")?;
        for name in detached {
            self.drop_detached(&name).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_partition_names_go_into_sql() {
        assert!(leaf("messages_t30_20261008").is_ok());
        assert!(leaf("deliveries_t365_20261001").is_ok());
        for bad in ["messages", "messages_t30", "messages_t_20261008", "messages_t30_2026100", "x; DROP TABLE y"] {
            assert!(leaf(bad).is_err(), "{bad}");
        }
        assert_eq!(parent_of("messages_t30_20261008"), "messages_t30");
        assert_eq!(deliveries_of("messages_t30_20261008"), "deliveries_t30_20261008");
    }

    #[test]
    fn a_shorter_waiting_value_needs_a_confirmation() {
        let state = |waiting, confirmed| RetentionState {
            interface: "adt".into(),
            days: 90,
            changing_from: None,
            rows_left: None,
            waiting,
            confirmed,
            changed_at: Utc::now(),
        };
        assert!(state(Some(7), None).needs_confirmation());
        assert!(!state(Some(7), Some(7)).needs_confirmation());
        assert!(state(Some(7), Some(30)).needs_confirmation());
        assert!(!state(Some(365), None).needs_confirmation(), "longer keeps everything");
        assert!(!state(None, None).needs_confirmation());
    }
}
