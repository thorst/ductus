//! Database-backed tests. Skipped unless DUCTUS_TEST_DATABASE_URL is set
//! (CI sets it; locally: postgres://todd:ductus@localhost/ductus_dev).

use ductus_store::{
    Batch, BatchMessage, Browse, DedupeKey, DestinationId, Done, MessageRef, NewMessage, QueueCount, Reconciled, Role,
    Status, Store,
};

/// The retention tier test messages go to.
const TIER: ductus_store::Tier = 21;

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
            tier: TIER,
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
async fn activity_and_pauses_survive_a_restart() {
    use chrono::{Duration, Utc};
    use ductus_store::Seen;
    let Some(store) = store().await else { return };
    let t = Iface::new(&store, "activity", &["lab", "billing"]).await;
    let (lab, billing) = (t.id("lab"), t.id("billing"));
    let (interfaces, destinations) = store.activity(&[&t.name], &t.ids).await.unwrap();
    assert_eq!(interfaces.len(), 1);
    assert_eq!(interfaces[0].last_received, None, "never received");
    let watched = interfaces[0].watched_since;
    assert!(destinations.iter().all(|d| d.last_sent.is_none() && d.pause.paused_at.is_none()));

    // Times only move forward: an older one (another engine's, say) doesn't overwrite.
    let now = Utc::now();
    let earlier = now - Duration::minutes(5);
    let error = Some((now, "connection refused".to_string()));
    let seen = [Seen { destination: lab, last_sent: Some(now), last_error: error.clone() }];
    store.save_activity(&[(&t.name, now)], &seen).await.unwrap();
    let older = [Seen { destination: lab, last_sent: Some(earlier), last_error: Some((earlier, "old".into())) }];
    store.save_activity(&[(&t.name, earlier)], &older).await.unwrap();

    // Pause, then add the reason later (act first, explain after).
    let paused = store.pause(billing, "ops-token", None).await.unwrap();
    assert_eq!((paused.paused_by.as_deref(), paused.pause_reason.as_deref()), (Some("ops-token"), None));
    let explained = store.pause(billing, "todd", Some("vendor breach: hold until cleared")).await.unwrap();
    assert_eq!(explained.paused_at, paused.paused_at, "still the same pause");
    assert_eq!(explained.paused_by.as_deref(), Some("ops-token"));
    assert_eq!(explained.reason_by.as_deref(), Some("todd"));
    assert!(explained.reason_at.is_some());

    // What a restart loads: the same, and the interface is still watched from the first time.
    let (interfaces, destinations) = store.activity(&[&t.name], &t.ids).await.unwrap();
    assert_eq!(interfaces[0].watched_since, watched);
    let ms = |t: Option<chrono::DateTime<Utc>>| t.map(|t| t.timestamp_millis());
    assert_eq!(ms(interfaces[0].last_received), ms(Some(now)));
    let lab_row = destinations.iter().find(|d| d.destination_id == lab).unwrap();
    assert_eq!(ms(lab_row.last_sent), ms(Some(now)));
    assert_eq!(lab_row.last_error.as_deref(), Some("connection refused"));
    let billing_row = destinations.iter().find(|d| d.destination_id == billing).unwrap();
    assert_eq!(billing_row.pause.pause_reason.as_deref(), Some("vendor breach: hold until cleared"));

    // Resuming returns the pause it ended; resuming again finds none.
    let ended = store.unpause(billing).await.unwrap();
    assert_eq!(ended.pause_reason.as_deref(), Some("vendor breach: hold until cleared"));
    assert_eq!(store.unpause(billing).await.unwrap().paused_at, None);
    let (_, destinations) = store.activity(&[&t.name], &t.ids).await.unwrap();
    assert!(destinations.iter().all(|d| d.pause == ductus_store::PauseRow::default()));
}

