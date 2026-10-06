//! Database-backed tests. Skipped unless DUCTUS_TEST_DATABASE_URL is set
//! (CI sets it; locally: postgres://todd:ductus@localhost/ductus_dev).

use ductus_store::{Batch, BatchMessage, DedupeKey, NewMessage, Status, Store};

async fn store() -> Option<Store> {
    let url = std::env::var("DUCTUS_TEST_DATABASE_URL").ok()?;
    Some(Store::connect(&url, 4).await.expect("connecting to test database"))
}

/// A unique interface name per test, so tests can share one database.
fn iface(test: &str) -> String {
    format!("test-{test}-{}", std::process::id())
}

fn msg<'a>(interface: &'a str, lane: i32, payload: &'a [u8]) -> NewMessage<'a> {
    NewMessage { interface, partition_key: lane, ordering_key: None, control_id: None, payload, relaxed: false }
}

#[tokio::test]
async fn pending_returns_only_owned_lanes_in_arrival_order() {
    let Some(store) = store().await else { return };
    let name = iface("lanes");
    for (lane, body) in [(0, "a"), (1, "b"), (0, "c"), (2, "d"), (1, "e")] {
        store.insert(msg(&name, lane, body.as_bytes())).await.unwrap();
    }

    let owned = store.pending(&name, &[0, 1], 100).await.unwrap();
    let bodies: Vec<_> = owned.iter().map(|p| String::from_utf8_lossy(&p.payload).into_owned()).collect();
    assert_eq!(bodies, ["a", "b", "c", "e"]);

    let limited = store.pending(&name, &[0, 1], 2).await.unwrap();
    assert_eq!(limited.len(), 2);
}

#[tokio::test]
async fn finished_messages_leave_the_queue() {
    let Some(store) = store().await else { return };
    let name = iface("finish");
    let sent = store.insert(msg(&name, 0, b"one")).await.unwrap();
    let filtered = store.insert(msg(&name, 0, b"two")).await.unwrap();
    store.insert(msg(&name, 0, b"three")).await.unwrap();

    store.set_status(sent, Status::Sent, None).await.unwrap();
    store.set_status(filtered, Status::Filtered, None).await.unwrap();

    let left = store.pending(&name, &[0], 100).await.unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].payload, b"three");

    let counts: Vec<_> = store.counts().await.unwrap().into_iter().filter(|(i, _, _)| *i == name).collect();
    assert_eq!(
        counts,
        vec![(name.clone(), Status::Pending, 1), (name.clone(), Status::Sent, 1), (name.clone(), Status::Filtered, 1)]
    );
}

#[tokio::test]
async fn duplicates_within_the_window_are_stored_but_not_queued() {
    let Some(store) = store().await else { return };
    let name = iface("dedupe");
    let day = DedupeKey { key: "MSG0001", window: std::time::Duration::from_secs(86_400) };

    let first = store.insert_deduped(msg(&name, 0, b"one"), day).await.unwrap();
    let again = store.insert_deduped(msg(&name, 0, b"one again"), DedupeKey { key: "MSG0001", ..day }).await.unwrap();
    let other = store.insert_deduped(msg(&name, 0, b"two"), DedupeKey { key: "MSG0002", ..day }).await.unwrap();
    assert!(!first.duplicate && again.duplicate && !other.duplicate);

    let pending: Vec<_> = store.pending(&name, &[0], 10).await.unwrap().into_iter().map(|p| p.payload).collect();
    assert_eq!(pending, vec![b"one".to_vec(), b"two".to_vec()]);
    let dupes = store.counts().await.unwrap().into_iter().find(|(i, s, _)| *i == name && *s == Status::Duplicate);
    assert_eq!(dupes.map(|(_, _, n)| n), Some(1));

    // Outside the window, the same key is new again.
    let instant = DedupeKey { key: "MSG0001", window: std::time::Duration::ZERO };
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert!(!store.insert_deduped(msg(&name, 0, b"one, much later"), instant).await.unwrap().duplicate);
}

#[tokio::test]
async fn relaxed_inserts_are_stored_and_queued() {
    let Some(store) = store().await else { return };
    let name = iface("relaxed");
    store.insert(NewMessage { relaxed: true, ..msg(&name, 0, b"fast") }).await.unwrap();
    let day = DedupeKey { key: "K1", window: std::time::Duration::from_secs(60) };
    store.insert_deduped(NewMessage { relaxed: true, ..msg(&name, 0, b"fast deduped") }, day).await.unwrap();
    let pending: Vec<_> = store.pending(&name, &[0], 10).await.unwrap().into_iter().map(|p| p.payload).collect();
    assert_eq!(pending, vec![b"fast".to_vec(), b"fast deduped".to_vec()]);
}

