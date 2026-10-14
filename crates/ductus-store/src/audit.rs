//! The audit log: who did what, to what, from where, and how it ended (`audit_log`, migration
//! 0019). Rows about changes are written in the transaction of the change itself (`insert`), so
//! there is no change without its row; for actions that have no transaction (pausing a
//! destination, a drain) the row is written first, and the action refused if it can't be. The log
//! is kept `audit.keep` (six years unless the project says otherwise) and trimmed in batches.

use crate::Store;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use sqlx::PgConnection;
use std::net::IpAddr;
use std::time::Duration;

/// Who did it, and how they came.
#[derive(Debug, Clone, Copy)]
pub struct By<'a> {
    /// `user`, `token`, `system` or `anonymous`.
    pub kind: &'static str,
    /// `users.id` or `api_tokens.id`, by `kind`.
    pub id: Option<i32>,
    /// The person: the user, or the owner of a user's token.
    pub user_id: Option<i32>,
    /// A snapshot: names can be reused, and accounts removed.
    pub name: &'a str,
    /// `session` (the web UI), `token`, `cli` or `engine`.
    pub via: &'static str,
    pub ip: Option<IpAddr>,
}

impl<'a> By<'a> {
    pub fn user(id: i32, name: &'a str, via: &'static str, ip: Option<IpAddr>) -> Self {
        Self { kind: "user", id: Some(id), user_id: Some(id), name, via, ip }
    }

    pub fn token(id: i32, owner: Option<i32>, name: &'a str, via: &'static str, ip: Option<IpAddr>) -> Self {
        Self { kind: "token", id: Some(id), user_id: owner, name, via, ip }
    }

    /// Someone at the server's command line (`name` is their operating-system user).
    pub fn cli(name: &'a str) -> Self {
        Self { kind: "system", id: None, user_id: None, name, via: "cli", ip: None }
    }

    /// The engine itself.
    pub fn engine() -> By<'static> {
        By { kind: "system", id: None, user_id: None, name: "engine", via: "engine", ip: None }
    }

    /// Someone who tried a name nobody has (what they typed isn't kept: it may be a password).
    pub fn anonymous(ip: Option<IpAddr>) -> By<'static> {
        By { kind: "anonymous", id: None, user_id: None, name: "(unknown username)", via: "session", ip }
    }

    /// The API token used, when it was one.
    pub fn token_id(&self) -> Option<i32> {
        if self.kind == "token" { self.id } else { None }
    }
}

/// One thing that happened.
#[derive(Debug, Clone)]
pub struct Audit<'a> {
    pub by: By<'a>,
    /// `message.payload.read`, `user.create`, ... (see `docs/nuances.md`).
    pub action: &'a str,
    pub subject: Option<&'a str>,
    pub interface: Option<&'a str>,
    pub message_id: Option<i64>,
    /// `ok`, `denied` or `failed`.
    pub outcome: &'static str,
    pub detail: Option<serde_json::Value>,
}

impl<'a> Audit<'a> {
    pub fn new(by: By<'a>, action: &'a str) -> Self {
        Self { by, action, subject: None, interface: None, message_id: None, outcome: "ok", detail: None }
    }

    pub fn subject(mut self, subject: &'a str) -> Self {
        self.subject = Some(subject);
        self
    }

    pub fn interface(mut self, interface: &'a str) -> Self {
        self.interface = Some(interface);
        self
    }

    pub fn message(mut self, id: i64) -> Self {
        self.message_id = Some(id);
        self
    }

    pub fn failed(mut self) -> Self {
        self.outcome = "failed";
        self
    }

    pub fn detail(mut self, detail: serde_json::Value) -> Self {
        self.detail = Some(detail);
        self
    }
}

/// One row of the log.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AuditRow {
    pub id: i64,
    pub at: DateTime<Utc>,
    pub actor_kind: String,
    pub actor_id: Option<i32>,
    pub user_id: Option<i32>,
    pub actor_name: String,
    pub via: Option<String>,
    pub ip: Option<String>,
    pub action: String,
    pub subject: Option<String>,
    pub interface: Option<String>,
    pub message_id: Option<i64>,
    pub outcome: String,
    /// JSON text.
    pub detail: Option<String>,
}