#[tokio::test]
async fn baselines_and_the_oldest_waiting_message() {
    let Some(store) = store().await else { return };
    let t = Iface::new(&store, "baseline", &["lab", "billing"]).await;
    let (lab, billing) = (t.id("lab"), t.id("billing"));
    let (_, rows) = store.activity(&[&t.name], &t.ids).await.unwrap();
    assert!(rows.iter().all(|r| r.baseline_inflow.is_none()), "nothing learned yet");

    // Two destinations in one statement, each its own 168 values (NULLs kept).
    let hours = |f: fn(usize) -> Option<f32>| (0..168).map(f).collect::<Vec<_>>();
    let saved = [
        ductus_store::BaselineRow {
            destination: lab,
            inflow: hours(|h| (h == 7).then_some(120.0)),
            send_ms: hours(|h| (h == 7).then_some(12.5)),
        },
        ductus_store::BaselineRow { destination: billing, inflow: hours(|h| Some(h as f32)), send_ms: hours(|_| None) },
    ];
    store.save_baselines(&saved).await.unwrap();
    let (_, rows) = store.activity(&[&t.name], &t.ids).await.unwrap();
    for s in &saved {
        let row = rows.iter().find(|r| r.destination_id == s.destination).unwrap();
        assert_eq!(row.baseline_inflow.as_ref(), Some(&s.inflow));
        assert_eq!(row.baseline_send_ms.as_ref(), Some(&s.send_ms));
    }
    let short = ductus_store::BaselineRow { destination: lab, inflow: vec![None], send_ms: vec![None] };
    assert!(store.save_baselines(&[short]).await.is_err());

    // The oldest waiting message per destination; none once it's delivered.
    assert!(store.oldest_pending(&t.ids).await.unwrap().is_empty());
    let first = store.insert(t.msg(3, b"first")).await.unwrap();
    store.insert(t.msg(1, b"second")).await.unwrap();
    store.finish(billing, &[sent(first)]).await.unwrap();
    let oldest = store.oldest_pending(&t.ids).await.unwrap();
    let at = |id| oldest.iter().find(|o| o.destination_id == id).map(|o| o.received_at.timestamp_micros());
    assert_eq!(at(lab), Some(first.received_at.timestamp_micros()), "lab still has the first");
    assert!(at(billing).is_some_and(|t| t >= first.received_at.timestamp_micros()), "billing: the second");
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
async fn lanes_show_each_backlog_and_its_first_message() {
    let Some(store) = store().await else { return };
    let t = Iface::new(&store, "hotspots", &["lab", "billing"]).await;
    let (lab, billing) = (t.id("lab"), t.id("billing"));
    let keyed = |lane: i32, key: &'static str, payload: &'static [u8]| NewMessage {
        ordering_key: Some(key),
        ..t.msg(lane, payload)
    };
    let first = store.insert(keyed(3, "MRN7", b"a")).await.unwrap();
    for (lane, key, body) in [(3, "MRN7", b"b"), (5, "MRN9", b"c"), (3, "MRN8", b"d")] {
        store.insert(keyed(lane, key, body)).await.unwrap();
    }
    let in_five = store.pending(lab, &[5], 10).await.unwrap()[0].reference();
    store.record_failure(first, lab, "HTTP 503: busy").await.unwrap();
    store.finish(lab, &[sent(in_five)]).await.unwrap();

    let lanes = store.lanes(lab).await.unwrap();
    assert_eq!(lanes.len(), 1, "lane 5 is done for the lab: {lanes:?}");
    let l = &lanes[0];
    assert_eq!((l.lane, l.waiting, l.head_id), (3, 3, first.id));
    assert_eq!(
        (l.attempts, l.last_error.as_deref(), l.ordering_key.as_deref()),
        (1, Some("HTTP 503: busy"), Some("MRN7"))
    );
    assert_eq!(l.received_at, first.received_at);

    // Billing's queue is its own: both lanes, most waiting first, nothing failed.
    let lanes = store.lanes(billing).await.unwrap();
    assert_eq!(lanes.iter().map(|l| (l.lane, l.waiting, l.attempts)).collect::<Vec<_>>(), [(3, 3, 0), (5, 1, 0)]);
    assert_eq!(lanes[1].ordering_key.as_deref(), Some("MRN9"));
    assert!(store.lanes(DestinationId::MAX).await.unwrap().is_empty());
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

/// The queue read walks the destination's queue in message order, so it stops after its rows
/// however many deliveries came before them. Without `deliveries_queue`, Postgres walked each
/// partition's primary key past every delivered row instead (~300 ms a read with 1M delivered
/// messages ahead of a 10k backlog, migration 0011). Checks the plan Postgres would cache.
#[tokio::test]
async fn the_queue_read_uses_the_queue_index() {
    let Some(store) = store().await else { return };
    let t = Iface::single(&store, "queue-plan").await;
    let delivered: Vec<String> = (0..3000).map(|n| format!("old{n}")).collect();
    let items: Vec<_> = delivered.iter().map(|p| (p.as_str(), None)).collect();
    store.insert_batch(&batch(&t, &items, None)).await.unwrap();
    let done: Vec<_> = store.pending(t.dest(), &[0], 3000).await.unwrap().iter().map(|p| sent(p.reference())).collect();
    store.finish(t.dest(), &done).await.unwrap();
    store.insert_batch(&batch(&t, &[("new", None)], None)).await.unwrap();

    let pool = sqlx::PgPool::connect(&std::env::var("DUCTUS_TEST_DATABASE_URL").unwrap()).await.unwrap();
    sqlx::query("ANALYZE deliveries").execute(&pool).await.unwrap();
    let mut connection = pool.acquire().await.unwrap();
    sqlx::query("SET plan_cache_mode = force_generic_plan").execute(&mut *connection).await.unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!("PREPARE queue_read(int, int[], bigint) AS {}", ductus_store::QUEUE_READ)))
        .execute(&mut *connection)
        .await
        .unwrap();
    let plan: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "EXPLAIN (COSTS OFF) EXECUTE queue_read({}, '{{0,1}}', 50)",
        t.dest()
    )))
    .fetch_all(&mut *connection)
    .await
    .unwrap();
    // The partition holding the delivered rows and the backlog. (On nearly empty partitions of
    // the shared test database either index is as cheap, and the planner picks either.)
    let partition: String = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT tableoid::regclass::text FROM deliveries WHERE destination_id = {} LIMIT 1",
        t.dest()
    )))
    .fetch_one(&mut *connection)
    .await
    .unwrap();
    let scan = plan.iter().find(|l| l.contains("Scan") && l.contains(&format!(" on {partition} ")));
    assert!(
        scan.is_some_and(|l| l.contains("destination_id_message_id")),
        "the queue read doesn't use deliveries_queue on {partition}:\n{}",
        plan.join("\n")
    );
    let read = store.pending(t.dest(), &[0], 50).await.unwrap();
    assert_eq!(payloads(read), [b"new".to_vec()]);
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
                origin: None,
            })
            .collect(),
        dedupe_window: std::time::Duration::from_secs(3600),
        relaxed: false,
        state,
        destinations: &t.ids,
        tier: TIER,
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

