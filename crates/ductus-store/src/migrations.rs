//! The migrations this binary carries and what each does to a database, for `ductus upgrade
//! --check`: which ones a start would apply, which tables they work through and how big those are
//! now, and what the database says it has already (newer ones, edited ones, one that failed).
//!
//! **Every migration file starts with its header** (from 0025 on):
//!
//! ```sql
//! -- Upgrade: adds `x` to every delivery (one line: what changes for the people running it).
//! -- Heavy: none
//! ```
//!
//! `Heavy:` names each existing table the migration works through row by row (an index built, a
//! column filled, rows copied) with what that does while it runs, one per line (`-- Heavy:
//! deliveries: builds an index; storing waits until it's built`), or says `none`. A test fails a
//! file without one. Applied migrations can't be given a header: sqlx checksums each file's whole
//! text and refuses a start when an applied one changed, so 0001-0024 are described in `EARLIER`,
//! which never grows.

use super::Store;
use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;

/// The migrator the engine runs at start.
pub(crate) fn migrator() -> sqlx::migrate::Migrator {
    sqlx::migrate!("./migrations")
}

/// The last migration written before headers: later ones must carry theirs.
const LAST_WITHOUT_HEADER: i64 = 24;

/// A migration's version, what it does, and the tables it works through with how (`Heavy`).
type Described = (i64, &'static str, &'static [(&'static str, &'static str)]);

/// What 0001-0024 do, written after the fact (their files can't change).
const EARLIER: [Described; LAST_WITHOUT_HEADER as usize] = [
    (1, "messages, partitioned by day", &[]),
    (2, "two engines can create the same partition at once", &[]),
    (3, "keys for duplicate detection", &[]),
    (4, "what file, SFTP and schedule sources remember between runs", &[]),
    (5, "a status per destination (deliveries)", &[]),
    (6, "partition maintenance in two steps (no deadlock with settling)", &[]),
    (7, "partition maintenance skips days dropped while it runs", &[]),
    (
        8,
        "API tokens; indexes for browsing messages and quarantines",
        &[
            ("messages", "builds two indexes; storing waits until they're built"),
            ("deliveries", "builds an index; storing and sending wait until it's built"),
        ],
    ),
    (9, "routed messages remember the message they came from (a column, nothing rewritten)", &[]),
    (
        10,
        "retention tiers: messages and deliveries partitioned again",
        &[
            ("messages", "every row copied into the new tables; receiving waits until it's done"),
            ("deliveries", "every row copied into the new tables; sending waits until it's done"),
        ],
    ),
    (
        11,
        "each destination's queue in message order (an index)",
        &[("deliveries", "builds an index; storing and sending wait until it's built")],
    ),
    (12, "last activity and pauses, kept across restarts", &[]),
    (13, "what's normal per hour of the week, per destination", &[]),
    (14, "the held area for messages kept past their retention", &[]),
    (15, "web UI sessions", &[]),
    (16, "what people did to messages (resend, edit)", &[]),
    (17, "the project's documentation fields, for SQL reports", &[]),
    (18, "people: accounts, roles, sessions; every existing API token is revoked (make new ones)", &[]),
    (19, "the audit log", &[]),
    (20, "alerts: state, events, the outbox", &[]),
    (21, "certificates the engine watches, and its connection checks", &[]),
    (22, "directory (LDAP) accounts", &[]),
    (23, "one engine per interface, and the lanes each interface last ran with", &[]),
    (24, "the engine's own certificates watched like the ones it connects to", &[]),
];

/// A table a migration works through row by row, and what that does while it runs.
#[derive(Debug, Clone, PartialEq)]
pub struct Heavy {
    pub table: String,
    pub how: String,
}

/// A migration this binary carries.
#[derive(Debug, Clone)]
pub struct Migration {
    pub version: i64,
    /// From the file name (`0023_engines.sql` → `engines`), as `_sqlx_migrations` records it.
    pub description: String,
    /// What it does, from its header.
    pub what: String,
    pub heavy: Vec<Heavy>,
    pub checksum: Vec<u8>,
}

/// A migration as `_sqlx_migrations` records it.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Applied {
    pub version: i64,
    pub description: String,
    /// False: it failed part-way without a transaction to undo it; every start refuses until
    /// someone looks.
    pub success: bool,
    pub checksum: Vec<u8>,
}

