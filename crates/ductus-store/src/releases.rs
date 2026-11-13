//! Releases stored (migration 0030): a release (`ductus_runtime::release`) kept in the database so
//! every engine on it and the web UI see it, and each check of one against an environment.
//!
//! A release is stored without repeating what's there already: texts once by content in
//! `code_files` (shared with code versions), other files in `release_blobs`, and each folder's list
//! of files once in `release_folders` (an interface's folder, a folder at the project's top, or the
//! top's own files). Storing a release whose id is here already writes nothing.

use crate::Store;
use crate::audit::{self, Audit};
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

/// One file of a release to store: a text (normalized, as code versions hash them) or bytes.
#[derive(Debug, Clone, Copy)]
pub struct ReleaseFile<'a> {
    /// Relative to the project, with `/`.
    pub path: &'a str,
    /// SHA-256 of `bytes`.
    pub hash: &'a [u8],
    pub bytes: &'a [u8],
    /// Not text: kept in `release_blobs`, not `code_files`.
    pub binary: bool,
}

/// A release to store. The caller has read it with `ductus_runtime::release::read`, which
/// recomputed `id` from the files.
#[derive(Debug, Clone)]
pub struct NewRelease<'a> {
    pub id: &'a [u8],
    pub project: &'a str,
    /// The manifest, as JSON.
    pub manifest: &'a serde_json::Value,
    pub made_at: Option<DateTime<Utc>>,
    pub made_by: &'a str,
    pub reason: Option<&'a str>,
    pub git_commit: Option<&'a str>,
    /// The branch it was made on (`git.branch`): added to `releases.branches` even when the
    /// release was stored before (the same files made on another branch are the same release).
    pub branch: Option<&'a str>,
    pub interfaces: usize,
    /// 20-byte commit ids, sorted (empty without git).
    pub ancestry: &'a [u8],
    pub files: &'a [ReleaseFile<'a>],
    /// Who stores it (as the audit log names them).
    pub by: &'a str,
}

/// What storing a release did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseStored {
    /// False: it was here already (stored then, by them); nothing was written.
    pub new: bool,
    pub at: DateTime<Utc>,
    pub by: String,
    /// Folders and files this release added (the rest were here already).
    pub new_folders: usize,
    pub new_files: usize,
}

/// A release as listed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseRow {
    pub id: Vec<u8>,
    pub project: String,
    pub made_at: Option<DateTime<Utc>>,
    pub made_by: String,
    pub reason: Option<String>,
    pub git_commit: Option<String>,
    pub interfaces: i32,
    pub files: i32,
    pub bytes: i64,
    pub stored_at: DateTime<Utc>,
    pub stored_by: String,
    /// Every branch it was made on, as its files said when they were stored (empty: none said).
    pub branches: Vec<String>,
    /// The environments it went to, by name: deployed there on this database, or a file carrying
    /// it said so (`release_places`). Filled in lists and summaries.
    pub environments: Vec<String>,
    /// The last check against each environment, by environment's name.
    pub checks: Vec<CheckRow>,
}

/// A check of a release against an environment (`release_checks`).
#[derive(Debug, Clone, PartialEq)]
pub struct CheckRow {
    pub id: i32,
    pub at: DateTime<Utc>,
    pub environment: String,
    pub instance: String,
    pub host: String,
    pub by: String,
    pub ok: bool,
    pub problems: i32,
    pub seconds: f32,
    /// The gate's report (`ductus release check --json`); `Null` in lists.
    pub report: serde_json::Value,
}

impl Eq for CheckRow {}

/// A check to record.
#[derive(Debug, Clone)]
pub struct NewCheck<'a> {
    pub release: &'a [u8],
    pub environment: &'a str,
    pub instance: &'a str,
    pub host: &'a str,
    pub by: &'a str,
    pub ok: bool,
    pub problems: i32,
    pub seconds: f32,
    pub report: &'a serde_json::Value,
    /// The Ductus version that ran it: a deploy reuses only a check this version ran (the pre-run).
    pub ductus_version: &'a str,
}

/// The release last deployed to an environment for a project: what an engine in release mode runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastDeployed {
    pub project: String,
    pub release: Vec<u8>,
    /// The deployment that ran it last, when and by whom.
    pub deployment: i32,
    pub at: DateTime<Utc>,
    pub by: String,
}

/// The files outside every interface as an environment runs them now (the last deployment that
/// recorded a project version: release mode), with that deployment: what a deploy holds a new
/// project version against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRunning {
    pub version: Vec<u8>,
    pub deployment: i32,
    pub at: DateTime<Utc>,
    pub how: String,
    pub by: String,
    pub host: String,
    pub git_commit: Option<String>,
    pub uncommitted: Option<Vec<String>>,
    pub release: Option<Vec<u8>>,
}