/// A route stores what a destination sent into the target interface in the same transaction
/// that records the destination's statuses: both happen, or neither.
#[tokio::test]
async fn routed_messages_are_queued_in_the_target_exactly_once() {
    let Some(store) = store().await else { return };
    let source = Iface::new(&store, "route-from", &["lab"]).await;
    let target = Iface::single(&store, "route-to").await;
    let a = store.insert(source.msg(0, b"a")).await.unwrap();
    let b = store.insert(source.msg(0, b"b")).await.unwrap();
    let c = store.insert(source.msg(0, b"c")).await.unwrap();
    let routed = |items: &[(&'static str, i64)], destinations| Batch {
        interface: &target.name,
        messages: items
            .iter()
            .map(|(payload, origin)| BatchMessage {
                partition_key: 0,
                ordering_key: None,
                control_id: None,
                payload: payload.as_bytes(),
                dedupe_key: Some("same key"),
                origin: Some(*origin),
            })
            .collect(),
        dedupe_window: std::time::Duration::from_secs(3600),
        relaxed: true,
        state: None,
        destinations,
        tier: TIER,
    };
    let done = [sent(a), sent(b), Done::with(c, Status::Filtered, Some("killed: not for the lab"))];

    // A failure (here: the same destination twice) stores nothing on either side.
    let twice = [target.dest(), target.dest()];
    assert!(store.finish_routed(source.dest(), &done, &routed(&[("A", a.id)], &twice)).await.is_err());
    assert_eq!(store.pending(source.dest(), &[0], 10).await.unwrap().len(), 3, "still pending in the source");
    assert!(store.payloads(&target.name, Status::Pending).await.unwrap().is_empty(), "nothing in the target");

    let both = routed(&[("A", a.id), ("B", b.id)], &target.ids);
    let queued = store.finish_routed(source.dest(), &done, &both).await.unwrap();
    assert_eq!(queued, [true, false], "the target's dedupe applies");
    // A retry of a call that committed (e.g. its settle failed afterwards) routes nothing again.
    assert_eq!(store.finish_routed(source.dest(), &done, &both).await.unwrap(), [false, false]);
    assert!(store.pending(source.dest(), &[0], 10).await.unwrap().is_empty());
    assert_eq!(
        (status_of(&store, &source.name, b"a").await, status_of(&store, &source.name, b"c").await),
        (Status::Sent, Status::Filtered),
        "the source's overall status is settled"
    );
    let queued = store.pending(target.dest(), &[0], 10).await.unwrap();
    assert_eq!((queued.len(), queued[0].payload.as_slice(), queued[0].origin), (1, &b"A"[..], Some(a.id)));
    let row = store.message(queued[0].id).await.unwrap().unwrap().0;
    assert_eq!(row.origin_id, Some(a.id), "the browser shows where it came from");
    assert_eq!(store.payloads(&target.name, Status::Duplicate).await.unwrap(), [b"B".to_vec()]);
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

/// The same, with batches that overlap without matching: in the engine, two destinations of one
/// message flush whatever each has done (50 at a time, at the end of a read, before a back-off,
/// or after 20 ms for a route), so their batches share some messages but not all. Settling
/// locked message rows in whatever order the plan visited them; a plan driven by the finished
/// set (hash order, which differs between two such sets) deadlocked once in the routing demo.
/// This test doesn't reproduce that (here the planner scans `messages`, in physical order, the
/// same for both); `Store::settle` now locks in id order whatever the plan.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn misaligned_batches_settle_without_deadlock() {
    let Some(store) = store().await else { return };
    let t = Iface::new(&store, "misaligned", &["a", "b", "c", "d"]).await;
    let texts: Vec<String> = (0..4000).map(|i| format!("m{i}")).collect();
    let items: Vec<(&str, Option<&str>)> = texts.iter().map(|s| (s.as_str(), None)).collect();
    store.insert_batch(&batch(&t, &items, None)).await.unwrap();
    let refs: Vec<MessageRef> =
        store.pending(t.id("a"), &[0], 4000).await.unwrap().iter().map(|p| p.reference()).collect();
    let finish = |destination: DestinationId, size: usize| {
        let (store, refs) = (store.clone(), refs.clone());
        tokio::spawn(async move {
            for batch in refs.chunks(size) {
                let done: Vec<_> = batch.iter().map(|m| sent(*m)).collect();
                store.finish(destination, &done).await?;
            }
            anyhow::Ok(())
        })
    };
    let (a, b, c, d) =
        tokio::join!(finish(t.id("a"), 50), finish(t.id("b"), 37), finish(t.id("c"), 23), finish(t.id("d"), 61));
    for (name, result) in [("a", a), ("b", b), ("c", c), ("d", d)] {
        result.unwrap().unwrap_or_else(|e| panic!("destination {name}: {e:#}"));
    }
    assert_eq!(store.payloads(&t.name, Status::Sent).await.unwrap().len(), 4000);
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

/// How far ahead the furthest `messages` partition goes, in days from today (by Postgres's date):
/// the 7d tier's, whose partitions are a day each.
async fn furthest_partition(pool: &sqlx::PgPool) -> i32 {
    sqlx::query_scalar(
        "SELECT (max(to_date(right(c.relname, 8), 'YYYYMMDD')) - current_date)::int
         FROM pg_inherits i JOIN pg_class c ON c.oid = i.inhrelid
         WHERE i.inhparent = 'messages_t7'::regclass AND c.relname ~ '_[0-9]{8}$'",
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

/// Drop every partition starting beyond the 3 days ahead the engine keeps (tests make further
/// ones; future partitions never hold data). Listed now, not when the test started: tests run at
/// the same time, and one's maintenance re-fills days another dropped. One table and tier per
/// statement: holding two tier tables at once deadlocks with queries, which lock every tier (it
/// did, when this dropped all tiers in one statement).
async fn drop_test_partitions(pool: &sqlx::PgPool) {
    for table in ["deliveries", "messages"] {
        for tier in ductus_store::TIERS {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "DO $$ DECLARE p text; BEGIN
                     FOR p IN SELECT c.relname FROM pg_inherits i JOIN pg_class c ON c.oid = i.inhrelid
                              WHERE i.inhparent = '{table}_t{tier}'::regclass AND c.relname ~ '_[0-9]{{8}}$'
                                AND to_date(right(c.relname, 8), 'YYYYMMDD') > current_date + 3
                     LOOP
                         EXECUTE format('DROP TABLE IF EXISTS %I', p);
                     END LOOP;
                 END $$"
            )))
            .execute(pool)
            .await
            .unwrap();
        }
    }
}

/// Tests that hold long locks on `messages` on purpose take this, so they don't hold up each
/// other's timing (an urgent maintenance waiting behind one test's read queues the other's inserts).
static LONG_LOCKS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A long read of `messages` (a big report, say) must not make inserts, and so ACKs, wait
/// behind partition maintenance. Creating a partition needs a lock that waits for the report;
/// while it waits, every later statement on the table queues behind it. Maintenance gives up
/// after a short lock timeout instead (partitions are made days ahead: it retries in an hour).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn partition_maintenance_never_holds_up_inserts() {
    let Some(store) = store().await else { return };
    let _serial = LONG_LOCKS.lock().await;
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
                tier: TIER,
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

/// The last resort: when the missing partition is needed soon, maintenance doesn't give up on
/// a long read; it waits for it, so the partition exists before messages need it. (Here "soon"
/// is a far-off day: tests can't drop tomorrow's partitions, which other tests are using.)
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn partition_maintenance_waits_when_the_day_is_close() {
    let Some(store) = store().await else { return };
    let _serial = LONG_LOCKS.lock().await;
    let pool = sqlx::PgPool::connect(&std::env::var("DUCTUS_TEST_DATABASE_URL").unwrap()).await.unwrap();
    let day = furthest_partition(&pool).await + 40; // far beyond what other tests create
    assert!(store.first_missing_partition_day(day).await.unwrap() <= day, "there's a day to create");
    assert_eq!(store.first_missing_partition_day(0).await.unwrap(), 1, "today's partitions exist");

    // Short, so tests running alongside wait no longer than this for their inserts.
    let mut report = pool.begin().await.unwrap();
    sqlx::query("SELECT count(*) FROM messages").execute(&mut *report).await.unwrap();
    let maintenance = {
        let store = store.clone();
        tokio::spawn(async move { store.ensure_partitions_urgent_within(day, day).await })
    };
    tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
    let waited = !maintenance.is_finished();
    report.rollback().await.unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), maintenance).await;
    let exists = store.first_missing_partition_day(day).await.unwrap() > day;
    drop_test_partitions(&pool).await;
    assert!(waited, "past the 2 s lock timeout, still waiting for the read");
    result.expect("done once the read ended").unwrap().unwrap();
    assert!(exists, "every day up to {day} has both partitions");
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
    // Its maintenance creates partitions every 100 ms: while another test holds a long read on
    // purpose, each of those waits up to 2 s and the inserts queue behind it (seen: only the
    // batch writer got anything stored in 5 s).
    let _serial = LONG_LOCKS.lock().await;
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
                        origin: None,
                    })
                    .collect();
                let batch = Batch {
                    interface: &t.name,
                    messages,
                    dedupe_window: window,
                    relaxed: false,
                    state: None,
                    destinations: &t.ids,
                    tier: TIER,
                };
                let duplicates = store.insert_batch(&batch).await.unwrap();
                queued.fetch_add(duplicates.iter().filter(|d| !**d).count(), Relaxed);
                round += 1;
            }
        }));
    }
    // `archive` routes everything to a second interface (`Store::finish_routed`), drained by the
    // last task; it stops once the first ones have stopped.
    let routed = Arc::new(Iface::single(&store, "stress-routed").await);
    let sources_done = Arc::new(AtomicUsize::new(0));
    // (destination, where it routes to, whether it's the routed interface's own drain)
    let drains = [
        (t.id("lab"), None, false),
        (t.id("billing"), None, false),
        (t.id("archive"), Some(routed.clone()), false),
        (routed.dest(), None, true),
    ];
    for (n, (destination, route, is_route_target)) in drains.into_iter().enumerate() {
        let (store, stop, lanes, sources_done) = (store.clone(), stop.clone(), lanes.clone(), sources_done.clone());
        tasks.push(tokio::spawn(async move {
            // A source drain counts as done however it ends: one that panics must fail the test,
            // not leave the routed drain waiting for it forever.
            struct Done(Arc<AtomicUsize>, bool);
            impl Drop for Done {
                fn drop(&mut self) {
                    if !self.1 {
                        self.0.fetch_add(1, Relaxed);
                    }
                }
            }
            let _done = Done(sources_done.clone(), is_route_target);
            let mut rounds = 0usize;
            loop {
                let stopped = if is_route_target { sources_done.load(Relaxed) == 3 } else { stop.load(Relaxed) };
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
                match &route {
                    Some(target) => {
                        let messages = pending
                            .iter()
                            .map(|p| BatchMessage {
                                partition_key: (p.id % 8) as i32,
                                ordering_key: None,
                                control_id: None,
                                payload: &p.payload,
                                dedupe_key: None,
                                origin: Some(p.id),
                            })
                            .collect();
                        let batch = Batch {
                            interface: &target.name,
                            messages,
                            dedupe_window: window,
                            relaxed: true,
                            state: None,
                            destinations: &target.ids,
                            tier: TIER,
                        };
                        store.finish_routed(destination, &done, &batch).await.unwrap();
                    }
                    None => store.finish(destination, &done).await.unwrap(),
                }
            }
        }));
    }
    // The REST API's reads: the monitor's queue counts and someone paging through messages;
    // monitoring saving last activity, and someone pausing and resuming a destination.
    store.activity(&[&t.name], &t.ids).await.unwrap();
    {
        let (store, t, stop) = (store.clone(), t.clone(), stop.clone());
        tasks.push(tokio::spawn(async move {
            let mut round = 0usize;
            while !stop.load(Relaxed) {
                round += 1;
                let now = chrono::Utc::now();
                let seen: Vec<_> = t
                    .ids
                    .iter()
                    .map(|&destination| ductus_store::Seen { destination, last_sent: Some(now), last_error: None })
                    .collect();
                store.save_activity(&[(&t.name, now)], &seen).await.unwrap();
                if round.is_multiple_of(2) {
                    store.pause(t.id("lab"), "stress", Some("testing")).await.unwrap();
                } else {
                    store.unpause(t.id("lab")).await.unwrap();
                }
                store.queue_counts().await.unwrap();
                store.oldest_pending(&t.ids).await.unwrap();
                if round.is_multiple_of(10) {
                    let baselines: Vec<_> = t
                        .ids
                        .iter()
                        .map(|&destination| ductus_store::BaselineRow {
                            destination,
                            inflow: vec![Some(round as f32); 168],
                            send_ms: vec![None; 168],
                        })
                        .collect();
                    store.save_baselines(&baselines).await.unwrap();
                }
                let page = Browse { interface: &t.name, status: None, before: None, limit: 50 };
                let rows = store.browse(&page).await.unwrap();
                if let Some(last) = rows.last() {
                    store.message(last.id).await.unwrap().expect("a listed message can be read");
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        }));
    }
    // A retention change under way: the interface's finished messages move to the 49d tier (with
    // their deliveries) while new ones keep arriving in the 21d tier and destinations settle.
    let mover = {
        let (store, t, stop) = (store.clone(), t.clone(), stop.clone());
        tokio::spawn(async move {
            let mut moved = 0;
            while !stop.load(Relaxed) {
                moved += store.retention_step(&t.name, 49, 200).await.unwrap().moved;
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            moved
        })
    };
    // The held area: today's unfinished messages (pending ones, which the destinations are
    // draining right now) move out of their partition with their deliveries, as an expired
    // partition's do. A drain whose update meets a row moved under it must run it again and find
    // the row in the held area.
    let holder = {
        let (store, stop, pool) = (store.clone(), stop.clone(), pool.clone());
        tokio::spawn(async move {
            let partition: String =
                sqlx::query_scalar("SELECT ductus_create_tier_partition('messages', $1, current_date)")
                    .bind(i32::from(TIER))
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            let (mut after, mut held) = (0, 0);
            while !stop.load(Relaxed) {
                let (n, last) = store.hold_step(&partition, after, 100).await.unwrap();
                held += n;
                after = last.unwrap_or(0); // from the start again once through
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            held
        })
    };
    let maintenance = {
        let (store, stop) = (store.clone(), stop.clone());
        tokio::spawn(async move {
            let mut day = 0;
            while !stop.load(Relaxed) {
                day += 1;
                maintain(&store, furthest + day).await;
                store.purge_dedupe_keys(window).await.unwrap();
                if day % 5 == 0 {
                    store.drop_expired().await.unwrap();
                }
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
    let moved = mover.await;
    let held = holder.await;
    drop_test_partitions(&pool).await;
    for r in results {
        r.expect("a writer or drain failed");
    }
    let days = days.expect("maintenance failed");
    let moved = moved.expect("moving to another tier failed");
    assert!(moved > 0, "the retention change moved something");
    let held = held.expect("moving to the held area failed");
    assert!(held > 0, "something moved to the held area");
    // What's left moves now (from the held area too: all of it is finished and within the
    // window); then every message is in the 49d tier, each with its deliveries.
    while !store.retention_step(&t.name, 49, 1000).await.unwrap().is_empty() {}
    let misplaced: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM messages WHERE interface = $1 AND tier <> 49)
              + (SELECT count(*) FROM deliveries WHERE destination_id = ANY($2) AND tier <> 49)",
    )
    .bind(&t.name)
    .bind(&t.ids)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(misplaced, 0);

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
    assert!(store.payloads(&routed.name, Status::Pending).await.unwrap().is_empty(), "routed: nothing left pending");
    assert_eq!(store.payloads(&routed.name, Status::Sent).await.unwrap().len(), queued, "each routed exactly once");
    println!(
        "stress: {} messages stored ({queued} queued, {duplicates} duplicates), 3 destinations (one routing {queued} to another interface), {moved} moved to another tier and {held} to the held area meanwhile, {days} maintenance rounds, {seconds}s",
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

#[tokio::test]
async fn tokens_are_stored_hashed_and_can_be_revoked() {
    let Some(store) = store().await else { return };
    let name = format!("test-token-{}", std::process::id());
    let (token, secret) = store.create_token(&name, Role::Operator).await.unwrap();
    assert!(secret.starts_with(ductus_store::TOKEN_PREFIX) && secret.len() == 68, "{secret}");
    assert_eq!((token.name.as_str(), token.role), (name.as_str(), Role::Operator));
    let err = store.create_token(&name, Role::Viewer).await.unwrap_err().to_string();
    assert!(err.contains("already exists"), "{err}");

    let found = store.authenticate(&secret).await.unwrap().expect("the token works");
    assert_eq!(found.id, token.id);
    assert!(store.tokens(false).await.unwrap().iter().any(|t| t.id == token.id && t.last_used_at.is_some()));
    assert!(store.authenticate("dux_0000").await.unwrap().is_none());
    assert!(store.authenticate("not a token").await.unwrap().is_none());

    assert!(store.revoke_token(&name).await.unwrap());
    assert!(!store.revoke_token(&name).await.unwrap(), "nothing left to revoke");
    assert!(store.authenticate(&secret).await.unwrap().is_none(), "revoked tokens stop working");
    assert!(!store.tokens(false).await.unwrap().iter().any(|t| t.id == token.id));
    assert!(store.tokens(true).await.unwrap().iter().any(|t| t.id == token.id && t.revoked_at.is_some()));
    // The name is free again.
    let (again, _) = store.create_token(&name, Role::Viewer).await.unwrap();
    store.revoke_token(&name).await.unwrap();
    assert_ne!(again.id, token.id);
}

/// A browser signs in with a token and keeps a session instead: it acts as the token, ends on
/// sign-out, when the token is revoked, or when it expires; a session is never a token.
#[tokio::test]
async fn sessions_act_as_their_token_until_they_end() {
    let Some(url) = std::env::var("DUCTUS_TEST_DATABASE_URL").ok() else { return };
    let store = Store::connect(&url, 4).await.unwrap();
    let pool = sqlx::PgPool::connect(&url).await.unwrap();
    let name = format!("test-session-{}", std::process::id());
    let (token, token_secret) = store.create_token(&name, Role::Operator).await.unwrap();

    let (secret, expires_at) = store.create_session(&token).await.unwrap();
    assert!(secret.starts_with(ductus_store::SESSION_PREFIX) && secret.len() == 68, "{secret}");
    let days = (expires_at - chrono::Utc::now()).num_hours();
    assert!((167..=168).contains(&days), "a week from now: {expires_at}");
    let (found, _) = store.authenticate_session(&secret).await.unwrap().expect("the session works");
    assert_eq!((found.id, found.role), (token.id, Role::Operator));
    assert!(store.authenticate(&secret).await.unwrap().is_none(), "a session isn't a token");
    assert!(store.authenticate_session(&token_secret).await.unwrap().is_none(), "nor a token a session");

    // Sign-out.
    assert!(store.end_session(&secret).await.unwrap());
    assert!(store.authenticate_session(&secret).await.unwrap().is_none());

    // Idle too long, or past its end.
    let (idle, _) = store.create_session(&token).await.unwrap();
    let (old, _) = store.create_session(&token).await.unwrap();
    let (live, _) = store.create_session(&token).await.unwrap();
    for (set, secret) in
        [("last_seen_at = now() - interval '13 hours'", &idle), ("expires_at = now() - interval '1 second'", &old)]
    {
        let sql = format!("UPDATE api_sessions SET {set} WHERE hash = sha256(convert_to($1, 'UTF8'))");
        let changed = sqlx::query(sqlx::AssertSqlSafe(sql)).bind(secret).execute(&pool).await.unwrap();
        assert_eq!(changed.rows_affected(), 1);
    }
    assert!(store.authenticate_session(&idle).await.unwrap().is_none(), "idle 13 hours");
    assert!(store.authenticate_session(&old).await.unwrap().is_none(), "past its end");
    assert!(store.authenticate_session(&live).await.unwrap().is_some());

    // Revoking the token ends its sessions.
    store.revoke_token(&name).await.unwrap();
    assert!(store.authenticate_session(&live).await.unwrap().is_none());
}

#[tokio::test]
async fn messages_page_newest_first_and_queues_are_counted() {
    let Some(store) = store().await else { return };
    let t = Iface::new(&store, "browse", &["lab", "billing"]).await;
    let mut refs = Vec::new();
    for n in 0..5 {
        refs.push(store.insert(t.msg(0, format!("m{n}").as_bytes())).await.unwrap());
    }
    // m0 sent everywhere; m1 quarantined by lab; the rest pending.
    store.finish(t.id("lab"), &[sent(refs[0])]).await.unwrap();
    store.finish(t.id("billing"), &[sent(refs[0]), sent(refs[1])]).await.unwrap();
    store.finish(t.id("lab"), &[Done::with(refs[1], Status::Quarantined, Some("AR: unknown patient"))]).await.unwrap();

    let page = |status, before| Browse { interface: &t.name, status, before, limit: 2 };
    let first = store.browse(&page(None, None)).await.unwrap();
    assert_eq!(first.iter().map(|r| r.id).collect::<Vec<_>>(), [refs[4].id, refs[3].id]);
    let second = store.browse(&page(None, Some(first[1].id))).await.unwrap();
    assert_eq!(second.iter().map(|r| r.id).collect::<Vec<_>>(), [refs[2].id, refs[1].id]);
    let quarantined = store.browse(&page(Some(Status::Quarantined), None)).await.unwrap();
    assert_eq!(quarantined.len(), 1);
    assert_eq!(quarantined[0].last_error.as_deref(), Some("lab: AR: unknown patient"));
    assert_eq!((quarantined[0].size, quarantined[0].status), (2, Status::Quarantined));

    let (row, payload) = store.message(refs[1].id).await.unwrap().unwrap();
    assert_eq!((row.interface.as_str(), payload.as_slice()), (t.name.as_str(), &b"m1"[..]));
    assert!(store.message(-1).await.unwrap().is_none());

    let counts = store.queue_counts().await.unwrap();
    let count = |d| counts.iter().find(|c| c.destination_id == d).copied();
    assert_eq!(count(t.id("lab")), Some(QueueCount { destination_id: t.id("lab"), pending: 3, quarantined: 1 }));
    assert_eq!(
        count(t.id("billing")),
        Some(QueueCount { destination_id: t.id("billing"), pending: 3, quarantined: 0 })
    );
}

/// A message stored `days_ago` (at noon) in a tier, with one delivery per destination, written
/// straight to the tables: the engine only stores messages now. Creates the partitions it needs.
async fn insert_at(pool: &sqlx::PgPool, t: &Iface, days_ago: i32, tier: i16, status: Status, payload: &str) -> i64 {
    sqlx::query("SELECT ductus_create_tier_partition('messages', $1, current_date - $2)")
        .bind(i32::from(tier))
        .bind(days_ago)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("SELECT ductus_ensure_delivery_partitions($1)").bind(i32::from(tier)).execute(pool).await.unwrap();
    sqlx::query_scalar(
        "WITH m AS (
             INSERT INTO messages (received_at, interface, partition_key, payload, status, tier)
             VALUES (current_date - $2 + interval '12 hours', $1, 0, $3, $4, $5) RETURNING id, received_at
         ), d AS (
             INSERT INTO deliveries (message_id, received_at, destination_id, partition_key, status, tier)
             SELECT id, received_at, unnest($6::int[]), 0, $4, $5 FROM m
         )
         SELECT id FROM m",
    )
    .bind(&t.name)
    .bind(days_ago)
    .bind(payload.as_bytes())
    .bind(status as i16)
    .bind(tier)
    .bind(&t.ids)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Each of an interface's messages (by payload) and the tier it's in, in payload order.
async fn tiers_of(pool: &sqlx::PgPool, interface: &str) -> Vec<(String, i16)> {
    let rows: Vec<(Vec<u8>, i16)> =
        sqlx::query_as("SELECT payload, tier FROM messages WHERE interface = $1 ORDER BY payload")
            .bind(interface)
            .fetch_all(pool)
            .await
            .unwrap();
    rows.into_iter().map(|(p, t)| (String::from_utf8(p).unwrap(), t)).collect()
}

/// The 7d tier's `messages` partition for `days_ago`, and whether it exists.
async fn seven_day_partition(pool: &sqlx::PgPool, days_ago: i32) -> String {
    sqlx::query_scalar("SELECT 'messages_t7_' || to_char(current_date - $1, 'YYYYMMDD')")
        .bind(days_ago)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn exists(pool: &sqlx::PgPool, table: &str) -> bool {
    sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL").bind(table).fetch_one(pool).await.unwrap()
}

/// One change at a time: longer starts at once, shorter waits for a confirmation, and a value
/// configured while a change runs waits for it.
#[tokio::test]
async fn retention_changes_one_at_a_time_and_shorter_needs_a_confirmation() {
    let Some(store) = store().await else { return };
    let t = Iface::single(&store, "ret-apply").await;
    let s = store.apply_retention(&t.name, 21).await.unwrap();
    assert_eq!((s.days, s.changing_from, s.waiting), (21, None, None), "first time: applied as is");
    assert!(store.apply_retention(&t.name, 30).await.is_err(), "not a tier");

    let s = store.apply_retention(&t.name, 49).await.unwrap();
    assert_eq!((s.days, s.changing_from), (49, Some(21)), "longer starts now");
    let s = store.apply_retention(&t.name, 7).await.unwrap();
    assert_eq!((s.days, s.changing_from, s.waiting), (49, Some(21), Some(7)), "waits for the running change");
    assert_eq!(store.finish_retention_change(&t.name).await.unwrap(), 0);

    let s = store.apply_retention(&t.name, 7).await.unwrap();
    assert_eq!((s.days, s.changing_from, s.waiting), (49, None, Some(7)));
    assert!(s.needs_confirmation(), "shorter deletes messages");
    let s = store.confirm_retention(&t.name).await.unwrap().expect("something to confirm");
    assert_eq!(s.confirmed, Some(7));
    let s = store.apply_retention(&t.name, 7).await.unwrap();
    assert_eq!((s.days, s.changing_from, s.waiting, s.confirmed), (7, Some(49), None, None), "confirmed: started");
    assert_eq!(store.confirm_retention(&t.name).await.unwrap(), None, "nothing waits");

    // Configured back to the applied value: whatever waited is dropped.
    store.finish_retention_change(&t.name).await.unwrap();
    let s = store.apply_retention(&t.name, 21).await.unwrap();
    assert_eq!((s.days, s.changing_from), (21, Some(7)));
    store.finish_retention_change(&t.name).await.unwrap();
    assert_eq!(store.apply_retention(&t.name, 7).await.unwrap().waiting, Some(7));
    let s = store.apply_retention(&t.name, 21).await.unwrap();
    assert_eq!((s.days, s.changing_from, s.waiting), (21, None, None));
}

/// Shortening 7w → 1w: finished messages past 7 days are deleted with their deliveries, newer
/// ones move to the 7d tier, quarantined ones are never deleted, pending ones move once delivered.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_shorter_retention_deletes_what_is_past_it_and_moves_the_rest() {
    let Some(store) = store().await else { return };
    let _serial = LONG_LOCKS.lock().await;
    let pool = sqlx::PgPool::connect(&std::env::var("DUCTUS_TEST_DATABASE_URL").unwrap()).await.unwrap();
    let t = Iface::new(&store, "ret-shorten", &["lab", "billing"]).await;
    store.apply_retention(&t.name, 49).await.unwrap();
    insert_at(&pool, &t, 40, 49, Status::Sent, "a old sent").await;
    insert_at(&pool, &t, 40, 49, Status::Quarantined, "b old quarantined").await;
    insert_at(&pool, &t, 2, 49, Status::Sent, "c new sent").await;
    let pending = insert_at(&pool, &t, 2, 49, Status::Pending, "d new pending").await;

    store.apply_retention(&t.name, 7).await.unwrap();
    assert_eq!(store.retention_would_delete(&t.name, 7).await.unwrap(), 1);
    store.confirm_retention(&t.name).await.unwrap();
    let s = store.apply_retention(&t.name, 7).await.unwrap();
    assert_eq!((s.days, s.changing_from), (7, Some(49)));
    assert_eq!(store.start_retention_change(&t.name, 7).await.unwrap(), 3, "a, c, d (b stays)");

    let mut done = ductus_store::Step::default();
    loop {
        let step = store.retention_step(&t.name, 7, 1).await.unwrap();
        if step.is_empty() {
            break;
        }
        done.deleted += step.deleted;
        done.moved += step.moved;
    }
    assert_eq!(done, ductus_store::Step { deleted: 1, moved: 1 });
    assert_eq!(store.retention_waiting_for_delivery(&t.name, 7).await.unwrap(), 1, "d is still pending");

    let received_at: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("SELECT received_at FROM messages WHERE id = $1")
            .bind(pending)
            .fetch_one(&pool)
            .await
            .unwrap();
    let d = MessageRef { id: pending, received_at };
    store.finish(t.id("lab"), &[sent(d)]).await.unwrap();
    store.finish(t.id("billing"), &[sent(d)]).await.unwrap();
    assert_eq!(store.retention_step(&t.name, 7, 10).await.unwrap(), ductus_store::Step { deleted: 0, moved: 1 });
    assert_eq!(store.retention_waiting_for_delivery(&t.name, 7).await.unwrap(), 0);
    assert_eq!(store.finish_retention_change(&t.name).await.unwrap(), 1, "the old quarantined one stays where it was");

    assert_eq!(
        tiers_of(&pool, &t.name).await,
        [("b old quarantined".into(), 49), ("c new sent".into(), 7), ("d new pending".into(), 7)]
    );
    let deliveries: Vec<(i16, i64)> = sqlx::query_as(
        "SELECT d.tier, count(*) FROM deliveries d JOIN destinations t ON t.id = d.destination_id
         WHERE t.interface = $1 GROUP BY 1 ORDER BY 1",
    )
    .bind(&t.name)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(deliveries, [(7, 4), (49, 2)], "deliveries went with their messages");
    let s = store.retention().await.unwrap().into_iter().find(|s| s.interface == t.name).unwrap();
    assert_eq!((s.days, s.changing_from, s.rows_left), (7, None, None));
}

/// Expired partitions are dropped, `deliveries` with `messages`, except while they hold
/// quarantined (or pending) messages, or rows of an interface that keeps its messages longer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expired_partitions_are_dropped_unless_something_must_stay() {
    let Some(store) = store().await else { return };
    let _serial = LONG_LOCKS.lock().await;
    let pool = sqlx::PgPool::connect(&std::env::var("DUCTUS_TEST_DATABASE_URL").unwrap()).await.unwrap();
    let a = Iface::single(&store, "ret-drop-a").await;
    let b = Iface::single(&store, "ret-drop-b").await;
    insert_at(&pool, &a, 30, 7, Status::Sent, "sent long ago").await;
    insert_at(&pool, &a, 20, 7, Status::Quarantined, "quarantined long ago").await;
    insert_at(&pool, &b, 15, 7, Status::Sent, "b's, kept longer now").await;
    store.apply_retention(&b.name, 21).await.unwrap();
    let gone = seven_day_partition(&pool, 30).await;
    let quarantined = seven_day_partition(&pool, 20).await;
    let kept = seven_day_partition(&pool, 15).await;

    let expired = store.drop_expired().await.unwrap();
    assert!(expired.dropped.contains(&gone), "{expired:?}");
    let held = |name: &str| expired.kept.iter().find(|h| h.partition == name).cloned();
    let q = held(&quarantined).expect("quarantined messages keep their partition");
    assert_eq!((q.tier, q.pending, q.quarantined, q.kept_for.len()), (7, 0, 1, 0));
    assert_eq!(held(&kept).expect("b keeps it").kept_for, [b.name.as_str()]);
    assert!(!exists(&pool, &gone).await && !exists(&pool, &gone.replace("messages", "deliveries")).await);
    assert!(exists(&pool, &quarantined).await && exists(&pool, &quarantined.replace("messages", "deliveries")).await);

    // Once resolved (and moved away), they go too.
    sqlx::query("UPDATE deliveries SET status = 1 WHERE destination_id = $1")
        .bind(a.dest())
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE messages SET status = 1 WHERE interface = $1").bind(&a.name).execute(&pool).await.unwrap();
    sqlx::query("DELETE FROM deliveries WHERE destination_id = $1").bind(b.dest()).execute(&pool).await.unwrap();
    sqlx::query("DELETE FROM messages WHERE interface = $1").bind(&b.name).execute(&pool).await.unwrap();
    let expired = store.drop_expired().await.unwrap();
    assert!(expired.dropped.contains(&quarantined) && expired.dropped.contains(&kept), "{expired:?}");
    assert!(!exists(&pool, &quarantined).await && !exists(&pool, &kept).await);
}

/// An expired partition's unfinished messages move to the held area with all their deliveries
/// (quarantined ones too, unless the interface purges them), then the partition drops. Held
/// messages stay in their destination's queue and finish there; a past month drops once
/// nothing in it is unfinished.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expired_unfinished_messages_move_to_the_held_area() {
    let Some(store) = store().await else { return };
    let _serial = LONG_LOCKS.lock().await;
    let pool = sqlx::PgPool::connect(&std::env::var("DUCTUS_TEST_DATABASE_URL").unwrap()).await.unwrap();
    let keep = Iface::new(&store, "held-keep", &["lab", "billing"]).await;
    let purge = Iface::single(&store, "held-purge").await;
    for t in [&keep, &purge] {
        store.apply_retention(&t.name, 7).await.unwrap();
    }
    store.set_quarantined(&[(keep.name.clone(), false), (purge.name.clone(), true)]).await.unwrap();
    // 33 days ago: always last month or earlier, so its held month can be dropped.
    insert_at(&pool, &keep, 33, 7, Status::Sent, "a sent").await;
    let pending = insert_at(&pool, &keep, 33, 7, Status::Pending, "b pending").await;
    insert_at(&pool, &keep, 33, 7, Status::Quarantined, "c quarantined").await;
    insert_at(&pool, &purge, 33, 7, Status::Quarantined, "d quarantined, purged").await;
    // b: lab has it, billing is down.
    sqlx::query("UPDATE deliveries SET status = 1 WHERE message_id = $1 AND destination_id = $2")
        .bind(pending)
        .bind(keep.id("lab"))
        .execute(&pool)
        .await
        .unwrap();
    let partition = seven_day_partition(&pool, 33).await;

    let expired = store.drop_expired().await.unwrap();
    let kept = expired.kept.iter().find(|k| k.partition == partition).expect("kept: b and c are unfinished");
    // (At least: a run that failed midway leaves its messages in the same partition.)
    assert!(kept.pending >= 1 && kept.quarantined >= 1, "{kept:?}");
    assert!(kept.to_hold());
    let purged: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT count(*) FROM {partition} WHERE interface = $1 AND status = 3"
    )))
    .bind(&purge.name)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(purged, 1);

    let (mut after, mut moved) = (0, 0);
    loop {
        let (n, last) = store.hold_step(&partition, after, 1).await.unwrap();
        moved += n;
        match last {
            Some(id) => after = id,
            None => break,
        }
    }
    assert!(moved >= 2, "b and c (d's interface purges quarantined messages)");
    assert_eq!(
        tiers_of(&pool, &keep.name).await,
        [("a sent".into(), 7), ("b pending".into(), ductus_store::HELD), ("c quarantined".into(), ductus_store::HELD)]
    );
    let held_deliveries: i64 = sqlx::query_scalar("SELECT count(*) FROM deliveries WHERE message_id = $1 AND tier = 0")
        .bind(pending)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(held_deliveries, 2, "every delivery went with its message, finished or not");

    // The partition drops now, with the finished message and the purged quarantined one.
    let expired = store.drop_expired().await.unwrap();
    assert!(expired.dropped.contains(&partition), "{expired:?}");
    assert!(tiers_of(&pool, &purge.name).await.is_empty(), "purged with its partition");

    // Still in billing's queue; finishing it works as before.
    let queue = store.pending(keep.id("billing"), &[0], 10).await.unwrap();
    assert_eq!(payloads(queue), [b"b pending".to_vec()]);
    let received_at: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("SELECT received_at FROM messages WHERE id = $1")
            .bind(pending)
            .fetch_one(&pool)
            .await
            .unwrap();
    store.finish(keep.id("billing"), &[sent(MessageRef { id: pending, received_at })]).await.unwrap();
    assert_eq!(status_of(&store, &keep.name, b"b pending").await, Status::Sent);

    let area = store.held_area().await.unwrap();
    let mine = area.interfaces.iter().find(|i| i.interface == keep.name).expect("c is still held");
    assert_eq!((mine.pending, mine.quarantined), (0, 1));
    let month: String = sqlx::query_scalar("SELECT 'messages_held_' || to_char(current_date - 33, 'YYYYMM')")
        .fetch_one(&pool)
        .await
        .unwrap();
    let m = area.months.iter().find(|m| m.partition == month).expect("its month");
    assert!(m.messages >= 2 && m.quarantined >= 1 && m.bytes > 0, "{m:?}");

    // The month stays while c is unresolved; once it is, the month goes whole.
    assert!(!store.drop_finished_held().await.unwrap().contains(&month));
    assert!(exists(&pool, &month).await);
    // Resolved (with anything a failed run left in past months: only tests put rows there).
    for table in ["deliveries", "messages"] {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE {table} SET status = 1
             WHERE tier = 0 AND status IN (0, 3) AND received_at < date_trunc('month', now() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC'"
        )))
        .execute(&pool)
        .await
        .unwrap();
    }
    assert!(store.drop_finished_held().await.unwrap().contains(&month), "nothing unfinished in it now");
    assert!(!exists(&pool, &month).await && !exists(&pool, &month.replace("messages", "deliveries")).await);
}