/// This binary's migrations against a database's.
#[derive(Debug, Default)]
pub struct Plan {
    /// What a start of this binary would apply, in order.
    pub pending: Vec<Migration>,
    /// Applied by a newer version than this one (fine: they only add).
    pub newer: Vec<Applied>,
    /// Applied, but the file this binary carries has other text: a start refuses.
    pub edited: Vec<Applied>,
    /// Failed part-way: a start refuses.
    pub failed: Vec<Applied>,
    /// The database has no migrations table yet: a new database.
    pub new_database: bool,
}

/// Every migration this binary carries, oldest first, with what it does.
pub fn embedded() -> Result<Vec<Migration>> {
    migrator()
        .iter()
        .map(|m| {
            let (what, heavy) = describe(m.version, m.sql.as_str())
                .with_context(|| format!("migration {:04}_{}", m.version, m.description))?;
            Ok(Migration {
                version: m.version,
                description: m.description.to_string(),
                what,
                heavy,
                checksum: m.checksum.to_vec(),
            })
        })
        .collect()
}

/// What a migration does: its header, or `EARLIER` for those written before headers.
fn describe(version: i64, sql: &str) -> Result<(String, Vec<Heavy>)> {
    if version <= LAST_WITHOUT_HEADER {
        let Some((_, what, heavy)) = EARLIER.iter().find(|(v, ..)| *v == version) else {
            bail!("not described in EARLIER");
        };
        let heavy = heavy.iter().map(|(t, h)| Heavy { table: t.to_string(), how: h.to_string() }).collect();
        return Ok((what.to_string(), heavy));
    }
    header(sql)
}

/// A migration file's header: `-- Upgrade: ...` on the first line, then one or more `-- Heavy:`.
pub fn header(sql: &str) -> Result<(String, Vec<Heavy>)> {
    const HELP: &str = "start the file with `-- Upgrade: <what it does>` and then `-- Heavy: none` or one \
                        `-- Heavy: <table>: <what it does while it runs>` per table it works through";
    let mut lines = sql.lines().map(str::trim_end);
    let what = lines
        .next()
        .and_then(|l| l.strip_prefix("-- Upgrade:"))
        .map(str::trim)
        .filter(|w| !w.is_empty())
        .with_context(|| format!("no `-- Upgrade:` line first: {HELP}"))?;
    let mut heavy = Vec::new();
    let mut none = false;
    for line in lines.map_while(|l| l.strip_prefix("-- Heavy:")).map(str::trim) {
        if line == "none" {
            none = true;
            continue;
        }
        let Some((table, how)) = line.split_once(':').map(|(t, h)| (t.trim(), h.trim())) else {
            bail!("`-- Heavy: {line}` names no table: write `-- Heavy: <table>: <what it does while it runs>`");
        };
        if table.is_empty() || how.is_empty() || table.contains(char::is_whitespace) {
            bail!("`-- Heavy: {line}`: write `-- Heavy: <table>: <what it does while it runs>`");
        }
        heavy.push(Heavy { table: table.to_string(), how: how.to_string() });
    }
    match (none, heavy.is_empty()) {
        (false, true) => bail!("no `-- Heavy:` line after `-- Upgrade:`: {HELP}"),
        (true, false) => bail!("`-- Heavy: none` and a table both: keep one"),
        _ => Ok((what.to_string(), heavy)),
    }
}

/// Compare what this binary carries with what a database has applied.
pub fn plan(embedded: Vec<Migration>, applied: Option<Vec<Applied>>) -> Plan {
    let Some(applied) = applied else {
        return Plan { pending: embedded, new_database: true, ..Plan::default() };
    };
    let ours: BTreeMap<i64, &Migration> = embedded.iter().map(|m| (m.version, m)).collect();
    let mut plan = Plan::default();
    for a in &applied {
        match ours.get(&a.version) {
            None => plan.newer.push(a.clone()),
            Some(_) if !a.success => plan.failed.push(a.clone()),
            Some(m) if m.checksum != a.checksum => plan.edited.push(a.clone()),
            Some(_) => {}
        }
    }
    let done: std::collections::BTreeSet<i64> = applied.iter().map(|a| a.version).collect();
    plan.pending = embedded.into_iter().filter(|m| !done.contains(&m.version)).collect();
    plan
}

impl Store {
    /// What `_sqlx_migrations` records, oldest first; None when the table doesn't exist (a new
    /// database).
    pub async fn applied_migrations(&self) -> Result<Option<Vec<Applied>>> {
        if !self.has_table("_sqlx_migrations").await? {
            return Ok(None);
        }
        let rows =
            sqlx::query_as("SELECT version, description, success, checksum FROM _sqlx_migrations ORDER BY version")
                .fetch_all(&self.pool)
                .await
                .context("reading the migrations applied")?;
        Ok(Some(rows))
    }

