//! What runs (migration 0026): code versions, the files they're made of, and a deployment for
//! every start and handover. Written once per start, read by the API from the engine's memory.

use crate::Store;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use std::collections::{BTreeMap, HashMap};

/// An interface's code version, as a start computed it (`ductus_runtime::code::Version`).
#[derive(Debug, Clone)]
pub struct NewVersion<'a> {
    pub interface: &'a str,
    pub hash: &'a [u8],
    /// `(path, SHA-256, text)`, in path order.
    pub files: Vec<(&'a str, &'a [u8], &'a str)>,
    /// Which files each part runs, as JSON (`code_versions.parts`, `ductus_runtime::code::Parts`).
    pub parts: String,
}

/// A start or handover to record.
#[derive(Debug, Clone)]
pub struct NewDeployment<'a> {
    pub environment: &'a str,
    pub instance: &'a str,
    pub host: &'a str,
    /// The binary's version.
    pub version: &'a str,
    /// `start`, `service` or `handover`.
    pub how: &'a str,
    pub by: &'a str,
    pub reason: Option<&'a str>,
    /// None: not in a git repository (or no commit yet, with `uncommitted` set).
    pub git_commit: Option<&'a str>,
    /// None without git.
    pub uncommitted: Option<&'a [String]>,
    /// Every interface the engine runs.
    pub versions: &'a [NewVersion<'a>],
    /// Warnings for interfaces it changes, by interface (`deployment_changes.warning`): the
    /// version replaced may be someone else's work this one undoes.
    pub warnings: &'a HashMap<String, String>,
    /// The project's documentation, saved with the deployment (`docs_snapshot`, and a
    /// `docs_history` row for each entry that changed); None leaves both alone.
    pub docs: Option<&'a [crate::DocsRow]>,
    /// The release it runs (`ductus run --releases`); None in folder mode.
    pub release: Option<&'a [u8]>,
    /// The release's project version (the files outside every interface); None in folder mode.
    pub project_version: Option<&'a [u8]>,
    /// A rollback (`ductus deploy --rollback`, 0.7.4.4.3).
    pub rollback: bool,
}

/// What an interface runs now, as recorded (`interface_versions`), with the deployment it came
/// with: what a new deployment is held against before it replaces it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Current {
    pub interface: String,
    pub version: Vec<u8>,
    /// The paths its files had (`code_versions.files`).
    pub files: Vec<String>,
    pub deployment: i32,
    pub at: DateTime<Utc>,
    /// `start`, `service` or `handover`.
    pub how: String,
    pub by: String,
    pub host: String,
    pub git_commit: Option<String>,
    /// Null without git.
    pub uncommitted: Option<Vec<String>>,
    /// The release that deployment ran (release mode).
    pub release: Option<Vec<u8>>,
}

/// The deployment an interface's running version came with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Since {
    pub deployment: i32,
    pub at: DateTime<Utc>,
    pub by: String,
    pub reason: Option<String>,
    pub git_commit: Option<String>,
}

/// One interface of a recorded deployment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Running {
    pub interface: String,
    pub version: Vec<u8>,
    /// What it ran before this deployment, when that was another version (None: unchanged, or new here).
    pub previous: Option<Vec<u8>>,
    /// Its version is new for it here: new here, or changed.
    pub changed: bool,
    /// The deployment its version came with (this one, when `changed`).
    pub since: Since,
}

/// A deployment, recorded.
#[derive(Debug, Clone)]
pub struct Deployed {
    pub id: i32,
    pub at: DateTime<Utc>,
    /// In the order given.
    pub interfaces: Vec<Running>,
}

impl Deployed {
    pub fn changed(&self) -> usize {
        self.interfaces.iter().filter(|i| i.changed).count()
    }
}

/// Each code version's id (`code_versions.id`) by its hash: what deliveries record.
pub type VersionIds = HashMap<Vec<u8>, i32>;

