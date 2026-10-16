//! Alerts (migration 0020): each interface's and destination's alert (`alert_state`), what
//! happened to it (`alert_events`), and the outbox of notifications (`alert_deliveries`). A
//! notification is stored with its event, in the transaction that changes the alert, before
//! anything is sent; senders claim due rows with `FOR UPDATE SKIP LOCKED` and a lease, so two
//! engines on one database send each once (a crash after sending but before marking it sent sends
//! it again when the lease ends: at least once, like deliveries).

use crate::{Audit, Store, audit};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use std::time::Duration;

const ALERT_COLUMNS: &str = "id, key, interface, destination, state, severity, since, ok_since, last_notified_at,
    notify_count, summary, ack_by, ack_at, silenced_until, silenced_by, silence_reason, rev, updated_at, stage";

/// An alert as stored (`state` is `ok` when there's nothing wrong).
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct AlertRow {
    pub id: i32,
    pub key: String,
    pub interface: String,
    pub destination: Option<String>,
    pub state: String,
    pub severity: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub ok_since: Option<DateTime<Utc>>,
    pub last_notified_at: Option<DateTime<Utc>>,
    pub notify_count: i32,
    pub summary: Option<String>,
    pub ack_by: Option<String>,
    pub ack_at: Option<DateTime<Utc>>,
    pub silenced_until: Option<DateTime<Utc>>,
    pub silenced_by: Option<String>,
    pub silence_reason: Option<String>,
    pub rev: i64,
    pub updated_at: DateTime<Utc>,
    /// A certificate alert's step (the warn day it was last told about; 0: expired).
    pub stage: Option<i32>,
}

impl AlertRow {
    pub fn is_open(&self) -> bool {
        self.state != "ok"
    }

    pub fn is_silenced(&self, now: DateTime<Utc>) -> bool {
        self.silenced_until.is_some_and(|t| t > now)
    }
}

/// Something that happened to an alert.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AlertEventRow {
    pub id: i64,
    pub at: DateTime<Utc>,
    pub alert_id: i32,
    pub key: String,
    pub kind: String,
    pub state: Option<String>,
    pub severity: Option<String>,
    pub summary: Option<String>,
    pub by_name: Option<String>,
    /// Notifications for it: sent, still to send (waiting or retrying), failed for good.
    pub sent: i64,
    pub waiting: i64,
    pub failed: i64,
}

/// A change the engine decided on (`ductus_core::alerts::step`), applied only if nobody changed
/// the alert since it was read (`rev`).
#[derive(Debug, Clone)]
pub struct AlertChange<'a> {
    pub state: &'a str,
    pub severity: Option<&'a str>,
    pub since: Option<DateTime<Utc>>,
    pub ok_since: Option<DateTime<Utc>>,
    pub last_notified_at: Option<DateTime<Utc>>,
    pub summary: Option<&'a str>,
    /// A certificate alert's step (`None` for the others).
    pub stage: Option<i32>,
    /// It fired or recovered: start counting notifications again, end the acknowledgement.
    pub fresh: bool,
    /// It escalated: the acknowledgement ends.
    pub unacknowledge: bool,
    /// It recovered: notifications still waiting (for quiet hours to end) are no longer news.
    pub cancel_waiting: bool,
    /// The event it records, if any.
    pub event: Option<NewEvent<'a>>,
}

/// An event the engine records (`fired`, `escalated`, `reminded`, `recovered`), and where its
/// notifications go.
#[derive(Debug, Clone)]
pub struct NewEvent<'a> {
    pub kind: &'a str,
    /// What it is (a recovery: what it was).
    pub state: &'a str,
    pub severity: Option<&'a str>,
    pub since: Option<DateTime<Utc>>,
    /// (channel, due): due later when quiet hours hold it.
    pub deliveries: Vec<(String, DateTime<Utc>)>,
}

/// A notification to send, claimed by this engine until `lease` passes.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct OutboxItem {
    pub id: i64,
    pub channel: String,
    /// Including this one.
    pub attempts: i32,
    pub event_id: i64,
    pub kind: String,
    pub at: DateTime<Utc>,
    pub alert_id: i32,
    pub key: String,
    pub interface: String,
    pub destination: Option<String>,
    pub state: Option<String>,
    pub severity: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub summary: Option<String>,
}

/// The outbox: what's due, waiting and given up on, per channel.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct OutboxCount {
    pub channel: String,
    pub sent: i64,
    pub waiting: i64,
    pub failed: i64,
    pub last_error: Option<String>,
    pub last_sent: Option<DateTime<Utc>>,
}