    /// A table's size on disk with its indexes, partitions included; None when it doesn't exist.
    pub async fn table_bytes(&self, table: &str) -> Result<Option<i64>> {
        sqlx::query_scalar(
            "SELECT CASE WHEN to_regclass($1) IS NOT NULL
                    THEN (SELECT coalesce(sum(pg_total_relation_size(relid)), 0)::bigint
                          FROM pg_partition_tree(to_regclass($1))) END",
        )
        .bind(table)
        .fetch_one(&self.pool)
        .await
        .with_context(|| format!("reading the size of {table}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_migration_says_what_it_does() {
        // A new migration file without its header fails here, naming the file and what to write.
        let all = embedded().unwrap();
        assert!(all.len() as i64 >= LAST_WITHOUT_HEADER);
        for m in &all {
            assert!(!m.what.is_empty(), "{}", m.version);
        }
        // EARLIER describes exactly the files written before headers, in order, and never grows.
        let versions: Vec<i64> = EARLIER.iter().map(|(v, ..)| *v).collect();
        assert_eq!(versions, (1..=LAST_WITHOUT_HEADER).collect::<Vec<_>>());
        assert_eq!(all.iter().filter(|m| m.version <= LAST_WITHOUT_HEADER).count(), EARLIER.len());
    }

    #[test]
    fn headers_are_read_and_wrong_ones_say_what_to_write() {
        let (what, heavy) = header("-- Upgrade: adds an index\n-- Heavy: none\n\nCREATE TABLE x ();").unwrap();
        assert_eq!((what.as_str(), heavy.len()), ("adds an index", 0));

        let sql = "-- Upgrade: queue order\n-- Heavy: deliveries: builds an index; storing waits\n\
                   -- Heavy: messages: fills a column\n-- More about it.\n";
        let (_, heavy) = header(sql).unwrap();
        assert_eq!(
            heavy,
            [
                Heavy { table: "deliveries".into(), how: "builds an index; storing waits".into() },
                Heavy { table: "messages".into(), how: "fills a column".into() },
            ]
        );

        for (sql, says) in [
            ("-- Indexes for the queue.\nCREATE INDEX ...", "no `-- Upgrade:` line first"),
            ("-- Upgrade: x\nCREATE INDEX ...", "no `-- Heavy:` line"),
            ("-- Upgrade: x\n-- Heavy: deliveries\n", "names no table"),
            ("-- Upgrade: x\n-- Heavy: the big table: slow\n", "write `-- Heavy: <table>"),
            ("-- Upgrade: x\n-- Heavy: none\n-- Heavy: messages: copies it\n", "keep one"),
        ] {
            let error = format!("{:#}", header(sql).unwrap_err());
            assert!(error.contains(says), "{sql:?}: {error}");
        }
    }

    fn migration(version: i64, checksum: u8) -> Migration {
        Migration {
            version,
            description: format!("m{version}"),
            what: String::new(),
            heavy: Vec::new(),
            checksum: vec![checksum],
        }
    }

    fn applied(version: i64, checksum: u8, success: bool) -> Applied {
        Applied { version, description: format!("m{version}"), success, checksum: vec![checksum] }
    }

    #[test]
    fn a_plan_compares_versions_and_checksums() {
        let ours = || vec![migration(1, 1), migration(2, 2), migration(3, 3)];

        let new = plan(ours(), None);
        assert!(new.new_database);
        assert_eq!(new.pending.len(), 3);

        let behind = plan(ours(), Some(vec![applied(1, 1, true)]));
        assert_eq!(behind.pending.iter().map(|m| m.version).collect::<Vec<_>>(), [2, 3]);
        assert!(behind.newer.is_empty() && behind.edited.is_empty() && behind.failed.is_empty());

        let odd = plan(
            ours(),
            Some(vec![applied(1, 9, true), applied(2, 2, false), applied(3, 3, true), applied(4, 4, true)]),
        );
        assert!(odd.pending.is_empty() && !odd.new_database);
        assert_eq!(odd.edited.iter().map(|a| a.version).collect::<Vec<_>>(), [1]);
        assert_eq!(odd.failed.iter().map(|a| a.version).collect::<Vec<_>>(), [2]);
        assert_eq!(odd.newer.iter().map(|a| a.version).collect::<Vec<_>>(), [4]);
    }
}
