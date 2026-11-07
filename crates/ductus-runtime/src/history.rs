//! One timeline of who changed what and why, for one interface (0.7.3.3): `ductus history`,
//! `GET /interfaces/{name}/history` and the interface page's History tab all read it here.
//!
//! Four sources, merged newest first: the git history of exactly the files the interface's code
//! versions named (its interface.yaml, its code and the shared files it imports, every version's:
//! a file moved or deleted is still asked about under its old path), read from `.git` with
//! gitoxide; the deployments that changed it (with `all`, every start that ran it); the audit log's
//! entries naming it; and its documentation's changes. Read a page at a time: each source gives
//! its newest rows up to the cursor's time, and the page is the newest of them all.
//!
//! Every entry says which parts of the interface it touches (0.7.4.2, `parts`): its source, which
//! destinations, or the whole interface only. A page can be narrowed to one part (`Query::part`):
//! the audit log and the documentation are narrowed by the database, deployments and commits here,
//! a deployment's batch after another until the page is full.

pub mod parts;

use crate::git;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use ductus_store::{AuditEntry, DeploymentEntry, DocsEntry, HistoryPage, Store, VersionFiles};
pub use parts::{Part, Touches};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::Path;
use utoipa::ToSchema;

/// Rows read from each source beyond the page, so entries at the cursor's own time that are
/// already shown can be dropped without making the page short.
const SPARE: usize = 5;

/// Deployments read at a time while a page of one part fills: most of an interface's changes may
/// be another part's.
const BATCH: usize = 200;

/// What to read.
#[derive(Debug, Clone)]
pub struct Query {
    pub interface: String,
    /// Entries older than this one (a previous page's `next_before`).
    pub before: Option<String>,
    pub limit: usize,
    /// Every start that ran it, not only those that changed it.
    pub all: bool,
    /// The audit log's reads (payloads) and refused attempts too, not only changes. The API allows
    /// it with the `audit` permission.
    pub reads: bool,
    /// Only what touches this part (its source, or one destination); None: everything.
    pub part: Option<Part>,
}

/// A part the interface doesn't have (`--destination` naming none of its destinations).
#[derive(Debug)]
pub struct UnknownPart(pub String);

impl std::fmt::Display for UnknownPart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for UnknownPart {}

/// Where an entry comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = HistoryKind)]
pub enum Kind {
    /// A git commit that changed one of its files.
    Commit,
    /// The audit log: someone (or the engine) acting on it.
    Audit,
    /// Its documentation fields changed.
    Documentation,
    /// An engine start or handover that changed its code (or ran it, with `all`).
    Deployment,
}