impl Store {
    /// Make the alert of each (key, interface, destination) that has none yet.
    pub async fn ensure_alerts(&self, keys: &[(String, String, Option<String>)]) -> Result<()> {
        let (k, i, d): (Vec<&str>, Vec<&str>, Vec<Option<&str>>) =
            unzip3(keys.iter().map(|(k, i, d)| (k.as_str(), i.as_str(), d.as_deref())));
        sqlx::query(
            "INSERT INTO alert_state (key, interface, destination)
             SELECT * FROM unnest($1::text[], $2::text[], $3::text[]) ON CONFLICT (key) DO NOTHING",
        )
        .bind(&k)
        .bind(&i)
        .bind(&d)
        .execute(&self.pool)
        .await
        .context("making alerts")?;
        Ok(())
    }

    /// Every alert, open ones first (worst first), then by key.
    pub async fn alerts(&self) -> Result<Vec<AlertRow>> {
        let sql = format!(
            "SELECT {ALERT_COLUMNS} FROM alert_state
             ORDER BY state = 'ok', severity = 'critical' DESC, since, key"
        );
        sqlx::query_as(sqlx::AssertSqlSafe(sql)).fetch_all(&self.pool).await.context("reading alerts")
    }

    pub async fn alert(&self, id: i32) -> Result<Option<AlertRow>> {
        let sql = format!("SELECT {ALERT_COLUMNS} FROM alert_state WHERE id = $1");
        sqlx::query_as(sqlx::AssertSqlSafe(sql)).bind(id).fetch_optional(&self.pool).await.context("reading an alert")
    }

    /// Events, newest first: one alert's, or everyone's.
    pub async fn alert_events(&self, alert: Option<i32>, limit: i64) -> Result<Vec<AlertEventRow>> {
        sqlx::query_as(
            "SELECT e.id, e.at, e.alert_id, e.key, e.kind, e.state, e.severity, e.summary, e.by_name,
                    count(*) FILTER (WHERE d.status = 'sent') AS sent,
                    count(*) FILTER (WHERE d.status = 'pending') AS waiting,
                    count(*) FILTER (WHERE d.status = 'failed') AS failed
             FROM alert_events e LEFT JOIN alert_deliveries d ON d.event_id = e.id
             WHERE $1::int IS NULL OR e.alert_id = $1
             GROUP BY e.id ORDER BY e.id DESC LIMIT $2",
        )
        .bind(alert)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("reading alert events")
    }

    /// Apply a change if the alert is still at `rev`, with its event and the event's
    /// notifications, in one transaction. `false`: someone else changed it first (another
    /// engine, or a person acknowledging it); read it again.
    pub async fn change_alert(&self, id: i32, rev: i64, c: &AlertChange<'_>) -> Result<bool> {
        let mut tx = self.pool.begin().await.context("changing an alert")?;
        let changed: Option<String> = sqlx::query_scalar(
            "UPDATE alert_state SET state = $3, severity = $4, since = $5, ok_since = $6, last_notified_at = $7,
                    summary = COALESCE($8, summary), stage = $11,
                    notify_count = CASE WHEN $9 THEN 0 ELSE notify_count END,
                    ack_by = CASE WHEN $9 OR $10 THEN NULL ELSE ack_by END,
                    ack_at = CASE WHEN $9 OR $10 THEN NULL ELSE ack_at END,
                    rev = rev + 1, updated_at = now()
             WHERE id = $1 AND rev = $2 RETURNING key",
        )
        .bind(id)
        .bind(rev)
        .bind(c.state)
        .bind(c.severity)
        .bind(c.since)
        .bind(c.ok_since)
        .bind(c.last_notified_at)
        .bind(c.summary)
        .bind(c.fresh)
        .bind(c.unacknowledge)
        .bind(c.stage)
        .fetch_optional(&mut *tx)
        .await
        .context("changing an alert")?;
        let Some(key) = changed else { return Ok(false) };
        if c.cancel_waiting {
            sqlx::query(
                "UPDATE alert_deliveries SET status = 'cancelled'
                 WHERE status = 'pending' AND next_try_at > now() AND attempts = 0
                   AND event_id IN (SELECT id FROM alert_events WHERE alert_id = $1)",
            )
            .bind(id)
            .execute(&mut *tx)
            .await
            .context("cancelling waiting notifications")?;
        }
        if let Some(e) = &c.event {
            let event: i64 = sqlx::query_scalar(
                "INSERT INTO alert_events (alert_id, key, kind, state, severity, since, summary)
                 VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
            )
            .bind(id)
            .bind(&key)
            .bind(e.kind)
            .bind(e.state)
            .bind(e.severity)
            .bind(e.since)
            .bind(c.summary)
            .fetch_one(&mut *tx)
            .await
            .context("recording an alert event")?;
            let (channels, due): (Vec<&str>, Vec<DateTime<Utc>>) =
                e.deliveries.iter().map(|(ch, at)| (ch.as_str(), *at)).unzip();
            sqlx::query(
                "INSERT INTO alert_deliveries (event_id, channel, next_try_at)
                 SELECT $1, * FROM unnest($2::text[], $3::timestamptz[])",
            )
            .bind(event)
            .bind(&channels)
            .bind(&due)
            .execute(&mut *tx)
            .await
            .context("storing notifications")?;
        }
        tx.commit().await.context("changing an alert")?;
        Ok(true)
    }

