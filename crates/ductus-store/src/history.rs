//! What `ductus history` reads from the database for one interface (0.7.3.3): the deployments that
//! changed it (or, with `all`, every one that ran it), the audit log's entries naming it, its
//! documentation's changes, and the files its versions named (for its git history). Each read is
//! a page, newest first, up to a time: the caller merges them into one timeline.

use crate::Store;
use crate::audit::AuditRow;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use std::collections::BTreeSet;

/// One page of one source: rows at or before `until`, newest first, at most `limit`.
#[derive(Debug, Clone, Copy)]
pub struct Page<'a> {
    pub interface: &'a str,
    pub until: Option<DateTime<Utc>>,
    pub limit: i64,
}

/// A deployment in an interface's history.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct DeploymentEntry {
    pub id: i32,
    pub at: DateTime<Utc>,
    pub environment: String,
    pub host: String,
    /// The binary's version.
    pub version: String,
    pub how: String,
    pub by_name: String,
    pub reason: Option<String>,
    pub git_commit: Option<String>,
    pub uncommitted: Option<Vec<String>>,
    /// It changed this interface (else it ran it as it was: `all` only).
    pub changed: bool,
    /// The version it ran before (None: new here, or unchanged).
    pub from_version: Option<Vec<u8>>,
    /// The version it ran from then (None: unchanged).
    pub to_version: Option<Vec<u8>>,
    /// `deployment_changes.warning`.
    pub warning: Option<String>,
}

/// A change of an interface's documentation (`docs_history`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocsEntry {
    pub id: i64,
    pub at: DateTime<Utc>,
    pub deployment_id: Option<i32>,
    /// Who made that deployment.
    pub by: Option<String>,
    /// JSON text; None: it left the project.
    pub docs: Option<String>,
    /// What it was before (JSON text); None: the first row (new, or from before 0.7.3.3).
    pub before: Option<String>,
}

const DEPLOYMENT: &str = "d.id, d.at, d.environment, d.host, d.version, d.how, d.by_name, d.reason, d.git_commit,
    d.uncommitted";

impl Store {
    /// The deployments that changed `page.interface`, newest first; with `all`, also those that
    /// ran it as it was (recorded since 0.7.3.3: before that, which interfaces a start ran unchanged
    /// wasn't kept).
    pub async fn history_deployments(&self, page: Page<'_>, all: bool) -> Result<Vec<DeploymentEntry>> {
        let changed = format!(
            "SELECT {DEPLOYMENT}, true AS changed, f.hash AS from_version, t.hash AS to_version, c.warning
             FROM deployment_changes c
             JOIN deployments d ON d.id = c.deployment_id
             JOIN code_versions t ON t.id = c.to_version
             LEFT JOIN code_versions f ON f.id = c.from_version
             WHERE c.interface = $1 AND ($2::timestamptz IS NULL OR d.at <= $2)
             ORDER BY c.deployment_id DESC LIMIT $3"
        );
        let mut rows: Vec<DeploymentEntry> = sqlx::query_as(sqlx::AssertSqlSafe(changed))
            .bind(page.interface)
            .bind(page.until)
            .bind(page.limit)
            .fetch_all(&self.pool)
            .await
            .context("reading the deployments that changed the interface")?;
        if all {
            let ran = format!(
                "SELECT {DEPLOYMENT}, false AS changed, NULL::bytea AS from_version, NULL::bytea AS to_version,
                        NULL::text AS warning
                 FROM deployments d
                 WHERE d.interface_set IN (SELECT id FROM interface_sets WHERE interfaces @> ARRAY[$1])
                   AND NOT EXISTS (SELECT 1 FROM deployment_changes c WHERE c.deployment_id = d.id AND c.interface = $1)
                   AND ($2::timestamptz IS NULL OR d.at <= $2)
                 ORDER BY d.id DESC LIMIT $3"
            );
            rows.extend(
                sqlx::query_as::<_, DeploymentEntry>(sqlx::AssertSqlSafe(ran))
                    .bind(page.interface)
                    .bind(page.until)
                    .bind(page.limit)
                    .fetch_all(&self.pool)
                    .await
                    .context("reading the deployments that ran the interface")?,
            );
            rows.sort_by_key(|d| std::cmp::Reverse((d.at, d.id)));
            rows.truncate(page.limit as usize);
        }
        Ok(rows)
    }

    /// The audit log's entries naming `page.interface`, newest first. Without `all`, only what
    /// someone (or the engine) changed and was allowed to: no payload reads, nothing refused.
    pub async fn history_audit(&self, page: Page<'_>, all: bool) -> Result<Vec<AuditRow>> {
        // Two statements, not one with a flag: the changes-only one must match the partial index
        // `audit_log_interface_changes` (migration 0028) as written, or an interface's payload reads
        // (often most of its rows) are read through to find a page of changes.
        let only = if all { "" } else { "AND action <> 'message.payload.read' AND outcome = 'ok'" };
        sqlx::query_as::<_, AuditRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {} FROM audit_log
             WHERE interface = $1 AND ($2::timestamptz IS NULL OR at <= $2) {only}
             ORDER BY at DESC, id DESC LIMIT $3",
            crate::audit::COLUMNS
        )))
        .bind(page.interface)
        .bind(page.until)
        .bind(page.limit)
        .fetch_all(&self.pool)
        .await
        .context("reading the audit log's entries for the interface")
    }

    /// The changes of `page.interface`'s documentation, newest first, each with what it was before.
    pub async fn history_docs(&self, page: Page<'_>) -> Result<Vec<DocsEntry>> {
        type Row = (i64, DateTime<Utc>, Option<i32>, Option<String>, Option<String>, Option<String>);
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT h.id, h.at, h.deployment_id, d.by_name, h.docs::text,
                    (SELECT p.docs::text FROM docs_history p
                     WHERE p.kind = h.kind AND p.name = h.name AND p.id < h.id ORDER BY p.id DESC LIMIT 1)
             FROM docs_history h
             LEFT JOIN deployments d ON d.id = h.deployment_id
             WHERE h.kind = 'interface' AND h.name = $1 AND ($2::timestamptz IS NULL OR h.at <= $2)
             ORDER BY h.id DESC LIMIT $3",
        )
        .bind(page.interface)
        .bind(page.until)
        .bind(page.limit)
        .fetch_all(&self.pool)
        .await
        .context("reading the interface's documentation history")?;
        Ok(rows
            .into_iter()
            .map(|(id, at, deployment_id, by, docs, before)| DocsEntry { id, at, deployment_id, by, docs, before })
            .collect())
    }

    /// Every path any of `interface`'s code versions named: the files whose git history is its.
    pub async fn version_paths(&self, interface: &str) -> Result<BTreeSet<String>> {
        let all: Vec<String> = sqlx::query_scalar("SELECT files::text FROM code_versions WHERE interface = $1")
            .bind(interface)
            .fetch_all(&self.pool)
            .await
            .context("reading the files the interface's versions named")?;
        Ok(all.iter().flat_map(|f| crate::deployments::paths(f)).collect())
    }
}