impl Store {
    /// Save the versions and files not seen before, and return every version's id. One
    /// transaction that reads first and writes only what's new (a start that changes nothing
    /// writes nothing). Engines on one database may save at once (shared files and versions are
    /// written once, whoever comes first). Before the deployment is recorded: the engine's
    /// workers wait for these ids before saving what they decided (`deliveries.code_version`).
    pub async fn save_versions(&self, versions: &[NewVersion<'_>]) -> Result<VersionIds> {
        let mut tx = self.pool.begin().await.context("starting to save the code versions")?;
        let hashes: Vec<&[u8]> = versions.iter().map(|v| v.hash).collect();
        let known: Vec<(Vec<u8>, i32, bool)> =
            sqlx::query_as("SELECT hash, id, parts IS NULL FROM code_versions WHERE hash = ANY($1)")
                .bind(&hashes)
                .fetch_all(&mut *tx)
                .await
                .context("reading the code versions")?;
        // Versions saved before 0.7.4.2 get their parts the first time an engine runs them again:
        // written once, then a start that changes nothing writes nothing again.
        let mut without: Vec<&NewVersion> = versions
            .iter()
            .filter(|v| known.iter().any(|(hash, _, missing)| *missing && hash.as_slice() == v.hash))
            .collect();
        if !without.is_empty() {
            without.sort_by(|a, b| a.hash.cmp(b.hash));
            without.dedup_by(|a, b| a.hash == b.hash);
            let hashes: Vec<&[u8]> = without.iter().map(|v| v.hash).collect();
            let parts: Vec<&str> = without.iter().map(|v| v.parts.as_str()).collect();
            sqlx::query(
                "UPDATE code_versions v SET parts = u.parts::jsonb
                 FROM unnest($1::bytea[], $2::text[]) AS u(hash, parts)
                 WHERE v.hash = u.hash AND v.parts IS NULL",
            )
            .bind(&hashes)
            .bind(&parts)
            .execute(&mut *tx)
            .await
            .context("saving which files each destination's code loads")?;
        }
        let mut ids: VersionIds = known.into_iter().map(|(hash, id, _)| (hash, id)).collect();
        let new: Vec<&NewVersion> = versions.iter().filter(|v| !ids.contains_key(v.hash)).collect();
        if !new.is_empty() {
            // Their files: each text once (sorted, so engines writing the same ones at once lock
            // them in one order).
            let files: BTreeMap<&[u8], &str> =
                new.iter().flat_map(|v| v.files.iter().map(|(_, hash, text)| (*hash, *text))).collect();
            let file_hashes: Vec<&[u8]> = files.keys().copied().collect();
            let known: Vec<Vec<u8>> = sqlx::query_scalar("SELECT hash FROM code_files WHERE hash = ANY($1)")
                .bind(&file_hashes)
                .fetch_all(&mut *tx)
                .await
                .context("reading the code files")?;
            let missing: Vec<(&[u8], &str)> =
                files.into_iter().filter(|(hash, _)| !known.iter().any(|k| k.as_slice() == *hash)).collect();
            if !missing.is_empty() {
                let (hashes, texts): (Vec<&[u8]>, Vec<&str>) = missing.into_iter().unzip();
                sqlx::query(
                    "INSERT INTO code_files (hash, content)
                     SELECT * FROM unnest($1::bytea[], $2::text[]) ORDER BY 1
                     ON CONFLICT (hash) DO NOTHING",
                )
                .bind(&hashes)
                .bind(&texts)
                .execute(&mut *tx)
                .await
                .context("saving the code files")?;
            }
            let mut sorted = new.clone();
            sorted.sort_by(|a, b| a.hash.cmp(b.hash));
            let hashes: Vec<&[u8]> = sorted.iter().map(|v| v.hash).collect();
            let interfaces: Vec<&str> = sorted.iter().map(|v| v.interface).collect();
            let listed: Vec<String> = sorted
                .iter()
                .map(|v| {
                    let files: Vec<_> = v
                        .files
                        .iter()
                        .map(|(path, hash, _)| serde_json::json!({ "path": path, "hash": hex(hash) }))
                        .collect();
                    serde_json::Value::Array(files).to_string()
                })
                .collect();
            let parts: Vec<&str> = sorted.iter().map(|v| v.parts.as_str()).collect();
            sqlx::query(
                "INSERT INTO code_versions (hash, interface, files, parts)
                 SELECT hash, interface, files::jsonb, parts::jsonb
                 FROM unnest($1::bytea[], $2::text[], $3::text[], $4::text[]) AS u(hash, interface, files, parts)
                 ORDER BY hash
                 ON CONFLICT (hash) DO NOTHING",
            )
            .bind(&hashes)
            .bind(&interfaces)
            .bind(&listed)
            .bind(&parts)
            .execute(&mut *tx)
            .await
            .context("saving the code versions")?;
            // Another engine may have written some of them meanwhile: read them all back.
            ids.extend(
                sqlx::query_as::<_, (Vec<u8>, i32)>("SELECT hash, id FROM code_versions WHERE hash = ANY($1)")
                    .bind(&hashes)
                    .fetch_all(&mut *tx)
                    .await
                    .context("reading the code versions saved")?,
            );
        }
        tx.commit().await.context("committing the code versions")?;
        Ok(ids)
    }

    /// Record a start or handover: the deployment and the interfaces it changed, its versions
    /// saved first (`save_versions`, whose `ids` these are). One transaction; a start that changes
    /// nothing writes one row. Engines on one database may record at once (each its own interfaces).
    pub async fn record_deployment(&self, d: &NewDeployment<'_>, ids: &VersionIds) -> Result<Deployed> {
        if let Some(v) = d.versions.iter().find(|v| !ids.contains_key(v.hash)) {
            anyhow::bail!("the code version of `{}` wasn't saved first", v.interface);
        }
        let mut tx = self.pool.begin().await.context("starting to record the deployment")?;
        let names: Vec<&str> = d.versions.iter().map(|v| v.interface).collect();
        type Current = (String, i32, Vec<u8>, i32, DateTime<Utc>, String, Option<String>, Option<String>);
        let current: HashMap<String, (i32, Vec<u8>, Since)> = sqlx::query_as::<_, Current>(
            "SELECT i.interface, i.version_id, v.hash, d.id, d.at, d.by_name, d.reason, d.git_commit
             FROM interface_versions i
             JOIN code_versions v ON v.id = i.version_id
             JOIN deployments d ON d.id = i.deployment_id
             WHERE i.interface = ANY($1)",
        )
        .bind(&names)
        .fetch_all(&mut *tx)
        .await
        .context("reading what the interfaces ran")?
        .into_iter()
        .map(|(interface, id, hash, deployment, at, by, reason, git_commit)| {
            (interface, (id, hash, Since { deployment, at, by, reason, git_commit }))
        })
        .collect();
        let changed: Vec<&NewVersion> =
            d.versions.iter().filter(|v| current.get(v.interface).is_none_or(|(id, ..)| *id != ids[v.hash])).collect();
        let set = interface_set(&mut tx, &names).await?;
        let (id, at): (i32, DateTime<Utc>) = sqlx::query_as(
            "INSERT INTO deployments (environment, instance, host, version, how, by_name, reason, git_commit,
                                      uncommitted, interfaces, changed, interface_set, release_id, project_version,
                                      rollback)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)
             RETURNING id, at",
        )
        .bind(d.environment)
        .bind(d.instance)
        .bind(d.host)
        .bind(d.version)
        .bind(d.how)
        .bind(d.by)
        .bind(d.reason)
        .bind(d.git_commit)
        .bind(d.uncommitted)
        .bind(d.versions.len() as i32)
        .bind(changed.len() as i32)
        .bind(set)
        .bind(d.release)
        .bind(d.project_version)
        .bind(d.rollback.then_some(true))
        .fetch_one(&mut *tx)
        .await
        .context("saving the deployment")?;
        if !changed.is_empty() {
            let mut changed = changed.clone();
            changed.sort_by(|a, b| a.interface.cmp(b.interface));
            let interfaces: Vec<&str> = changed.iter().map(|v| v.interface).collect();
            let from: Vec<Option<i32>> = changed.iter().map(|v| current.get(v.interface).map(|(id, ..)| *id)).collect();
            let to: Vec<i32> = changed.iter().map(|v| ids[v.hash]).collect();
            let warnings: Vec<Option<&str>> =
                changed.iter().map(|v| d.warnings.get(v.interface).map(String::as_str)).collect();
            sqlx::query(
                "INSERT INTO deployment_changes (deployment_id, interface, from_version, to_version, warning)
                 SELECT $1, * FROM unnest($2::text[], $3::int[], $4::int[], $5::text[])",
            )
            .bind(id)
            .bind(&interfaces)
            .bind(&from)
            .bind(&to)
            .bind(&warnings)
            .execute(&mut *tx)
            .await
            .context("saving what the deployment changed")?;
            sqlx::query(
                "INSERT INTO interface_versions (interface, version_id, deployment_id)
                 SELECT u.interface, u.version_id, $1 FROM unnest($2::text[], $3::int[]) AS u(interface, version_id)
                 ORDER BY u.interface
                 ON CONFLICT (interface) DO UPDATE SET version_id = EXCLUDED.version_id,
                                                       deployment_id = EXCLUDED.deployment_id",
            )
            .bind(id)
            .bind(&interfaces)
            .bind(&to)
            .execute(&mut *tx)
            .await
            .context("saving what the interfaces run")?;
        }
        if let Some(docs) = d.docs {
            crate::docs::save(&mut tx, docs, Some(id)).await?;
        }
        tx.commit().await.context("committing the deployment")?;
        let here = Since {
            deployment: id,
            at,
            by: d.by.to_string(),
            reason: d.reason.map(str::to_string),
            git_commit: d.git_commit.map(str::to_string),
        };
        let interfaces = d
            .versions
            .iter()
            .map(|v| {
                let before = current.get(v.interface);
                let changed = before.is_none_or(|(id, ..)| *id != ids[v.hash]);
                Running {
                    interface: v.interface.to_string(),
                    version: v.hash.to_vec(),
                    previous: before.filter(|_| changed).map(|(_, hash, _)| hash.clone()),
                    changed,
                    since: match before {
                        Some((_, _, since)) if !changed => since.clone(),
                        _ => here.clone(),
                    },
                }
            })
            .collect();
        Ok(Deployed { id, at, interfaces })
    }

    /// What `names` run now, as recorded, with the deployments their versions came with (the
    /// interfaces never run here are absent). Read before a deployment replaces them, to tell when
    /// that may undo someone else's work.
    pub async fn current_versions(&self, names: &[&str]) -> Result<Vec<Current>> {
        type Row = (
            String,
            Vec<u8>,
            String,
            i32,
            DateTime<Utc>,
            String,
            String,
            String,
            Option<String>,
            Option<Vec<String>>,
            Option<Vec<u8>>,
        );
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT i.interface, v.hash, v.files::text, d.id, d.at, d.how, d.by_name, d.host, d.git_commit, d.uncommitted,
                    d.release_id
             FROM interface_versions i
             JOIN code_versions v ON v.id = i.version_id
             JOIN deployments d ON d.id = i.deployment_id
             WHERE i.interface = ANY($1)
             ORDER BY i.interface",
        )
        .bind(names)
        .fetch_all(&self.pool)
        .await
        .context("reading what the interfaces run")?;
        Ok(rows
            .into_iter()
            .map(|(interface, version, files, deployment, at, how, by, host, git_commit, uncommitted, release)| {
                Current {
                    interface,
                    version,
                    files: paths(&files),
                    deployment,
                    at,
                    how,
                    by,
                    host,
                    git_commit,
                    uncommitted,
                    release,
                }
            })
            .collect())
    }
}