/// Where a release was deployed (0.7.4.4.3): a deployment on this database, or one on another
/// database that a file carrying it said (`release_places`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    pub environment: String,
    pub host: String,
    /// The deployment's id on the database it was recorded in.
    pub deployment: i32,
    pub at: DateTime<Utc>,
    pub by: String,
    pub ductus_version: String,
    /// Recorded on this database (`deployments`), not carried in by a file.
    pub here: bool,
}

/// A run of one release in an environment (consecutive deployments of it: a deploy, then the
/// restarts that ran it again), oldest first: what a rollback chooses from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub release: Vec<u8>,
    /// Its first deployment, when and by whom.
    pub deployment: i32,
    pub at: DateTime<Utc>,
    pub by: String,
    /// Why, as its first deployment said (`ductus deploy -m`); none for a plain start.
    pub reason: Option<String>,
    /// It began with a rollback (`ductus deploy --rollback`).
    pub rollback: bool,
}

/// A stored release's contents, read back: what `ductus_runtime::release` writes an archive from.
#[derive(Debug, Clone)]
pub struct ReleaseContents {
    pub manifest: serde_json::Value,
    pub ancestry: Vec<u8>,
    /// `(path, bytes, binary)`, in path order.
    pub files: Vec<(String, Vec<u8>, bool)>,
}

/// The folder a release keeps `path` in: an interface's folder (`interfaces/<name>`), a folder at
/// the project's top (`lib`), or the top itself ("").
pub fn folder_of(path: &str) -> &str {
    let mut parts = path.match_indices('/').map(|(i, _)| i);
    match parts.next() {
        None => "",
        Some(first) if &path[..first] == "interfaces" => parts.next().map_or(&path[..first], |second| &path[..second]),
        Some(first) => &path[..first],
    }
}

