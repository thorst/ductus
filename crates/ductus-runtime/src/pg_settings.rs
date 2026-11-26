//! What `ductus doctor` reads from the engine's Postgres besides timing its commits: the settings
//! that decide whether a commit is durable, how much the server caches, whether there are
//! connections for an engine and a handover, and whether old rows are cleaned up. One
//! `SELECT … FROM pg_settings` and one count of `pg_stat_activity`, on doctor's own connection.
//!
//! Each finding is a `DOC` code ([`CODES`], the reference's "Doctor findings" page), said in two
//! kinds: a **warning** where the setting risks messages or a start (`fsync` off, no
//! connections left), a **note** where it's tuning (`shared_buffers` at Postgres's default).
//! The advice only says what the setting is and what it's usually set to; it never invents a
//! number for the database's server, whose RAM and disks doctor can't see unless it runs there.

use sqlx::postgres::PgConnection;

/// The settings read, by name.
pub const NAMES: &[&str] = &[
    "synchronous_commit",
    "fsync",
    "wal_sync_method",
    "commit_delay",
    "commit_siblings",
    "shared_buffers",
    "effective_cache_size",
    "max_connections",
    "superuser_reserved_connections",
    "reserved_connections",
    "random_page_cost",
    "effective_io_concurrency",
    "autovacuum",
];

/// The engine's pooled connections when `--db-connections` (`DUCTUS_DB_CONNECTIONS`) isn't set.
pub const DEFAULT_DB_CONNECTIONS: u32 = 32;

/// Postgres's own default `shared_buffers` (128 MB) and `effective_cache_size` (4 GB).
const DEFAULT_SHARED_BUFFERS: u64 = 128 << 20;
const DEFAULT_CACHE_SIZE: u64 = 4 << 30;

/// One of doctor's `DOC` codes, for the reference page.
pub struct Code {
    pub code: &'static str,
    /// `warning` (a risk to messages or a start) or `note` (tuning).
    pub severity: &'static str,
    pub title: &'static str,
    /// When it's reported and what to do.
    pub about: &'static str,
}

const fn code(code: &'static str, severity: &'static str, title: &'static str, about: &'static str) -> Code {
    Code { code, severity, title, about }
}

