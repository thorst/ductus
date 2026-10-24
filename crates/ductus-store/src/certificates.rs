//! Certificates and connection checks (migration 0021): what each TLS server the engine connects
//! to presented at its last check (`certificate_status`), and the reports of the checks themselves
//! (`doctor_runs`, the last 30). Our own certificates (migration 0024: `side = 'ours'`) are rows
//! too, written from what the engine loaded.

use crate::Store;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};

/// Reports kept in `doctor_runs`.
pub const DOCTOR_RUNS_KEPT: i64 = 30;

/// A server's certificate as last seen. Without `fingerprint` the check read no certificate
/// (`error` says why); saving such a row keeps the dates from the check before. `read_at` is set
/// by the store (ignored when saving).
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct CertificateRow {
    /// `remote` (a server the engine connects to) or `ours` (a pair the engine presents).
    pub side: String,
    pub resource: String,
    pub host: String,
    pub port: i32,
    pub subject: Option<String>,
    pub issuer: Option<String>,
    pub fingerprint: Option<String>,
    pub not_before: Option<DateTime<Utc>>,
    pub not_after: Option<DateTime<Utc>>,
    pub chain_min_not_after: Option<DateTime<Utc>>,
    pub chain_min_subject: Option<String>,
    pub trusted: Option<bool>,
    pub error: Option<String>,
    pub checked_at: DateTime<Utc>,
    /// When the certificate in this row was read: `checked_at`, unless that check couldn't read one.
    pub read_at: Option<DateTime<Utc>>,
}

/// A connection check, without its report.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct DoctorRunRow {
    pub id: i64,
    pub at: DateTime<Utc>,
    pub mode: String,
    pub started_by: String,
    pub seconds: f64,
    pub checks: i32,
    pub failed: i32,
}

impl Store {
    /// Save what a check saw, one row per server (resource, host, port).
    pub async fn save_certificates(&self, rows: &[CertificateRow]) -> Result<()> {
        let mut tx = self.pool.begin().await.context("saving certificates")?;
        for r in rows {
            sqlx::query(
                "INSERT INTO certificate_status AS c (resource, host, port, subject, issuer, fingerprint, not_before,
                     not_after, chain_min_not_after, chain_min_subject, trusted, error, checked_at, read_at, side)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13,
                         CASE WHEN $6::text IS NULL THEN NULL ELSE $13 END, $14)
                 ON CONFLICT (resource, host, port) DO UPDATE SET
                     side = EXCLUDED.side,
                     subject = CASE WHEN EXCLUDED.fingerprint IS NULL THEN c.subject ELSE EXCLUDED.subject END,
                     issuer = CASE WHEN EXCLUDED.fingerprint IS NULL THEN c.issuer ELSE EXCLUDED.issuer END,
                     fingerprint = COALESCE(EXCLUDED.fingerprint, c.fingerprint),
                     not_before = CASE WHEN EXCLUDED.fingerprint IS NULL THEN c.not_before ELSE EXCLUDED.not_before END,
                     not_after = CASE WHEN EXCLUDED.fingerprint IS NULL THEN c.not_after ELSE EXCLUDED.not_after END,
                     chain_min_not_after = CASE WHEN EXCLUDED.fingerprint IS NULL THEN c.chain_min_not_after
                                                ELSE EXCLUDED.chain_min_not_after END,
                     chain_min_subject = CASE WHEN EXCLUDED.fingerprint IS NULL THEN c.chain_min_subject
                                              ELSE EXCLUDED.chain_min_subject END,
                     trusted = CASE WHEN EXCLUDED.fingerprint IS NULL THEN c.trusted ELSE EXCLUDED.trusted END,
                     error = EXCLUDED.error,
                     checked_at = EXCLUDED.checked_at,
                     read_at = COALESCE(EXCLUDED.read_at, c.read_at)",
            )
            .bind(&r.resource)
            .bind(&r.host)
            .bind(r.port)
            .bind(&r.subject)
            .bind(&r.issuer)
            .bind(&r.fingerprint)
            .bind(r.not_before)
            .bind(r.not_after)
            .bind(r.chain_min_not_after)
            .bind(&r.chain_min_subject)
            .bind(r.trusted)
            .bind(&r.error)
            .bind(r.checked_at)
            .bind(&r.side)
            .execute(&mut *tx)
            .await
            .with_context(|| format!("saving the certificate of {}:{}", r.host, r.port))?;
        }
        tx.commit().await.context("saving certificates")?;
        Ok(())
    }

    /// Every certificate seen, earliest expiry first (never read ones last).
    pub async fn certificates(&self) -> Result<Vec<CertificateRow>> {
        sqlx::query_as(
            "SELECT side, resource, host, port, subject, issuer, fingerprint, not_before, not_after, chain_min_not_after,
                    chain_min_subject, trusted, error, checked_at, read_at
             FROM certificate_status ORDER BY chain_min_not_after NULLS LAST, resource, host, port",
        )
        .fetch_all(&self.pool)
        .await
        .context("reading certificates")
    }

    /// Keep a check's report (JSON), and only the last `DOCTOR_RUNS_KEPT`.
    pub async fn record_doctor_run(&self, run: &DoctorRunRow, report: &serde_json::Value) -> Result<i64> {
        let mut tx = self.pool.begin().await.context("recording a connection check")?;
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO doctor_runs (at, mode, started_by, seconds, checks, failed, report)
             VALUES ($1, $2, $3, $4, $5, $6, $7::text::jsonb) RETURNING id",
        )
        .bind(run.at)
        .bind(&run.mode)
        .bind(&run.started_by)
        .bind(run.seconds)
        .bind(run.checks)
        .bind(run.failed)
        .bind(report.to_string())
        .fetch_one(&mut *tx)
        .await
        .context("recording a connection check")?;
        sqlx::query(
            "DELETE FROM doctor_runs WHERE id < (SELECT min(id) FROM (
                 SELECT id FROM doctor_runs ORDER BY id DESC LIMIT $1) kept)",
        )
        .bind(DOCTOR_RUNS_KEPT)
        .execute(&mut *tx)
        .await
        .context("trimming connection checks")?;
        tx.commit().await.context("recording a connection check")?;
        Ok(id)
    }

    /// The latest checks, newest first: all of them, or only the scheduled ones.
    pub async fn doctor_runs(&self, scheduled_only: bool, limit: i64) -> Result<Vec<DoctorRunRow>> {
        sqlx::query_as(
            "SELECT id, at, mode, started_by, seconds, checks, failed FROM doctor_runs
             WHERE NOT $1 OR started_by = 'schedule' ORDER BY id DESC LIMIT $2",
        )
        .bind(scheduled_only)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("reading connection checks")
    }

    /// One check's report.
    pub async fn doctor_report(&self, id: i64) -> Result<Option<serde_json::Value>> {
        let text: Option<String> = sqlx::query_scalar("SELECT report::text FROM doctor_runs WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .context("reading a connection check")?;
        text.map(|t| serde_json::from_str(&t).context("reading a connection check's report")).transpose()
    }
}
