//! Database-backed tests. Skipped unless DUCTUS_TEST_DATABASE_URL is set
//! (CI sets it; locally: postgres://todd:ductus@localhost/ductus_dev).

use ductus_store::{NewMessage, Status, Store};

async fn store() -> Option<Store> {
    let url = std::env::var("DUCTUS_TEST_DATABASE_URL").ok()?;
    Some(Store::connect(&url, 4).await.expect("connecting to test database"))
}

/// A unique interface name per test, so tests can share one database.
fn iface(test: &str) -> String {
    format!("test-{test}-{}", std::process::id())
}

fn msg<'a>(interface: &'a str, lane: i32, payload: &'a [u8]) -> NewMessage<'a> {
    NewMessage { interface, partition_key: lane, ordering_key: None, control_id: None, payload }
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