#[tokio::test]
async fn statuses_update_in_bulk() {
    let Some(store) = store().await else { return };
    let name = iface("bulk");
    let mut refs = Vec::new();
    for i in 0..5 {
        refs.push(store.insert(msg(&name, 0, format!("m{i}").as_bytes())).await.unwrap());
    }
    store.set_status_many(&refs[..3], Status::Sent).await.unwrap();
    store.set_status_many(&[], Status::Sent).await.unwrap();

    let left: Vec<_> = store.pending(&name, &[0], 10).await.unwrap().into_iter().map(|p| p.payload).collect();
    assert_eq!(left, vec![b"m3".to_vec(), b"m4".to_vec()]);
}

#[tokio::test]
async fn failures_count_attempts_and_keep_the_message_pending() {
    let Some(store) = store().await else { return };
    let name = iface("failures");
    let m = store.insert(msg(&name, 0, b"x")).await.unwrap();

    assert_eq!(store.record_failure(m, "connection refused").await.unwrap(), 1);
    assert_eq!(store.record_failure(m, "timeout").await.unwrap(), 2);

    let pending = store.pending(&name, &[0], 10).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].attempts, 2);

    store.set_status(m, Status::Quarantined, Some("gave up")).await.unwrap();
    assert!(store.pending(&name, &[0], 10).await.unwrap().is_empty());
}

fn batch<'a>(interface: &'a str, items: &[(&'a str, Option<&'a str>)], state: Option<(&'a str, &'a str)>) -> Batch<'a> {
    Batch {
        interface,
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
    }
}

#[tokio::test]
async fn batches_keep_order_dedupe_and_save_state_together() {
    let Some(store) = store().await else { return };
    let name = iface("batch");
    let first = batch(
        &name,
        &[("a", Some("K1")), ("b", None), ("c", Some("K1")), ("d", Some("K2"))],
        Some(("file:x", r#"{"offset":10}"#)),
    );
    assert_eq!(store.insert_batch(&first).await.unwrap(), [false, false, true, false], "K1 repeats inside the batch");
    let second = batch(&name, &[("e", Some("K2")), ("f", Some("K3"))], Some(("file:x", r#"{"offset":20}"#)));
    assert_eq!(store.insert_batch(&second).await.unwrap(), [true, false], "K2 was seen in the earlier batch");

    let pending = store.payloads(&name, Status::Pending).await.unwrap();
    assert_eq!(pending, [b"a".to_vec(), b"b".to_vec(), b"d".to_vec(), b"f".to_vec()]);
    assert_eq!(store.payloads(&name, Status::Duplicate).await.unwrap().len(), 2);
    assert_eq!(store.source_state(&name, "file:x").await.unwrap().as_deref(), Some(r#"{"offset": 20}"#));

    // A state-only batch (end of file) and cleanup.
    store.insert_batch(&batch(&name, &[], Some(("file:x", r#"{"done":true}"#)))).await.unwrap();
    assert_eq!(store.source_state(&name, "file:x").await.unwrap().as_deref(), Some(r#"{"done": true}"#));
    store.delete_source_state(&name, "file:x").await.unwrap();
    assert_eq!(store.source_state(&name, "file:x").await.unwrap(), None);
}

#[tokio::test]
async fn scheduled_runs_are_one_at_a_time() {
    let Some(store) = store().await else { return };
    let name = iface("schedule");
    let run = |payload: &'static [u8]| msg(&name, 0, payload);
    let first = store.insert_scheduled(run(b"run 1"), "schedule", r#"{"n":1}"#, "{}").await.unwrap();
    let second = store.insert_scheduled(run(b"run 2"), "schedule", r#"{"n":2}"#, r#"{"skipped":2}"#).await.unwrap();
    assert!(first.is_some() && second.is_none(), "run 1 is still pending, so run 2 is skipped");
    assert_eq!(store.source_state(&name, "schedule").await.unwrap().as_deref(), Some(r#"{"skipped": 2}"#));

    store.set_status(first.unwrap(), Status::Sent, None).await.unwrap();
    assert!(store.insert_scheduled(run(b"run 3"), "schedule", r#"{"n":3}"#, "{}").await.unwrap().is_some());
    assert_eq!(store.source_state(&name, "schedule").await.unwrap().as_deref(), Some(r#"{"n": 3}"#));
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

#[tokio::test]
async fn partition_maintenance_is_idempotent() {
    let Some(store) = store().await else { return };
    store.ensure_partitions(5).await.unwrap();
    store.ensure_partitions(5).await.unwrap();
}