/// An interface a deployment changed (`deployment_changes`), with the versions' hashes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Changed {
    pub interface: String,
    /// None: new here.
    pub from: Option<Vec<u8>>,
    pub to: Vec<u8>,
    /// The "replaces" check's warning (folder mode, or a forced deploy).
    pub warning: Option<String>,
}

impl Store {
    /// The first deployment in `environment` of `release` recorded at or after `since`, and what it
    /// changed: a deploy's answer reads it once the new engine has recorded it.
    pub async fn deployment_of_release(
        &self,
        environment: &str,
        release: &[u8],
        since: DateTime<Utc>,
    ) -> Result<Option<(i32, Vec<Changed>)>> {
        let id: Option<i32> = sqlx::query_scalar(
            "SELECT id FROM deployments WHERE environment = $1 AND release_id = $2 AND at >= $3 ORDER BY id LIMIT 1",
        )
        .bind(environment)
        .bind(release)
        .bind(since)
        .fetch_optional(&self.pool)
        .await
        .context("reading the deployment")?;
        let Some(id) = id else { return Ok(None) };
        type Row = (String, Option<Vec<u8>>, Vec<u8>, Option<String>);
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT c.interface, f.hash, t.hash, c.warning
             FROM deployment_changes c
             LEFT JOIN code_versions f ON f.id = c.from_version
             JOIN code_versions t ON t.id = c.to_version
             WHERE c.deployment_id = $1 ORDER BY c.interface",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await
        .context("reading what the deployment changed")?;
        let changed =
            rows.into_iter().map(|(interface, from, to, warning)| Changed { interface, from, to, warning }).collect();
        Ok(Some((id, changed)))
    }
}