    /// Whether anyone was told about the alert's current episode, or will be soon (a notification
    /// due now, still being tried): a recovery is news only to them.
    pub async fn alert_was_told(&self, id: i32) -> Result<bool> {
        sqlx::query_scalar(
            "SELECT notify_count > 0 OR EXISTS (
                 SELECT 1 FROM alert_deliveries d JOIN alert_events e ON e.id = d.event_id
                 WHERE e.alert_id = s.id AND e.at >= s.since AND d.status = 'pending'
                   AND (d.attempts > 0 OR d.next_try_at <= now()))
             FROM alert_state s WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .context("reading whether an alert was sent")
        .map(|told| told.unwrap_or(false))
    }

    /// Acknowledge an open alert: no reminders until it escalates or recovers. `None`: it isn't
    /// open (any more).
    pub async fn acknowledge_alert(&self, id: i32, by: &str, a: &Audit<'_>) -> Result<Option<AlertRow>> {
        let mut tx = self.pool.begin().await.context("acknowledging an alert")?;
        let sql = format!(
            "UPDATE alert_state SET ack_by = $2, ack_at = now(), rev = rev + 1, updated_at = now()
             WHERE id = $1 AND state <> 'ok' RETURNING {ALERT_COLUMNS}"
        );
        let row: Option<AlertRow> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
            .bind(id)
            .bind(by)
            .fetch_optional(&mut *tx)
            .await
            .context("acknowledging an alert")?;
        let Some(row) = row else { return Ok(None) };
        event(&mut tx, &row, "acknowledged", by, None).await?;
        audit::insert(&mut tx, &Audit { subject: Some(&row.key), interface: Some(&row.interface), ..a.clone() })
            .await?;
        tx.commit().await.context("acknowledging an alert")?;
        Ok(Some(row))
    }

    /// Silence an alert until `until` (open or not: a destination can be silenced for planned work
    /// before anything fires), or end a silence (`until` = `None`). `None`: no such alert.
    pub async fn silence_alert(
        &self,
        id: i32,
        until: Option<DateTime<Utc>>,
        by: &str,
        reason: Option<&str>,
        a: &Audit<'_>,
    ) -> Result<Option<AlertRow>> {
        let mut tx = self.pool.begin().await.context("silencing an alert")?;
        let sql = format!(
            "UPDATE alert_state SET silenced_until = $2, silenced_by = $3, silence_reason = $4, rev = rev + 1,
                    updated_at = now()
             WHERE id = $1 RETURNING {ALERT_COLUMNS}"
        );
        let row: Option<AlertRow> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
            .bind(id)
            .bind(until)
            .bind(until.map(|_| by))
            .bind(until.and(reason))
            .fetch_optional(&mut *tx)
            .await
            .context("silencing an alert")?;
        let Some(row) = row else { return Ok(None) };
        let kind = if until.is_some() { "silenced" } else { "unsilenced" };
        let detail = serde_json::json!({ "until": until.map(|t| t.to_rfc3339()), "reason": reason });
        event(&mut tx, &row, kind, by, Some(detail.clone())).await?;
        let audit_row =
            Audit { subject: Some(&row.key), interface: Some(&row.interface), detail: Some(detail), ..a.clone() };
        audit::insert(&mut tx, &audit_row).await?;
        tx.commit().await.context("silencing an alert")?;
        Ok(Some(row))
    }