/// Every `DOC` code doctor reports. A code keeps its meaning once released: a new check gets a
/// new code, and a retired one's number isn't reused.
pub const CODES: &[Code] = &[
    code(
        "DOC001",
        "warning",
        "`fsync` is off",
        "Postgres doesn't flush its writes to the disk, so a crash of the database's server (power, the kernel) can corrupt the database, not just lose its last moments. Off is for throwaway test databases only: set `fsync = on` in `postgresql.conf` and reload.",
    ),
    code(
        "DOC002",
        "warning",
        "`synchronous_commit` is off",
        "Commits don't wait for the disk, so a strict interface's ACK no longer means the message is on disk: a crash of the database's server can lose the last moments' messages (up to three times `wal_writer_delay`, 600 ms by default), as a relaxed interface can. Set it `on` for the engine's login (`ALTER ROLE ductus SET synchronous_commit = on`), and use `durability: relaxed` for the interfaces that can afford it instead.",
    ),
    code(
        "DOC003",
        "note",
        "`commit_delay` is set",
        "Each commit waits up to `commit_delay` microseconds for other transactions to share its disk flush, when at least `commit_siblings` are open. It can raise what many strict sender connections store a second, and adds up to that wait to each ACK. Whether it helps is the disk's: compare `ductus bench` with it and without (`ductus bench compare`).",
    ),
    code(
        "DOC004",
        "note",
        "`shared_buffers` is Postgres's default",
        "`shared_buffers` (the memory Postgres caches the database's pages in) is 128 MB or less, Postgres's default, sized to start on any machine. On a database server of its own, about a quarter of its RAM is usual (a restart applies it); doctor can't see that server's RAM, so the number is yours to work out.",
    ),
    code(
        "DOC005",
        "note",
        "`effective_cache_size` is Postgres's default",
        "`effective_cache_size` is 4 GB, Postgres's default: the planner's guess of how much of the database the server's memory caches (Postgres's and the operating system's). It allocates nothing. On a database server of its own, half to three quarters of its RAM is usual.",
    ),
    code(
        "DOC006",
        "warning",
        "Too few connections for a handover",
        "`max_connections`, less the ones reserved for superusers (`superuser_reserved_connections`, `reserved_connections`), is below what two engines open: each opens up to its `--db-connections` (32 by default) and one for its lock, and a handover runs the old and the new engine for a moment. An engine that can't get a connection waits, then fails. Raise `max_connections` (a restart applies it), or lower `--db-connections`.",
    ),
    code(
        "DOC007",
        "warning",
        "Few connections are free now",
        "So many connections to the database's server are open now (all its databases count) that a starting engine, or a handover's new one, wouldn't get all it opens. See who holds them in `pg_stat_activity`; raise `max_connections` or close the idle ones (a pooler such as PgBouncer in front of other applications).",
    ),
    code(
        "DOC008",
        "note",
        "`random_page_cost` is priced for spinning disks",
        "`random_page_cost` is 4, Postgres's default, priced for hard disks: the planner then avoids indexes it would gladly use on SSD or NVMe, where 1.1 is usual. Leave it on hard disks.",
    ),
    code(
        "DOC009",
        "note",
        "`effective_io_concurrency` is 1 or less",
        "Postgres reads ahead one page at a time where it could ask for several at once (bitmap scans, and from Postgres 18 sequential reads too). On SSD or NVMe, 200 is usual; Postgres 18's default is 16.",
    ),
    code(
        "DOC010",
        "warning",
        "`autovacuum` is off",
        "Deliveries and messages are updated as they move, and without autovacuum their old row versions aren't cleaned up: tables and indexes grow and the engine's queries slow over days (retention drops whole partitions, but the current ones still fill). Set `autovacuum = on` and reload.",
    ),
];

/// What was read: each setting as `pg_settings` gives it, and the connections open now.
#[derive(Debug, Default)]
pub struct Read {
    /// `(name, setting, unit)`.
    pub settings: Vec<(String, String, Option<String>)>,
    /// Client connections open to the server now (every database's).
    pub in_use: i64,
    /// Connections one engine opens: its pool and its lock's.
    pub per_engine: u32,
}

/// A finding: its code, whether it's a warning, and what it says (the setting's value in it).
#[derive(Debug, PartialEq)]
pub struct Finding {
    pub code: &'static str,
    pub warning: bool,
    pub text: String,
}

/// Read the settings and the connections in use, on doctor's connection.
pub async fn read(connection: &mut PgConnection) -> Result<Read, sqlx::Error> {
    let settings: Vec<(String, String, Option<String>)> =
        sqlx::query_as("SELECT name, setting, unit FROM pg_settings WHERE name = ANY($1)")
            .bind(NAMES)
            .fetch_all(&mut *connection)
            .await?;
    let in_use: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE backend_type = 'client backend'")
        .fetch_one(&mut *connection)
        .await?;
    Ok(Read { settings, in_use, per_engine: per_engine() })
}

/// What one engine opens: `DUCTUS_DB_CONNECTIONS` (or its default) and the lock's.
fn per_engine() -> u32 {
    let pooled = std::env::var("DUCTUS_DB_CONNECTIONS").ok().and_then(|v| v.trim().parse().ok());
    pooled.unwrap_or(DEFAULT_DB_CONNECTIONS) + 1
}

impl Read {
    fn raw(&self, name: &str) -> Option<&str> {
        self.settings.iter().find(|(n, _, _)| n == name).map(|(_, s, _)| s.as_str())
    }

    fn number(&self, name: &str) -> Option<f64> {
        self.raw(name)?.parse().ok()
    }