/// One entry of the timeline.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[schema(as = HistoryEntry)]
pub struct Entry {
    pub at: DateTime<Utc>,
    pub kind: Kind,
    /// Who: a commit's author, who deployed (or asked for the handover), who acted.
    pub by: String,
    /// What happened, in one line.
    pub what: String,
    /// This entry's place in the timeline: pass it as `before` to read what's older.
    pub key: String,
    /// Which parts of the interface it touches (none: the whole interface only).
    pub touches: Touches,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<CommitDetail>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deployment: Option<DeploymentDetail>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audit: Option<AuditDetail>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub documentation: Option<DocumentationDetail>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[schema(as = HistoryCommit)]
pub struct CommitDetail {
    /// The commit, in hex.
    pub id: String,
    pub email: String,
    /// The message's first line.
    pub summary: String,
    /// The interface's files it changed.
    pub files: Vec<FileChange>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[schema(as = HistoryFile)]
pub struct FileChange {
    /// Relative to the project, with `/`.
    pub path: String,
    #[schema(value_type = String, example = "modified")]
    /// `added`, `modified` or `deleted`.
    pub change: git::How,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[schema(as = HistoryDeployment)]
pub struct DeploymentDetail {
    pub id: i32,
    /// `start`, `service` or `handover`.
    pub how: String,
    pub reason: Option<String>,
    pub host: String,
    pub environment: String,
    /// The binary's version.
    pub version: String,
    /// The project's commit then (hex); none without git.
    pub git_commit: Option<String>,
    /// Files it ran that weren't the commit's; none without git.
    pub uncommitted: Option<Vec<String>>,
    /// It changed this interface's code (false: it ran it as it was; `all` only).
    pub changed: bool,
    /// The code version before (12 hex characters); none when new here, or unchanged.
    pub from: Option<String>,
    /// The code version from then; none when unchanged.
    pub to: Option<String>,
    /// Why this change may undo someone else's (folder mode), when it may.
    pub warning: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[schema(as = HistoryAudit)]
pub struct AuditDetail {
    pub id: i64,
    /// `destination.pause`, `message.replay`, …
    pub action: String,
    /// `ok`, `denied` or `failed`.
    pub outcome: String,
    pub subject: Option<String>,
    pub message: Option<i64>,
    #[schema(value_type = Option<Object>)]
    pub detail: Option<Value>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[schema(as = HistoryDocumentation)]
pub struct DocumentationDetail {
    /// The deployment that changed it; none for the snapshot taken at the upgrade to 0.7.3.3.
    pub deployment: Option<i32>,
    /// The interface left the project (or the first row, as the snapshot was at the upgrade).
    pub removed: bool,
    /// The fields that changed, each with its value before and after.
    pub fields: Vec<FieldChange>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[schema(as = HistoryField)]
pub struct FieldChange {
    /// `owner`, `custom.baa_signed`, `destinations.lab.custom.contact`.
    pub field: String,
    #[schema(value_type = Option<Object>)]
    pub before: Option<Value>,
    #[schema(value_type = Option<Object>)]
    pub after: Option<Value>,
}

/// A page of the timeline.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[schema(as = HistoryPage)]
pub struct Page {
    pub interface: String,
    /// Newest first.
    pub entries: Vec<Entry>,
    /// Pass as `before` for the next (older) page; absent on the last one.
    pub next_before: Option<String>,
    /// What the timeline lacks, in words: no git repository, a database not read, …
    pub notes: Vec<String>,
    /// The destinations it has or had (its configuration's, and every one an engine ran on this
    /// database), by name: the parts a history can be narrowed to, besides its source.
    pub destinations: Vec<String>,
}

/// Where `read` finds things.
pub struct Sources<'a> {
    /// None: the database isn't read (git only).
    pub store: Option<&'a Store>,
    /// The project's folder (its repository's working folder or inside it).
    pub root: &'a Path,
    /// Files asked about besides those the recorded versions name: what the project runs now.
    pub paths: BTreeSet<String>,
    /// What the project runs now, when it was loaded (the CLI): its files and parts count with the
    /// recorded versions'.
    pub current: Option<VersionFiles>,
    /// Its destinations now (its interface.yaml's).
    pub destinations: Vec<String>,
}

impl crate::code::Version {
    /// As the store reads versions back: its files and parts.
    pub fn files_and_parts(&self) -> VersionFiles {
        VersionFiles {
            files: self.files.iter().map(|f| (f.path.clone(), crate::code::hex(&f.hash))).collect(),
            parts: serde_json::to_string(&self.parts).ok(),
        }
    }
}

/// One page of `q.interface`'s timeline.
pub async fn read(sources: Sources<'_>, q: &Query) -> Result<Page> {
    let cursor = q.before.as_deref().map(Cursor::parse).transpose()?;
    let fetch = q.limit + SPARE;
    let page =
        HistoryPage { interface: &q.interface, until: cursor.as_ref().map(|c| c.at), limit: fetch as i64, below: None };
    let part = q.part.as_ref();
    let store_part = part.map(Part::store);
    let mut entries = Vec::new();
    let mut notes = Vec::new();
    let mut paths = sources.paths;
    let mut versions: Vec<VersionFiles> = sources.current.into_iter().collect();
    let mut destinations: BTreeSet<String> = sources.destinations.into_iter().collect();
    let mut more = false;
    let started = std::time::Instant::now();
    match sources.store {
        Some(store) => {
            // All at once (each a round trip); the part asked for is checked after: it must be one
            // it has or had (a typo would show an empty history).
            let ((deployed, deployments_more), audit, docs, recorded, names) = tokio::try_join!(
                self::deployments(store, page, q.all, part),
                store.history_audit(page, q.reads, store_part),
                store.history_docs(page, store_part),
                store.version_files(&q.interface),
                store.destination_names(&q.interface),
            )?;
            destinations.extend(names);
            check(part, &q.interface, &destinations)?;
            // A source that gave a full fetch may have more: then so does the timeline, even if
            // this page shows fewer (documentation rows that change nothing visible are left out).
            more |= deployments_more || [audit.len(), docs.len()].contains(&fetch);
            entries.extend(deployed);
            entries.extend(audit.into_iter().map(audit_entry));
            entries.extend(docs.into_iter().filter_map(|d| documentation(d, part)));
            paths.extend(recorded.iter().flat_map(|v| v.files.iter().map(|(p, _)| p.clone())));
            versions.extend(recorded);
        }
        None => {
            check(part, &q.interface, &destinations)?;
            notes.push(
                "no database read (DUCTUS_DATABASE_URL isn't set): only git's history, of the files the project runs now"
                    .into(),
            )
        }
    }
    let database = started.elapsed();
    let commits = std::time::Instant::now();
    if paths.is_empty() {
        notes.push("no files known for this interface: no git history".into());
    } else {
        let root = sources.root.to_path_buf();
        let walk = paths.clone();
        match tokio::task::spawn_blocking(move || git::history(&root, &walk)).await.context("reading git's history")? {
            None => notes.push("the project isn't in a git repository: no commits".into()),
            Some(Err(e)) => notes.push(format!("the project's git history couldn't be read: {e:#}")),
            Some(Ok(changes)) => {
                let owners = parts::Owners::of(&versions);
                // Commits of one second keep git's order (newest first): their place in the walk.
                let older = |(_, c): &(usize, &git::Change)| cursor.as_ref().is_none_or(|k| c.at <= k.at);
                let page: Vec<Entry> = changes
                    .iter()
                    .enumerate()
                    .filter(older)
                    .map(|(walked, c)| (walked, c, owners.commit(c)))
                    .filter(|(_, _, touches)| part.is_none_or(|p| touches.shows(p)))
                    .take(fetch)
                    .map(|(walked, c, touches)| commit((walked, c.clone()), touches))
                    .collect();
                more |= page.len() == fetch;
                entries.extend(page);
            }
        }
    }
    for v in &versions {
        if let Some(p) = v.parts.as_deref().and_then(|p| serde_json::from_str::<crate::code::Parts>(p).ok()) {
            destinations.extend(p.destinations.into_keys());
        }
    }
    if versions.iter().any(|v| v.parts.is_none()) && part.is_some() {
        notes.push(
            "some of its versions were recorded before 0.7.4.2, without which files each destination's code loads: \
             a change of their code is shown under every part, marked"
                .into(),
        );
    }
    tracing::debug!(
        interface = %q.interface,
        database = ?database,
        git = ?commits.elapsed(),
        files = paths.len(),
        "history read"
    );
    if let Some(cursor) = &cursor {
        entries.retain(|e| e.key.as_str() < cursor.key.as_str());
    }
    entries.sort_by(|a, b| b.key.cmp(&a.key));
    let next_before = if entries.len() > q.limit {
        Some(entries[q.limit - 1].key.clone())
    } else if more {
        entries.last().map(|e| e.key.clone())
    } else {
        None
    };
    entries.truncate(q.limit);
    let destinations = destinations.into_iter().collect();
    Ok(Page { interface: q.interface.clone(), entries, next_before, notes, destinations })
}

/// Refuse a destination the interface has never had, naming those it has.
fn check(part: Option<&Part>, interface: &str, known: &BTreeSet<String>) -> Result<()> {
    let Some(Part::Destination(name)) = part else { return Ok(()) };
    if known.contains(name) {
        return Ok(());
    }
    let have = match known.len() {
        0 => "none is known".to_string(),
        _ => format!("it has {}", known.iter().map(|n| format!("`{n}`")).collect::<Vec<_>>().join(", ")),
    };
    Err(UnknownPart(format!("`{interface}` has no destination `{name}`: {have}")).into())
}

/// The deployments for the page: the changes (with what each touches) and, with `all`, the starts
/// that ran it unchanged. Narrowed to `part`, the changes are read a batch at a time until the
/// page has enough of that part's (most of an interface's changes may be another part's). Each
/// version's interface.yaml is read from `code_files` when the two differ. True when there may be
/// more.
async fn deployments(
    store: &Store,
    page: HistoryPage<'_>,
    all: bool,
    part: Option<&Part>,
) -> Result<(Vec<Entry>, bool)> {
    let fetch = page.limit as usize;
    let batch = if part.is_some() { fetch.max(BATCH) } else { fetch };
    let mut out = Vec::new();
    let mut more = false;
    if all {
        let ran = store.history_runs(page).await?;
        more |= ran.len() == fetch;
        out.extend(ran.into_iter().map(|d| deployment(d, Touches { everything: true, ..Touches::default() })));
    }
    let mut below = None;
    let mut changes = 0;
    loop {
        let rows = store.history_deployments(HistoryPage { limit: batch as i64, below, ..page }, false).await?;
        let full = rows.len() == batch;
        below = rows.last().map(|d| i64::from(d.id));
        for d in rows {
            let touches = changed(&d);
            if part.is_none_or(|p| touches.shows(p)) {
                changes += 1;
                out.push(deployment(d, touches));
            }
        }
        if part.is_none() || changes >= fetch || !full {
            more |= full;
            break;
        }
    }
    out.sort_by(|a, b| b.key.cmp(&a.key));
    more |= out.len() > fetch;
    out.truncate(fetch);
    Ok((out, more))
}

/// What a deployment that changed the interface touches: everything when it's new here; else its
/// interface.yaml compared (the two texts come with the row when they differ) and its code by each
/// part's files.
fn changed(d: &DeploymentEntry) -> Touches {
    let (Some(from), Some(to)) = (&d.from, &d.to) else { return Touches { everything: true, ..Touches::default() } };
    let touches = parts::code(from, to);
    match (&d.from_yaml, &d.to_yaml) {
        (Some(a), Some(b)) => touches.with(parts::yaml(Some(a), Some(b))),
        (None, None) => touches,                  // the same interface.yaml
        _ => Touches { unsure: true, ..touches }, // never: a version's files are kept
    }
}

/// An entry's place: its time, then kind, then id, as one string that sorts as the timeline does.
struct Cursor {
    at: DateTime<Utc>,
    key: String,
}

impl Cursor {
    fn parse(key: &str) -> Result<Self> {
        let micros: i64 = key
            .split('-')
            .next()
            .and_then(|m| m.parse().ok())
            .with_context(|| format!("`{key}` isn't a place in the timeline (a page's `next_before`)"))?;
        let at = DateTime::from_timestamp_micros(micros).context("a time out of range")?;
        Ok(Self { at, key: key.to_string() })
    }
}

fn key(at: DateTime<Utc>, kind: Kind, id: &str) -> String {
    format!("{:017}-{}-{id}", at.timestamp_micros().max(0), kind as u8)
}

fn short(hash: &[u8]) -> String {
    crate::code::short(hash)
}

/// `walked`: its place in git's walk (0 the newest), which orders commits of the same second.
fn commit((walked, c): (usize, git::Change), touches: Touches) -> Entry {
    let files: Vec<String> = c.files.iter().map(|(p, h)| format!("{p} ({})", how(*h))).collect();
    Entry {
        at: c.at,
        kind: Kind::Commit,
        by: c.author.clone(),
        what: format!("committed {}: {} ({})", &c.id[..12.min(c.id.len())], c.summary, files.join(", ")),
        key: key(c.at, Kind::Commit, &format!("{:010}-{}", 9_999_999_999 - walked as u64, c.id)),
        touches,
        commit: Some(CommitDetail {
            id: c.id,
            email: c.email,
            summary: c.summary,
            files: c.files.into_iter().map(|(path, change)| FileChange { path, change }).collect(),
        }),
        deployment: None,
        audit: None,
        documentation: None,
    }
}

fn how(h: git::How) -> &'static str {
    match h {
        git::How::Added => "added",
        git::How::Modified => "modified",
        git::How::Deleted => "deleted",
    }
}

fn deployment(d: DeploymentEntry, touches: Touches) -> Entry {
    let (from, to) = (d.from_version.as_deref().map(short), d.to_version.as_deref().map(short));
    let mut what = match (&from, &to, d.changed) {
        (Some(f), Some(t), _) => format!("deployed {f} → {t}"),
        (None, Some(t), _) => format!("deployed {t} (new here)"),
        _ => format!("ran it unchanged ({})", d.how),
    };
    if d.changed && d.how == "handover" {
        what.push_str(" by a handover");
    }
    if let Some(reason) = &d.reason {
        what.push_str(&format!(": {reason}"));
    }
    if let Some(commit) = &d.git_commit {
        what.push_str(&format!(" (commit {}", &commit[..12.min(commit.len())]));
        match d.uncommitted.as_ref().map(Vec::len).unwrap_or_default() {
            0 => what.push(')'),
            n => what.push_str(&format!(", {n} file(s) not committed)")),
        }
    }
    if d.warning.is_some() {
        what.push_str(" ⚠");
    }
    Entry {
        at: d.at,
        kind: Kind::Deployment,
        by: d.by_name,
        what,
        key: key(d.at, Kind::Deployment, &format!("{:010}", d.id)),
        touches,
        commit: None,
        deployment: Some(DeploymentDetail {
            id: d.id,
            how: d.how,
            reason: d.reason,
            host: d.host,
            environment: d.environment,
            version: d.version,
            git_commit: d.git_commit,
            uncommitted: d.uncommitted,
            changed: d.changed,
            from,
            to,
            warning: d.warning,
        }),
        audit: None,
        documentation: None,
    }
}

/// What an audit row touches (as `Store::history_audit` narrows them): a pause, a resume or a
/// destination's alert by its subject; a resend, correction or dismissal by the destinations its
/// message actions name (none recorded: not known); raised lanes are the source's (`ordering`).
fn audit_touches(r: &AuditEntry) -> Touches {
    let row = &r.row;
    let interface = row.interface.as_deref().unwrap_or_default();
    let subject = row.subject.as_deref().unwrap_or_default();
    let mut touches = Touches::default();
    match row.action.as_str() {
        "destination.pause" | "destination.resume" => {
            if let Some(d) = subject.strip_prefix(interface).and_then(|s| s.strip_prefix('/')) {
                touches.destinations.push(d.to_string());
            }
        }
        a if a.starts_with("alert.") => {
            if let Some(d) = subject.strip_prefix("dest:").and_then(|s| s.strip_prefix(interface)?.strip_prefix('/')) {
                touches.destinations.push(d.to_string());
            }
        }
        "message.release" | "message.replay" | "message.edit" | "message.dismiss" => match r.acted_on.is_empty() {
            true => touches.unsure = true,
            false => touches.destinations = r.acted_on.clone(),
        },
        "interface.partitions_raised" => touches.source = true,
        _ => {}
    }
    touches
}

fn audit_entry(entry: AuditEntry) -> Entry {
    let touches = audit_touches(&entry);
    let r = entry.row;
    let detail: Option<Value> = r.detail.as_deref().and_then(|d| serde_json::from_str(d).ok());
    let subject = r.subject.clone().unwrap_or_default();
    let message = r.message_id.map(|m| format!("message {m}")).unwrap_or_else(|| "messages".into());
    let mut what = match r.action.as_str() {
        "destination.pause" => format!("paused {subject}"),
        "destination.resume" => format!("resumed {subject}"),
        "message.release" => format!("resent the quarantined deliveries of {message}"),
        "message.replay" => format!("sent {message} again"),
        "message.edit" => format!("corrected {message} and sent the copy"),
        "message.dismiss" => format!("dismissed {message}'s quarantine"),
        "message.payload.read" => format!("read {message}'s payload"),
        "retention.change" => "began applying a new retention".into(),
        "retention.confirm" => "confirmed a shorter retention".into(),
        "alert.ack" => format!("acknowledged an alert ({subject})"),
        "alert.silence" => format!("silenced an alert ({subject})"),
        "alert.unsilence" => format!("ended an alert's silence ({subject})"),
        other => format!("{other} {subject}").trim().to_string(),
    };
    if let Some(reason) = detail.as_ref().and_then(|d| d["reason"].as_str()).filter(|r| !r.is_empty()) {
        what.push_str(&format!(": {reason}"));
    }
    if r.outcome != "ok" {
        what = format!("{} ({})", what, r.outcome);
    }
    Entry {
        at: r.at,
        kind: Kind::Audit,
        by: r.actor_name,
        what,
        key: key(r.at, Kind::Audit, &format!("{:019}", r.id)),
        touches,
        commit: None,
        deployment: None,
        audit: Some(AuditDetail {
            id: r.id,
            action: r.action,
            outcome: r.outcome,
            subject: r.subject,
            message: r.message_id,
            detail,
        }),
        documentation: None,
    }
}

/// None when no field's value changed (only its spelling: `""` for absent, say). Narrowed to a
/// destination, only its fields (`destinations.<name>.…`); the rest are the whole interface's.
fn documentation(d: DocsEntry, part: Option<&Part>) -> Option<Entry> {
    let parse = |t: &Option<String>| t.as_deref().and_then(|t| serde_json::from_str::<Value>(t).ok());
    let (before, after) = (parse(&d.before), parse(&d.docs));
    let mut fields = Vec::new();
    diff("", before.as_ref(), after.as_ref(), &mut fields);
    fn destination(field: &str) -> Option<&str> {
        field.strip_prefix("destinations.").and_then(|f| f.split('.').next())
    }
    match part {
        None => {}
        Some(Part::Source) => return None,
        Some(Part::Destination(name)) => {
            fields.retain(|f| destination(&f.field) == Some(name.as_str()));
            if fields.is_empty() {
                return None;
            }
        }
    }
    let removed = d.docs.is_none();
    let mut touches = Touches { everything: removed, ..Touches::default() };
    for f in &fields {
        if let Some(name) = destination(&f.field)
            && !touches.destinations.iter().any(|d| d == name)
        {
            touches.destinations.push(name.to_string());
        }
    }
    touches.destinations.sort();
    let what = if removed {
        "documentation removed: the interface left the project".to_string()
    } else if d.before.is_none() && d.deployment_id.is_none() {
        "documentation as it was at the upgrade to 0.7.3.3".to_string()
    } else if fields.is_empty() {
        return None;
    } else {
        // New here: what it says; changed: from what to what.
        let added = d.before.is_none();
        let shown: Vec<String> = fields
            .iter()
            .take(3)
            .map(|f| match added {
                true => format!("{}: {}", f.field, value(&f.after)),
                false => format!("{}: {} → {}", f.field, value(&f.before), value(&f.after)),
            })
            .collect();
        let more = fields.len().saturating_sub(3);
        format!(
            "documentation {}: {}{}",
            if added { "added" } else { "changed" },
            shown.join("; "),
            if more > 0 { format!(" and {more} more") } else { String::new() }
        )
    };
    Some(Entry {
        at: d.at,
        kind: Kind::Documentation,
        by: d.by.unwrap_or_default(),
        what,
        key: key(d.at, Kind::Documentation, &format!("{:019}", d.id)),
        touches,
        commit: None,
        deployment: None,
        audit: None,
        documentation: Some(DocumentationDetail { deployment: d.deployment_id, removed, fields }),
    })
}

/// A value in a line of text: strings as they are, `–` for none.
fn value(v: &Option<Value>) -> String {
    match v {
        None | Some(Value::Null) => "–".into(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

/// The leaves that differ between `a` and `b`, by their dotted path.
fn diff(path: &str, a: Option<&Value>, b: Option<&Value>, out: &mut Vec<FieldChange>) {
    let join = |k: &str| if path.is_empty() { k.to_string() } else { format!("{path}.{k}") };
    match (a, b) {
        (Some(Value::Object(x)), Some(Value::Object(y))) => {
            let keys: BTreeSet<&String> = x.keys().chain(y.keys()).collect();
            for k in keys {
                diff(&join(k), x.get(k), y.get(k), out);
            }
        }
        (Some(Value::Object(x)), None) => {
            for (k, v) in x {
                diff(&join(k), Some(v), None, out);
            }
        }
        (None, Some(Value::Object(y))) => {
            for (k, v) in y {
                diff(&join(k), None, Some(v), out);
            }
        }
        (x, y) => {
            // Absent, null and "" are all "no value".
            let some = |v: &&Value| !v.is_null() && v.as_str() != Some("");
            let (x, y) = (x.filter(some), y.filter(some));
            if x != y {
                out.push(FieldChange { field: path.to_string(), before: x.cloned(), after: y.cloned() });
            }
        }
    }
}

/// The timeline as text, one entry per line (`ductus history`), each ending with the parts it
/// touches (`[source, lab]`), or, narrowed to `part`, a mark on those that may not be its.
pub fn text(page: &Page, part: Option<&Part>) -> String {
    let mut out = String::new();
    for e in &page.entries {
        let kind = match e.kind {
            Kind::Commit => "commit",
            Kind::Deployment => "deployed",
            Kind::Audit => "audit",
            Kind::Documentation => "docs",
        };
        let by = if e.by.is_empty() { String::new() } else { format!("{}  ", e.by) };
        let t = &e.touches;
        let parts = match part {
            None if t.any() && !(t.everything && e.kind == Kind::Deployment) => format!("  [{}]", t.words()),
            Some(p) if t.unsure && !t.surely(p) => {
                format!("  [may not be {}: which part its code is wasn't recorded before 0.7.4.2]", p.label())
            }
            _ => String::new(),
        };
        out.push_str(&format!("{}  {kind:<8}  {by}{}{parts}\n", e.at.format("%Y-%m-%d %H:%M"), e.what));
        if let Some(w) = e.deployment.as_ref().and_then(|d| d.warning.as_deref()) {
            out.push_str(&format!("{:18}⚠ {w}\n", ""));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documentation_changes_are_named_by_field() {
        let before = serde_json::json!({"owner": "ann", "custom": {"baa_signed": "no", "tier": 1},
                                        "destinations": {"lab": {"custom": {"contact": "x"}}}});
        let after = serde_json::json!({"owner": "ann", "custom": {"baa_signed": "yes"},
                                       "destinations": {"lab": {"custom": {"contact": "y"}}, "new": {"custom": {}}}});
        let mut out = Vec::new();
        diff("", Some(&before), Some(&after), &mut out);
        let fields: Vec<(&str, String, String)> =
            out.iter().map(|f| (f.field.as_str(), value(&f.before), value(&f.after))).collect();
        assert_eq!(
            fields,
            [
                ("custom.baa_signed", "no".into(), "yes".into()),
                ("custom.tier", "1".into(), "–".into()),
                ("destinations.lab.custom.contact", "x".into(), "y".into()),
            ],
            "an empty object added has no leaf"
        );
    }

    /// Keys sort as the timeline: newer first whatever the kind, and at one time a deployment
    /// before what it brought.
    #[test]
    fn keys_sort_as_the_timeline() {
        let t = |s: i64| DateTime::from_timestamp(1_800_000_000 + s, 0).unwrap();
        let mut keys = [
            key(t(0), Kind::Commit, "ab"),
            key(t(5), Kind::Audit, &format!("{:019}", 3)),
            key(t(5), Kind::Deployment, &format!("{:010}", 12)),
            key(t(5), Kind::Documentation, &format!("{:019}", 99)),
            key(t(9), Kind::Deployment, &format!("{:010}", 2)),
        ];
        keys.sort_by(|a, b| b.cmp(a));
        let order: Vec<char> = keys.iter().map(|k| k.split('-').nth(1).unwrap().chars().next().unwrap()).collect();
        assert_eq!(order, ['3', '3', '2', '1', '0']);
        let cursor = Cursor::parse(&keys[1]).unwrap();
        assert_eq!(cursor.at, t(5));
        assert!(Cursor::parse("yesterday").is_err());
    }
}
