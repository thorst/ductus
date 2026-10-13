//! The project's documentation, kept for SQL reports (migration 0017).

use crate::Store;
use anyhow::{Context, Result};

/// One row of the snapshot: `kind` is `interface` or `resource`, `docs` is JSON text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocsRow {
    pub kind: &'static str,
    pub name: String,
    pub docs: String,
}

impl Store {
    /// Make the snapshot match `rows` exactly: changed rows are rewritten, unchanged ones are
    /// left alone, and rows for entries the project no longer has are removed. One transaction.
    pub async fn save_docs_snapshot(&self, rows: &[DocsRow]) -> Result<()> {
        let kinds: Vec<&str> = rows.iter().map(|r| r.kind).collect();
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        let docs: Vec<&str> = rows.iter().map(|r| r.docs.as_str()).collect();
        let mut tx = self.pool.begin().await.context("starting the documentation snapshot")?;
        sqlx::query(
            "INSERT INTO docs_snapshot (kind, name, docs)
             SELECT kind, name, docs::jsonb FROM unnest($1::text[], $2::text[], $3::text[]) AS u(kind, name, docs)
             ON CONFLICT (kind, name) DO UPDATE SET docs = EXCLUDED.docs, updated_at = now()
             WHERE docs_snapshot.docs IS DISTINCT FROM EXCLUDED.docs",
        )
        .bind(&kinds)
        .bind(&names)
        .bind(&docs)
        .execute(&mut *tx)
        .await
        .context("saving the documentation snapshot")?;
        sqlx::query(
            "DELETE FROM docs_snapshot d
             WHERE NOT EXISTS (SELECT 1 FROM unnest($1::text[], $2::text[]) AS u(kind, name)
                               WHERE u.kind = d.kind AND u.name = d.name)",
        )
        .bind(&kinds)
        .bind(&names)
        .execute(&mut *tx)
        .await
        .context("removing documentation of entries the project no longer has")?;
        tx.commit().await.context("committing the documentation snapshot")?;
        Ok(())
    }
}