    /// A memory setting in bytes (`pg_settings` gives `16384` with the unit `8kB`).
    fn bytes(&self, name: &str) -> Option<u64> {
        let (_, setting, unit) = self.settings.iter().find(|(n, _, _)| n == name)?;
        let value: u64 = setting.parse().ok()?;
        let unit = match unit.as_deref().unwrap_or("B") {
            "B" => 1,
            "kB" => 1 << 10,
            "8kB" => 8 << 10,
            "16kB" => 16 << 10,
            "32kB" => 32 << 10,
            "MB" => 1 << 20,
            "GB" => 1 << 30,
            _ => return None,
        };
        Some(value * unit)
    }

    /// Connections a login like the engine's may open: `max_connections` less the reserved ones.
    fn open_to_us(&self) -> Option<i64> {
        let max = self.number("max_connections")? as i64;
        let reserved = ["superuser_reserved_connections", "reserved_connections"]
            .iter()
            .filter_map(|n| self.number(n))
            .sum::<f64>() as i64;
        Some(max - reserved)
    }

    /// The line doctor prints: each setting that matters here, as set.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        for name in ["synchronous_commit", "fsync", "wal_sync_method"] {
            if let Some(v) = self.raw(name) {
                parts.push(format!("{name} {v}"));
            }
        }
        if let Some(us) = self.number("commit_delay").filter(|us| *us > 0.0) {
            parts.push(format!("commit_delay {us:.0} µs"));
        }
        for name in ["shared_buffers", "effective_cache_size"] {
            if let Some(b) = self.bytes(name) {
                parts.push(format!("{name} {}", size(b)));
            }
        }
        if let Some(max) = self.raw("max_connections") {
            parts.push(format!("max_connections {max} ({} open now)", self.in_use));
        }
        for name in ["random_page_cost", "effective_io_concurrency", "autovacuum"] {
            if let Some(v) = self.raw(name) {
                parts.push(format!("{name} {v}"));
            }
        }
        parts.join(", ")
    }
}

/// What the settings mean for the engine, warnings first.
pub fn judge(r: &Read) -> Vec<Finding> {
    let mut found = Vec::new();
    let mut say = |code: &'static str, warning: bool, text: String| found.push(Finding { code, warning, text });
    if r.raw("fsync") == Some("off") {
        say(
            "DOC001",
            true,
            "fsync is off: Postgres doesn't flush its writes to the disk, so a crash of the database's server can corrupt the database. Off is for throwaway test databases only: set `fsync = on`.".into(),
        );
    }
    if r.raw("synchronous_commit") == Some("off") {
        say(
            "DOC002",
            true,
            "synchronous_commit is off: a strict interface's ACK doesn't mean its message is on disk, and a crash of the database's server can lose the last moments' messages. Set it `on` for the engine's login and use `durability: relaxed` where that's acceptable.".into(),
        );
    }
    let per_engine = i64::from(r.per_engine);
    if let Some(open) = r.open_to_us() {
        if open < per_engine * 2 {
            say(
                "DOC006",
                true,
                format!(
                    "max_connections leaves {open} for logins like the engine's, and a handover runs two engines for a moment, each opening up to {per_engine} ({} pooled, `--db-connections`, and its lock's): {} in all. Raise max_connections, or lower `--db-connections`.",
                    per_engine - 1,
                    per_engine * 2
                ),
            );
        } else if r.in_use + per_engine > open {
            say(
                "DOC007",
                true,
                format!(
                    "{} of the {open} connections to the database's server are open now: a starting engine, or a handover's new one, wouldn't get the {per_engine} it opens. See who holds them in pg_stat_activity.",
                    r.in_use
                ),
            );
        }
    }
    if r.raw("autovacuum") == Some("off") {
        say(
            "DOC010",
            true,
            "autovacuum is off: deliveries' and messages' old row versions aren't cleaned up, so tables and indexes grow and the engine's queries slow over days. Set `autovacuum = on`.".into(),
        );
    }
    if let Some(us) = r.number("commit_delay").filter(|us| *us > 0.0) {
        let siblings = r.raw("commit_siblings").unwrap_or("5");
        say(
            "DOC003",
            false,
            format!(
                "commit_delay is {us:.0} µs: with {siblings} or more transactions open, each commit waits that long to share its disk flush, which can raise many senders' rate and adds that much to each ACK. Compare `ductus bench` runs with it and without."
            ),
        );
    }
    if let Some(b) = r.bytes("shared_buffers").filter(|b| *b <= DEFAULT_SHARED_BUFFERS) {
        say(
            "DOC004",
            false,
            format!(
                "shared_buffers is {}, Postgres's default or less: on a database server of its own, about a quarter of its RAM is usual (a restart applies it).",
                size(b)
            ),
        );
    }
    if r.bytes("effective_cache_size") == Some(DEFAULT_CACHE_SIZE) {
        say(
            "DOC005",
            false,
            "effective_cache_size is 4 GB, Postgres's default: on a database server of its own, half to three quarters of its RAM is usual (it's the planner's guess and allocates nothing).".into(),
        );
    }
    if r.number("random_page_cost") == Some(4.0) {
        say(
            "DOC008",
            false,
            "random_page_cost is 4, priced for hard disks: on SSD or NVMe, 1.1 is usual, so the planner uses indexes as readily as they deserve.".into(),
        );
    }
    if r.number("effective_io_concurrency").is_some_and(|n| n <= 1.0) {
        let n = r.raw("effective_io_concurrency").unwrap_or_default();
        say(
            "DOC009",
            false,
            format!("effective_io_concurrency is {n}: on SSD or NVMe, 200 is usual (Postgres 18's default is 16)."),
        );
    }
    found
}

