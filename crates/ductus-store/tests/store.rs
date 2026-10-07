//! Database-backed tests. Skipped unless DUCTUS_TEST_DATABASE_URL is set
//! (CI sets it; locally: postgres://todd:ductus@localhost/ductus_dev).

use ductus_store::{
    Batch, BatchMessage, DedupeKey, DestinationId, Done, MessageRef, NewMessage, Reconciled, Status, Store,
};

async fn store() -> Option<Store> {
    let url = std::env::var("DUCTUS_TEST_DATABASE_URL").ok()?;
    Some(Store::connect(&url, 4).await.expect("connecting to test database"))
}

/// A test interface (unique per test and run, so tests can share one database) and its
/// registered destinations.
struct Iface {
    name: String,
    destinations: Vec<(String, DestinationId)>,
    ids: Vec<DestinationId>,
}

impl Iface {
    async fn new(store: &Store, test: &str, destinations: &[&str]) -> Self {
        // Process id and time: process ids repeat across quick runs, and a reused name would
        // see the dedupe keys an earlier run left behind.
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let name = format!("test-{test}-{}-{nanos}", std::process::id());
        let pairs: Vec<_> = destinations.iter().map(|d| (name.clone(), d.to_string())).collect();
        let ids = store.destination_ids(&pairs).await.unwrap();
        let destinations = destinations.iter().map(|d| d.to_string()).zip(ids.iter().copied()).collect();
        Self { name, destinations, ids }
    }

    /// One destination, named as a single `destination:` is.
    async fn single(store: &Store, test: &str) -> Self {
        Self::new(store, test, &["default"]).await
    }

    fn id(&self, destination: &str) -> DestinationId {
        self.destinations.iter().find(|(d, _)| d == destination).map(|(_, id)| *id).expect("a known destination")
    }

    /// The first (or only) destination.
    fn dest(&self) -> DestinationId {
        self.ids[0]
    }

    fn msg<'a>(&'a self, lane: i32, payload: &'a [u8]) -> NewMessage<'a> {
        NewMessage {
            interface: &self.name,
            partition_key: lane,
            ordering_key: None,
            control_id: None,
            payload,
            relaxed: false,
            destinations: &self.ids,
        }
    }

    /// The configuration `reconcile` gets for these destinations.
    fn config(&self, destinations: &[&str]) -> Vec<(String, String)> {
        destinations.iter().map(|d| (self.name.clone(), d.to_string())).collect()
    }
}

fn sent(message: MessageRef) -> Done {
    Done::sent(message, None)
}

fn payloads(pending: Vec<ductus_store::Pending>) -> Vec<Vec<u8>> {
    pending.into_iter().map(|p| p.payload).collect()
}

/// The overall status of one message.
async fn status_of(store: &Store, name: &str, payload: &[u8]) -> Status {
    for status in [Status::Pending, Status::Sent, Status::Filtered, Status::Quarantined, Status::Duplicate] {
        if store.payloads(name, status).await.unwrap().iter().any(|p| p == payload) {
            return status;
        }
    }
    panic!("no message {}", String::from_utf8_lossy(payload));
}

#[tokio::test]
async fn pending_returns_only_owned_lanes_in_arrival_order() {
    let Some(store) = store().await else { return };
    let t = Iface::single(&store, "lanes").await;
    for (lane, body) in [(0, "a"), (1, "b"), (0, "c"), (2, "d"), (1, "e")] {
        store.insert(t.msg(lane, body.as_bytes())).await.unwrap();
    }

    let owned = store.pending(t.dest(), &[0, 1], 100).await.unwrap();
    let bodies: Vec<_> = owned.iter().map(|p| String::from_utf8_lossy(&p.payload).into_owned()).collect();
    assert_eq!(bodies, ["a", "b", "c", "e"]);

    let limited = store.pending(t.dest(), &[0, 1], 2).await.unwrap();
    assert_eq!(limited.len(), 2);
}

