//! File and SFTP sources: watch a folder, store each file (or each record in it), then move or
//! delete the file.
//!
//! Persist before acknowledge, for files: a file is moved or deleted only after everything in it
//! is committed. Records are stored in batches, each committed together with the byte offset it
//! reached (`source_state`), so a crash mid-file resumes at that offset with no record lost or
//! stored twice. The last batch also marks the file done, so a crash before the move doesn't
//! read the file again either.

use crate::gate::Closed;
use crate::ingest::Intake;
use crate::sftp::{self, Connection};
use crate::split::{Records, Splitter};
use anyhow::{Context, Result, bail};
use ductus_core::config::{After, FileSource, Source, Split};
use ductus_core::format::Format;
use ductus_core::resources::SftpServer;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeek, BufReader};

/// Records per transaction: big enough that commits don't dominate, small enough that a crash
/// re-reads little.
const BATCH_RECORDS: usize = 500;
const BATCH_BYTES: usize = 4 << 20;
/// Unsplit files become one message each; Postgres caps a value at 1 GB, and messages that big
/// would hurt everything else. Bigger files are left in place with an error.
const MAX_WHOLE_FILE: u64 = 256 << 20;
/// How long a repeated pickup error stays out of the log.
const QUIET: std::time::Duration = std::time::Duration::from_secs(60);

pub trait ReadSeek: AsyncRead + AsyncSeek + Unpin + Send {}
impl<T: AsyncRead + AsyncSeek + Unpin + Send> ReadSeek for T {}

#[derive(Debug, Clone)]
struct Entry {
    name: String,
    size: u64,
    /// Modification time, nanoseconds since 1970 (SFTP has seconds only).
    modified: i64,
}

/// Where the files are: a local folder or an SFTP server.
enum Backend {
    Local { dir: PathBuf, archive: PathBuf },
    Sftp { connection: Box<Connection>, dir: String, archive: String },
}

impl Backend {
    async fn list(&mut self) -> Result<Vec<Entry>> {
        let mut entries = Vec::new();
        match self {
            Backend::Local { dir, .. } => {
                let mut read =
                    tokio::fs::read_dir(&*dir).await.with_context(|| format!("reading {}", dir.display()))?;
                while let Some(e) = read.next_entry().await? {
                    let Ok(meta) = e.metadata().await else { continue }; // deleted meanwhile
                    if !meta.is_file() {
                        continue;
                    }
                    let modified = meta
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map_or(0, |d| d.as_nanos() as i64);
                    entries.push(Entry {
                        name: e.file_name().to_string_lossy().into_owned(),
                        size: meta.len(),
                        modified,
                    });
                }
            }
            Backend::Sftp { connection, dir, .. } => {
                for e in
                    connection.sftp().await?.read_dir(dir.as_str()).await.with_context(|| format!("reading {dir}"))?
                {
                    if !e.file_type().is_file() {
                        continue;
                    }
                    let meta = e.metadata();
                    let modified = i64::from(meta.mtime.unwrap_or(0)) * 1_000_000_000;
                    entries.push(Entry { name: e.file_name(), size: meta.size.unwrap_or(0), modified });
                }
            }
        }
        Ok(entries)
    }

    async fn open(&mut self, name: &str) -> Result<Box<dyn ReadSeek>> {
        Ok(match self {
            Backend::Local { dir, .. } => Box::new(tokio::fs::File::open(dir.join(name)).await?),
            Backend::Sftp { connection, dir, .. } => {
                Box::new(connection.sftp().await?.open(sftp::join(dir, name)).await?)
            }
        })
    }

    /// Move a processed file into the archive folder (created if needed). A file of the same
    /// name already there is kept: the new one gets a timestamp added to its name.
    async fn archive(&mut self, name: &str) -> Result<()> {
        let stamp = || chrono::Local::now().format("%Y%m%d%H%M%S%3f").to_string();
        match self {
            Backend::Local { dir, archive } => {
                tokio::fs::create_dir_all(&*archive)
                    .await
                    .with_context(|| format!("creating {}", archive.display()))?;
                let mut target = archive.join(name);
                if tokio::fs::try_exists(&target).await? {
                    target = archive.join(format!("{name}.{}", stamp()));
                }
                tokio::fs::rename(dir.join(name), &target)
                    .await
                    .with_context(|| format!("moving {name} to the archive"))
            }
            Backend::Sftp { connection, dir, archive } => {
                let sftp = connection.sftp().await?;
                if !sftp.try_exists(archive.as_str()).await? {
                    sftp.create_dir(archive.as_str()).await.with_context(|| format!("creating {archive}"))?;
                }
                let mut target = sftp::join(archive, name);
                if sftp.try_exists(&target).await? {
                    target = sftp::join(archive, &format!("{name}.{}", stamp()));
                }
                sftp.rename(sftp::join(dir, name), target)
                    .await
                    .with_context(|| format!("moving {name} to the archive"))
            }
        }
    }