/// One folder's list as stored (`release_folders.files`) and its hash.
fn folder_list(files: &[&ReleaseFile<'_>]) -> (Vec<u8>, String) {
    let list: Vec<serde_json::Value> = files
        .iter()
        .map(|f| {
            let mut entry = serde_json::json!({ "path": f.path, "hash": hex(f.hash) });
            if f.binary {
                entry["binary"] = true.into();
            }
            entry
        })
        .collect();
    let text = serde_json::Value::Array(list).to_string();
    (Sha256::digest(text.as_bytes()).to_vec(), text)
}

const RELEASE_COLUMNS: &str = "id, project, made_at, made_by, reason, git_commit, interfaces, files, bytes, stored_at, stored_by, \
     coalesce(branches, '{}')";

type Row = (
    Vec<u8>,
    String,
    Option<DateTime<Utc>>,
    String,
    Option<String>,
    Option<String>,
    i32,
    i32,
    i64,
    DateTime<Utc>,
    String,
    Vec<String>,
);

fn release_row(r: Row) -> ReleaseRow {
    let (id, project, made_at, made_by, reason, git_commit, interfaces, files, bytes, stored_at, stored_by, branches) =
        r;
    ReleaseRow {
        id,
        project,
        made_at,
        made_by,
        reason,
        git_commit,
        interfaces,
        files,
        bytes,
        stored_at,
        stored_by,
        branches,
        environments: Vec::new(),
        checks: Vec::new(),
    }
}

type Check = (i32, Vec<u8>, DateTime<Utc>, String, String, String, String, bool, i32, f32, Option<String>);

fn check_row(c: Check) -> (Vec<u8>, CheckRow) {
    let (id, release, at, environment, instance, host, by, ok, problems, seconds, report) = c;
    let report = report.and_then(|r| serde_json::from_str(&r).ok()).unwrap_or_default();
    (release, CheckRow { id, at, environment, instance, host, by, ok, problems, seconds, report })
}

impl Store {
    /// Store a release: its new files and folders, then the release, with a `release.store` row
    /// in the audit log, in one transaction. A release here already is said so and nothing is
    /// written (two engines or CI jobs storing one at once: one stores it, the other is told).
    pub async fn store_release(&self, r: &NewRelease<'_>, audit: &Audit<'_>) -> Result<ReleaseStored> {
        let mut tx = self.pool.begin().await.context("starting to store the release")?;
        if let Some((at, by)) = stored_before(&mut tx, r.id).await? {
            add_branch(&mut tx, r.id, r.branch).await?;
            tx.commit().await.context("committing the release's branch")?;
            return Ok(ReleaseStored { new: false, at, by, new_folders: 0, new_files: 0 });
        }

        let mut folders: BTreeMap<&str, Vec<&ReleaseFile>> = BTreeMap::new();
        for f in r.files {
            folders.entry(folder_of(f.path)).or_default().push(f);
        }
        let mut lists: BTreeMap<Vec<u8>, (String, &[&ReleaseFile])> = BTreeMap::new();
        let mut order = Vec::with_capacity(folders.len());
        for files in folders.values_mut() {
            files.sort_by(|a, b| a.path.cmp(b.path));
            let (hash, text) = folder_list(files);
            order.push(hash.clone());
            lists.insert(hash, (text, files.as_slice()));
        }
        let hashes: Vec<&[u8]> = lists.keys().map(Vec::as_slice).collect();
        let known: BTreeSet<Vec<u8>> = sqlx::query_scalar("SELECT hash FROM release_folders WHERE hash = ANY($1)")
            .bind(&hashes)
            .fetch_all(&mut *tx)
            .await
            .context("reading the release folders stored")?
            .into_iter()
            .collect();
        type Folder<'f> = (String, &'f [&'f ReleaseFile<'f>]);
        let new: Vec<(&Vec<u8>, &Folder)> = lists.iter().filter(|(hash, _)| !known.contains(*hash)).collect();

        // The new folders' files not stored yet: texts in `code_files`, the rest in `release_blobs`
        // (each sorted, so engines storing at once lock rows in one order).
        let mut texts: BTreeMap<&[u8], &[u8]> = BTreeMap::new();
        let mut blobs: BTreeMap<&[u8], &[u8]> = BTreeMap::new();
        for (_, (_, files)) in &new {
            for f in files.iter() {
                let into = if f.binary { &mut blobs } else { &mut texts };
                into.insert(f.hash, f.bytes);
            }
        }
        let mut new_files = 0;
        for (table, files) in [("code_files", &texts), ("release_blobs", &blobs)] {
            if files.is_empty() {
                continue;
            }
            let hashes: Vec<&[u8]> = files.keys().copied().collect();
            let known: BTreeSet<Vec<u8>> =
                sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT hash FROM {table} WHERE hash = ANY($1)")))
                    .bind(&hashes)
                    .fetch_all(&mut *tx)
                    .await
                    .with_context(|| format!("reading {table}"))?
                    .into_iter()
                    .collect();
            let missing: Vec<(&[u8], &[u8])> =
                files.iter().filter(|(h, _)| !known.contains(**h)).map(|(h, b)| (*h, *b)).collect();
            if missing.is_empty() {
                continue;
            }
            new_files += missing.len();
            let (hashes, contents): (Vec<&[u8]>, Vec<&[u8]>) = missing.into_iter().unzip();
            if table == "code_files" {
                let texts: Vec<&str> = contents
                    .iter()
                    .map(|b| std::str::from_utf8(b).context("a release's text file isn't UTF-8"))
                    .collect::<Result<_>>()?;
                sqlx::query(
                    "INSERT INTO code_files (hash, content)
                     SELECT * FROM unnest($1::bytea[], $2::text[]) ORDER BY 1
                     ON CONFLICT (hash) DO NOTHING",
                )
                .bind(&hashes)
                .bind(&texts)
                .execute(&mut *tx)
                .await
                .context("saving the release's text files")?;
            } else {
                sqlx::query(
                    "INSERT INTO release_blobs (hash, content)
                     SELECT * FROM unnest($1::bytea[], $2::bytea[]) ORDER BY 1
                     ON CONFLICT (hash) DO NOTHING",
                )
                .bind(&hashes)
                .bind(&contents)
                .execute(&mut *tx)
                .await
                .context("saving the release's files that aren't text")?;
            }
        }
        if !new.is_empty() {
            let hashes: Vec<&[u8]> = new.iter().map(|(h, _)| h.as_slice()).collect();
            let lists: Vec<&str> = new.iter().map(|(_, (text, _))| text.as_str()).collect();
            sqlx::query(
                "INSERT INTO release_folders (hash, files)
                 SELECT hash, files::jsonb FROM unnest($1::bytea[], $2::text[]) AS u(hash, files) ORDER BY hash
                 ON CONFLICT (hash) DO NOTHING",
            )
            .bind(&hashes)
            .bind(&lists)
            .execute(&mut *tx)
            .await
            .context("saving the release's folders")?;
        }

        let bytes: i64 = r.files.iter().map(|f| f.bytes.len() as i64).sum();
        let stored: Option<DateTime<Utc>> = sqlx::query_scalar(
            "INSERT INTO releases (id, project, manifest, made_at, made_by, reason, git_commit, interfaces, files,
                                   bytes, ancestry, folders, stored_by, branches)
             VALUES ($1, $2, $3::jsonb, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13,
                     CASE WHEN $14::text IS NULL THEN NULL ELSE ARRAY[$14::text] END)
             ON CONFLICT (id) DO NOTHING RETURNING stored_at",
        )
        .bind(r.id)
        .bind(r.project)
        .bind(r.manifest.to_string())
        .bind(r.made_at)
        .bind(r.made_by)
        .bind(r.reason)
        .bind(r.git_commit)
        .bind(r.interfaces as i32)
        .bind(r.files.len() as i32)
        .bind(bytes)
        .bind(r.ancestry)
        .bind(&order)
        .bind(r.by)
        .bind(r.branch)
        .fetch_optional(&mut *tx)
        .await
        .context("saving the release")?;
        let Some(at) = stored else {
            // Stored by someone else since the read above.
            let (at, by) = stored_before(&mut tx, r.id).await?.context("the release was stored and is gone")?;
            add_branch(&mut tx, r.id, r.branch).await?;
            tx.commit().await.context("committing the release's branch")?;
            return Ok(ReleaseStored { new: false, at, by, new_folders: 0, new_files: 0 });
        };
        let short = &hex(r.id)[..12];
        let mut detail = serde_json::json!({
            "project": r.project,
            "commit": r.git_commit,
            "made_by": r.made_by,
            "reason": r.reason,
            "interfaces": r.interfaces,
            "files": r.files.len(),
        });
        // The caller's own (a pull's `from`) kept beside.
        if let (Some(serde_json::Value::Object(own)), Some(all)) = (&audit.detail, detail.as_object_mut()) {
            all.extend(own.clone());
        }
        audit::insert(&mut tx, &Audit { subject: Some(short), detail: Some(detail), ..audit.clone() }).await?;
        tx.commit().await.context("committing the release")?;
        Ok(ReleaseStored { new: true, at, by: r.by.to_string(), new_folders: new.len(), new_files })
    }

    /// Releases stored, newest first, `before` a time if given, at most `limit`, each with its
    /// last check per environment (reports left out).
    pub async fn releases(&self, before: Option<DateTime<Utc>>, limit: i64) -> Result<Vec<ReleaseRow>> {
        self.release_rows(None, before, limit).await
    }

    /// One release as `releases` lists it.
    pub async fn release_summary(&self, id: &[u8]) -> Result<Option<ReleaseRow>> {
        Ok(self.release_rows(Some(id), None, 1).await?.pop())
    }

    async fn release_rows(
        &self,
        id: Option<&[u8]>,
        before: Option<DateTime<Utc>>,
        limit: i64,
    ) -> Result<Vec<ReleaseRow>> {
        let sql = format!(
            "SELECT {RELEASE_COLUMNS} FROM releases
             WHERE ($1::bytea IS NULL OR id = $1) AND ($2::timestamptz IS NULL OR stored_at < $2)
             ORDER BY stored_at DESC, id LIMIT $3"
        );
        let mut rows: Vec<ReleaseRow> = sqlx::query_as::<_, Row>(sqlx::AssertSqlSafe(sql))
            .bind(id)
            .bind(before)
            .bind(limit)
            .fetch_all(&self.pool)
            .await
            .context("reading the releases")?
            .into_iter()
            .map(release_row)
            .collect();
        let ids: Vec<Vec<u8>> = rows.iter().map(|r| r.id.clone()).collect();
        let checks: Vec<Check> = sqlx::query_as(
            "SELECT DISTINCT ON (release_id, environment)
                    id, release_id, at, environment, instance, host, by_name, ok, problems, seconds, NULL::text
             FROM release_checks WHERE release_id = ANY($1)
             ORDER BY release_id, environment, at DESC, id DESC",
        )
        .bind(&ids)
        .fetch_all(&self.pool)
        .await
        .context("reading the releases' checks")?;
        for (release, check) in checks.into_iter().map(check_row) {
            if let Some(row) = rows.iter_mut().find(|r| r.id == release) {
                row.checks.push(check);
            }
        }
        // Where each went: this database's deployments of it (on the partial index of deployments
        // that ran a release) and what files carrying it said.
        let went: Vec<(Vec<u8>, Vec<String>)> = sqlx::query_as(
            "SELECT release_id, array_agg(DISTINCT environment ORDER BY environment) FROM (
                 SELECT release_id, environment FROM deployments
                 WHERE release_id IS NOT NULL AND release_id = ANY($1)
                 UNION ALL
                 SELECT release_id, environment FROM release_places WHERE release_id = ANY($1)) AS went
             GROUP BY release_id",
        )
        .bind(&ids)
        .fetch_all(&self.pool)
        .await
        .context("reading where the releases went")?;
        for (release, environments) in went {
            if let Some(row) = rows.iter_mut().find(|r| r.id == release) {
                row.environments = environments;
            }
        }
        Ok(rows)
    }

    /// Which of `ids` are stored here (a pull's list of another engine's releases).
    pub async fn stored_releases(&self, ids: &[Vec<u8>]) -> Result<std::collections::HashSet<Vec<u8>>> {
        let found: Vec<Vec<u8>> = sqlx::query_scalar("SELECT id FROM releases WHERE id = ANY($1)")
            .bind(ids)
            .fetch_all(&self.pool)
            .await
            .context("reading which releases are stored here")?;
        Ok(found.into_iter().collect())
    }

    /// The release whose id starts with `prefix` (hex, at least 4 characters): None when there's
    /// none; an error naming them when several do.
    pub async fn find_release(&self, prefix: &str) -> Result<Option<Vec<u8>>> {
        let prefix = prefix.trim().to_ascii_lowercase();
        if prefix.len() < 4 || prefix.len() > 64 || !prefix.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("`{prefix}` isn't a release id: give its first 12 characters (or more) in hex");
        }
        let found: Vec<Vec<u8>> =
            sqlx::query_scalar("SELECT id FROM releases WHERE encode(id, 'hex') LIKE $1 || '%' ORDER BY id LIMIT 5")
                .bind(&prefix)
                .fetch_all(&self.pool)
                .await
                .context("finding the release")?;
        match found.as_slice() {
            [] => Ok(None),
            [one] => Ok(Some(one.clone())),
            several => bail!(
                "several releases start with `{prefix}` ({}): give more of its id",
                several.iter().map(|id| hex(&id[..6])).collect::<Vec<_>>().join(", ")
            ),
        }
    }

    /// One release, its manifest, and every check of it (newest first, with their reports).
    pub async fn release(&self, id: &[u8]) -> Result<Option<(ReleaseRow, serde_json::Value)>> {
        let sql = format!("SELECT {RELEASE_COLUMNS} FROM releases WHERE id = $1");
        let row: Option<Row> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .context("reading the release")?;
        let Some(row) = row else { return Ok(None) };
        let mut release = release_row(row);
        let manifest: String = sqlx::query_scalar("SELECT manifest::text FROM releases WHERE id = $1")
            .bind(&release.id)
            .fetch_one(&self.pool)
            .await
            .context("reading the release's manifest")?;
        let manifest = serde_json::from_str(&manifest).context("reading the release's manifest")?;
        // Reports only for each environment's newest check: at a thousand interfaces one is hundreds
        // of KB (validate's output), and a release deployed a dozen times has a dozen (`release_check`
        // reads any one of them).
        let checks: Vec<Check> = sqlx::query_as(
            "SELECT id, release_id, at, environment, instance, host, by_name, ok, problems, seconds,
                    CASE WHEN row_number() OVER (PARTITION BY environment ORDER BY at DESC, id DESC) = 1
                         THEN report::text END
             FROM release_checks WHERE release_id = $1 ORDER BY at DESC, id DESC",
        )
        .bind(&release.id)
        .fetch_all(&self.pool)
        .await
        .context("reading the release's checks")?;
        release.checks = checks.into_iter().map(|c| check_row(c).1).collect();
        Ok(Some((release, manifest)))
    }

    /// One check of release `id`, with its report; None when it has no such check.
    pub async fn release_check(&self, id: &[u8], check: i32) -> Result<Option<CheckRow>> {
        let row: Option<Check> = sqlx::query_as(
            "SELECT id, release_id, at, environment, instance, host, by_name, ok, problems, seconds, report::text
             FROM release_checks WHERE release_id = $1 AND id = $2",
        )
        .bind(id)
        .bind(check)
        .fetch_optional(&self.pool)
        .await
        .context("reading the release's check")?;
        Ok(row.map(|c| check_row(c).1))
    }

    /// A stored release's manifest, ancestry and files, to write it as an archive again.
    pub async fn release_contents(&self, id: &[u8]) -> Result<Option<ReleaseContents>> {
        let row: Option<(String, Vec<u8>, Vec<Vec<u8>>)> =
            sqlx::query_as("SELECT manifest::text, ancestry, folders FROM releases WHERE id = $1")
                .bind(id)
                .fetch_optional(&self.pool)
                .await
                .context("reading the release")?;
        let Some((manifest, ancestry, folders)) = row else { return Ok(None) };
        let manifest: serde_json::Value = serde_json::from_str(&manifest).context("reading the release's manifest")?;
        let lists: Vec<(Vec<u8>, String)> =
            sqlx::query_as("SELECT hash, files::text FROM release_folders WHERE hash = ANY($1)")
                .bind(&folders)
                .fetch_all(&self.pool)
                .await
                .context("reading the release's folders")?;
        if lists.len() != folders.len() {
            bail!("the release's folders aren't all stored (the database was changed by hand?)");
        }
        let mut entries: Vec<(String, Vec<u8>, bool)> = Vec::new();
        for (_, list) in &lists {
            let list: Vec<serde_json::Value> = serde_json::from_str(list).context("reading a release folder")?;
            for e in list {
                let path = e["path"].as_str().context("a release folder's entry has no path")?.to_string();
                let hash = unhex(e["hash"].as_str().unwrap_or_default())
                    .with_context(|| format!("`{path}` in a release folder has no hash"))?;
                entries.push((path, hash, e["binary"] == true));
            }
        }
        let hashes = |binary: bool| -> Vec<&[u8]> {
            entries.iter().filter(|(_, _, b)| *b == binary).map(|(_, h, _)| h.as_slice()).collect()
        };
        let texts: BTreeMap<Vec<u8>, String> =
            sqlx::query_as::<_, (Vec<u8>, String)>("SELECT hash, content FROM code_files WHERE hash = ANY($1)")
                .bind(hashes(false))
                .fetch_all(&self.pool)
                .await
                .context("reading the release's text files")?
                .into_iter()
                .collect();
        let blobs: BTreeMap<Vec<u8>, Vec<u8>> =
            sqlx::query_as::<_, (Vec<u8>, Vec<u8>)>("SELECT hash, content FROM release_blobs WHERE hash = ANY($1)")
                .bind(hashes(true))
                .fetch_all(&self.pool)
                .await
                .context("reading the release's other files")?
                .into_iter()
                .collect();
        let mut files = Vec::with_capacity(entries.len());
        for (path, hash, binary) in entries {
            let bytes =
                if binary { blobs.get(&hash).cloned() } else { texts.get(&hash).map(|t| t.clone().into_bytes()) };
            let bytes = bytes.with_context(|| format!("`{path}` of the release isn't stored"))?;
            files.push((path, bytes, binary));
        }
        files.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(Some(ReleaseContents { manifest, ancestry, files }))
    }

    /// Record a check of a release against an environment, with a `release.check` row in the
    /// audit log, in one transaction.
    pub async fn record_check(&self, c: &NewCheck<'_>, audit: &Audit<'_>) -> Result<i32> {
        let mut tx = self.pool.begin().await.context("starting to record the check")?;
        let id: i32 = sqlx::query_scalar(
            "INSERT INTO release_checks (release_id, environment, instance, host, by_name, ok, problems, seconds, report,
                                         ductus_version)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::jsonb, $10) RETURNING id",
        )
        .bind(c.release)
        .bind(c.environment)
        .bind(c.instance)
        .bind(c.host)
        .bind(c.by)
        .bind(c.ok)
        .bind(c.problems)
        .bind(c.seconds)
        .bind(c.report.to_string())
        .bind(c.ductus_version)
        .fetch_one(&mut *tx)
        .await
        .context("saving the check")?;
        let short = &hex(c.release)[..12];
        let detail = serde_json::json!({ "environment": c.environment, "ok": c.ok, "problems": c.problems });
        audit::insert(&mut tx, &Audit { subject: Some(short), detail: Some(detail), ..audit.clone() }).await?;
        tx.commit().await.context("committing the check")?;
        Ok(id)
    }

    /// The pre-run (0.7.4.4): the newest check of `release` a deploy may reuse instead of running
    /// the gate again. It passed, against `environment`, on this database (the row is here), by
    /// `ductus_version`, at or after `since`, and **no deployment of any kind** has been recorded
    /// in that environment since it ran (a start or a handover changes what the gate would see).
    pub async fn reusable_check(
        &self,
        release: &[u8],
        environment: &str,
        ductus_version: &str,
        since: DateTime<Utc>,
    ) -> Result<Option<CheckRow>> {
        let found: Option<Check> = sqlx::query_as(
            "SELECT c.id, c.release_id, c.at, c.environment, c.instance, c.host, c.by_name, c.ok, c.problems, c.seconds,
                    c.report::text
             FROM release_checks c
             WHERE c.release_id = $1 AND c.environment = $2 AND c.ductus_version = $3 AND c.at >= $4 AND c.ok
               AND NOT EXISTS (SELECT 1 FROM deployments d WHERE d.environment = $2 AND d.at > c.at)
             ORDER BY c.at DESC, c.id DESC LIMIT 1",
        )
        .bind(release)
        .bind(environment)
        .bind(ductus_version)
        .bind(since)
        .fetch_optional(&self.pool)
        .await
        .context("reading the checks a deploy may reuse")?;
        Ok(found.map(|c| check_row(c).1))
    }

    /// The release last deployed to `environment` for each project deployed there, newest first:
    /// what an engine in release mode runs (`ductus run --releases`).
    pub async fn last_deployed(&self, environment: &str) -> Result<Vec<LastDeployed>> {
        type Last = (String, Vec<u8>, i32, DateTime<Utc>, String);
        let mut rows: Vec<Last> = sqlx::query_as(
            "SELECT DISTINCT ON (r.project) r.project, d.release_id, d.id, d.at, d.by_name
             FROM deployments d JOIN releases r ON r.id = d.release_id
             WHERE d.environment = $1 AND d.release_id IS NOT NULL
             ORDER BY r.project, d.id DESC",
        )
        .bind(environment)
        .fetch_all(&self.pool)
        .await
        .context("reading the releases deployed")?;
        rows.sort_by_key(|a| std::cmp::Reverse(a.2));
        Ok(rows
            .into_iter()
            .map(|(project, release, deployment, at, by)| LastDeployed { project, release, deployment, at, by })
            .collect())
    }

    /// A stored release's manifest and ancestry (20-byte commit ids, sorted; empty without git),
    /// without its files or checks.
    pub async fn release_manifest(&self, id: &[u8]) -> Result<Option<(serde_json::Value, Vec<u8>)>> {
        let row: Option<(String, Vec<u8>)> =
            sqlx::query_as("SELECT manifest::text, ancestry FROM releases WHERE id = $1")
                .bind(id)
                .fetch_optional(&self.pool)
                .await
                .context("reading the release")?;
        row.map(|(m, a)| Ok((serde_json::from_str(&m).context("reading the release's manifest")?, a))).transpose()
    }

    /// The project version `project` runs now in `environment` (release mode records one), with the
    /// deployment it came with: the first deployment of that version since the last that ran
    /// another (who brought it, not who restarted it since).
    pub async fn project_running(&self, environment: &str, project: &str) -> Result<Option<ProjectRunning>> {
        type Found =
            (Vec<u8>, i32, DateTime<Utc>, String, String, String, Option<String>, Option<Vec<String>>, Option<Vec<u8>>);
        let found: Option<Found> = sqlx::query_as(
            "WITH mine AS (
                 SELECT d.* FROM deployments d JOIN releases r ON r.id = d.release_id
                 WHERE d.environment = $1 AND d.release_id IS NOT NULL AND r.project = $2),
             last AS (SELECT project_version FROM mine ORDER BY id DESC LIMIT 1),
             changed AS (
                 SELECT coalesce(max(m.id), 0) AS id FROM mine m, last WHERE m.project_version <> last.project_version)
             SELECT m.project_version, m.id, m.at, m.how, m.by_name, m.host, m.git_commit, m.uncommitted, m.release_id
             FROM mine m, last, changed
             WHERE m.project_version = last.project_version AND m.id > changed.id
             ORDER BY m.id LIMIT 1",
        )
        .bind(environment)
        .bind(project)
        .fetch_optional(&self.pool)
        .await
        .context("reading the project version that runs")?;
        Ok(found.map(|(version, deployment, at, how, by, host, git_commit, uncommitted, release)| ProjectRunning {
            version,
            deployment,
            at,
            how,
            by,
            host,
            git_commit,
            uncommitted,
            release,
        }))
    }

    /// Where release `id` was deployed: each environment's first deployment of it on this database,
    /// then what files carrying it said about other databases (`release_places`), oldest first.
    /// A record that came back to the database it was made on is listed once (as `here`).
    pub async fn release_places(&self, id: &[u8]) -> Result<Vec<Place>> {
        type Row = (String, String, i32, DateTime<Utc>, String, String, bool);
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT environment, host, id, at, by_name, version, true FROM (
                 SELECT DISTINCT ON (environment) environment, host, id, at, by_name, version
                 FROM deployments WHERE release_id = $1 ORDER BY environment, id) AS here
             UNION ALL
             SELECT p.environment, p.host, p.deployment, p.at, p.by_name, p.ductus_version, false
             FROM release_places p
             WHERE p.release_id = $1
               AND NOT EXISTS (SELECT 1 FROM deployments d WHERE d.id = p.deployment AND d.release_id = $1
                                                             AND d.environment = p.environment AND d.host = p.host)
             ORDER BY 4, 1",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await
        .context("reading where the release was deployed")?;
        Ok(rows
            .into_iter()
            .map(|(environment, host, deployment, at, by, ductus_version, here)| Place {
                environment,
                host,
                deployment,
                at,
                by,
                ductus_version,
                here,
            })
            .collect())
    }

    /// Every branch release `id` was made on, as its files said when they were stored (empty:
    /// none said: a detached HEAD, made without git, or stored before 0.7.4.4.3).
    pub async fn release_branches(&self, id: &[u8]) -> Result<Vec<String>> {
        let branches: Option<Vec<String>> = sqlx::query_scalar("SELECT branches FROM releases WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .context("reading the branches the release was made on")?
            .flatten();
        Ok(branches.unwrap_or_default())
    }

    /// Keep what a file said about where release `id` was deployed (`places`, from its manifest),
    /// stored by `by`: those not kept already. Returns how many were new.
    pub async fn add_release_places(&self, id: &[u8], places: &[Place], by: &str) -> Result<usize> {
        if places.is_empty() {
            return Ok(0);
        }
        let environments: Vec<&str> = places.iter().map(|p| p.environment.as_str()).collect();
        let hosts: Vec<&str> = places.iter().map(|p| p.host.as_str()).collect();
        let deployments: Vec<i32> = places.iter().map(|p| p.deployment).collect();
        let at: Vec<DateTime<Utc>> = places.iter().map(|p| p.at).collect();
        let names: Vec<&str> = places.iter().map(|p| p.by.as_str()).collect();
        let versions: Vec<&str> = places.iter().map(|p| p.ductus_version.as_str()).collect();
        let added = sqlx::query(
            "INSERT INTO release_places (release_id, environment, host, deployment, at, by_name, ductus_version, received_by)
             SELECT $1, * , $8 FROM unnest($2::text[], $3::text[], $4::int[], $5::timestamptz[], $6::text[], $7::text[])
             ORDER BY 1, 2, 3
             ON CONFLICT DO NOTHING",
        )
        .bind(id)
        .bind(&environments)
        .bind(&hosts)
        .bind(&deployments)
        .bind(&at)
        .bind(&names)
        .bind(&versions)
        .bind(by)
        .execute(&self.pool)
        .await
        .context("keeping where the release was deployed")?;
        Ok(added.rows_affected() as usize)
    }

    /// The releases `project` ran in `environment`, oldest first, one entry per run (consecutive
    /// deployments of one release are one run): what a rollback chooses from.
    pub async fn release_runs(&self, environment: &str, project: &str) -> Result<Vec<Run>> {
        type Row = (Vec<u8>, i32, DateTime<Utc>, String, Option<String>, bool);
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT release_id, id, at, by_name, reason, rollback FROM (
                 SELECT d.release_id, d.id, d.at, d.by_name, d.reason, coalesce(d.rollback, false) AS rollback,
                        lag(d.release_id) OVER (ORDER BY d.id) AS before
                 FROM deployments d JOIN releases r ON r.id = d.release_id
                 WHERE d.environment = $1 AND d.release_id IS NOT NULL AND r.project = $2) AS runs
             WHERE before IS DISTINCT FROM release_id
             ORDER BY id",
        )
        .bind(environment)
        .bind(project)
        .fetch_all(&self.pool)
        .await
        .context("reading the releases run here")?;
        Ok(rows
            .into_iter()
            .map(|(release, deployment, at, by, reason, rollback)| Run {
                release,
                deployment,
                at,
                by,
                reason,
                rollback,
            })
            .collect())
    }
}