/// Which rows to read.
#[derive(Debug, Clone, Default)]
pub struct AuditFilter<'a> {
    /// A user's or token's name (a user's tokens, listed as `name/token`, count as theirs).
    pub actor: Option<&'a str>,
    /// The person: their own and their tokens' rows.
    pub user_id: Option<i32>,
    /// An action (`message.dismiss`) or a group of them (`message`).
    pub action: Option<&'a str>,
    pub interface: Option<&'a str>,
    pub message_id: Option<i64>,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    /// Older than this id (newest first).
    pub before: Option<i64>,
    /// Newer than this id (oldest first: what an export follows).
    pub after: Option<i64>,
    pub limit: i64,
}

const COLUMNS: &str = "id, at, actor_kind, actor_id, user_id, actor_name, via, host(ip) AS ip, action, subject,
    interface, message_id, outcome, detail::text AS detail";

/// Write a row inside the caller's transaction.
pub(crate) async fn insert(conn: &mut PgConnection, a: &Audit<'_>) -> Result<i64> {
    sqlx::query_scalar(
        "INSERT INTO audit_log (actor_kind, actor_id, user_id, actor_name, via, ip, action, subject, interface,
                                message_id, outcome, detail)
         VALUES ($1, $2, $3, $4, $5, $6::text::inet, $7, $8, $9, $10, $11, $12::text::jsonb) RETURNING id",
    )
    .bind(a.by.kind)
    .bind(a.by.id)
    .bind(a.by.user_id)
    .bind(a.by.name)
    .bind(a.by.via)
    .bind(a.by.ip.map(|ip| ip.to_string()))
    .bind(a.action)
    .bind(a.subject)
    .bind(a.interface)
    .bind(a.message_id)
    .bind(a.outcome)
    .bind(a.detail.as_ref().map(|d| d.to_string()))
    .fetch_one(conn)
    .await
    .with_context(|| format!("writing the audit log ({})", a.action))
}

/// What a previous run of the engine on a host left behind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviousRun {
    /// No earlier start on record.
    First,
    /// The last start has a stop with its instance.
    Stopped(DateTime<Utc>),
    /// The last start has no stop: the engine was killed, crashed, or its host lost power.
    Unclean(DateTime<Utc>),
}

impl Store {
    /// Write a row of its own.
    pub async fn record(&self, a: &Audit<'_>) -> Result<i64> {
        let mut conn = self.pool.acquire().await.context("writing the audit log")?;
        insert(&mut conn, a).await
    }