    async fn delete(&mut self, name: &str) -> Result<()> {
        match self {
            Backend::Local { dir, .. } => tokio::fs::remove_file(dir.join(name)).await?,
            Backend::Sftp { connection, dir, .. } => {
                connection.sftp().await?.remove_file(sftp::join(dir, name)).await?
            }
        }
        Ok(())
    }

    fn reset(&mut self) {
        if let Backend::Sftp { connection, .. } = self {
            connection.reset();
        }
    }
}

/// Progress through one file, saved with each batch.
#[derive(Debug, Default, Serialize, Deserialize)]
struct FileState {
    size: u64,
    modified: i64,
    /// Byte offset reached: everything before it is stored.
    offset: u64,
    records: u64,
    /// Everything is stored; only the move/delete is left.
    done: bool,
}

pub struct FileWatcher {
    intake: Arc<Intake>,
    source: FileSource,
    backend: Backend,
    splitter: Option<Splitter>,
    /// Size and time of each candidate at the last look: unchanged across two looks = ready.
    last_seen: HashMap<String, (u64, i64)>,
    /// Files that can't be processed (too big), reported once and left in place.
    refused: HashSet<(String, u64)>,
    /// Whether this engine holds its interfaces: pickup waits while it doesn't (`ownership`).
    ownership: Arc<crate::ownership::Ownership>,
}

impl FileWatcher {
    /// For a `file` or `sftp` source. `server` is the SFTP server from resources.yaml.
    pub fn new(
        intake: Arc<Intake>,
        server: Option<SftpServer>,
        ownership: Arc<crate::ownership::Ownership>,
    ) -> Result<Self> {
        let config = intake.config.clone();
        let (Source::File(source) | Source::Sftp(source)) = &config.source else {
            bail!("interface `{}` doesn't have a file source", config.name)
        };
        let backend = match (&config.source, server) {
            (Source::Sftp(_), Some(server)) => Backend::Sftp {
                connection: Box::new(Connection::new(source.server.as_deref().unwrap_or_default(), server)),
                dir: source.dir.clone(),
                archive: source.archive(),
            },
            (Source::Sftp(_), None) => bail!("interface `{}`: SFTP server not found", config.name),
            _ => Backend::Local { dir: config.dir.join(&source.dir), archive: config.dir.join(source.archive()) },
        };
        let splitter = (source.split == Split::Records).then(|| match &config.format {
            Format::Hl7 => Splitter::Hl7,
            Format::Csv(options) => Splitter::Csv { header: options.header },
            _ => Splitter::Lines,
        });
        Ok(Self {
            intake,
            source: source.clone(),
            backend,
            splitter,
            last_seen: HashMap::new(),
            refused: HashSet::new(),
            ownership,
        })
    }

    fn name(&self) -> &str {
        &self.intake.config.name
    }

    pub async fn run(mut self) {
        let what = match &self.backend {
            Backend::Local { dir, .. } => dir.display().to_string(),
            Backend::Sftp { connection: _, dir, .. } => format!("sftp:{dir}"),
        };
        tracing::debug!(interface = %self.name(), dir = %what, pattern = %self.source.pattern, "watching for files");
        let mut ticker = tokio::time::interval(self.source.poll);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // The same error on every look (server down, wrong password) is logged once a minute.
        let mut last_error: Option<(String, std::time::Instant)> = None;
        loop {
            ticker.tick().await;
            if !self.intake.gate.is_open() {
                // Draining: a file being read stopped between batches, its progress saved.
                self.intake.gate.opened().await;
                continue;
            }
            if !self.ownership.is_held() {
                // Cut off from the database: another engine may be picking these files up soon.
                self.ownership.until_held().await;
                continue;
            }
            match self.poll().await {
                Ok(()) => {
                    if last_error.take().is_some() {
                        tracing::info!(interface = %self.name(), "file pickup working again");
                    }
                }
                Err(e) if e.root_cause().is::<Closed>() => {}
                Err(e) => {
                    // Nothing is lost: the file stays where it is and its progress is saved.
                    let error = format!("{e:#}");
                    let repeat = last_error.as_ref().is_some_and(|(last, at)| *last == error && at.elapsed() < QUIET);
                    if !repeat {
                        tracing::warn!(interface = %self.name(), %error, "file pickup failed; will retry");
                        last_error = Some((error, std::time::Instant::now()));
                    }
                    self.backend.reset();
                }
            }
        }
    }