#[tokio::test]
async fn finished_messages_leave_the_queue() {
    let Some(store) = store().await else { return };
    let t = Iface::single(&store, "finish").await;
    let one = store.insert(t.msg(0, b"one")).await.unwrap();
    let two = store.insert(t.msg(0, b"two")).await.unwrap();
    store.insert(t.msg(0, b"three")).await.unwrap();

    store.finish(t.dest(), &[sent(one)]).await.unwrap();
    store.finish(t.dest(), &[Done::with(two, Status::Filtered, Some("killed: test patient"))]).await.unwrap();

    let left = store.pending(t.dest(), &[0], 100).await.unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].payload, b"three");

    let name = &t.name;
    let counts: Vec<_> = store.counts().await.unwrap().into_iter().filter(|(i, _, _)| i == name).collect();
    assert_eq!(
        counts,
        vec![(name.clone(), Status::Pending, 1), (name.clone(), Status::Sent, 1), (name.clone(), Status::Filtered, 1)]
    );
    // With one destination, the message's reason is the destination's, unprefixed.
    let two = store.deliveries(two).await.unwrap();
    assert_eq!((two[0].status, two[0].last_error.as_deref()), (Status::Filtered, Some("killed: test patient")));
}

#[tokio::test]
async fn duplicates_within_the_window_are_stored_but_not_queued() {
    let Some(store) = store().await else { return };
    let t = Iface::single(&store, "dedupe").await;
    let day = DedupeKey { key: "MSG0001", window: std::time::Duration::from_secs(86_400) };

    let first = store.insert_deduped(t.msg(0, b"one"), day).await.unwrap();
    let again = store.insert_deduped(t.msg(0, b"one again"), DedupeKey { key: "MSG0001", ..day }).await.unwrap();
    let other = store.insert_deduped(t.msg(0, b"two"), DedupeKey { key: "MSG0002", ..day }).await.unwrap();
    assert!(!first.duplicate && again.duplicate && !other.duplicate);
    assert!(store.deliveries(again.message).await.unwrap().is_empty(), "duplicates aren't delivered");

    assert_eq!(payloads(store.pending(t.dest(), &[0], 10).await.unwrap()), [b"one".to_vec(), b"two".to_vec()]);
    let dupes = store.counts().await.unwrap().into_iter().find(|(i, s, _)| *i == t.name && *s == Status::Duplicate);
    assert_eq!(dupes.map(|(_, _, n)| n), Some(1));

    // Outside the window, the same key is new again.
    let instant = DedupeKey { key: "MSG0001", window: std::time::Duration::ZERO };
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert!(!store.insert_deduped(t.msg(0, b"one, much later"), instant).await.unwrap().duplicate);
}

#[tokio::test]
async fn relaxed_inserts_are_stored_and_queued() {
    let Some(store) = store().await else { return };
    let t = Iface::single(&store, "relaxed").await;
    store.insert(NewMessage { relaxed: true, ..t.msg(0, b"fast") }).await.unwrap();
    let day = DedupeKey { key: "K1", window: std::time::Duration::from_secs(60) };
    store.insert_deduped(NewMessage { relaxed: true, ..t.msg(0, b"fast deduped") }, day).await.unwrap();
    let pending = payloads(store.pending(t.dest(), &[0], 10).await.unwrap());
    assert_eq!(pending, [b"fast".to_vec(), b"fast deduped".to_vec()]);
}

#[tokio::test]
async fn statuses_update_in_bulk() {
    let Some(store) = store().await else { return };
    let t = Iface::single(&store, "bulk").await;
    let mut refs = Vec::new();
    for i in 0..5 {
        refs.push(store.insert(t.msg(0, format!("m{i}").as_bytes())).await.unwrap());
    }
    store.finish(t.dest(), &refs[..3].iter().copied().map(sent).collect::<Vec<_>>()).await.unwrap();
    store.finish(t.dest(), &[]).await.unwrap();

    assert_eq!(payloads(store.pending(t.dest(), &[0], 10).await.unwrap()), [b"m3".to_vec(), b"m4".to_vec()]);
    assert_eq!(store.payloads(&t.name, Status::Sent).await.unwrap().len(), 3);
}

#[tokio::test]
async fn failures_count_attempts_and_keep_the_message_pending() {
    let Some(store) = store().await else { return };
    let t = Iface::single(&store, "failures").await;
    let m = store.insert(t.msg(0, b"x")).await.unwrap();

    assert_eq!(store.record_failure(m, t.dest(), "connection refused").await.unwrap(), 1);
    assert_eq!(store.record_failure(m, t.dest(), "timeout").await.unwrap(), 2);

    let pending = store.pending(t.dest(), &[0], 10).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].attempts, 2);

    store.finish(t.dest(), &[Done::with(m, Status::Quarantined, Some("gave up"))]).await.unwrap();
    assert!(store.pending(t.dest(), &[0], 10).await.unwrap().is_empty());
    assert_eq!(store.payloads(&t.name, Status::Quarantined).await.unwrap(), [b"x".to_vec()]);
}