/// The paths of a `code_versions.files` value (as text).
pub(crate) fn paths(files: &str) -> Vec<String> {
    let files: serde_json::Value = serde_json::from_str(files).unwrap_or_default();
    files
        .as_array()
        .map(|a| a.iter().filter_map(|f| f["path"].as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

/// The id of the set of `names` (`interface_sets`), saved the first time it's seen.
async fn interface_set(tx: &mut sqlx::PgConnection, names: &[&str]) -> Result<i32> {
    use sha2::Digest;
    let mut sorted = names.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut hash = sha2::Sha256::new();
    for name in &sorted {
        hash.update(name.as_bytes());
        hash.update(b"\n");
    }
    let hash = hash.finalize().to_vec();
    let find = || sqlx::query_scalar::<_, i32>("SELECT id FROM interface_sets WHERE hash = $1").bind(&hash);
    if let Some(id) = find().fetch_optional(&mut *tx).await.context("reading the set of interfaces")? {
        return Ok(id);
    }
    // Another engine may save the same set at once: whoever comes first, both read it back.
    sqlx::query("INSERT INTO interface_sets (hash, interfaces) VALUES ($1, $2) ON CONFLICT (hash) DO NOTHING")
        .bind(&hash)
        .bind(&sorted)
        .execute(&mut *tx)
        .await
        .context("saving the set of interfaces")?;
    find().fetch_one(&mut *tx).await.context("reading the set of interfaces saved")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