/// `128 MB`, `4 GB`, `1.5 GB`.
fn size(bytes: u64) -> String {
    let gb = bytes as f64 / f64::from(1u32 << 30);
    if gb >= 1.0 {
        let text = format!("{gb:.1}");
        format!("{} GB", text.trim_end_matches(".0"))
    } else {
        format!("{} MB", bytes >> 20)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Postgres 18's defaults (as `initdb` leaves them, `pg_settings`' own units), with `changes`.
    fn read(changes: &[(&str, &str)], in_use: i64) -> Read {
        let defaults = [
            ("synchronous_commit", "on", None),
            ("fsync", "on", None),
            ("wal_sync_method", "fdatasync", None),
            ("commit_delay", "0", Some("us")),
            ("commit_siblings", "5", None),
            ("shared_buffers", "16384", Some("8kB")),
            ("effective_cache_size", "524288", Some("8kB")),
            ("max_connections", "100", None),
            ("superuser_reserved_connections", "3", None),
            ("reserved_connections", "0", None),
            ("random_page_cost", "4", None),
            ("effective_io_concurrency", "16", None),
            ("autovacuum", "on", None),
        ];
        let settings = defaults
            .iter()
            .map(|(name, value, unit)| {
                let value = changes.iter().find(|(n, _)| n == name).map_or(*value, |(_, v)| *v);
                (name.to_string(), value.to_string(), unit.map(str::to_string))
            })
            .collect();
        Read { settings, in_use, per_engine: 33 }
    }

    fn codes(r: &Read) -> Vec<&'static str> {
        judge(r).iter().map(|f| f.code).collect()
    }

    #[test]
    fn postgres_s_defaults_are_tuning_notes_and_no_warning() {
        let r = read(&[], 6);
        let found = judge(&r);
        assert_eq!(found.iter().map(|f| f.code).collect::<Vec<_>>(), ["DOC004", "DOC005", "DOC008"]);
        assert!(found.iter().all(|f| !f.warning), "{found:#?}");
        assert!(found[0].text.starts_with("shared_buffers is 128 MB, Postgres's default"), "{}", found[0].text);
        assert_eq!(
            r.summary(),
            "synchronous_commit on, fsync on, wal_sync_method fdatasync, shared_buffers 128 MB, effective_cache_size 4 GB, max_connections 100 (6 open now), random_page_cost 4, effective_io_concurrency 16, autovacuum on"
        );
    }

    #[test]
    fn a_tuned_server_has_nothing_to_say() {
        let tuned = [
            ("shared_buffers", "524288"),
            ("effective_cache_size", "1572864"),
            ("random_page_cost", "1.1"),
            ("effective_io_concurrency", "200"),
            ("max_connections", "200"),
        ];
        assert!(judge(&read(&tuned, 40)).is_empty());
        assert!(read(&tuned, 0).summary().contains("shared_buffers 4 GB, effective_cache_size 12 GB"));
    }

    #[test]
    fn risks_are_warnings_and_each_says_its_value() {
        let r = read(&[("fsync", "off"), ("synchronous_commit", "off"), ("autovacuum", "off")], 6);
        let found = judge(&r);
        assert_eq!(
            found[..3].iter().map(|f| (f.code, f.warning)).collect::<Vec<_>>(),
            [("DOC001", true), ("DOC002", true), ("DOC010", true)]
        );

        let few = read(&[("max_connections", "50")], 6);
        let text = &judge(&few)[0].text;
        assert!(text.starts_with("max_connections leaves 47 for logins like the engine's"), "{text}");
        assert!(text.contains("each opening up to 33 (32 pooled") && text.contains("66 in all"), "{text}");
        assert_eq!(codes(&read(&[("max_connections", "69")], 0))[0], "DOC004", "69 - 3 = 66 is enough");
        assert_eq!(codes(&read(&[("max_connections", "68")], 0))[0], "DOC006");
        assert_eq!(codes(&read(&[("reserved_connections", "40")], 0))[0], "DOC006", "reserved ones count");

        let busy = judge(&read(&[], 70));
        assert_eq!(busy[0].code, "DOC007");
        assert!(busy[0].text.starts_with("70 of the 97 connections"), "{}", busy[0].text);
        assert_eq!(codes(&read(&[], 64))[0], "DOC004", "64 + 33 = 97 still fits");
    }

    #[test]
    fn commit_delay_and_old_io_concurrency_are_notes() {
        let r = read(&[("commit_delay", "100"), ("commit_siblings", "8"), ("effective_io_concurrency", "1")], 6);
        let found = judge(&r);
        let delay = found.iter().find(|f| f.code == "DOC003").unwrap();
        assert!(!delay.warning && delay.text.starts_with("commit_delay is 100 µs: with 8 or more"), "{delay:?}");
        assert!(found.iter().any(|f| f.code == "DOC009" && f.text.contains("is 1:")), "{found:#?}");
        assert!(r.summary().contains("commit_delay 100 µs"), "{}", r.summary());
    }

    #[test]
    fn an_older_postgres_without_some_settings_is_read_as_far_as_it_goes() {
        let mut r = read(&[], 6);
        r.settings.retain(|(n, _, _)| n != "reserved_connections");
        assert_eq!(r.open_to_us(), Some(97));
        let empty = Read::default();
        assert!(judge(&empty).is_empty());
        assert_eq!(empty.summary(), "");
    }

    #[test]
    fn every_code_is_listed_in_order_and_used() {
        let source = include_str!("pg_settings.rs");
        let body = &source[source.find("pub fn judge").unwrap()..source.find("fn size(").unwrap()];
        let used: std::collections::BTreeSet<&str> =
            body.match_indices("\"DOC").map(|(at, _)| &body[at + 1..at + 7]).collect();
        let listed: Vec<&str> = CODES.iter().map(|c| c.code).collect();
        assert_eq!(used.into_iter().collect::<Vec<_>>(), listed, "each code reported and listed, in order");
        assert!(CODES.iter().all(|c| matches!(c.severity, "warning" | "note")));
    }
}