#[tokio::test]
async fn each_destination_has_its_own_queue_and_the_message_its_overall_status() {
    let Some(store) = store().await else { return };
    let t = Iface::new(&store, "destinations", &["billing", "lab"]).await;
    let (lab, billing) = (t.id("lab"), t.id("billing"));
    let insert = |payload: &'static [u8]| store.insert(t.msg(0, payload));
    let (a, b, c) = (insert(b"a").await.unwrap(), insert(b"b").await.unwrap(), insert(b"c").await.unwrap());

    // The lab gets everything (keeping what it was sent); billing is down.
    let to_lab = [a, b, c].map(|m| Done::sent(m, Some(format!("lab:{}", m.id).into_bytes())));
    store.finish(lab, &to_lab).await.unwrap();
    assert!(store.pending(lab, &[0], 10).await.unwrap().is_empty());
    assert_eq!(store.pending(billing, &[0], 10).await.unwrap().len(), 3, "billing's queue is its own");
    assert_eq!(status_of(&store, &t.name, b"a").await, Status::Pending, "pending until every destination is done");

    // Billing comes back: rejects a, filters b, takes c.
    store.record_failure(a, billing, "connection refused").await.unwrap();
    let billed = [
        Done::with(a, Status::Quarantined, Some("HTTP 400: bad account")),
        Done::with(b, Status::Filtered, Some("killed: not billable")),
        sent(c),
    ];
    store.finish(billing, &billed).await.unwrap(); // one batch, mixed outcomes

    assert_eq!(status_of(&store, &t.name, b"a").await, Status::Quarantined, "someone must look at it");
    assert_eq!(status_of(&store, &t.name, b"b").await, Status::Sent, "the lab got it");
    assert_eq!(status_of(&store, &t.name, b"c").await, Status::Sent);
    // The message's reason names the destination when there are several.
    let reason: Option<String> = sqlx::query_scalar("SELECT last_error FROM messages WHERE id = $1")
        .bind(a.id)
        .fetch_one(&sqlx::PgPool::connect(&std::env::var("DUCTUS_TEST_DATABASE_URL").unwrap()).await.unwrap())
        .await
        .unwrap();
    assert_eq!(reason.as_deref(), Some("billing: HTTP 400: bad account"));

    let a = store.deliveries(a).await.unwrap();
    assert_eq!(
        a.iter().map(|d| (d.destination.as_str(), d.status)).collect::<Vec<_>>(),
        [("billing", Status::Quarantined), ("lab", Status::Sent)]
    );
    assert_eq!((a[0].attempts, a[0].last_error.as_deref()), (1, Some("HTTP 400: bad account")));
    assert_eq!((a[0].output.as_deref(), a[1].output.is_some()), (None, true), "only what was sent is kept");

    let counts: Vec<_> = store.delivery_counts().await.unwrap().into_iter().filter(|(i, ..)| *i == t.name).collect();
    let counts: Vec<_> = counts.iter().map(|(_, d, s, n)| (d.as_str(), *s, *n)).collect();
    assert_eq!(
        counts,
        [
            ("billing", Status::Sent, 1),
            ("billing", Status::Filtered, 1),
            ("billing", Status::Quarantined, 1),
            ("lab", Status::Sent, 3)
        ]
    );
}

/// Two destinations finishing the same messages at the same moment must never leave one
/// pending (each seeing the other still pending).
#[tokio::test]
async fn destinations_finishing_at_once_never_leave_a_message_pending() {
    let Some(store) = store().await else { return };
    let t = Iface::new(&store, "race", &["a", "b"]).await;
    let mut refs = Vec::new();
    for i in 0..300 {
        let payload = format!("m{i}").into_bytes();
        refs.push(store.insert(t.msg(0, &payload)).await.unwrap());
    }
    let finish = |destination: DestinationId| {
        let (store, refs) = (store.clone(), refs.clone());
        tokio::spawn(async move {
            for m in refs {
                store.finish(destination, &[sent(m)]).await.unwrap();
            }
        })
    };
    let (a, b) = tokio::join!(finish(t.id("a")), finish(t.id("b")));
    a.unwrap();
    b.unwrap();
    assert!(store.payloads(&t.name, Status::Pending).await.unwrap().is_empty());
    assert_eq!(store.payloads(&t.name, Status::Sent).await.unwrap().len(), 300);
}