    /// Claim up to `limit` due notifications for `lease` (an engine that dies with them leaves
    /// them to whoever claims next, after the lease).
    pub async fn claim_alert_deliveries(&self, limit: i64, lease: Duration) -> Result<Vec<OutboxItem>> {
        sqlx::query_as(
            "WITH due AS (
                 SELECT id FROM alert_deliveries WHERE status = 'pending' AND next_try_at <= now()
                 ORDER BY next_try_at, id LIMIT $1 FOR UPDATE SKIP LOCKED)
             UPDATE alert_deliveries d
             SET attempts = d.attempts + 1, next_try_at = now() + make_interval(secs => $2)
             FROM due, alert_events e, alert_state s
             WHERE d.id = due.id AND e.id = d.event_id AND s.id = e.alert_id
             RETURNING d.id, d.channel, d.attempts, e.id AS event_id, e.kind, e.at, s.id AS alert_id, s.key,
                       s.interface, s.destination, e.state, e.severity, e.since, e.summary",
        )
        .bind(limit)
        .bind(lease.as_secs_f64())
        .fetch_all(&self.pool)
        .await
        .context("claiming notifications")
    }

    /// A notification went out.
    pub async fn alert_delivery_sent(&self, id: i64) -> Result<()> {
        sqlx::query(
            "WITH sent AS (
                 UPDATE alert_deliveries SET status = 'sent', sent_at = now(), last_error = NULL
                 WHERE id = $1 AND status = 'pending' RETURNING event_id)
             UPDATE alert_state SET notify_count = notify_count + 1
             WHERE id = (SELECT e.alert_id FROM alert_events e JOIN sent ON e.id = sent.event_id)",
        )
        .bind(id)
        .execute(&self.pool)
        .await
        .context("marking a notification sent")?;
        Ok(())
    }

    /// A notification didn't go out: try again at `retry_at`, or give up (`None`).
    pub async fn alert_delivery_failed(&self, id: i64, error: &str, retry_at: Option<DateTime<Utc>>) -> Result<()> {
        sqlx::query(
            "UPDATE alert_deliveries
             SET last_error = $2, status = CASE WHEN $3::timestamptz IS NULL THEN 'failed' ELSE 'pending' END,
                 next_try_at = COALESCE($3, next_try_at)
             WHERE id = $1 AND status = 'pending'",
        )
        .bind(id)
        .bind(error)
        .bind(retry_at)
        .execute(&self.pool)
        .await
        .context("marking a notification failed")?;
        Ok(())
    }

    /// Per channel: notifications sent, waiting and given up on in the last `days`, the last error.
    pub async fn outbox_counts(&self, days: i32) -> Result<Vec<OutboxCount>> {
        sqlx::query_as(
            "SELECT d.channel,
                    count(*) FILTER (WHERE d.status = 'sent') AS sent,
                    count(*) FILTER (WHERE d.status = 'pending') AS waiting,
                    count(*) FILTER (WHERE d.status = 'failed') AS failed,
                    (array_agg(d.last_error ORDER BY d.id DESC) FILTER (WHERE d.last_error IS NOT NULL))[1] AS last_error,
                    max(d.sent_at) AS last_sent
             FROM alert_deliveries d JOIN alert_events e ON e.id = d.event_id
             WHERE e.at > now() - make_interval(days => $1)
             GROUP BY d.channel ORDER BY d.channel",
        )
        .bind(days)
        .fetch_all(&self.pool)
        .await
        .context("counting notifications")
    }

    /// Delete alert history older than `keep` (notifications with their events), in batches.
    pub async fn trim_alerts(&self, keep: Duration, batch: i64, max_batches: u32) -> Result<u64> {
        let mut deleted = 0;
        for _ in 0..max_batches {
            let done = sqlx::query(
                "DELETE FROM alert_events WHERE id IN (
                     SELECT id FROM alert_events WHERE at < now() - make_interval(secs => $1) ORDER BY id LIMIT $2)",
            )
            .bind(keep.as_secs_f64())
            .bind(batch)
            .execute(&self.pool)
            .await
            .context("trimming alert history")?
            .rows_affected();
            deleted += done;
            if (done as i64) < batch {
                break;
            }
        }
        Ok(deleted)
    }
}

async fn event(
    conn: &mut sqlx::PgConnection,
    row: &AlertRow,
    kind: &str,
    by: &str,
    detail: Option<serde_json::Value>,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO alert_events (alert_id, key, kind, state, severity, since, summary, by_name, detail)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::text::jsonb)",
    )
    .bind(row.id)
    .bind(&row.key)
    .bind(kind)
    .bind(&row.state)
    .bind(&row.severity)
    .bind(row.since)
    .bind(&row.summary)
    .bind(by)
    .bind(detail.map(|d| d.to_string()))
    .execute(conn)
    .await
    .with_context(|| format!("recording an alert event ({kind})"))?;
    Ok(())
}

fn unzip3<A, B, C>(items: impl Iterator<Item = (A, B, C)>) -> (Vec<A>, Vec<B>, Vec<C>) {
    let (mut a, mut b, mut c) = (Vec::new(), Vec::new(), Vec::new());
    for (x, y, z) in items {
        a.push(x);
        b.push(y);
        c.push(z);
    }
    (a, b, c)
}
