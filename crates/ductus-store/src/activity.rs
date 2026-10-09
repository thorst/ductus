//! What monitoring keeps across restarts (migration 0012): when each interface last received
//! something and each destination last sent or failed, and pauses.

use crate::{DestinationId, Store};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};

/// An interface's activity as stored.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct InterfaceActivity {
    pub interface: String,
    pub watched_since: DateTime<Utc>,
    pub last_received: Option<DateTime<Utc>>,
}

/// A destination's activity as stored.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct DestinationActivity {
    pub destination_id: DestinationId,
    pub last_sent: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub last_error_at: Option<DateTime<Utc>>,
    #[sqlx(flatten)]
    pub pause: PauseRow,
}

/// A pause as stored: all `None` when not paused.
#[derive(Debug, Clone, Default, PartialEq, sqlx::FromRow)]
pub struct PauseRow {
    pub paused_at: Option<DateTime<Utc>>,
    pub paused_by: Option<String>,
    pub pause_reason: Option<String>,
    pub reason_by: Option<String>,
    pub reason_at: Option<DateTime<Utc>>,
}

/// Times to save: `(interface, last received)`.
pub type Received<'a> = (&'a str, DateTime<Utc>);

/// Times to save for a destination.
#[derive(Debug, Clone)]
pub struct Seen {
    pub destination: DestinationId,
    pub last_sent: Option<DateTime<Utc>>,
    pub last_error: Option<(DateTime<Utc>, String)>,
}

impl Store {
    /// The stored activity of these interfaces and destinations, creating their rows (an
    /// interface seen for the first time is watched from now).
    pub async fn activity(
        &self,
        interfaces: &[&str],
        destinations: &[DestinationId],
    ) -> Result<(Vec<InterfaceActivity>, Vec<DestinationActivity>)> {
        sqlx::query("INSERT INTO interface_activity (interface) SELECT unnest($1::text[]) ON CONFLICT DO NOTHING")
            .bind(interfaces)
            .execute(&self.pool)
            .await
            .context("registering interfaces for monitoring")?;
        sqlx::query(
            "INSERT INTO destination_activity (destination_id) SELECT unnest($1::int[]) ON CONFLICT DO NOTHING",
        )
        .bind(destinations)
        .execute(&self.pool)
        .await
        .context("registering destinations for monitoring")?;
        let interfaces = sqlx::query_as(
            "SELECT interface, watched_since, last_received FROM interface_activity
             WHERE interface = ANY($1) ORDER BY interface",
        )
        .bind(interfaces)
        .fetch_all(&self.pool)
        .await
        .context("reading interface activity")?;
        let destinations = sqlx::query_as(
            "SELECT destination_id, last_sent, last_error, last_error_at,
                    paused_at, paused_by, pause_reason, reason_by, reason_at
             FROM destination_activity WHERE destination_id = ANY($1) ORDER BY destination_id",
        )
        .bind(destinations)
        .fetch_all(&self.pool)
        .await
        .context("reading destination activity")?;
        Ok((interfaces, destinations))
    }

    /// Save the latest times (never moving one back: another engine may have saved a later one).
    pub async fn save_activity(&self, received: &[Received<'_>], seen: &[Seen]) -> Result<()> {
        if !received.is_empty() {
            let (names, times): (Vec<&str>, Vec<DateTime<Utc>>) = received.iter().copied().unzip();
            sqlx::query(
                "UPDATE interface_activity a SET last_received = greatest(a.last_received, u.at), updated_at = now()
                 FROM unnest($1::text[], $2::timestamptz[]) AS u(interface, at) WHERE a.interface = u.interface",
            )
            .bind(&names)
            .bind(&times)
            .execute(&self.pool)
            .await
            .context("saving when interfaces last received")?;
        }
        if !seen.is_empty() {
            let ids: Vec<DestinationId> = seen.iter().map(|s| s.destination).collect();
            let sent: Vec<Option<DateTime<Utc>>> = seen.iter().map(|s| s.last_sent).collect();
            let error_at: Vec<Option<DateTime<Utc>>> =
                seen.iter().map(|s| s.last_error.as_ref().map(|e| e.0)).collect();
            let errors: Vec<Option<&str>> = seen.iter().map(|s| s.last_error.as_ref().map(|e| e.1.as_str())).collect();
            sqlx::query(
                "UPDATE destination_activity a
                 SET last_sent = greatest(a.last_sent, u.sent),
                     last_error = CASE WHEN u.error_at > a.last_error_at OR a.last_error_at IS NULL
                                       THEN coalesce(u.error, a.last_error) ELSE a.last_error END,
                     last_error_at = greatest(a.last_error_at, u.error_at),
                     updated_at = now()
                 FROM unnest($1::int[], $2::timestamptz[], $3::timestamptz[], $4::text[]) AS u(id, sent, error_at, error)
                 WHERE a.destination_id = u.id",
            )
            .bind(&ids)
            .bind(&sent)
            .bind(&error_at)
            .bind(&errors)
            .execute(&self.pool)
            .await
            .context("saving when destinations last sent")?;
        }
        Ok(())
    }

    /// Pause a destination (by `by`, the API token's name), or add a reason to its pause. A pause
    /// that's already on keeps its time and who did it; a reason given replaces the one before.
    pub async fn pause(&self, destination: DestinationId, by: &str, reason: Option<&str>) -> Result<PauseRow> {
        sqlx::query_as(
            "UPDATE destination_activity
             SET paused_at = coalesce(paused_at, now()), paused_by = coalesce(paused_by, $2),
                 pause_reason = coalesce($3::text, pause_reason),
                 reason_by = CASE WHEN $3::text IS NULL THEN reason_by ELSE $2 END,
                 reason_at = CASE WHEN $3::text IS NULL THEN reason_at ELSE now() END,
                 updated_at = now()
             WHERE destination_id = $1
             RETURNING paused_at, paused_by, pause_reason, reason_by, reason_at",
        )
        .bind(destination)
        .bind(by)
        .bind(reason)
        .fetch_one(&self.pool)
        .await
        .context("pausing the destination")
    }

    /// End a destination's pause; returns the pause it ended (empty if it wasn't paused).
    pub async fn unpause(&self, destination: DestinationId) -> Result<PauseRow> {
        sqlx::query_as(
            "UPDATE destination_activity a
             SET paused_at = NULL, paused_by = NULL, pause_reason = NULL, reason_by = NULL, reason_at = NULL,
                 updated_at = now()
             FROM (SELECT * FROM destination_activity WHERE destination_id = $1 FOR UPDATE) old
             WHERE a.destination_id = old.destination_id
             RETURNING old.paused_at, old.paused_by, old.pause_reason, old.reason_by, old.reason_at",
        )
        .bind(destination)
        .fetch_one(&self.pool)
        .await
        .context("resuming the destination")
    }
}
