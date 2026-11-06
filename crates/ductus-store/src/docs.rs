//! The project's documentation, kept for SQL reports (migration 0017), and its history (0028).

use crate::Store;
use anyhow::{Context, Result};

/// One row of the snapshot: `kind` is `interface` or `resource`, `docs` is JSON text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocsRow {
    pub kind: &'static str,
    pub name: String,
    pub docs: String,
}

/// The advisory lock a documentation save holds: "ductus docs" in ASCII, apart from the engines'
/// own keys (`engines.lock_key`, random).
const DOCS_LOCK: i64 = 0x6475_6374_7573_6463;

impl Store {
    /// Make the snapshot match `rows` exactly, with a history row for each change and no
    /// deployment named: what a start does when its deployment can't be recorded (a recorded one
    /// saves the documentation with it: `record_deployment`). One transaction.
    pub async fn save_docs_snapshot(&self, rows: &[DocsRow]) -> Result<()> {
        let mut tx = self.pool.begin().await.context("starting the documentation snapshot")?;
        save(&mut tx, rows, None).await?;
        tx.commit().await.context("committing the documentation snapshot")?;
        Ok(())
    }
}

/// Make the snapshot match `rows` exactly: changed rows are rewritten, unchanged ones are left
/// alone, and rows for entries the project no longer has are removed. Each of those writes a
/// `docs_history` row with `deployment` (the same statement: the history can't miss a change).
pub(crate) async fn save(tx: &mut sqlx::PgConnection, rows: &[DocsRow], deployment: Option<i32>) -> Result<()> {
    // One save at a time: two engines on one database each upsert their entries and then delete
    // the other's, which in two transactions at once deadlocks. Held until the caller commits.
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(DOCS_LOCK)
        .execute(&mut *tx)
        .await
        .context("waiting for another engine's documentation snapshot")?;
    let kinds: Vec<&str> = rows.iter().map(|r| r.kind).collect();
    let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
    let docs: Vec<&str> = rows.iter().map(|r| r.docs.as_str()).collect();
    sqlx::query(
        "WITH saved AS (
             INSERT INTO docs_snapshot (kind, name, docs)
             SELECT kind, name, docs::jsonb FROM unnest($1::text[], $2::text[], $3::text[]) AS u(kind, name, docs)
             ORDER BY kind, name
             ON CONFLICT (kind, name) DO UPDATE SET docs = EXCLUDED.docs, updated_at = now()
             WHERE docs_snapshot.docs IS DISTINCT FROM EXCLUDED.docs
             RETURNING kind, name, docs
         )
         INSERT INTO docs_history (deployment_id, kind, name, docs)
         SELECT $4, kind, name, docs FROM saved ORDER BY kind, name",
    )
    .bind(&kinds)
    .bind(&names)
    .bind(&docs)
    .bind(deployment)
    .execute(&mut *tx)
    .await
    .context("saving the documentation snapshot")?;
    // Entries the project no longer has go, except an interface another live engine runs (engines
    // with projects of their own on one database: removing each other's entries made every start
    // a "removed" and the other's next start an "added" in the history). Resources record no
    // owner: the last start's project wins for those (a nuance).
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "WITH gone AS (
             DELETE FROM docs_snapshot d
             WHERE NOT EXISTS (SELECT 1 FROM unnest($1::text[], $2::text[]) AS u(kind, name)
                               WHERE u.kind = d.kind AND u.name = d.name)
               AND NOT (d.kind = 'interface' AND EXISTS (
                   SELECT 1 FROM interface_owners o JOIN engines e ON e.instance = o.instance
                   WHERE o.interface = d.name AND e.stopped_at IS NULL AND {}))
             RETURNING kind, name
         )
         INSERT INTO docs_history (deployment_id, kind, name, docs)
         SELECT $3, kind, name, NULL FROM gone ORDER BY kind, name",
        crate::engines::ALIVE
    )))
    .bind(&kinds)
    .bind(&names)
    .bind(deployment)
    .execute(&mut *tx)
    .await
    .context("removing documentation of entries the project no longer has")?;
    Ok(())
}