    /// Log a payload read, unless the same actor read the same message within `window`: a screen
    /// that reloads itself would otherwise write a row every time. Returns whether a row was
    /// written; either way a row covering this read exists when it returns.
    pub async fn record_payload_read(&self, a: &Audit<'_>, window: Duration) -> Result<bool> {
        if window.is_zero() {
            self.record(a).await?;
            return Ok(true);
        }
        let written: Option<i64> = sqlx::query_scalar(
            "INSERT INTO audit_log (actor_kind, actor_id, user_id, actor_name, via, ip, action, subject, interface,
                                    message_id, outcome, detail)
             SELECT $1, $2, $3, $4, $5, $6::text::inet, $7, $8, $9, $10, $11, $12::text::jsonb
             WHERE NOT EXISTS (
                 SELECT 1 FROM audit_log
                 WHERE message_id = $10 AND action = $7 AND actor_kind = $1 AND actor_id IS NOT DISTINCT FROM $2
                   AND at > now() - make_interval(secs => $13))
             RETURNING id",
        )
        .bind(a.by.kind)
        .bind(a.by.id)
        .bind(a.by.user_id)
        .bind(a.by.name)
        .bind(a.by.via)
        .bind(a.by.ip.map(|ip| ip.to_string()))
        .bind(a.action)
        .bind(a.subject)
        .bind(a.interface)
        .bind(a.message_id)
        .bind(a.outcome)
        .bind(a.detail.as_ref().map(|d| d.to_string()))
        .bind(window.as_secs_f64())
        .fetch_optional(&self.pool)
        .await
        .context("writing the audit log (payload read)")?;
        Ok(written.is_some())
    }

    /// A page of the log: newest first (`before`), or oldest first from `after`.
    pub async fn audit_log(&self, f: &AuditFilter<'_>) -> Result<Vec<AuditRow>> {
        let order = if f.after.is_some() { "ASC" } else { "DESC" };
        let sql = format!(
            "SELECT {COLUMNS} FROM audit_log
             WHERE ($1::text IS NULL OR lower(actor_name) = lower($1)
                    OR left(lower(actor_name), length($1) + 1) = lower($1) || '/')
               AND ($2::int IS NULL OR user_id = $2)
               AND ($3::text IS NULL OR action = $3 OR left(action, length($3) + 1) = $3 || '.')
               AND ($4::text IS NULL OR interface = $4)
               AND ($5::bigint IS NULL OR message_id = $5)
               AND ($6::timestamptz IS NULL OR at >= $6) AND ($7::timestamptz IS NULL OR at < $7)
               AND ($8::bigint IS NULL OR id < $8) AND ($9::bigint IS NULL OR id > $9)
             ORDER BY id {order} LIMIT $10"
        );
        sqlx::query_as::<_, AuditRow>(sqlx::AssertSqlSafe(sql))
            .bind(f.actor)
            .bind(f.user_id)
            .bind(f.action)
            .bind(f.interface)
            .bind(f.message_id)
            .bind(f.from)
            .bind(f.to)
            .bind(f.before)
            .bind(f.after)
            .bind(f.limit)
            .fetch_all(&self.pool)
            .await
            .context("reading the audit log")
    }

    /// Delete what is older than `keep`: `message_actions` and `audit_log` rows, in batches, at
    /// most `max_batches` of each per call (an hourly job: the next call continues). Returns
    /// (actions, audit rows) deleted.
    pub async fn trim_audit(&self, keep: Duration, batch: i64, max_batches: u32) -> Result<(u64, u64)> {
        let mut deleted = [0u64; 2];
        for (n, table) in ["message_actions", "audit_log"].into_iter().enumerate() {
            let sql = format!(
                "DELETE FROM {table} WHERE id IN (
                     SELECT id FROM {table} WHERE at < now() - make_interval(secs => $1) ORDER BY id LIMIT $2)"
            );
            for _ in 0..max_batches {
                let done = sqlx::query(sqlx::AssertSqlSafe(sql.clone()))
                    .bind(keep.as_secs_f64())
                    .bind(batch)
                    .execute(&self.pool)
                    .await
                    .with_context(|| format!("trimming {table}"))?
                    .rows_affected();
                deleted[n] += done;
                if (done as i64) < batch {
                    break;
                }
            }
        }
        Ok((deleted[0], deleted[1]))
    }

    /// Clear `previous_output` (a copy of a payload) from actions older than their interface's
    /// message retention: `windows` has each interface's retention in days, `other_days` is for
    /// interfaces not in it (removed ones: the longest window). The rows stay. Returns how many
    /// were cleared.
    pub async fn clear_action_outputs(
        &self,
        windows: &[(String, i32)],
        other_days: i32,
        batch: i64,
        max_batches: u32,
    ) -> Result<u64> {
        let (interfaces, days): (Vec<&str>, Vec<i32>) = windows.iter().map(|(i, d)| (i.as_str(), *d)).unzip();
        let mut cleared = 0;
        for _ in 0..max_batches {
            let done = sqlx::query(
                "UPDATE message_actions SET previous_output = NULL
                 WHERE id IN (
                     SELECT a.id FROM message_actions a
                     WHERE a.previous_output IS NOT NULL
                       AND a.at < now() - make_interval(days => COALESCE(
                             (SELECT w.days FROM unnest($1::text[], $2::int[]) AS w(interface, days)
                              WHERE w.interface = a.interface), $3))
                     ORDER BY a.id LIMIT $4)",
            )
            .bind(&interfaces)
            .bind(&days)
            .bind(other_days)
            .bind(batch)
            .execute(&self.pool)
            .await
            .context("clearing copies of payloads from message actions")?
            .rows_affected();
            cleared += done;
            if (done as i64) < batch {
                break;
            }
        }
        Ok(cleared)
    }

    /// How the engine that ran last on `host` ended (its `engine.start` with no `engine.stop` of
    /// the same instance is a crash, a kill or a power cut).
    pub async fn previous_run(&self, host: &str) -> Result<PreviousRun> {
        let last: Option<(DateTime<Utc>, Option<String>)> = sqlx::query_as(
            "SELECT at, detail->>'instance' FROM audit_log
             WHERE action = 'engine.start' AND detail->>'host' = $1 ORDER BY id DESC LIMIT 1",
        )
        .bind(host)
        .fetch_optional(&self.pool)
        .await
        .context("reading the previous start")?;
        let Some((at, instance)) = last else { return Ok(PreviousRun::First) };
        let stopped: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM audit_log WHERE action = 'engine.stop' AND detail->>'instance' = $1
                              AND at >= $2)",
        )
        .bind(instance)
        .bind(at)
        .fetch_one(&self.pool)
        .await
        .context("reading the previous stop")?;
        Ok(if stopped { PreviousRun::Stopped(at) } else { PreviousRun::Unclean(at) })
    }
}