#[tokio::test]
async fn reconcile_matches_deliveries_to_the_configured_destinations() {
    let Some(url) = std::env::var("DUCTUS_TEST_DATABASE_URL").ok() else { return };
    let store = Store::connect(&url, 4).await.unwrap();
    let pool = sqlx::PgPool::connect(&url).await.unwrap();
    // Stored with destinations `lab` and `fax`; `emr` is registered for later.
    let t = Iface::new(&store, "reconcile", &["fax", "lab", "emr"]).await;
    let other = Iface::single(&store, "reconcile-other").await;
    // Stored by an older version: pending messages without deliveries.
    for interface in [&t.name, &other.name] {
        sqlx::query("INSERT INTO messages (interface, partition_key, payload) VALUES ($1, 0, 'old')")
            .bind(interface)
            .execute(&pool)
            .await
            .unwrap();
    }
    let fax_and_lab = [t.id("fax"), t.id("lab")];
    let m = store.insert(NewMessage { destinations: &fax_and_lab, ..t.msg(0, b"new") }).await.unwrap();
    store.finish(t.id("lab"), &[sent(m)]).await.unwrap();

    // Now the interface sends to `lab` and `emr`: `fax` was removed, `emr` added. The other
    // interface, reconciled in the same call, has its single destination.
    let mut now = t.config(&["emr", "lab"]);
    now.extend(other.config(&["default"]));
    let changes = store.reconcile(&now).await.unwrap();
    let mut added = vec![(t.name.clone(), 3), (other.name.clone(), 1)];
    added.sort();
    assert_eq!(changes, Reconciled { added, removed: vec![(t.name.clone(), "fax".into(), 1)], settled: 0 });
    assert_eq!(store.pending(other.dest(), &[0], 10).await.unwrap().len(), 1);
    let new = store.deliveries(m).await.unwrap();
    let new: Vec<_> = new.iter().map(|d| (d.destination.as_str(), d.status)).collect();
    assert_eq!(new, [("emr", Status::Pending), ("fax", Status::Filtered), ("lab", Status::Sent)]);
    assert_eq!(store.pending(t.id("lab"), &[0], 10).await.unwrap().len(), 1, "the old message, for the lab");
    assert_eq!(store.pending(t.id("emr"), &[0], 10).await.unwrap().len(), 2, "both, still in flight");

    // Removing `emr` again leaves nothing pending: the messages get their overall status.
    let old = store.pending(t.id("lab"), &[0], 10).await.unwrap()[0].reference();
    store.finish(t.id("lab"), &[sent(old)]).await.unwrap();
    let lab = t.config(&["lab"]);
    let changes = store.reconcile(&lab).await.unwrap();
    assert_eq!(changes, Reconciled { added: vec![], removed: vec![(t.name.clone(), "emr".into(), 2)], settled: 2 });
    assert_eq!(store.payloads(&t.name, Status::Sent).await.unwrap(), [b"old".to_vec(), b"new".to_vec()]);
    assert_eq!(store.reconcile(&lab).await.unwrap(), Reconciled::default(), "nothing left to do");

    // An older version finished a message the deliveries still show pending: it takes that status.
    let only_lab = [t.id("lab")];
    let m = store.insert(NewMessage { destinations: &only_lab, ..t.msg(0, b"rolled back") }).await.unwrap();
    sqlx::query("UPDATE messages SET status = 1 WHERE id = $1").bind(m.id).execute(&pool).await.unwrap();
    assert_eq!(store.reconcile(&lab).await.unwrap(), Reconciled::default());
    assert_eq!(store.deliveries(m).await.unwrap()[0].status, Status::Sent);
}

fn batch<'a>(t: &'a Iface, items: &[(&'a str, Option<&'a str>)], state: Option<(&'a str, &'a str)>) -> Batch<'a> {
    Batch {
        interface: &t.name,
        messages: items
            .iter()
            .map(|(payload, key)| BatchMessage {
                partition_key: 0,
                ordering_key: None,
                control_id: None,
                payload: payload.as_bytes(),
                dedupe_key: *key,
            })
            .collect(),
        dedupe_window: std::time::Duration::from_secs(3600),
        relaxed: false,
        state,
        destinations: &t.ids,
    }
}