/// Add `branch` to the branches release `id` was made on, unless it's there.
async fn add_branch(tx: &mut sqlx::PgConnection, id: &[u8], branch: Option<&str>) -> Result<()> {
    let Some(branch) = branch else { return Ok(()) };
    sqlx::query(
        "UPDATE releases SET branches = array_append(coalesce(branches, '{}'), $2)
         WHERE id = $1 AND NOT ($2 = ANY(coalesce(branches, '{}')))",
    )
    .bind(id)
    .bind(branch)
    .execute(tx)
    .await
    .context("adding the branch the release was made on")?;
    Ok(())
}

/// When and by whom release `id` was stored, if it is.
async fn stored_before(tx: &mut sqlx::PgConnection, id: &[u8]) -> Result<Option<(DateTime<Utc>, String)>> {
    sqlx::query_as("SELECT stored_at, stored_by FROM releases WHERE id = $1")
        .bind(id)
        .fetch_optional(tx)
        .await
        .context("reading the releases stored")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || text.is_empty() {
        return None;
    }
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_are_kept_by_interface_folder_or_top_folder() {
        assert_eq!(folder_of("ductus.yaml"), "");
        assert_eq!(folder_of("lib/codes.ts"), "lib");
        assert_eq!(folder_of("lib/deep/x.ts"), "lib");
        assert_eq!(folder_of("interfaces/adt/interface.yaml"), "interfaces/adt");
        assert_eq!(folder_of("interfaces/adt/fixtures/a.hl7"), "interfaces/adt");
        assert_eq!(folder_of("interfaces/README.md"), "interfaces");
    }

    #[test]
    fn hex_reads_back() {
        assert_eq!(unhex(&hex(&[0, 1, 0xab, 0xff])), Some(vec![0, 1, 0xab, 0xff]));
        assert_eq!(unhex("abc"), None);
        assert_eq!(unhex("zz"), None);
    }
}
