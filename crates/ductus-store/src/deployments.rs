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
        let mut ids: VersionIds =
            sqlx::query_as::<_, (Vec<u8>, i32)>("SELECT hash, id FROM code_versions WHERE hash = ANY($1)")
                .bind(&hashes)
                .fetch_all(&mut *tx)
                .await
                .context("reading the code versions")?
                .into_iter()
                .collect();
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
            sqlx::query(
                "INSERT INTO code_versions (hash, interface, files)
                 SELECT hash, interface, files::jsonb FROM unnest($1::bytea[], $2::text[], $3::text[])
                     AS u(hash, interface, files) ORDER BY hash
                 ON CONFLICT (hash) DO NOTHING",
            )
            .bind(&hashes)
            .bind(&interfaces)
            .bind(&listed)
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
        let (id, at): (i32, DateTime<Utc>) = sqlx::query_as(
            "INSERT INTO deployments (environment, instance, host, version, how, by_name, reason, git_commit,
                                      uncommitted, interfaces, changed)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
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
        .fetch_one(&mut *tx)
        .await
        .context("saving the deployment")?;
        if !changed.is_empty() {
            let mut changed = changed.clone();
            changed.sort_by(|a, b| a.interface.cmp(b.interface));
            let interfaces: Vec<&str> = changed.iter().map(|v| v.interface).collect();
            let from: Vec<Option<i32>> = changed.iter().map(|v| current.get(v.interface).map(|(id, ..)| *id)).collect();
            let to: Vec<i32> = changed.iter().map(|v| ids[v.hash]).collect();
            sqlx::query(
                "INSERT INTO deployment_changes (deployment_id, interface, from_version, to_version)
                 SELECT $1, * FROM unnest($2::text[], $3::int[], $4::int[])",
            )
            .bind(id)
            .bind(&interfaces)
            .bind(&from)
            .bind(&to)
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
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