#[tokio::test]
async fn batches_keep_order_dedupe_and_save_state_together() {
    let Some(store) = store().await else { return };
    let t = Iface::single(&store, "batch").await;
    let first = batch(
        &t,
        &[("a", Some("K1")), ("b", None), ("c", Some("K1")), ("d", Some("K2"))],
        Some(("file:x", r#"{"offset":10}"#)),
    );
    assert_eq!(store.insert_batch(&first).await.unwrap(), [false, false, true, false], "K1 repeats inside the batch");
    let second = batch(&t, &[("e", Some("K2")), ("f", Some("K3"))], Some(("file:x", r#"{"offset":20}"#)));
    assert_eq!(store.insert_batch(&second).await.unwrap(), [true, false], "K2 was seen in the earlier batch");

    let pending = store.payloads(&t.name, Status::Pending).await.unwrap();
    assert_eq!(pending, [b"a".to_vec(), b"b".to_vec(), b"d".to_vec(), b"f".to_vec()]);
    let queued = payloads(store.pending(t.dest(), &[0], 10).await.unwrap());
    assert_eq!(queued, pending, "only messages that aren't duplicates are queued, in order");
    assert_eq!(store.payloads(&t.name, Status::Duplicate).await.unwrap().len(), 2);
    assert_eq!(store.source_state(&t.name, "file:x").await.unwrap().as_deref(), Some(r#"{"offset": 20}"#));

    // A state-only batch (end of file) and cleanup.
    store.insert_batch(&batch(&t, &[], Some(("file:x", r#"{"done":true}"#)))).await.unwrap();
    assert_eq!(store.source_state(&t.name, "file:x").await.unwrap().as_deref(), Some(r#"{"done": true}"#));
    store.delete_source_state(&t.name, "file:x").await.unwrap();
    assert_eq!(store.source_state(&t.name, "file:x").await.unwrap(), None);
}

#[tokio::test]
async fn scheduled_runs_are_one_at_a_time() {
    let Some(store) = store().await else { return };
    let t = Iface::single(&store, "schedule").await;
    let first = store.insert_scheduled(t.msg(0, b"run 1"), "schedule", r#"{"n":1}"#, "{}").await.unwrap();
    let second =
        store.insert_scheduled(t.msg(0, b"run 2"), "schedule", r#"{"n":2}"#, r#"{"skipped":2}"#).await.unwrap();
    assert!(first.is_some() && second.is_none(), "run 1 is still pending, so run 2 is skipped");
    assert_eq!(store.source_state(&t.name, "schedule").await.unwrap().as_deref(), Some(r#"{"skipped": 2}"#));

    store.finish(t.dest(), &[sent(first.unwrap())]).await.unwrap();
    assert!(store.insert_scheduled(t.msg(0, b"run 3"), "schedule", r#"{"n":3}"#, "{}").await.unwrap().is_some());
    assert_eq!(store.source_state(&t.name, "schedule").await.unwrap().as_deref(), Some(r#"{"n": 3}"#));
}

/// Rollback safety: an older binary must start on a database a newer version already migrated.
#[tokio::test]
async fn starts_on_a_database_migrated_by_a_newer_version() {
    let Some(url) = std::env::var("DUCTUS_TEST_DATABASE_URL").ok() else { return };
    let pool = sqlx::PgPool::connect(&url).await.unwrap();
    Store::connect(&url, 2).await.unwrap(); // make sure the migrations table exists
    let future = 99_000_000_000_i64 + i64::from(std::process::id());
    sqlx::query(
        "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time)
         VALUES ($1, 'from a newer engine', true, '\\x00', 0)",
    )
    .bind(future)
    .execute(&pool)
    .await
    .unwrap();

    let result = Store::connect(&url, 2).await;
    sqlx::query("DELETE FROM _sqlx_migrations WHERE version = $1").bind(future).execute(&pool).await.unwrap();
    result.expect("older binary should start on a newer schema");
}

/// Two destinations flushing overlapping batches (workers batch sent statuses, ≤50) in
/// different orders must not deadlock on the message rows they both settle.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn overlapping_batches_settle_without_deadlock() {
    let Some(store) = store().await else { return };
    let t = Iface::new(&store, "batches", &["a", "b"]).await;
    let mut refs = Vec::new();
    for i in 0..1000 {
        refs.push(store.insert(t.msg(0, format!("m{i}").as_bytes())).await.unwrap());
    }
    let finish = |destination: DestinationId, reverse: bool| {
        let (store, mut refs) = (store.clone(), refs.clone());
        if reverse {
            refs.reverse();
        }
        tokio::spawn(async move {
            for batch in refs.chunks(50) {
                let done: Vec<_> = batch.iter().map(|m| sent(*m)).collect();
                store.finish(destination, &done).await.unwrap();
            }
        })
    };
    let (a, b) = tokio::join!(finish(t.id("a"), false), finish(t.id("b"), true));
    a.unwrap();
    b.unwrap();
    assert_eq!(store.payloads(&t.name, Status::Sent).await.unwrap().len(), 1000);
}

/// docs/schema.md describes every table and column (so the published schema can't drift).
#[tokio::test]
async fn the_schema_doc_describes_every_table_and_column() {
    let Some(url) = std::env::var("DUCTUS_TEST_DATABASE_URL").ok() else { return };
    let _ = store().await; // migrations applied
    let pool = sqlx::PgPool::connect(&url).await.unwrap();
    let doc = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/schema.md")).unwrap();
    let section = |table: &str| -> Option<String> {
        let start = doc.find(&format!("\n## `{table}`\n"))?;
        let rest = &doc[start + 1..];
        Some(rest[..rest[3..].find("\n## ").map(|e| e + 3).unwrap_or(rest.len())].to_string())
    };
    // Our tables are the ones the migrations create (demos add their own lookup tables to the
    // same database; partitions follow their parent).
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/migrations");
    let mut tables = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        for line in std::fs::read_to_string(entry.unwrap().path()).unwrap().lines() {
            if let Some(rest) = line.strip_prefix("CREATE TABLE ")
                && let Some((name, _)) = rest.split_once(" (")
            {
                tables.push(name.trim().to_string());
            }
        }
    }
    assert!(tables.contains(&"deliveries".to_string()), "{tables:?}");
    let columns: Vec<(String, String)> = sqlx::query_as(
        "SELECT table_name::text, column_name::text FROM information_schema.columns
         WHERE table_schema = 'public' AND table_name = ANY($1) ORDER BY 1, ordinal_position",
    )
    .bind(&tables)
    .fetch_all(&pool)
    .await
    .unwrap();
    let mut missing = Vec::new();
    for (table, column) in &columns {
        match section(table) {
            None => missing.push(format!("table `{table}`")),
            Some(s) if !s.contains(&format!("| `{column}` |")) => missing.push(format!("`{table}.{column}`")),
            _ => {}
        }
    }
    missing.dedup();
    assert!(missing.is_empty(), "docs/schema.md doesn't describe: {}", missing.join(", "));
    assert!(columns.len() >= 30, "found the tables: {}", columns.len());
}

/// How far ahead the furthest `messages` partition goes, in days from today (by Postgres's date).
async fn furthest_partition(pool: &sqlx::PgPool) -> i32 {
    sqlx::query_scalar(
        "SELECT (max(to_date(substr(c.relname, 10), 'YYYYMMDD')) - current_date)::int
         FROM pg_inherits i JOIN pg_class c ON c.oid = i.inhrelid
         WHERE i.inhparent = 'messages'::regclass AND c.relname ~ '^messages_[0-9]{8}$'",
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Partition maintenance as the engine runs it: giving up on a lock timeout is by design (another
/// test may hold a long read, as `partition_maintenance_never_holds_up_inserts` does); any other
/// error, a deadlock above all, fails the test.
async fn maintain(store: &Store, days_ahead: i32) {
    if let Err(e) = store.ensure_partitions(days_ahead).await {
        let e = format!("{e:#}");
        assert!(e.contains("lock timeout"), "{e}");
    }
}

/// Drop every partition beyond the 3 days ahead the engine keeps (tests make further ones; future
/// partitions never hold data). Measured now, not when the test started: tests run at the same
/// time, and one's maintenance re-fills days another dropped. One table per statement: dropping
/// both in one transaction would lock both parents together, which deadlocks with inserts.
async fn drop_test_partitions(pool: &sqlx::PgPool) {
    let furthest = furthest_partition(pool).await;
    for table in ["deliveries", "messages"] {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DO $$ BEGIN FOR i IN 4 .. {furthest} LOOP
                 EXECUTE format('DROP TABLE IF EXISTS %I', '{table}_' || to_char(current_date + i, 'YYYYMMDD'));
             END LOOP; END $$"
        )))
        .execute(pool)
        .await
        .unwrap();
    }
}

/// A long read of `messages` (a big report, say) must not make inserts, and so ACKs, wait
/// behind partition maintenance. Creating a partition needs a lock that waits for the report;
/// while it waits, every later statement on the table queues behind it. Maintenance gives up
/// after a short lock timeout instead (partitions are made days ahead: it retries in an hour).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn partition_maintenance_never_holds_up_inserts() {
    let Some(store) = store().await else { return };
    let t = Iface::single(&store, "convoy").await;
    let pool = sqlx::PgPool::connect(&std::env::var("DUCTUS_TEST_DATABASE_URL").unwrap()).await.unwrap();
    let furthest = furthest_partition(&pool).await;

    // The report: an open transaction that has read `messages`, for 4 s.
    let mut report = pool.begin().await.unwrap();
    sqlx::query("SELECT count(*) FROM messages").execute(&mut *report).await.unwrap();
    let maintenance = {
        let store = store.clone();
        tokio::spawn(async move { store.ensure_partitions(furthest + 1).await })
    };
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let start = std::time::Instant::now();
    let insert = {
        let store = store.clone();
        let name = t.name.clone();
        let ids = t.ids.clone();
        tokio::spawn(async move {
            let m = NewMessage {
                interface: &name,
                partition_key: 0,
                ordering_key: None,
                control_id: None,
                payload: b"during maintenance",
                relaxed: false,
                destinations: &ids,
            };
            store.insert(m).await.map(|_| start.elapsed())
        })
    };
    tokio::time::sleep(std::time::Duration::from_secs(4)).await;
    report.rollback().await.unwrap();
    let waited = insert.await.unwrap().unwrap();
    let maintained = maintenance.await.unwrap();
    drop_test_partitions(&pool).await;
    // (Before the lock timeout: 4.05 s, the whole report.)
    assert!(waited < std::time::Duration::from_secs(3), "the insert waited {waited:?} behind maintenance");
    // Maintenance gave up (lock timeout); or found nothing to create, if another test running
    // at the same time made that day's partition first.
    if let Err(e) = maintained {
        let e = format!("{e:#}");
        assert!(e.contains("lock timeout"), "{e}");
    }
}

/// Everything the engine does to the database, at once, for a while (`DUCTUS_STRESS_SECONDS`,
/// default 5): single inserts with colliding dedupe keys (MLLP connections), a batch writer (a
/// file source), three destinations draining and recording failures, partition maintenance
/// creating partitions, and dedupe key purges. Any deadlock or lock error fails it; at the end
/// nothing may be left pending and every delivery is recorded exactly once.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn everything_at_once_without_lock_errors() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};
    let Some(store) = store().await else { return };
    let seconds: u64 = std::env::var("DUCTUS_STRESS_SECONDS").ok().and_then(|s| s.parse().ok()).unwrap_or(5);
    let t = Arc::new(Iface::new(&store, "stress", &["lab", "billing", "archive"]).await);
    let pool = sqlx::PgPool::connect(&std::env::var("DUCTUS_TEST_DATABASE_URL").unwrap()).await.unwrap();
    let furthest = furthest_partition(&pool).await;
    let stop = Arc::new(AtomicBool::new(false));
    let queued = Arc::new(AtomicUsize::new(0));
    let window = std::time::Duration::from_secs(3600);
    let lanes: Vec<i32> = (0..8).collect();
    let mut tasks = Vec::new();

    for writer in 0..4 {
        let (store, t, stop, queued) = (store.clone(), t.clone(), stop.clone(), queued.clone());
        tasks.push(tokio::spawn(async move {
            let mut i = 0usize;
            while !stop.load(Relaxed) {
                let key = format!("k{}", (i * 4 + writer) / 2); // pairs of writers share each key
                let payload = format!("single-{writer}-{i}");
                let m = t.msg((i % 8) as i32, payload.as_bytes());
                let stored = store.insert_deduped(m, DedupeKey { key: &key, window }).await.unwrap();
                if !stored.duplicate {
                    queued.fetch_add(1, Relaxed);
                }
                i += 1;
            }
        }));
    }
    {
        let (store, t, stop, queued) = (store.clone(), t.clone(), stop.clone(), queued.clone());
        tasks.push(tokio::spawn(async move {
            let mut round = 0usize;
            while !stop.load(Relaxed) {
                let texts: Vec<(String, String)> =
                    (0..50).map(|n| (format!("batch-{round}-{n}"), format!("k{}", round * 50 + n))).collect();
                let messages = texts
                    .iter()
                    .enumerate()
                    .map(|(n, (payload, key))| BatchMessage {
                        partition_key: (n % 8) as i32,
                        ordering_key: None,
                        control_id: None,
                        payload: payload.as_bytes(),
                        dedupe_key: Some(key),
                    })
                    .collect();
                let batch = Batch {
                    interface: &t.name,
                    messages,
                    dedupe_window: window,
                    relaxed: false,
                    state: None,
                    destinations: &t.ids,
                };
                let duplicates = store.insert_batch(&batch).await.unwrap();
                queued.fetch_add(duplicates.iter().filter(|d| !**d).count(), Relaxed);
                round += 1;
            }
        }));
    }
    for (n, destination) in t.ids.clone().into_iter().enumerate() {
        let (store, stop, lanes) = (store.clone(), stop.clone(), lanes.clone());
        tasks.push(tokio::spawn(async move {
            let mut rounds = 0usize;
            loop {
                let stopped = stop.load(Relaxed);
                let pending = store.pending(destination, &lanes, 50).await.unwrap();
                if pending.is_empty() {
                    if stopped {
                        break; // everything stored before the stop has been drained
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    continue;
                }
                rounds += 1;
                if rounds.is_multiple_of(5 + n) {
                    store.record_failure(pending[0].reference(), destination, "busy").await.unwrap();
                }
                let done: Vec<_> = pending.iter().map(|p| sent(p.reference())).collect();
                store.finish(destination, &done).await.unwrap();
            }
        }));
    }
    let maintenance = {
        let (store, stop) = (store.clone(), stop.clone());
        tokio::spawn(async move {
            let mut day = 0;
            while !stop.load(Relaxed) {
                day += 1;
                maintain(&store, furthest + day).await;
                store.purge_dedupe_keys(window).await.unwrap();
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            day
        })
    };

    tokio::time::sleep(std::time::Duration::from_secs(seconds)).await;
    stop.store(true, Relaxed);
    let mut results = Vec::new();
    for task in tasks {
        results.push(task.await);
    }
    let days = maintenance.await;
    drop_test_partitions(&pool).await;
    for r in results {
        r.expect("a writer or drain failed");
    }
    let days = days.expect("maintenance failed");

    let queued = queued.load(Relaxed);
    assert!(queued > 1000, "too little traffic to mean anything: {queued}");
    assert!(store.payloads(&t.name, Status::Pending).await.unwrap().is_empty(), "nothing left pending");
    assert_eq!(store.payloads(&t.name, Status::Sent).await.unwrap().len(), queued);
    let deliveries: Vec<_> =
        store.delivery_counts().await.unwrap().into_iter().filter(|(i, ..)| *i == t.name).collect();
    for (_, destination, status, n) in &deliveries {
        assert_eq!((*status, *n as usize), (Status::Sent, queued), "{destination}: {deliveries:?}");
    }
    assert_eq!(deliveries.len(), 3);
    let duplicates = store.payloads(&t.name, Status::Duplicate).await.unwrap().len();
    assert!(duplicates > 0, "dedupe keys collided");
    println!(
        "stress: {} messages stored ({queued} queued, {duplicates} duplicates), 3 destinations, {days} partitions created, {seconds}s",
        queued + duplicates
    );
}

/// Creating new day partitions while destinations settle messages must not deadlock. (Partition
/// maintenance locked `messages` then `deliveries` in one transaction; settling reads
/// `deliveries` then updates `messages`. Seen in CI; in production it would meet the hourly
/// maintenance on the day a new partition is created.)
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn partition_maintenance_never_deadlocks_with_settling() {
    let Some(store) = store().await else { return };
    let t = Iface::new(&store, "maint", &["a", "b"]).await;
    let mut refs = Vec::new();
    for i in 0..200 {
        refs.push(store.insert(t.msg(0, format!("m{i}").as_bytes())).await.unwrap());
    }
    // Each call reaches one day further than any partition so far: it creates one new partition
    // of each table (dropped again at the end, so repeated runs don't pile them up).
    let pool = sqlx::PgPool::connect(&std::env::var("DUCTUS_TEST_DATABASE_URL").unwrap()).await.unwrap();
    let furthest = furthest_partition(&pool).await;
    const NEW_DAYS: i32 = 20;
    let maintenance = {
        let store = store.clone();
        tokio::spawn(async move {
            for day in 1..=NEW_DAYS {
                maintain(&store, furthest + day).await;
            }
        })
    };
    let finish = |destination: DestinationId| {
        let (store, refs) = (store.clone(), refs.clone());
        tokio::spawn(async move {
            for m in refs {
                store.finish(destination, &[sent(m)]).await.unwrap();
            }
        })
    };
    let (m, a, b) = tokio::join!(maintenance, finish(t.id("a")), finish(t.id("b")));
    drop_test_partitions(&pool).await;
    m.unwrap();
    a.unwrap();
    b.unwrap();
    assert_eq!(store.payloads(&t.name, Status::Sent).await.unwrap().len(), 200);
}

#[tokio::test]
async fn partition_maintenance_is_idempotent() {
    let Some(store) = store().await else { return };
    maintain(&store, 5).await;
    maintain(&store, 5).await;
}
