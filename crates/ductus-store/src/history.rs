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
    /// Only rows whose id is below this one: the next batch of the same page (0.7.4.2: a page of
    /// one destination's history reads the interface's deployments until it has enough of its).
    pub below: Option<i64>,
}

/// One part of an interface (0.7.4.2): what `ductus history --source` and `--destination NAME` show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part<'a> {
    Source,
    Destination(&'a str),
}

/// An audit log row in an interface's history, with the destinations its message actions named.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AuditEntry {
    #[sqlx(flatten)]
    pub row: AuditRow,
    /// A resend, correction or dismissal's destinations (`message_actions`, by `audit_id`), in name
    /// order; empty for every other action, and for those recorded without their actions.
    pub acted_on: Vec<String>,
}

/// One code version of an interface: its files and which of them each part runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionFiles {
    /// `(path, SHA-256 in hex)`, in path order.
    pub files: Vec<(String, String)>,
    /// `code_versions.parts` as JSON text; None: saved before 0.7.4.2 and not run since.
    pub parts: Option<String>,
}

impl VersionFiles {
    fn of(files: &str, parts: Option<String>) -> Self {
        let files: serde_json::Value = serde_json::from_str(files).unwrap_or_default();
        let files = files
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|f| Some((f["path"].as_str()?.to_string(), f["hash"].as_str()?.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        Self { files, parts }
    }
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
    /// The version before: its files and parts (None: new here, or unchanged).
    #[sqlx(skip)]
    pub from: Option<VersionFiles>,
    /// The version from then (None: unchanged).
    #[sqlx(skip)]
    pub to: Option<VersionFiles>,
    /// The interface.yaml of the version before and of the one from then, when the two differ.
    #[sqlx(default)]
    pub from_yaml: Option<String>,
    #[sqlx(default)]
    pub to_yaml: Option<String>,
    #[sqlx(default)]
    from_files: Option<String>,
    #[sqlx(default)]
    from_parts: Option<String>,
    #[sqlx(default)]
    to_files: Option<String>,
    #[sqlx(default)]
    to_parts: Option<String>,
}

impl DeploymentEntry {
    /// Its versions' files and parts read from the row's text.
    fn read_versions(mut self) -> Self {
        self.from = self.from_files.take().map(|f| VersionFiles::of(&f, self.from_parts.take()));
        self.to = self.to_files.take().map(|f| VersionFiles::of(&f, self.to_parts.take()));
        self
    }
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

/// A subquery: the hash of the interface.yaml among a version's `files` (the column named).
fn yaml_hash(files: &str) -> String {
    format!(
        "SELECT decode(e->>'hash', 'hex') AS hash FROM jsonb_array_elements({files}) e WHERE e->>'path' LIKE '%/interface.yaml' LIMIT 1"
    )
}

impl Store {
    /// The deployments that changed `page.interface`, newest first; with `all`, also those that
    /// ran it as it was (recorded since 0.7.3.3: before that, which interfaces a start ran unchanged
    /// wasn't kept).
    ///
    /// Each change comes with both versions' files and parts (what history by destination compares).
    /// With `page.below`, only the changes of deployments below it, and no unchanged ones: the next
    /// batch of a page.
    pub async fn history_deployments(&self, page: Page<'_>, all: bool) -> Result<Vec<DeploymentEntry>> {
        // Each version's interface.yaml text comes with it when the two differ (what tells which
        // parts the change touched), in this statement: another round trip costs more than the
        // texts (a statement with a large parameter waits ~40 ms for a delayed ACK until sqlx
        // sets TCP_NODELAY).
        let changed = format!(
            "SELECT {DEPLOYMENT}, true AS changed, f.hash AS from_version, t.hash AS to_version, c.warning,
                    f.files::text AS from_files, f.parts::text AS from_parts,
                    t.files::text AS to_files, t.parts::text AS to_parts,
                    CASE WHEN fy.hash <> ty.hash THEN (SELECT content FROM code_files WHERE hash = fy.hash) END
                        AS from_yaml,
                    CASE WHEN fy.hash <> ty.hash THEN (SELECT content FROM code_files WHERE hash = ty.hash) END
                        AS to_yaml
             FROM deployment_changes c
             JOIN deployments d ON d.id = c.deployment_id
             JOIN code_versions t ON t.id = c.to_version
             LEFT JOIN code_versions f ON f.id = c.from_version
             LEFT JOIN LATERAL ({}) fy ON true
             LEFT JOIN LATERAL ({}) ty ON true
             WHERE c.interface = $1 AND ($2::timestamptz IS NULL OR d.at <= $2)
               AND ($4::integer IS NULL OR c.deployment_id < $4)
             ORDER BY c.deployment_id DESC LIMIT $3",
            yaml_hash("f.files"),
            yaml_hash("t.files"),
        );
        let mut rows: Vec<DeploymentEntry> = sqlx::query_as::<_, DeploymentEntry>(sqlx::AssertSqlSafe(changed))
            .bind(page.interface)
            .bind(page.until)
            .bind(page.limit)
            .bind(page.below.map(|b| b as i32))
            .fetch_all(&self.pool)
            .await
            .context("reading the deployments that changed the interface")?
            .into_iter()
            .map(DeploymentEntry::read_versions)
            .collect();
        if all && page.below.is_none() {
            rows.extend(self.history_runs(page).await?);
            rows.sort_by_key(|d| std::cmp::Reverse((d.at, d.id)));
            rows.truncate(page.limit as usize);
        }
        Ok(rows)
    }

    /// The deployments that ran `page.interface` without changing it, newest first (recorded since
    /// 0.7.3.3).
    pub async fn history_runs(&self, page: Page<'_>) -> Result<Vec<DeploymentEntry>> {
        let ran = format!(
            "SELECT {DEPLOYMENT}, false AS changed, NULL::bytea AS from_version, NULL::bytea AS to_version,
                    NULL::text AS warning
             FROM deployments d
             WHERE d.interface_set IN (SELECT id FROM interface_sets WHERE interfaces @> ARRAY[$1])
               AND NOT EXISTS (SELECT 1 FROM deployment_changes c WHERE c.deployment_id = d.id AND c.interface = $1)
               AND ($2::timestamptz IS NULL OR d.at <= $2)
             ORDER BY d.id DESC LIMIT $3"
        );
        sqlx::query_as::<_, DeploymentEntry>(sqlx::AssertSqlSafe(ran))
            .bind(page.interface)
            .bind(page.until)
            .bind(page.limit)
            .fetch_all(&self.pool)
            .await
            .context("reading the deployments that ran the interface")
    }

    /// The audit log's entries naming `page.interface`, newest first. Without `all`, only what
    /// someone (or the engine) changed and was allowed to: no payload reads, nothing refused.
    pub async fn history_audit(&self, page: Page<'_>, all: bool, part: Option<Part<'_>>) -> Result<Vec<AuditEntry>> {
        // Two statements, not one with a flag: the changes-only one must match the partial index
        // `audit_log_interface_changes` (migration 0028) as written, or an interface's payload reads
        // (often most of its rows) are read through to find a page of changes.
        let only = if all { "" } else { "AND action <> 'message.payload.read' AND outcome = 'ok'" };
        // A part's entries (0.7.4.2), as `ductus_runtime::history` tags them: a pause, a resume or a
        // destination's alert by its subject; a resend, correction or dismissal by the destinations
        // its message actions name, or under every destination when none were recorded with it (a
        // refused attempt, or before 0.5.8.2); changed lanes are the source's (its `ordering`).
        let (part, name) = match part {
            None => ("", None),
            Some(Part::Source) => (
                "AND action IN ('interface.partitions_raised', 'interface.lanes_changed', 'interface.lanes_settled')",
                None,
            ),
            Some(Part::Destination(name)) => (
                "AND ((action IN ('destination.pause', 'destination.resume') AND subject = interface || '/' || $4)
                      OR (action LIKE 'alert.%' AND subject = 'dest:' || interface || '/' || $4)
                      OR (action IN ('message.release', 'message.replay', 'message.edit', 'message.dismiss')
                          AND (EXISTS (SELECT 1 FROM message_actions m JOIN destinations x ON x.id = m.destination_id
                                       WHERE m.audit_id = audit_log.id AND x.name = $4)
                               OR NOT EXISTS (SELECT 1 FROM message_actions m WHERE m.audit_id = audit_log.id))))",
                Some(name),
            ),
        };
        let sql = format!(
            "SELECT {},
                    ARRAY(SELECT DISTINCT x.name FROM message_actions m JOIN destinations x ON x.id = m.destination_id
                          WHERE m.audit_id = audit_log.id ORDER BY 1) AS acted_on
             FROM audit_log
             WHERE interface = $1 AND ($2::timestamptz IS NULL OR at <= $2) {only} {part}
             ORDER BY at DESC, id DESC LIMIT $3",
            crate::audit::COLUMNS
        );
        let mut query = sqlx::query_as::<_, AuditEntry>(sqlx::AssertSqlSafe(sql))
            .bind(page.interface)
            .bind(page.until)
            .bind(page.limit);
        if let Some(name) = name {
            query = query.bind(name);
        }
        query.fetch_all(&self.pool).await.context("reading the audit log's entries for the interface")
    }

    /// The changes of `page.interface`'s documentation, newest first, each with what it was before.
    /// With a destination, only the changes of its fields (`destinations.<name>`); the source has
    /// none (an interface's documentation is the whole interface's).
    pub async fn history_docs(&self, page: Page<'_>, part: Option<Part<'_>>) -> Result<Vec<DocsEntry>> {
        type Row = (i64, DateTime<Utc>, Option<i32>, Option<String>, Option<String>, Option<String>);
        let (part, name) = match part {
            None => ("", None),
            Some(Part::Source) => return Ok(Vec::new()),
            Some(Part::Destination(name)) => {
                ("WHERE docs->'destinations'->$4 IS DISTINCT FROM before->'destinations'->$4", Some(name))
            }
        };
        let mut query = sqlx::query_as::<_, Row>(sqlx::AssertSqlSafe(format!(
            "SELECT id, at, deployment_id, by_name, docs::text, before::text FROM (
                 SELECT h.id, h.at, h.deployment_id, d.by_name, h.docs,
                        (SELECT p.docs FROM docs_history p
                         WHERE p.kind = h.kind AND p.name = h.name AND p.id < h.id ORDER BY p.id DESC LIMIT 1) AS before
                 FROM docs_history h
                 LEFT JOIN deployments d ON d.id = h.deployment_id
                 WHERE h.kind = 'interface' AND h.name = $1 AND ($2::timestamptz IS NULL OR h.at <= $2)
             ) h {part}
             ORDER BY id DESC LIMIT $3",
        )))
        .bind(page.interface)
        .bind(page.until)
        .bind(page.limit);
        if let Some(name) = name {
            query = query.bind(name);
        }
        let rows = query.fetch_all(&self.pool).await.context("reading the interface's documentation history")?;
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

    /// Every one of `interface`'s code versions, with which files each part runs: what tells which
    /// parts a commit on a file touches (0.7.4.2).
    pub async fn version_files(&self, interface: &str) -> Result<Vec<VersionFiles>> {
        let rows: Vec<(String, Option<String>)> =
            sqlx::query_as("SELECT files::text, parts::text FROM code_versions WHERE interface = $1 ORDER BY id")
                .bind(interface)
                .fetch_all(&self.pool)
                .await
                .context("reading the interface's code versions")?;
        Ok(rows.into_iter().map(|(files, parts)| VersionFiles::of(&files, parts)).collect())
    }

    /// Every destination `interface` has had on this database (`destinations`: each one an engine
    /// has run, `default` for the singular `destination:` before 0.7.4), by name.
    pub async fn destination_names(&self, interface: &str) -> Result<Vec<String>> {
        sqlx::query_scalar("SELECT name FROM destinations WHERE interface = $1 ORDER BY name")
            .bind(interface)
            .fetch_all(&self.pool)
            .await
            .context("reading the interface's destinations")
    }
}