    /// One look at the folder: process every file that's ready, oldest first.
    async fn poll(&mut self) -> Result<()> {
        let entries = self.backend.list().await?;
        let names: HashSet<String> = entries.iter().map(|e| e.name.clone()).collect();
        let marker = self.source.marker.clone();
        let mut seen = HashMap::new();
        let mut ready = Vec::new();
        for e in entries {
            if e.name.starts_with('.')
                || marker.as_deref().is_some_and(|m| e.name.ends_with(m))
                || !ductus_core::glob(&self.source.pattern, &e.name)
            {
                continue;
            }
            let is_ready = match &marker {
                Some(m) => names.contains(&format!("{}{m}", e.name)),
                None => self.last_seen.get(&e.name) == Some(&(e.size, e.modified)),
            };
            seen.insert(e.name.clone(), (e.size, e.modified));
            if is_ready {
                ready.push(e);
            }
        }
        self.last_seen = seen;
        ready.sort_by(|a, b| (a.modified, &a.name).cmp(&(b.modified, &b.name)));
        for e in ready {
            if self.refused.contains(&(e.name.clone(), e.size)) {
                continue;
            }
            // Stop at the first failure: later files wait, so files are taken in order.
            self.process(&e).await.with_context(|| format!("file {}", e.name))?;
            self.last_seen.remove(&e.name);
        }
        Ok(())
    }

    async fn process(&mut self, e: &Entry) -> Result<()> {
        let key = format!("file:{}", e.name);
        let store = self.intake.store().clone();
        let saved: Option<FileState> = match store.source_state(self.name(), &key).await? {
            Some(json) => Some(serde_json::from_str(&json).context("reading saved file progress")?),
            None => None,
        };
        let mut state = match saved {
            Some(s) if (s.size, s.modified) == (e.size, e.modified) => {
                if s.offset > 0 && !s.done {
                    tracing::info!(interface = %self.name(), file = %e.name, offset = s.offset, records = s.records, "resuming a partly read file");
                }
                s
            }
            Some(_) => {
                tracing::warn!(interface = %self.name(), file = %e.name, "file changed since it was partly read; reading it from the start");
                FileState { size: e.size, modified: e.modified, ..Default::default() }
            }
            None => FileState { size: e.size, modified: e.modified, ..Default::default() },
        };

        if !state.done {
            let started = std::time::Instant::now();
            let stored = match self.splitter {
                Some(splitter) => self.read_records(e, splitter, &key, &mut state).await?,
                None => match self.read_whole(e, &key, &mut state).await? {
                    Some(n) => n,
                    None => return Ok(()),
                },
            };
            let seconds = started.elapsed().as_secs_f64();
            tracing::info!(
                interface = %self.name(), file = %e.name, records = state.records, queued = stored,
                seconds = format!("{seconds:.2}"), "file stored"
            );
        }

        match self.source.after {
            After::Move => self.backend.archive(&e.name).await?,
            After::Delete => self.backend.delete(&e.name).await?,
        }
        if let Some(marker) = &self.source.marker {
            self.backend.delete(&format!("{}{marker}", e.name)).await?;
        }
        store.delete_source_state(self.name(), &key).await?;
        Ok(())
    }

    /// The whole file as one message. None when the file was refused (too big).
    async fn read_whole(&mut self, e: &Entry, key: &str, state: &mut FileState) -> Result<Option<usize>> {
        if e.size > MAX_WHOLE_FILE {
            tracing::error!(
                interface = %self.name(), file = %e.name, size = e.size,
                "file is over {} MB; left in place. Use `split: records` to read it record by record",
                MAX_WHOLE_FILE >> 20
            );
            self.refused.insert((e.name.clone(), e.size));
            return Ok(None);
        }
        let mut data = Vec::with_capacity(e.size as usize);
        self.backend.open(&e.name).await?.read_to_end(&mut data).await?;
        let payloads = if data.iter().all(u8::is_ascii_whitespace) { vec![] } else { vec![data] };
        state.offset = e.size;
        state.records = payloads.len() as u64;
        state.done = true;
        let queued = self.intake.accept_batch(&payloads, Some((key, &serde_json::to_string(state)?))).await?;
        Ok(Some(queued))
    }

    async fn read_records(&mut self, e: &Entry, splitter: Splitter, key: &str, state: &mut FileState) -> Result<usize> {
        let reader = BufReader::with_capacity(256 << 10, self.backend.open(&e.name).await?);
        let mut records = Records::open(reader, splitter, state.offset).await?;
        let mut batch = Vec::with_capacity(BATCH_RECORDS);
        let (mut bytes, mut queued) = (0, 0);
        loop {
            let record = records.next().await?;
            let end = record.is_none();
            if let Some(r) = record {
                bytes += r.payload.len();
                batch.push(r.payload);
                state.offset = r.end;
            }
            if end || batch.len() >= BATCH_RECORDS || bytes >= BATCH_BYTES {
                state.records += batch.len() as u64;
                state.done = end;
                queued += self.intake.accept_batch(&batch, Some((key, &serde_json::to_string(state)?))).await?;
                batch.clear();
                bytes = 0;
            }
            if end {
                return Ok(queued);
            }
        }
    }
}
