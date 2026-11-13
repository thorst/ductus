//! Database-backed tests. Skipped unless DUCTUS_TEST_DATABASE_URL is set
//! (CI sets it; locally: scripts/dev/db.sh prints the one the scripts use).

use ductus_store::{
    Batch, BatchMessage, Browse, DedupeKey, DestinationId, Done, MessageRef, NewMessage, QueueCount, Reconciled, Role,
    SessionFor, SessionLimits, Status, Store, testing::TestUsers,
};

/// The retention tier test messages go to.
const TIER: ductus_store::Tier = 21;

async fn store() -> Option<Store> {
    let url = std::env::var("DUCTUS_TEST_DATABASE_URL").ok()?;
    Some(Store::connect(&url, 4).await.expect("connecting to test database"))
}

/// The engine's start says in words what was wrong with its database URL, not the server's text
/// with its internal source line ("… at line 1016").
#[tokio::test]
async fn a_wrong_database_url_is_said_in_words() {
    let Some(url) = std::env::var("DUCTUS_TEST_DATABASE_URL").ok() else { return };
    let mut nowhere = url::Url::parse(&url).unwrap();
    nowhere.set_path("/ductus_no_such_database");
    let e = Store::connect(nowhere.as_str(), 1).await.err().expect("no such database").to_string();
    assert!(e.contains("there's no database `ductus_no_such_database` on that server"), "{e}");
    assert!(!e.contains(" at line "), "{e}");
    // A server that trusts local logins takes any password: then there's nothing to check.
    let mut wrong = url::Url::parse(&url).unwrap();
    wrong.set_password(Some("not-the-password")).unwrap();
    if let Err(e) = Store::connect_read_only(wrong.as_str()).await {
        let user = wrong.username().to_string();
        assert!(e.to_string().contains(&format!("the password for `{user}` was refused")), "{e}");
    }
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

const OPERATOR: ductus_store::Actor<'static> = ductus_store::Actor {
    by: ductus_store::By { kind: "system", id: None, user_id: None, name: "test-operator", via: "cli", ip: None },
    reason: Some("vendor fixed their side"),
};

/// An audit entry for tests that aren't about the log.
fn quiet() -> ductus_store::Audit<'static> {
    ductus_store::Audit::new(ductus_store::By::cli("test"), "test")
}

async fn status_by_id(store: &Store, id: i64) -> Status {
    store.message(id).await.unwrap().expect("the message exists").0.status
}

async fn delivery_states(store: &Store, m: MessageRef) -> Vec<(String, Status, i32)> {
    store.deliveries(m).await.unwrap().into_iter().map(|d| (d.destination, d.status, d.attempts)).collect()
}

#[tokio::test]
async fn resend_puts_deliveries_back_in_place_and_records_what_they_were() {
    use ductus_store::Resend;
    let Some(store) = store().await else { return };
    let t = Iface::new(&store, "resend", &["billing", "lab"]).await;
    let (lab, billing) = (t.id("lab"), t.id("billing"));
    let a = store.insert(t.msg(3, b"a")).await.unwrap();
    let b = store.insert(t.msg(3, b"b")).await.unwrap();
    store.record_failure(a, lab, "timeout").await.unwrap();
    store.finish(lab, &[Done::with(a, Status::Quarantined, Some("HTTP 400: bad PID-3"))]).await.unwrap();
    store.finish(billing, &[sent(a)]).await.unwrap();
    assert_eq!(status_by_id(&store, a.id).await, Status::Quarantined);

    // Without destinations named: the quarantined ones only (billing has it already).
    let quarantined = Resend { messages: &[a.id, b.id], destinations: &t.ids, quarantined_only: true };
    let resent = store.resend(quarantined, OPERATOR).await.unwrap();
    assert_eq!(resent.len(), 1, "b is still pending: nothing to resend");
    let r = &resent[0];
    assert_eq!((r.message_id, r.destination.as_str(), r.partition_key), (a.id, "lab", 3));
    assert_eq!(
        (r.previous_status, r.previous_attempts, r.previous_error.as_deref()),
        (Status::Quarantined, 1, Some("HTTP 400: bad PID-3"))
    );
    assert_eq!(status_by_id(&store, a.id).await, Status::Pending, "pending again until the lab is done");
    assert_eq!(
        delivery_states(&store, a).await,
        [("billing".into(), Status::Sent, 0), ("lab".into(), Status::Pending, 0)],
        "attempts start over (the old count is in the record)"
    );
    // In place: the lab's worker takes it first in its lane, ahead of b.
    let queue = store.pending(lab, &[3], 10).await.unwrap();
    assert_eq!(queue.iter().map(|p| p.id).collect::<Vec<_>>(), [a.id, b.id]);
    assert!(store.resend(quarantined, OPERATOR).await.unwrap().is_empty(), "pending again: nothing to resend");

    store.finish(lab, &[sent(a), sent(b)]).await.unwrap();
    store.finish(billing, &[sent(b)]).await.unwrap();
    assert_eq!(status_by_id(&store, a.id).await, Status::Sent);

    // Naming a destination resends whatever it ended as: a replay of what billing was sent.
    let replay = Resend { messages: &[a.id], destinations: &[billing], quarantined_only: false };
    let resent = store.resend(replay, OPERATOR).await.unwrap();
    assert_eq!(
        resent.iter().map(|r| (r.destination.as_str(), r.previous_status)).collect::<Vec<_>>(),
        [("billing", Status::Sent)]
    );
    assert_eq!(status_by_id(&store, a.id).await, Status::Pending);
    assert!(store.pending(lab, &[3], 10).await.unwrap().is_empty(), "the lab isn't sent it again");
    store.finish(billing, &[sent(a)]).await.unwrap();
    assert_eq!(status_by_id(&store, a.id).await, Status::Sent);

    let actions = store.actions(a.id).await.unwrap();
    let actions: Vec<_> = actions
        .iter()
        .map(|a| (a.action.as_str(), a.destination.as_str(), a.previous_status, a.actor.as_str()))
        .collect();
    assert_eq!(actions, [("resend", "lab", Some(3), "test-operator"), ("resend", "billing", Some(1), "test-operator")]);

    // Duplicates are never queued, so never resent.
    let window = std::time::Duration::from_secs(60);
    let key = format!("{}-dup", t.name);
    store.insert_deduped(t.msg(0, b"d1"), DedupeKey { key: &key, window }).await.unwrap();
    let dup = store.insert_deduped(t.msg(0, b"d2"), DedupeKey { key: &key, window }).await.unwrap();
    assert!(dup.duplicate);
    let r = Resend { messages: &[dup.message.id], destinations: &t.ids, quarantined_only: false };
    assert!(store.resend(r, OPERATOR).await.unwrap().is_empty());
}

#[tokio::test]
async fn an_edited_copy_replaces_the_quarantined_delivery() {
    use ductus_store::{Copy, Edited};
    let Some(store) = store().await else { return };
    let t = Iface::new(&store, "edit", &["billing", "lab"]).await;
    let (lab, billing) = (t.id("lab"), t.id("billing"));
    // A routed message, so the copy's `origin` can be checked.
    let source = store.insert(t.msg(0, b"source")).await.unwrap();
    let mut routed = batch(&t, &[("MSH|bad", None)], None);
    routed.messages[0].origin = Some(source.id);
    store.insert_batch(&routed).await.unwrap();
    let newest = Browse { interface: &t.name, status: None, before: None, limit: 1 };
    let row = store.browse(&newest).await.unwrap().remove(0);
    let original = MessageRef { id: row.id, received_at: row.received_at };
    let copy = |destinations| Copy {
        original: original.id,
        interface: &t.name,
        partition_key: 5,
        ordering_key: Some("MRN-1"),
        control_id: Some("CTL-2"),
        payload: b"MSH|fixed",
        tier: TIER,
        destinations,
    };
    let (lab_only, billing_only) = ([lab], [billing]);

    // Pending for the lab: a worker may be sending it, so it can't be replaced.
    match store.copy_edited(copy(&lab_only), OPERATOR).await.unwrap() {
        Edited::StillPending(d) => assert_eq!(d, [lab]),
        other => panic!("expected StillPending, got {other:?}"),
    }
    assert!(store.actions(original.id).await.unwrap().is_empty(), "nothing recorded");

    store.finish(lab, &[Done::with(original, Status::Quarantined, Some("AR: bad PID-3"))]).await.unwrap();
    store.finish(billing, &[sent(original)]).await.unwrap();
    let Edited::Copied { copy: made, replaced } = store.copy_edited(copy(&lab_only), OPERATOR).await.unwrap() else {
        panic!("expected a copy")
    };
    assert_eq!(replaced, [lab]);
    assert!(made.id > original.id);

    // The original: the lab's delivery replaced (filtered), billing's untouched, so it's sent.
    let deliveries = store.deliveries(original).await.unwrap();
    let lab_delivery = deliveries.iter().find(|d| d.destination == "lab").unwrap();
    assert_eq!(lab_delivery.status, Status::Filtered);
    assert_eq!(lab_delivery.last_error.as_deref(), Some(format!("replaced by edited copy {}", made.id).as_str()));
    assert_eq!(status_by_id(&store, original.id).await, Status::Sent);

    // The copy: the edited payload, its own keys, the original's origin, queued for the lab only.
    let (row, payload) = store.message(made.id).await.unwrap().unwrap();
    assert_eq!((payload.as_slice(), row.status, row.origin_id), (&b"MSH|fixed"[..], Status::Pending, Some(source.id)));
    assert_eq!((row.ordering_key.as_deref(), row.control_id.as_deref()), (Some("MRN-1"), Some("CTL-2")));
    assert_eq!(delivery_states(&store, made).await, [("lab".into(), Status::Pending, 0)]);
    assert_eq!(store.pending(lab, &[5], 10).await.unwrap().iter().map(|p| p.id).collect::<Vec<_>>(), [made.id]);

    let actions = store.actions(original.id).await.unwrap();
    assert_eq!(actions.len(), 1);
    assert_eq!(
        (actions[0].action.as_str(), actions[0].previous_status, actions[0].copy_id),
        ("edit", Some(3), Some(made.id))
    );
    assert_eq!(store.actions(made.id).await.unwrap().len(), 1, "the copy finds the edit that made it");

    // Copying again (to billing, which has it): nothing replaced, the copy still recorded.
    let Edited::Copied { replaced, .. } = store.copy_edited(copy(&billing_only), OPERATOR).await.unwrap() else {
        panic!("expected a copy")
    };
    assert!(replaced.is_empty());
    assert_eq!(status_by_id(&store, original.id).await, Status::Sent);
}

/// A test alert (unique per test and run): its row as made.
async fn test_alert(store: &Store, test: &str) -> ductus_store::AlertRow {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let interface = format!("test-{test}-{}-{nanos}", std::process::id());
    let key = format!("dest:{interface}/lab");
    // Left by a run that failed before cleaning up (an open test alert would show in engines'
    // alert counts).
    let pool = sqlx::PgPool::connect(&std::env::var("DUCTUS_TEST_DATABASE_URL").unwrap()).await.unwrap();
    sqlx::query("DELETE FROM alert_state WHERE key LIKE 'dest:test-%' AND updated_at < now() - interval '10 minutes'")
        .execute(&pool)
        .await
        .unwrap();
    let keys = [(key.clone(), interface.clone(), Some("lab".to_string()))];
    store.ensure_alerts(&keys).await.unwrap();
    store.ensure_alerts(&keys).await.unwrap(); // again: nothing changes
    store.alerts().await.unwrap().into_iter().find(|a| a.key == key).expect("made")
}

/// An alert that fires, as the engine records it.
fn fired<'a>(deliveries: Vec<(String, chrono::DateTime<chrono::Utc>)>) -> ductus_store::AlertChange<'a> {
    let now = chrono::Utc::now();
    ductus_store::AlertChange {
        state: "stalled",
        severity: Some("critical"),
        since: Some(now),
        ok_since: None,
        last_notified_at: Some(now),
        summary: Some("stalled: nothing sent for 2 min"),
        stage: None,
        fresh: true,
        unacknowledge: false,
        cancel_waiting: false,
        event: Some(ductus_store::NewEvent {
            kind: "fired",
            state: "stalled",
            severity: Some("critical"),
            since: Some(now),
            deliveries,
        }),
    }
}

/// A change is stored once (compare-and-set on `rev`, as two engines would race), with its event and
/// its notifications; two senders claiming at once send each notification once; failures retry or
/// give up; and a recovery cancels what still waits for quiet hours to end.
#[tokio::test]
async fn alert_notifications_are_stored_first_and_sent_once() {
    let Some(store) = store().await else { return };
    let row = test_alert(&store, "outbox").await;
    assert_eq!((row.state.as_str(), row.rev, row.notify_count), ("ok", 0, 0));
    let now = chrono::Utc::now();
    let later = now + chrono::Duration::hours(8);
    let deliveries = vec![("oncall".to_string(), now), ("chat".to_string(), now), ("quiet".to_string(), later)];
    assert!(store.change_alert(row.id, row.rev, &fired(deliveries.clone())).await.unwrap());
    assert!(!store.change_alert(row.id, row.rev, &fired(deliveries)).await.unwrap(), "stale: the other engine's");
    let row = store.alert(row.id).await.unwrap().unwrap();
    assert_eq!((row.state.as_str(), row.severity.as_deref(), row.rev), ("stalled", Some("critical"), 1));
    let events = store.alert_events(Some(row.id), 10).await.unwrap();
    assert_eq!(events.len(), 1, "one event, though two engines tried");
    assert_eq!((events[0].kind.as_str(), events[0].waiting), ("fired", 3));
    assert!(store.alert_was_told(row.id).await.unwrap(), "due now and not sent yet still counts");

    // Two senders at once: each due notification is claimed by exactly one.
    let lease = std::time::Duration::from_secs(60);
    let (a, b) = tokio::join!(store.claim_alert_deliveries(50, lease), store.claim_alert_deliveries(50, lease));
    let mine: Vec<_> = a.unwrap().into_iter().chain(b.unwrap()).filter(|i| i.alert_id == row.id).collect();
    let mut channels: Vec<&str> = mine.iter().map(|i| i.channel.as_str()).collect();
    channels.sort();
    assert_eq!(channels, ["chat", "oncall"], "the quiet-hours one isn't due");
    assert!(mine.iter().all(|i| i.attempts == 1 && i.kind == "fired" && i.key == row.key));
    let again = store.claim_alert_deliveries(50, lease).await.unwrap();
    assert!(again.iter().all(|i| i.alert_id != row.id), "claimed ones wait for their lease");

    let oncall = mine.iter().find(|i| i.channel == "oncall").unwrap();
    let chat = mine.iter().find(|i| i.channel == "chat").unwrap();
    store.alert_delivery_sent(oncall.id).await.unwrap();
    store.alert_delivery_sent(oncall.id).await.unwrap(); // a retried mark counts once
    store.alert_delivery_failed(chat.id, "connection refused", Some(chrono::Utc::now())).await.unwrap();
    let retry = store.claim_alert_deliveries(50, lease).await.unwrap();
    let retry: Vec<_> = retry.into_iter().filter(|i| i.alert_id == row.id).collect();
    assert_eq!((retry.len(), retry[0].attempts), (1, 2), "retried");
    store.alert_delivery_failed(retry[0].id, "still refused", None).await.unwrap();
    assert_eq!(store.alert(row.id).await.unwrap().unwrap().notify_count, 1);
    let e = &store.alert_events(Some(row.id), 10).await.unwrap()[0];
    assert_eq!((e.sent, e.waiting, e.failed), (1, 1, 1));
    let counts = store.outbox_counts(1).await.unwrap();
    let chat = counts.iter().find(|c| c.channel == "chat").unwrap();
    assert!(chat.failed >= 1 && chat.last_error.is_some());

    // Recovered: what still waits for the morning is no longer news.
    let row = store.alert(row.id).await.unwrap().unwrap();
    let recovered = ductus_store::AlertChange {
        state: "ok",
        severity: None,
        since: None,
        ok_since: None,
        last_notified_at: None,
        summary: None,
        stage: None,
        fresh: true,
        unacknowledge: false,
        cancel_waiting: true,
        event: Some(ductus_store::NewEvent {
            kind: "recovered",
            state: "stalled",
            severity: Some("critical"),
            since: row.since,
            deliveries: vec![("oncall".to_string(), now)],
        }),
    };
    assert!(store.change_alert(row.id, row.rev, &recovered).await.unwrap());
    let row = store.alert(row.id).await.unwrap().unwrap();
    assert_eq!((row.state.as_str(), row.notify_count, row.since), ("ok", 0, None));
    let events = store.alert_events(Some(row.id), 10).await.unwrap();
    assert_eq!((events[0].kind.as_str(), events[0].state.as_deref()), ("recovered", Some("stalled")));
    assert_eq!((events[1].sent, events[1].waiting, events[1].failed), (1, 0, 1), "the morning one was cancelled");

    // An engine that dies holding a claim: whoever claims after the lease sends it (at least once).
    let short = std::time::Duration::from_millis(500);
    let held: Vec<_> =
        store.claim_alert_deliveries(50, short).await.unwrap().into_iter().filter(|i| i.alert_id == row.id).collect();
    assert_eq!((held.len(), held[0].kind.as_str()), (1, "recovered"));
    tokio::time::sleep(std::time::Duration::from_millis(700)).await;
    let taken: Vec<_> =
        store.claim_alert_deliveries(50, lease).await.unwrap().into_iter().filter(|i| i.alert_id == row.id).collect();
    assert_eq!((taken.len(), taken[0].id, taken[0].attempts), (1, held[0].id, 2));

    // History past `alerts.keep` goes, with its notifications.
    let pool = sqlx::PgPool::connect(&std::env::var("DUCTUS_TEST_DATABASE_URL").unwrap()).await.unwrap();
    sqlx::query("UPDATE alert_events SET at = now() - interval '400 days' WHERE alert_id = $1")
        .bind(row.id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(store.trim_alerts(std::time::Duration::from_secs(365 * 86_400), 1000, 10).await.unwrap() >= 2);
    assert!(store.alert_events(Some(row.id), 10).await.unwrap().is_empty());
    forget_alert(&pool, row.id).await;
}

/// Remove a test alert and its history.
async fn forget_alert(pool: &sqlx::PgPool, id: i32) {
    sqlx::query("DELETE FROM alert_events WHERE alert_id = $1").bind(id).execute(pool).await.unwrap();
    sqlx::query("DELETE FROM alert_state WHERE id = $1").bind(id).execute(pool).await.unwrap();
}

/// Acknowledging needs an open alert; silencing works on any. Both are events of the alert and
/// rows of the audit log, written together.
#[tokio::test]
async fn acknowledging_and_silencing_are_logged_with_the_change() {
    let Some(store) = store().await else { return };
    let row = test_alert(&store, "ack").await;
    let by = ductus_store::By::cli("test-user-anna");
    let audit = |action| ductus_store::Audit::new(by, action);
    assert!(store.acknowledge_alert(row.id, "anna", &audit("alert.ack")).await.unwrap().is_none(), "nothing open");

    let until = chrono::Utc::now() + chrono::Duration::hours(2);
    let silenced =
        store.silence_alert(row.id, Some(until), "anna", Some("planned work"), &audit("alert.silence")).await.unwrap();
    let silenced = silenced.unwrap();
    assert!(silenced.is_silenced(chrono::Utc::now()));
    assert_eq!(
        (silenced.silenced_by.as_deref(), silenced.silence_reason.as_deref()),
        (Some("anna"), Some("planned work"))
    );

    assert!(store.change_alert(row.id, silenced.rev, &fired(Vec::new())).await.unwrap());
    let acked = store.acknowledge_alert(row.id, "anna", &audit("alert.ack")).await.unwrap().unwrap();
    assert_eq!(acked.ack_by.as_deref(), Some("anna"));
    let ended = store.silence_alert(row.id, None, "anna", None, &audit("alert.unsilence")).await.unwrap().unwrap();
    assert!(!ended.is_silenced(chrono::Utc::now()) && ended.silenced_by.is_none());

    let kinds: Vec<String> =
        store.alert_events(Some(row.id), 10).await.unwrap().into_iter().rev().map(|e| e.kind).collect();
    assert_eq!(kinds, ["silenced", "fired", "acknowledged", "unsilenced"]);
    let logged: Vec<String> = audit_of(&store, &row.interface).await.into_iter().map(|(a, ..)| a).collect();
    assert_eq!(logged, ["alert.silence", "alert.ack", "alert.unsilence"]);
    assert_eq!(audit_of(&store, &row.interface).await[0].2["reason"], "planned work");

    // A new episode ends the acknowledgement.
    let row = store.alert(row.id).await.unwrap().unwrap();
    assert!(store.change_alert(row.id, row.rev, &fired(Vec::new())).await.unwrap());
    assert!(store.alert(row.id).await.unwrap().unwrap().ack_by.is_none());
    let pool = sqlx::PgPool::connect(&std::env::var("DUCTUS_TEST_DATABASE_URL").unwrap()).await.unwrap();
    forget_alert(&pool, row.id).await;
}

/// Our certificates (migration 0024) are rows beside the remote ones, even under the same name:
/// they sit on the engine's server at port 0, which no remote row can, and keep their side.
#[tokio::test]
async fn our_certificates_are_rows_of_their_own() {
    let Some(store) = store().await else { return };
    let pool = sqlx::PgPool::connect(&std::env::var("DUCTUS_TEST_DATABASE_URL").unwrap()).await.unwrap();
    let name = format!("test-ours-{}", std::process::id());
    let now = chrono::Utc::now();
    let remote = ductus_store::CertificateRow {
        side: "remote".into(),
        resource: name.clone(),
        host: "empi.example.org".into(),
        port: 443,
        subject: Some("empi.example.org".into()),
        issuer: Some("Example CA".into()),
        fingerprint: Some("ab12".into()),
        not_before: Some(now - chrono::Duration::days(60)),
        not_after: Some(now + chrono::Duration::days(20)),
        chain_min_not_after: Some(now + chrono::Duration::days(20)),
        chain_min_subject: None,
        trusted: Some(true),
        error: None,
        checked_at: now,
        read_at: None,
    };
    let ours = ductus_store::CertificateRow {
        side: "ours".into(),
        host: "engine-1".into(),
        port: 0,
        fingerprint: Some("cd34".into()),
        trusted: None,
        error: Some("the private key isn't this certificate's key".into()),
        ..remote.clone()
    };
    store.save_certificates(&[remote, ours]).await.unwrap();
    let mut rows: Vec<_> = store.certificates().await.unwrap().into_iter().filter(|r| r.resource == name).collect();
    rows.sort_by_key(|r| r.port);
    let shown: Vec<_> = rows.iter().map(|r| (r.side.as_str(), r.host.as_str(), r.port, r.error.is_some())).collect();
    assert_eq!(shown, [("ours", "engine-1", 0, true), ("remote", "empi.example.org", 443, false)]);

    // An older engine's insert (no `side`) is a remote row.
    sqlx::query(
        "INSERT INTO certificate_status (resource, host, port, checked_at) VALUES ($1, 'old.example.org', 443, now())",
    )
    .bind(&name)
    .execute(&pool)
    .await
    .unwrap();
    let side: String =
        sqlx::query_scalar("SELECT side FROM certificate_status WHERE resource = $1 AND host = 'old.example.org'")
            .bind(&name)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(side, "remote");
    let wrong = sqlx::query("UPDATE certificate_status SET side = 'theirs' WHERE resource = $1").bind(&name);
    assert!(wrong.execute(&pool).await.is_err(), "only remote or ours");
    sqlx::query("DELETE FROM certificate_status WHERE resource = $1").bind(&name).execute(&pool).await.unwrap();
}

/// A certificate alert keeps its step; a check that couldn't read the certificate keeps the dates
/// from the one before (the certificate out there still ends then), with the error.
#[tokio::test]
async fn certificates_keep_their_dates_when_a_check_fails() {
    let Some(store) = store().await else { return };
    let pool = sqlx::PgPool::connect(&std::env::var("DUCTUS_TEST_DATABASE_URL").unwrap()).await.unwrap();
    let resource = format!("test-cert-{}", std::process::id());
    let now = chrono::Utc::now();
    let seen = ductus_store::CertificateRow {
        side: "remote".into(),
        resource: resource.clone(),
        host: "empi.example.org".into(),
        port: 443,
        subject: Some("empi.example.org".into()),
        issuer: Some("Example CA".into()),
        fingerprint: Some("ab12".into()),
        not_before: Some(now - chrono::Duration::days(60)),
        not_after: Some(now + chrono::Duration::days(20)),
        chain_min_not_after: Some(now + chrono::Duration::days(10)),
        chain_min_subject: Some("Example Intermediate".into()),
        trusted: Some(true),
        error: None,
        checked_at: now,
        read_at: None,
    };
    store.save_certificates(std::slice::from_ref(&seen)).await.unwrap();
    let mine = |rows: Vec<ductus_store::CertificateRow>| rows.into_iter().find(|r| r.resource == resource).unwrap();
    // Timestamps come back at microseconds.
    let seen = mine(store.certificates().await.unwrap());
    assert_eq!((seen.read_at, seen.fingerprint.as_deref()), (Some(seen.checked_at), Some("ab12")));

    let later = now + chrono::Duration::hours(6);
    let failed = ductus_store::CertificateRow {
        subject: None,
        issuer: None,
        fingerprint: None,
        not_before: None,
        not_after: None,
        chain_min_not_after: None,
        chain_min_subject: None,
        trusted: None,
        error: Some("connection refused".into()),
        checked_at: later,
        ..seen.clone()
    };
    store.save_certificates(&[failed]).await.unwrap();
    let row = mine(store.certificates().await.unwrap());
    assert_eq!(row.error.as_deref(), Some("connection refused"));
    assert!(row.checked_at > seen.checked_at);
    assert_eq!(
        (row.not_after, row.chain_min_not_after, row.trusted),
        (seen.not_after, seen.chain_min_not_after, Some(true))
    );
    assert_eq!(row.read_at, seen.read_at, "still the certificate read before");

    let renewed = ductus_store::CertificateRow {
        fingerprint: Some("cd34".into()),
        not_after: Some(now + chrono::Duration::days(400)),
        chain_min_not_after: Some(now + chrono::Duration::days(400)),
        chain_min_subject: None,
        checked_at: later,
        ..seen.clone()
    };
    store.save_certificates(std::slice::from_ref(&renewed)).await.unwrap();
    let row = mine(store.certificates().await.unwrap());
    assert_eq!(
        (row.fingerprint.as_deref(), row.error, row.chain_min_subject),
        (Some("cd34"), None, None),
        "the error is gone"
    );
    assert!(row.read_at > seen.read_at);
    sqlx::query("DELETE FROM certificate_status WHERE resource = $1").bind(&resource).execute(&pool).await.unwrap();

    // The alert's step is stored with its change.
    let row = test_alert(&store, "cert").await;
    let change = ductus_store::AlertChange {
        state: "certificate",
        severity: Some("warning"),
        stage: Some(14),
        ..fired(Vec::new())
    };
    let change = ductus_store::AlertChange { event: None, ..change };
    assert!(store.change_alert(row.id, row.rev, &change).await.unwrap());
    let row = store.alert(row.id).await.unwrap().unwrap();
    assert_eq!((row.state.as_str(), row.stage), ("certificate", Some(14)));
    forget_alert(&pool, row.id).await;
}

/// Connection checks keep their reports, the last 30.
#[tokio::test]
async fn doctor_runs_keep_the_last_thirty() {
    let Some(store) = store().await else { return };
    let mut last = 0;
    for n in 0..(ductus_store::DOCTOR_RUNS_KEPT + 3) {
        let run = ductus_store::DoctorRunRow {
            id: 0,
            at: chrono::Utc::now(),
            mode: "certificates".into(),
            started_by: if n % 2 == 0 { "schedule".into() } else { "test-user-anna".into() },
            seconds: 0.25,
            checks: 2,
            failed: (n % 3) as i32,
        };
        last = store.record_doctor_run(&run, &serde_json::json!({ "n": n })).await.unwrap();
    }
    let runs = store.doctor_runs(false, 100).await.unwrap();
    assert_eq!(runs.len() as i64, ductus_store::DOCTOR_RUNS_KEPT);
    assert_eq!(runs[0].id, last, "newest first");
    assert!(store.doctor_runs(true, 100).await.unwrap().iter().all(|r| r.started_by == "schedule"));
    assert_eq!(store.doctor_report(last).await.unwrap().unwrap()["n"], ductus_store::DOCTOR_RUNS_KEPT + 2);
    assert!(store.doctor_report(runs.last().unwrap().id - 1).await.unwrap().is_none(), "older ones are gone");
}

/// The audit rows of one interface, oldest first, as (action, message id, detail).
async fn audit_of(store: &Store, interface: &str) -> Vec<(String, Option<i64>, serde_json::Value)> {
    let filter =
        ductus_store::AuditFilter { interface: Some(interface), after: Some(0), limit: 100, ..Default::default() };
    store
        .audit_log(&filter)
        .await
        .unwrap()
        .into_iter()
        .map(|r| (r.action, r.message_id, r.detail.map(|d| serde_json::from_str(&d).unwrap()).unwrap_or_default()))
        .collect()
}

/// Dismissing ends a quarantine without sending: the delivery is filtered with who and why, the
/// message is settled again (also when other destinations sent it), a reason is required, and
/// only quarantined deliveries are touched. The request leaves one audit row.
#[tokio::test]
async fn a_dismissed_quarantine_is_filtered_settled_and_logged() {
    use ductus_store::Resend;
    let Some(store) = store().await else { return };
    let t = Iface::new(&store, "dismiss", &["billing", "lab"]).await;
    let (lab, billing) = (t.id("lab"), t.id("billing"));
    let a = store.insert(t.msg(1, b"a")).await.unwrap();
    let b = store.insert(t.msg(1, b"b")).await.unwrap();
    let c = store.insert(t.msg(1, b"c")).await.unwrap();
    store.finish(lab, &[Done::with(a, Status::Quarantined, Some("AR: unknown patient"))]).await.unwrap();
    store.finish(billing, &[sent(a)]).await.unwrap();
    store.finish(lab, &[Done::with(b, Status::Quarantined, Some("AR: bad segment"))]).await.unwrap();
    store.finish(billing, &[Done::with(b, Status::Quarantined, Some("HTTP 400"))]).await.unwrap();
    assert_eq!(status_by_id(&store, b.id).await, Status::Quarantined);

    let all = Resend { messages: &[a.id, b.id, c.id], destinations: &t.ids, quarantined_only: true };
    let without_reason = ductus_store::Actor { reason: None, ..OPERATOR };
    let err = store.dismiss(all, without_reason).await.unwrap_err().to_string();
    assert!(err.contains("say why"), "{err}");
    assert!(audit_of(&store, &t.name).await.is_empty(), "a refused dismissal leaves nothing");

    let done = store.dismiss(all, OPERATOR).await.unwrap();
    let dismissed: Vec<_> = done.iter().map(|d| (d.message_id, d.destination.as_str(), d.previous_status)).collect();
    assert_eq!(
        dismissed,
        [
            (a.id, "lab", Status::Quarantined),
            (b.id, "billing", Status::Quarantined),
            (b.id, "lab", Status::Quarantined)
        ],
        "c is pending: left alone"
    );
    let deliveries = store.deliveries(a).await.unwrap();
    let lab_delivery = deliveries.iter().find(|d| d.destination == "lab").unwrap();
    assert_eq!(lab_delivery.status, Status::Filtered);
    assert_eq!(lab_delivery.last_error.as_deref(), Some("dismissed by test-operator: vendor fixed their side"));
    assert_eq!(status_by_id(&store, a.id).await, Status::Sent, "billing sent it");
    assert_eq!(status_by_id(&store, b.id).await, Status::Filtered, "nothing sent it, nothing left to fix");
    assert_eq!(status_by_id(&store, c.id).await, Status::Pending);
    assert!(store.dismiss(all, OPERATOR).await.unwrap().is_empty(), "nothing is quarantined any more");

    let rows = audit_of(&store, &t.name).await;
    assert_eq!(rows.len(), 1, "one row for the request, none for the empty one: {rows:?}");
    let (action, message, detail) = &rows[0];
    assert_eq!((action.as_str(), *message), ("message.dismiss", None), "two messages: no single id");
    assert_eq!(detail["messages"], serde_json::json!([a.id, b.id]));
    assert_eq!(detail["reason"], "vendor fixed their side");
    assert_eq!(detail["deliveries"], 3);
    let actions: Vec<_> = store.actions(a.id).await.unwrap().into_iter().map(|x| (x.action, x.destination)).collect();
    assert_eq!(actions, [("dismiss".to_string(), "lab".to_string())]);

    // One message: its id is on the row.
    let d = store.insert(t.msg(1, b"d")).await.unwrap();
    store.finish(lab, &[Done::with(d, Status::Quarantined, Some("AR"))]).await.unwrap();
    let one = Resend { messages: &[d.id], destinations: &[lab], quarantined_only: true };
    assert_eq!(store.dismiss(one, OPERATOR).await.unwrap().len(), 1);
    assert_eq!(audit_of(&store, &t.name).await.last().unwrap().1, Some(d.id));
}

/// Resends (a release of a quarantine, a replay) and edits each leave one audit row, written with
/// the change; a resend that finds nothing to do leaves none.
#[tokio::test]
async fn resend_and_edit_write_their_audit_row_with_the_change() {
    use ductus_store::{Copy, Edited, Resend};
    let Some(store) = store().await else { return };
    let t = Iface::new(&store, "audit-actions", &["billing", "lab"]).await;
    let (lab, billing) = (t.id("lab"), t.id("billing"));
    let a = store.insert(t.msg(2, b"a")).await.unwrap();
    store.finish(lab, &[Done::with(a, Status::Quarantined, Some("AR"))]).await.unwrap();
    store.finish(billing, &[sent(a)]).await.unwrap();

    let nothing = Resend { messages: &[a.id], destinations: &[billing], quarantined_only: true };
    assert!(store.resend(nothing, OPERATOR).await.unwrap().is_empty());
    assert!(audit_of(&store, &t.name).await.is_empty(), "nothing resent, nothing recorded");

    let release = Resend { messages: &[a.id], destinations: &t.ids, quarantined_only: true };
    assert_eq!(store.resend(release, OPERATOR).await.unwrap().len(), 1);
    store.finish(lab, &[sent(a)]).await.unwrap();
    let replay = Resend { messages: &[a.id], destinations: &[billing], quarantined_only: false };
    assert_eq!(store.resend(replay, OPERATOR).await.unwrap().len(), 1);
    store.finish(billing, &[Done::with(a, Status::Quarantined, Some("again"))]).await.unwrap();

    let copy = Copy {
        original: a.id,
        interface: &t.name,
        partition_key: 2,
        ordering_key: None,
        control_id: None,
        payload: b"fixed",
        tier: TIER,
        destinations: &[billing],
    };
    let Edited::Copied { copy: made, .. } = store.copy_edited(copy, OPERATOR).await.unwrap() else { panic!("a copy") };

    // Naming the destination of a quarantined delivery (as the message page does) is still a
    // release: what it was decides, not how it was asked.
    let b = store.insert(t.msg(2, b"b")).await.unwrap();
    store.finish(lab, &[Done::with(b, Status::Quarantined, Some("AR"))]).await.unwrap();
    let named = Resend { messages: &[b.id], destinations: &[lab], quarantined_only: false };
    assert_eq!(store.resend(named, OPERATOR).await.unwrap().len(), 1);

    let rows = audit_of(&store, &t.name).await;
    let kinds: Vec<_> = rows.iter().map(|(a, m, _)| (a.as_str(), *m)).collect();
    assert_eq!(
        kinds,
        [
            ("message.release", Some(a.id)),
            ("message.replay", Some(a.id)),
            ("message.edit", Some(a.id)),
            ("message.release", Some(b.id))
        ]
    );
    assert_eq!(rows[0].2["destinations"], serde_json::json!([format!("{}/lab", t.name)]));
    assert_eq!(rows[2].2["copy"], made.id);
    assert_eq!(rows[2].2["reason"], "vendor fixed their side");
}

/// Reads of one payload by the same person within the window are one row; another person, another
/// message or an expired window are rows of their own.
#[tokio::test]
async fn payload_reads_within_the_window_are_one_row() {
    use ductus_store::{Audit, By};
    let Some(store) = store().await else { return };
    let t = Iface::single(&store, "payload-reads").await;
    let m = store.insert(t.msg(0, b"x")).await.unwrap();
    let other = store.insert(t.msg(0, b"y")).await.unwrap();
    let window = std::time::Duration::from_secs(600);
    let people = people();
    let (anna, ben) = (
        make_user(&store, &people, Role::Operator, &["payloads"]).await,
        make_user(&store, &people, Role::Operator, &["payloads"]).await,
    );
    fn read<'a>(who: &'a ductus_store::User, interface: &'a str, id: i64) -> Audit<'a> {
        Audit::new(By::user(who.id, &who.username, "session", None), "message.payload.read")
            .interface(interface)
            .message(id)
    }
    assert!(store.record_payload_read(&read(&anna, &t.name, m.id), window).await.unwrap());
    assert!(!store.record_payload_read(&read(&anna, &t.name, m.id), window).await.unwrap(), "the same read again");
    assert!(store.record_payload_read(&read(&ben, &t.name, m.id), window).await.unwrap(), "another person");
    assert!(store.record_payload_read(&read(&anna, &t.name, other.id), window).await.unwrap(), "another message");
    assert!(
        store.record_payload_read(&read(&anna, &t.name, m.id), std::time::Duration::ZERO).await.unwrap(),
        "no window: every read"
    );
    let rows = audit_of(&store, &t.name).await;
    assert_eq!(rows.len(), 4);
    assert!(rows.iter().all(|(a, _, _)| a == "message.payload.read"));
    people.forget().await.unwrap();
}

/// Copies of payloads kept with message actions go when the messages' own retention ends (the row
/// stays); audit rows and actions go after `audit.keep`.
#[tokio::test]
async fn payload_copies_and_old_audit_rows_are_cleared_on_schedule() {
    use ductus_store::Resend;
    let Some(url) = std::env::var("DUCTUS_TEST_DATABASE_URL").ok() else { return };
    let store = Store::connect(&url, 4).await.unwrap();
    let pool = sqlx::PgPool::connect(&url).await.unwrap();
    let t = Iface::single(&store, "trim").await;
    let m = store.insert(t.msg(0, b"x")).await.unwrap();
    store.finish(t.dest(), &[Done::sent(m, Some(b"what was sent".to_vec()))]).await.unwrap();
    let replay = Resend { messages: &[m.id], destinations: &t.ids, quarantined_only: false };
    store.resend(replay, OPERATOR).await.unwrap();
    store.finish(t.dest(), &[Done::sent(m, Some(b"and again".to_vec()))]).await.unwrap();
    let copies = || async {
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM message_actions WHERE interface = $1 AND previous_output IS NOT NULL",
        )
        .bind(&t.name)
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    assert_eq!(copies().await, 1);

    // 30 days old: inside a 49-day window, kept; 60 days old: cleared, the row stays.
    let age = |days: i32| {
        let sql = "UPDATE message_actions SET at = now() - make_interval(days => $2) WHERE interface = $1";
        sqlx::query(sql).bind(&t.name).bind(days).execute(&pool)
    };
    age(30).await.unwrap();
    let windows = [(t.name.clone(), 49)];
    store.clear_action_outputs(&windows, 49, 1000, 10).await.unwrap();
    assert_eq!(copies().await, 1, "inside its window");
    age(60).await.unwrap();
    assert!(store.clear_action_outputs(&windows, 49, 1000, 10).await.unwrap() >= 1);
    assert_eq!(copies().await, 0, "outside it");
    assert_eq!(store.actions(m.id).await.unwrap().len(), 1, "who did what stays");
    // An interface this engine no longer runs gets the longest window.
    age(30).await.unwrap();
    sqlx::query("UPDATE message_actions SET previous_output = 'x' WHERE interface = $1")
        .bind(&t.name)
        .execute(&pool)
        .await
        .unwrap();
    store.clear_action_outputs(&[], 49, 1000, 10).await.unwrap();
    assert_eq!(copies().await, 1);

    // Past audit.keep: the action and its audit row go; recent rows stay.
    let six_years = std::time::Duration::from_secs(6 * 365 * 86_400);
    sqlx::query("UPDATE message_actions SET at = now() - interval '7 years' WHERE interface = $1")
        .bind(&t.name)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE audit_log SET at = now() - interval '7 years' WHERE interface = $1")
        .bind(&t.name)
        .execute(&pool)
        .await
        .unwrap();
    let recent = store
        .record(&ductus_store::Audit::new(ductus_store::By::engine(), "test.recent").interface(&t.name))
        .await
        .unwrap();
    let (actions, rows) = store.trim_audit(six_years, 1000, 10).await.unwrap();
    assert!(actions >= 1 && rows >= 1, "{actions} actions, {rows} rows");
    assert!(store.actions(m.id).await.unwrap().is_empty());
    let left = audit_of(&store, &t.name).await;
    assert_eq!(left.iter().map(|(a, _, _)| a.as_str()).collect::<Vec<_>>(), ["test.recent"], "id {recent}");
}

/// A start with no stop of its instance is how a crash shows.
#[tokio::test]
async fn a_start_without_a_stop_is_a_crash() {
    use ductus_store::{Audit, By, PreviousRun};
    let Some(store) = store().await else { return };
    let host = unique("test-host");
    assert_eq!(store.previous_run(&host).await.unwrap(), PreviousRun::First);
    let started = |instance: &str| {
        Audit::new(By::engine(), "engine.start").detail(serde_json::json!({ "host": host, "instance": instance }))
    };
    store.record(&started("one")).await.unwrap();
    assert!(matches!(store.previous_run(&host).await.unwrap(), PreviousRun::Unclean(_)));
    let stopped = Audit::new(By::engine(), "engine.stop").detail(serde_json::json!({ "instance": "one" }));
    store.record(&stopped).await.unwrap();
    assert!(matches!(store.previous_run(&host).await.unwrap(), PreviousRun::Stopped(_)));
    store.record(&started("two")).await.unwrap();
    assert!(matches!(store.previous_run(&host).await.unwrap(), PreviousRun::Unclean(_)), "the newest start counts");
}

/// Changes to accounts and tokens write their audit row in their own transaction: a refused change
/// leaves none, a made one leaves one that names who did it and what was done to.
#[tokio::test]
async fn account_changes_are_logged_with_the_change() {
    use ductus_store::{Audit, AuditFilter, By};
    let Some(store) = store().await else { return };
    let people = people();
    let admin = make_user(&store, &people, Role::Viewer, &[]).await;
    let by = By::user(admin.id, &admin.username, "session", Some("192.0.2.7".parse().unwrap()));
    let name = people.name("test-user");
    let permissions = ["payloads".to_string()];
    let new = ductus_store::NewUser {
        username: &name,
        display_name: "",
        email: "",
        role: Role::Operator,
        permissions: &permissions,
        password_hash: Some("$argon2id$x"),
        must_change_password: true,
        source: "local",
    };
    store.create_user(&new, &Audit::new(by, "user.create")).await.unwrap();
    assert!(store.create_user(&new, &Audit::new(by, "user.create")).await.is_err(), "a duplicate name");
    let filter = AuditFilter { actor: Some(&admin.username), action: Some("user"), limit: 10, ..Default::default() };
    let rows = store.audit_log(&filter).await.unwrap();
    assert_eq!(rows.len(), 1, "the refused one left nothing: {rows:?}");
    let r = &rows[0];
    assert_eq!(
        (r.action.as_str(), r.subject.as_deref(), r.outcome.as_str()),
        ("user.create", Some(name.as_str()), "ok")
    );
    assert_eq!(
        (r.actor_kind.as_str(), r.via.as_deref(), r.ip.as_deref()),
        ("user", Some("session"), Some("192.0.2.7"))
    );
    assert!(r.detail.as_deref().unwrap().contains("payloads"));
    // Their own activity, by person.
    let mine = AuditFilter { user_id: Some(admin.id), limit: 10, ..Default::default() };
    assert_eq!(store.audit_log(&mine).await.unwrap().len(), 1);
    people.forget().await.unwrap();
}

/// A resend puts a finished delivery back to pending while another destination may be settling
/// the same message. Settling must read the deliveries after it holds the message's lock, or it
/// settles from the delivery as it was before the resend: quarantined with a delivery pending,
/// which nothing would ever correct (and startup's reconcile would undo the resend).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_resend_while_another_destination_settles_leaves_the_message_pending() {
    use ductus_store::Resend;
    let Some(store) = store().await else { return };
    let t = Iface::new(&store, "resend-race", &["a", "b"]).await;
    let (a, b) = (t.id("a"), t.id("b"));
    let mut refs = Vec::new();
    for i in 0..300 {
        refs.push(store.insert(t.msg(0, format!("m{i}").as_bytes())).await.unwrap());
    }
    let quarantined: Vec<Done> = refs.iter().map(|m| Done::with(*m, Status::Quarantined, Some("rejected"))).collect();
    store.finish(a, &quarantined).await.unwrap();
    let settle_b = {
        let (store, refs) = (store.clone(), refs.clone());
        tokio::spawn(async move {
            for m in refs {
                store.finish(b, &[sent(m)]).await.unwrap();
            }
        })
    };
    let resend_a = {
        let (store, refs) = (store.clone(), refs.clone());
        tokio::spawn(async move {
            for m in refs {
                let r = Resend { messages: &[m.id], destinations: &[a], quarantined_only: true };
                assert_eq!(store.resend(r, OPERATOR).await.unwrap().len(), 1);
            }
        })
    };
    let (x, y) = tokio::join!(settle_b, resend_a);
    x.unwrap();
    y.unwrap();
    assert!(store.payloads(&t.name, Status::Quarantined).await.unwrap().is_empty(), "every message waits for a again");
    assert_eq!(store.payloads(&t.name, Status::Pending).await.unwrap().len(), 300);
    let done: Vec<Done> = refs.iter().map(|m| sent(*m)).collect();
    store.finish(a, &done).await.unwrap();
    assert_eq!(store.payloads(&t.name, Status::Sent).await.unwrap().len(), 300);
}

/// A dismissal while the other destination is still settling the same messages: whichever lands
/// last, every message ends up settled from both deliveries (sent: `b` sent it), none pending with
/// nothing to wait for, none quarantined with nothing quarantined.
#[tokio::test]
async fn a_dismissal_while_another_destination_settles_leaves_the_message_settled() {
    use ductus_store::Resend;
    let Some(store) = store().await else { return };
    let t = Iface::new(&store, "dismiss-race", &["a", "b"]).await;
    let (a, b) = (t.id("a"), t.id("b"));
    let mut refs = Vec::new();
    for i in 0..300 {
        refs.push(store.insert(t.msg(0, format!("m{i}").as_bytes())).await.unwrap());
    }
    let quarantined: Vec<Done> = refs.iter().map(|m| Done::with(*m, Status::Quarantined, Some("rejected"))).collect();
    store.finish(a, &quarantined).await.unwrap();
    let settle_b = {
        let (store, refs) = (store.clone(), refs.clone());
        tokio::spawn(async move {
            for m in refs {
                store.finish(b, &[sent(m)]).await.unwrap();
            }
        })
    };
    let dismiss_a = {
        let (store, refs) = (store.clone(), refs.clone());
        tokio::spawn(async move {
            for m in refs {
                let r = Resend { messages: &[m.id], destinations: &[a], quarantined_only: true };
                assert_eq!(store.dismiss(r, OPERATOR).await.unwrap().len(), 1);
            }
        })
    };
    let (x, y) = tokio::join!(settle_b, dismiss_a);
    x.unwrap();
    y.unwrap();
    assert_eq!(store.payloads(&t.name, Status::Sent).await.unwrap().len(), 300);
    assert!(store.payloads(&t.name, Status::Pending).await.unwrap().is_empty());
    assert!(store.payloads(&t.name, Status::Quarantined).await.unwrap().is_empty());
}

#[tokio::test]
async fn the_quarantine_lists_newest_first_a_page_at_a_time() {
    let Some(store) = store().await else { return };
    let t = Iface::new(&store, "quarantine", &["billing", "lab"]).await;
    let (lab, billing) = (t.id("lab"), t.id("billing"));
    let mut refs = Vec::new();
    for i in 0..3 {
        refs.push(store.insert(t.msg(0, format!("q{i}").as_bytes())).await.unwrap());
    }
    let rejected = |m: MessageRef| Done::with(m, Status::Quarantined, Some("AR"));
    store.finish(lab, &refs.iter().map(|m| rejected(*m)).collect::<Vec<_>>()).await.unwrap();
    store.finish(billing, &[rejected(refs[1]), sent(refs[0]), sent(refs[2])]).await.unwrap();

    let page = store.quarantined(&t.ids, None, 3).await.unwrap();
    let seen: Vec<_> = page.iter().map(|q| (q.message_id, q.destination_id)).collect();
    let (high, low) = (lab.max(billing), lab.min(billing));
    assert_eq!(seen, [(refs[2].id, lab), (refs[1].id, high), (refs[1].id, low)]);
    assert_eq!((page[0].interface.as_str(), page[0].last_error.as_deref()), (t.name.as_str(), Some("AR")));
    let last = page.last().unwrap();
    let rest = store.quarantined(&t.ids, Some((last.message_id, last.destination_id)), 3).await.unwrap();
    assert_eq!(rest.iter().map(|q| (q.message_id, q.destination_id)).collect::<Vec<_>>(), [(refs[0].id, lab)]);
    assert_eq!(store.quarantined(&[billing], None, 10).await.unwrap().len(), 1, "one destination's");
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
    let reconcile = async |destinations: &[(String, String)]| {
        let ids = store.destination_ids(destinations).await.unwrap();
        store.reconcile(destinations, &ids).await.unwrap()
    };
    let names = [t.name.clone(), other.name.clone()];
    let mut now = t.config(&["emr", "lab"]);
    now.extend(other.config(&["default"]));
    let changes = reconcile(&now).await;
    let mut added = vec![(t.name.clone(), 3), (other.name.clone(), 1)];
    added.sort();
    assert_eq!(changes, Reconciled { added, removed: vec![(t.name.clone(), "fax".into(), 1)] });
    assert_eq!(store.settle_finished(&names).await.unwrap(), 0);
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
    let changes = reconcile(&lab).await;
    assert_eq!(changes, Reconciled { added: vec![], removed: vec![(t.name.clone(), "emr".into(), 2)] });
    assert_eq!(store.settle_finished(&names).await.unwrap(), 2);
    assert_eq!(store.payloads(&t.name, Status::Sent).await.unwrap(), [b"old".to_vec(), b"new".to_vec()]);
    assert_eq!(reconcile(&lab).await, Reconciled::default(), "nothing left to do");
    assert_eq!(store.settle_finished(&names).await.unwrap(), 0);

    // An older version finished a message the deliveries still show pending: it takes that status.
    let only_lab = [t.id("lab")];
    let m = store.insert(NewMessage { destinations: &only_lab, ..t.msg(0, b"rolled back") }).await.unwrap();
    sqlx::query("UPDATE messages SET status = 1 WHERE id = $1").bind(m.id).execute(&pool).await.unwrap();
    assert_eq!(reconcile(&lab).await, Reconciled::default());
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

/// Tests that save the documentation snapshot take this: a save removes every entry it wasn't
/// given, the other test's too.
static DOCS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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

/// Lock an engine named `instance` (as `ductus_runtime` does at start).
async fn lock(store: &Store, instance: &str, host: &str) -> ductus_store::EngineLock {
    let engine = ductus_store::NewEngine { instance, host, pid: 4120, version: "0.6.0-test", environment: "test" };
    store.lock_engine(&engine, None).await.unwrap()
}

#[tokio::test]
async fn an_interface_runs_on_one_engine_and_a_dead_ones_are_taken_over() {
    let Some(store) = store().await else { return };
    let (a, b) = (unique("engine-a"), unique("engine-b"));
    let [x, y, z] = ["x", "y", "z"].map(|n| unique(&format!("owned-{n}")));
    let lock_a = lock(&store, &a, "host-a").await;
    assert!(store.claim(&a, &[x.clone(), y.clone()]).await.unwrap().is_empty(), "nobody owned them");
    assert!(store.claim(&a, &[x.clone(), y.clone()]).await.unwrap().is_empty(), "claiming its own again is fine");

    let lock_b = lock(&store, &b, "host-b").await;
    // Overlapping: refused, naming the owner, and nothing of the set is claimed (all or none).
    let owned = store.claim(&b, &[y.clone(), z.clone()]).await.unwrap();
    assert_eq!(owned.len(), 1);
    assert_eq!((owned[0].interface.as_str(), owned[0].host.as_str(), owned[0].pid), (y.as_str(), "host-a", 4120));
    assert!(
        store.claim(&b, std::slice::from_ref(&z)).await.unwrap().is_empty(),
        "z was left free: disjoint sets share a database"
    );
    let others = store.other_engines(&b).await.unwrap();
    let seen_a = others.iter().find(|e| e.instance == a).expect("A is alive");
    assert_eq!(seen_a.interfaces, {
        let mut v = vec![x.clone(), y.clone()];
        v.sort();
        v
    });

    // A dies (its lock connection closes): B takes y over as soon as the server has ended A's
    // session, no timeout to wait out. Closing only sends Terminate: the server frees the lock a
    // moment later (a claim straight after it was refused once in CI), so give it 2 s.
    lock_a.release().await;
    let mut free = false;
    for _ in 0..40 {
        if store.claim(&b, std::slice::from_ref(&y)).await.unwrap().is_empty() {
            free = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(free, "a dead engine's interfaces are free");
    assert!(store.other_engines(&b).await.unwrap().iter().all(|e| e.instance != a), "a dead engine isn't listed");
    // A refused start leaves no record; a clean stop lets go and says when.
    store.forget_engine(&a).await.unwrap();
    store.release_engine(&b).await.unwrap();
    assert!(store.lost_claims(&b, &[y.clone(), z.clone()]).await.unwrap().is_empty());
    lock_b.release().await;
}

/// A takeover's check before it claims: interfaces owned by a live engine other than itself and
/// the one it takes over from; a dead owner's don't count.
#[tokio::test]
async fn a_takeover_sees_only_other_live_owners() {
    let Some(store) = store().await else { return };
    let (old, new, third) = (unique("engine-old"), unique("engine-new"), unique("engine-third"));
    let [x, y] = ["x", "y"].map(|n| unique(&format!("taken-{n}")));
    let lock_old = lock(&store, &old, "host").await;
    let lock_new = lock(&store, &new, "host").await;
    let lock_third = lock(&store, &third, "host").await;
    assert!(store.claim(&old, std::slice::from_ref(&x)).await.unwrap().is_empty());
    assert!(store.claim(&third, std::slice::from_ref(&y)).await.unwrap().is_empty());
    let both = [x.clone(), y.clone()];
    let owned = store.live_owners(&both, &[&new, &old]).await.unwrap();
    assert_eq!(
        owned.iter().map(|o| (o.interface.as_str(), o.instance.as_str())).collect::<Vec<_>>(),
        [(y.as_str(), third.as_str())]
    );
    lock_third.release().await;
    let mut gone = false;
    for _ in 0..40 {
        if store.live_owners(&both, &[&new, &old]).await.unwrap().is_empty() {
            gone = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(gone, "a dead owner's interfaces are free to claim");
    for engine in [&old, &new, &third] {
        store.release_engine(engine).await.unwrap();
        store.forget_engine(engine).await.unwrap();
    }
    lock_old.release().await;
    lock_new.release().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_engines_claiming_the_same_interfaces_at_once_one_wins() {
    let Some(store) = store().await else { return };
    let names: Vec<String> = (0..50).map(|i| unique(&format!("race-{i}"))).collect();
    for _ in 0..5 {
        let (a, b) = (unique("race-a"), unique("race-b"));
        let (lock_a, lock_b) = (lock(&store, &a, "host-a").await, lock(&store, &b, "host-b").await);
        // Opposite orders: the claim sorts them, so they wait for each other instead of deadlocking.
        let reversed: Vec<String> = names.iter().rev().cloned().collect();
        let (ra, rb) = tokio::join!(store.claim(&a, &names), store.claim(&b, &reversed));
        let (ra, rb) = (ra.unwrap(), rb.unwrap());
        assert!(ra.is_empty() != rb.is_empty(), "exactly one engine wins: {} / {}", ra.len(), rb.len());
        let loser = if ra.is_empty() { rb } else { ra };
        assert_eq!(loser.len(), names.len(), "the other gets nothing and is told about every one");
        lock_a.release().await;
        lock_b.release().await;
        store.forget_engine(&a).await.unwrap();
        store.forget_engine(&b).await.unwrap();
    }
}

#[tokio::test]
async fn a_lost_lock_connection_is_taken_again_with_the_claims() {
    let Some(store) = store().await else { return };
    let pool = sqlx::PgPool::connect(&std::env::var("DUCTUS_TEST_DATABASE_URL").unwrap()).await.unwrap();
    let a = unique("lost");
    let name = unique("lost-iface");
    let mut lock_a = lock(&store, &a, "host-a").await;
    assert!(store.claim(&a, std::slice::from_ref(&name)).await.unwrap().is_empty());
    lock_a.check(std::time::Duration::from_secs(3)).await.unwrap();

    // A database restart or failover, as far as this engine's session goes: it is killed.
    let killed: bool = sqlx::query_scalar(
        "SELECT pg_terminate_backend(pid) FROM pg_locks
         WHERE locktype = 'advisory' AND objsubid = 1 AND ((classid::bigint << 32) | objid::bigint) = $1",
    )
    .bind(lock_a.key)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(killed);
    assert!(lock_a.check(std::time::Duration::from_secs(3)).await.is_err(), "the engine notices");
    // Meanwhile another engine could take the interface (the engine looks dead); none did here.
    let engine =
        ductus_store::NewEngine { instance: &a, host: "host-a", pid: 4120, version: "0.6.0-test", environment: "test" };
    let again = store.lock_engine(&engine, Some(lock_a.key)).await.unwrap();
    assert_eq!(again.key, lock_a.key, "the same lock again");
    assert!(store.lost_claims(&a, std::slice::from_ref(&name)).await.unwrap().is_empty(), "still its own");
    assert!(store.claim(&a, std::slice::from_ref(&name)).await.unwrap().is_empty());
    store.release_engine(&a).await.unwrap();
    again.release().await;
}

#[tokio::test]
async fn lane_generations_are_recorded_retired_and_resends_go_to_the_newest() {
    use ductus_store::{LaneWrites, NewGeneration, lane_of};
    let Some(store) = store().await else { return };
    let t = Iface::new(&store, "lanes", &["lab", "billing"]).await;
    let only_lab = [t.id("lab")];
    for lane in [3, 40, 63] {
        store.insert(NewMessage { destinations: &only_lab, ..t.msg(lane, b"waiting") }).await.unwrap();
    }
    let done = store.insert(NewMessage { destinations: &only_lab, ..t.msg(50, b"sent") }).await.unwrap();
    store.finish(t.id("lab"), &[sent(done)]).await.unwrap();
    let names = [t.name.clone()];

    // What a start adopting these lanes reads: the highest lane anything waits in (the sent one
    // doesn't count), and how many wait.
    assert_eq!(store.highest_waiting_lanes(&names).await.unwrap(), [(t.name.clone(), 63)]);
    assert_eq!(store.waiting_by_interface(&names).await.unwrap(), [(t.name.clone(), 3)]);
    assert!(store.applied_lanes(&names).await.unwrap().is_empty(), "never recorded");
    assert!(store.lane_generations(&names).await.unwrap().is_empty());

    let generation = |first_lane, lanes, key: &str| NewGeneration {
        interface: t.name.clone(),
        first_lane,
        lanes,
        key: key.into(),
        mixed: false,
    };
    // The first start: 64 lanes. Then `ordering` changes to 32 lanes on another key.
    let first = LaneWrites {
        replaced: vec![t.name.clone()],
        added: vec![generation(0, 64, "PID-3.1")],
        applied: vec![(t.name.clone(), 64)],
        ..Default::default()
    };
    store.write_lanes(&first).await.unwrap();
    let change = LaneWrites {
        added: vec![generation(64, 32, "PID-18")],
        applied: vec![(t.name.clone(), 96)],
        ..Default::default()
    };
    store.write_lanes(&change).await.unwrap();
    let rows = store.lane_generations(&names).await.unwrap();
    let got: Vec<(i32, i32, Option<&str>, i32)> =
        rows.iter().map(|g| (g.first_lane, g.lanes, g.key.as_deref(), g.end())).collect();
    assert_eq!(got, [(0, 64, Some("PID-3.1"), 64), (64, 32, Some("PID-18"), 96)]);
    assert_eq!(store.applied_lanes(&names).await.unwrap(), [(t.name.clone(), 96)]);
    // Both in one statement, as a start reads them; an interface with nothing recorded has neither.
    let unknown = format!("{}-never-ran", t.name);
    let (both, applied) = store.lanes_recorded(&[t.name.clone(), unknown]).await.unwrap();
    assert_eq!(both, rows);
    assert_eq!(applied, [(t.name.clone(), 96)]);
    // A key recorded only where none was (the generations an upgrade made from the lane count).
    let keys = LaneWrites { keys: vec![(t.name.clone(), 0, "other".into())], ..Default::default() };
    store.write_lanes(&keys).await.unwrap();
    assert_eq!(store.lane_generations(&names).await.unwrap()[0].key.as_deref(), Some("PID-3.1"));

    // A resend of the sent message goes to its key's lane in the newest generation (64–95), not
    // back to lane 50, which no worker may read once the old lanes are retired; the message's
    // own lane follows (where `reconcile` queues it for a destination added later).
    let actor = ductus_store::Actor { by: ductus_store::By::engine(), reason: Some("test") };
    let resend = ductus_store::Resend { messages: &[done.id], destinations: &only_lab, quarantined_only: false };
    let resent = store.resend(resend, actor).await.unwrap();
    let lane = lane_of("", 64, 32); // `msg` stores no ordering key
    assert_eq!(resent.iter().map(|r| r.partition_key).collect::<Vec<_>>(), [lane]);
    assert!(
        store.pending(t.id("lab"), &[lane], 10).await.unwrap().iter().any(|p| p.id == done.id && p.lane == lane),
        "pending in the newest lanes"
    );
    assert!(!store.pending(t.id("lab"), &[50], 10).await.unwrap().iter().any(|p| p.id == done.id));

    // A handover that added lanes 96–103 failed: taken back where nothing was stored in them; kept
    // where something was (the resent message waits in 64–95, past an engine running lanes 0–63).
    let failed = LaneWrites {
        added: vec![generation(96, 8, "PID-18")],
        applied: vec![(t.name.clone(), 104)],
        ..Default::default()
    };
    store.write_lanes(&failed).await.unwrap();
    assert!(store.take_back_lanes(&[(t.name.clone(), 96)]).await.unwrap().is_empty());
    assert_eq!(store.lane_generations(&names).await.unwrap().len(), 2);
    assert_eq!(store.applied_lanes(&names).await.unwrap(), [(t.name.clone(), 96)]);
    assert_eq!(store.take_back_lanes(&[(t.name.clone(), 64)]).await.unwrap(), [(t.name.clone(), 1)]);
    assert_eq!(store.lane_generations(&names).await.unwrap().len(), 2, "kept: a message waits there");

    // Retired: everything older than the newest generation.
    assert_eq!(store.retire_lanes(&t.name, 64).await.unwrap(), 1);
    let rows = store.lane_generations(&names).await.unwrap();
    assert_eq!(rows.iter().map(|g| g.first_lane).collect::<Vec<_>>(), [64]);
    store.write_lanes(&LaneWrites { replaced: vec![t.name.clone()], ..Default::default() }).await.unwrap();
    assert!(store.lane_generations(&names).await.unwrap().is_empty());
}

#[tokio::test]
async fn with_waiting_finds_interfaces_with_anything_pending() {
    let Some(store) = store().await else { return };
    let busy = Iface::single(&store, "waiting-busy").await;
    let idle = Iface::single(&store, "waiting-idle").await;
    let removed = Iface::new(&store, "waiting-removed", &["old", "new"]).await;
    store.insert(busy.msg(0, b"waiting")).await.unwrap();
    let done = store.insert(idle.msg(0, b"done")).await.unwrap();
    store.finish(idle.dest(), &[sent(done)]).await.unwrap();
    // Pending only for a destination the interface may no longer have (reconcile filters it).
    let old = [removed.id("old")];
    store.insert(NewMessage { destinations: &old, ..removed.msg(0, b"for old") }).await.unwrap();
    let names = [busy.name.clone(), idle.name.clone(), removed.name.clone(), unique("never-ran")];
    let waiting = store.with_waiting(&names).await.unwrap();
    let mut waiting: Vec<_> = waiting.into_iter().collect();
    waiting.sort();
    let mut expected = vec![busy.name.clone(), removed.name.clone()];
    expected.sort();
    assert_eq!(waiting, expected);
}

#[tokio::test]
async fn old_engine_records_are_trimmed_live_ones_kept() {
    let Some(store) = store().await else { return };
    let pool = sqlx::PgPool::connect(&std::env::var("DUCTUS_TEST_DATABASE_URL").unwrap()).await.unwrap();
    let (old, live) = (unique("trim-old"), unique("trim-live"));
    let old_lock = lock(&store, &old, "host-a").await;
    let live_lock = lock(&store, &live, "host-b").await;
    old_lock.release().await;
    sqlx::query("UPDATE engines SET started_at = now() - interval '7 years' WHERE instance = ANY($1)")
        .bind([&old, &live])
        .execute(&pool)
        .await
        .unwrap();
    store.trim_engines(std::time::Duration::from_secs(6 * 365 * 86_400)).await.unwrap();
    let left: Vec<String> = sqlx::query_scalar("SELECT instance FROM engines WHERE instance = ANY($1)")
        .bind([&old, &live])
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(left, std::slice::from_ref(&live), "a running engine stays however long it has run");
    live_lock.release().await;
    store.forget_engine(&live).await.unwrap();
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
    // An operator replaying sent messages to the lab (`Store::resend`) the whole time: the source
    // drains keep going until the last replay is delivered too.
    let resends_done = Arc::new(AtomicBool::new(false));
    let resender = {
        let (store, t, stop, resends_done) = (store.clone(), t.clone(), stop.clone(), resends_done.clone());
        tokio::spawn(async move {
            let mut resent = 0;
            while !stop.load(Relaxed) {
                let page = Browse { interface: &t.name, status: Some(Status::Sent), before: None, limit: 5 };
                let ids: Vec<i64> = store.browse(&page).await.unwrap().iter().map(|m| m.id).collect();
                let r = ductus_store::Resend { messages: &ids, destinations: &[t.id("lab")], quarantined_only: false };
                resent += store.resend(r, OPERATOR).await.unwrap().len();
                tokio::time::sleep(std::time::Duration::from_millis(15)).await;
            }
            resends_done.store(true, Relaxed);
            resent
        })
    };
    // (destination, where it routes to, whether it's the routed interface's own drain)
    let drains = [
        (t.id("lab"), None, false),
        (t.id("billing"), None, false),
        (t.id("archive"), Some(routed.clone()), false),
        (routed.dest(), None, true),
    ];
    for (n, (destination, route, is_route_target)) in drains.into_iter().enumerate() {
        let (store, stop, lanes, sources_done) = (store.clone(), stop.clone(), lanes.clone(), sources_done.clone());
        let resends_done = resends_done.clone();
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
                let stopped = if is_route_target {
                    sources_done.load(Relaxed) == 3
                } else {
                    stop.load(Relaxed) && resends_done.load(Relaxed)
                };
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
                // Each decided by a code version (0.7.3.2), which the resends clear again.
                let version = Some(1 + n as i32);
                let done: Vec<_> = pending.iter().map(|p| sent(p.reference()).by(version)).collect();
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
                    // The message view: each delivery with the version and deployment that decided it.
                    store.deliveries(MessageRef { id: last.id, received_at: last.received_at }).await.unwrap();
                    store.actions(last.id).await.unwrap();
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
    // Engines starting and stopping on the same database (0.6.2): each takes its lock, claims the
    // interfaces (a second one at the same moment is refused), checks lanes and what's waiting
    // against the busy tables, then stops; the next takes over.
    let engines = {
        let (store, t, routed, stop) = (store.clone(), t.clone(), routed.clone(), stop.clone());
        tokio::spawn(async move {
            let names = [t.name.clone(), routed.name.clone()];
            let lanes_name = format!("{}-lanes", t.name);
            let (mut claimed, mut refused) = (0, 0);
            while !stop.load(Relaxed) {
                let (a, b) = (unique("stress-a"), unique("stress-b"));
                let (lock_a, lock_b) = (lock(&store, &a, "host-a").await, lock(&store, &b, "host-b").await);
                let (ra, rb) = tokio::join!(store.claim(&a, &names), store.claim(&b, &names));
                let (ra, rb) = (ra.unwrap(), rb.unwrap());
                assert!(ra.is_empty() != rb.is_empty(), "exactly one engine claims them");
                claimed += 1;
                refused += 1;
                // Lanes, as a start reads and changes them. Written under a name of their own: a
                // generation recorded for `t` would send its resends to lanes the drains don't read.
                store.lane_generations(&names).await.unwrap();
                store.highest_waiting_lanes(&names).await.unwrap();
                store.with_waiting(&names).await.unwrap();
                store.waiting_by_interface(&names).await.unwrap();
                let (first_lane, lanes) = if claimed % 2 == 0 { (0, 8) } else { (8, 4) };
                let writes = ductus_store::LaneWrites {
                    replaced: (first_lane == 0).then(|| lanes_name.clone()).into_iter().collect(),
                    added: vec![ductus_store::NewGeneration {
                        interface: lanes_name.clone(),
                        first_lane,
                        lanes,
                        key: String::new(),
                        mixed: false,
                    }],
                    applied: vec![(lanes_name.clone(), first_lane + lanes)],
                    ..Default::default()
                };
                store.write_lanes(&writes).await.unwrap();
                store.retire_lanes(&lanes_name, first_lane).await.unwrap();
                store.other_engines(&a).await.unwrap();
                let winner = if ra.is_empty() { &a } else { &b };
                store.release_engine(winner).await.unwrap();
                lock_a.release().await;
                lock_b.release().await;
                store.forget_engine(&a).await.unwrap();
                store.forget_engine(&b).await.unwrap();
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            let gone = ductus_store::LaneWrites { replaced: vec![lanes_name], ..Default::default() };
            store.write_lanes(&gone).await.unwrap();
            (claimed, refused)
        })
    };
    // Starts and handovers recording what they run (0.7.3), two engines at once with the same new
    // version now and then (each writes what it hasn't seen; shared files are written once), and
    // two engines with projects of their own at once (0.7.3.3): each its own set of interfaces, a
    // warning on what it changes, its documentation and its history, removing the other's entries.
    let _docs = DOCS.lock().await;
    let deploy_env = format!("test-deploy-stress-{}", t.name);
    let deployer = {
        let (store, t, stop, env, pool) = (store.clone(), t.clone(), stop.clone(), deploy_env.clone(), pool.clone());
        tokio::spawn(async move {
            let mut rounds = 0;
            while !stop.load(Relaxed) {
                rounds += 1;
                let shared = format!("// {env}\n");
                let interface = format!("{env}-{}", t.name);
                let own = format!("// {env} round {rounds}\n");
                let v = TestVersion::new(&interface, &[("t.ts", &own), ("lib.ts", &shared)]);
                let versions = [&v];
                let (x, y) = tokio::join!(deploy(&store, &env, "x", &versions), deploy(&store, &env, "y", &versions));
                x.unwrap();
                y.unwrap();
                // The same version as saved before 0.7.4.2 (no parts), run by two engines at once:
                // both fill them in (0.7.4.2).
                sqlx::query("UPDATE code_versions SET parts = NULL WHERE hash = $1")
                    .bind(&v.hash)
                    .execute(&pool)
                    .await
                    .unwrap();
                let (x, y) = tokio::join!(deploy(&store, &env, "x", &versions), deploy(&store, &env, "y", &versions));
                x.unwrap();
                y.unwrap();
                let project = |p: &str| {
                    let name = format!("{env}-{p}");
                    let v = TestVersion::new(&name, &[("t.ts", &format!("// {name} round {rounds}\n"))]);
                    let warnings = std::collections::HashMap::from([(name.clone(), format!("round {rounds}"))]);
                    let docs = [ductus_store::DocsRow {
                        kind: "interface",
                        name: name.clone(),
                        docs: format!(r#"{{"custom": {{"round": {rounds}}}}}"#),
                    }];
                    (name, v, warnings, docs)
                };
                let (px, py) = (project("x"), project("y"));
                let (vx, vy) = ([&px.1], [&py.1]);
                let (x, y) = tokio::join!(
                    deploy_with(&store, &env, &px.0, &vx, &px.2, Some(&px.3)),
                    deploy_with(&store, &env, &py.0, &vy, &py.2, Some(&py.3))
                );
                x.unwrap();
                y.unwrap();
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            rounds
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
    let resent = resender.await;
    let engines = engines.await;
    let deployed = deployer.await;
    forget_deployments(&pool, &deploy_env).await;
    assert!(deployed.expect("recording deployments failed") > 0, "deployments were recorded");
    drop_test_partitions(&pool).await;
    let (claims, refusals) = engines.expect("engines claiming interfaces failed");
    assert!(claims > 0 && refusals > 0, "engines claimed and were refused");
    for r in results {
        r.expect("a writer or drain failed");
    }
    let resent = resent.expect("resending failed");
    assert!(resent > 0, "something was resent");
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
    // Every delivery names the version that decided it, the resent ones too (cleared by the resend,
    // set again when decided again), whatever moved them meanwhile.
    let unnamed: i64 =
        sqlx::query_scalar("SELECT count(*) FROM deliveries WHERE destination_id = ANY($1) AND code_version IS NULL")
            .bind(&t.ids)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(unnamed, 0, "deliveries decided without their code version");
    let duplicates = store.payloads(&t.name, Status::Duplicate).await.unwrap().len();
    assert!(duplicates > 0, "dedupe keys collided");
    assert!(store.payloads(&routed.name, Status::Pending).await.unwrap().is_empty(), "routed: nothing left pending");
    assert_eq!(store.payloads(&routed.name, Status::Sent).await.unwrap().len(), queued, "each routed exactly once");
    println!(
        "stress: {} messages stored ({queued} queued, {duplicates} duplicates), 3 destinations (one routing {queued} to another interface), {resent} resent, {moved} moved to another tier and {held} to the held area meanwhile, {claims} engine starts racing for the interfaces, {days} maintenance rounds, {seconds}s",
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
    let (token, secret) = store.create_token(&name, Role::Operator, &[], None, &quiet()).await.unwrap();
    assert!(secret.starts_with(ductus_store::TOKEN_PREFIX) && secret.len() == 68, "{secret}");
    assert_eq!((token.name.as_str(), token.role), (name.as_str(), Role::Operator));
    let err = store.create_token(&name, Role::Viewer, &[], None, &quiet()).await.unwrap_err().to_string();
    assert!(err.contains("already exists"), "{err}");

    let found = store.authenticate(&secret).await.unwrap().expect("the token works");
    assert_eq!(found.token_id, Some(token.id));
    assert!(store.tokens(false, None).await.unwrap().iter().any(|t| t.id == token.id && t.last_used_at.is_some()));
    assert!(store.authenticate("dux_0000").await.unwrap().is_none());
    assert!(store.authenticate("not a token").await.unwrap().is_none());

    assert!(store.revoke_token(&name, None, &quiet()).await.unwrap());
    assert!(!store.revoke_token(&name, None, &quiet()).await.unwrap(), "nothing left to revoke");
    assert!(store.authenticate(&secret).await.unwrap().is_none(), "revoked tokens stop working");
    assert!(!store.tokens(false, None).await.unwrap().iter().any(|t| t.id == token.id));
    assert!(store.tokens(true, None).await.unwrap().iter().any(|t| t.id == token.id && t.revoked_at.is_some()));
    // The name is free again.
    let (again, _) = store.create_token(&name, Role::Viewer, &[], None, &quiet()).await.unwrap();
    store.revoke_token(&name, None, &quiet()).await.unwrap();
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
    let (token, token_secret) = store.create_token(&name, Role::Operator, &[], None, &quiet()).await.unwrap();
    let limits = SessionLimits::default();

    let (secret, expires_at) = store.create_session(SessionFor::Token(token.id), limits, &quiet()).await.unwrap();
    assert!(secret.starts_with(ductus_store::SESSION_PREFIX) && secret.len() == 68, "{secret}");
    let days = (expires_at - chrono::Utc::now()).num_hours();
    assert!((167..=168).contains(&days), "a week from now: {expires_at}");
    let (found, _) = store.authenticate_session(&secret, limits).await.unwrap().expect("the session works");
    assert_eq!((found.token_id, found.role), (Some(token.id), Role::Operator));
    assert!(store.authenticate(&secret).await.unwrap().is_none(), "a session isn't a token");
    assert!(store.authenticate_session(&token_secret, limits).await.unwrap().is_none(), "nor a token a session");

    // Sign-out.
    assert!(store.end_session(&secret, None).await.unwrap());
    assert!(store.authenticate_session(&secret, limits).await.unwrap().is_none());

    // Idle too long, or past its end.
    let (idle, _) = store.create_session(SessionFor::Token(token.id), limits, &quiet()).await.unwrap();
    let (old, _) = store.create_session(SessionFor::Token(token.id), limits, &quiet()).await.unwrap();
    let (live, _) = store.create_session(SessionFor::Token(token.id), limits, &quiet()).await.unwrap();
    for (set, secret) in
        [("last_seen_at = now() - interval '13 hours'", &idle), ("expires_at = now() - interval '1 second'", &old)]
    {
        let sql = format!("UPDATE api_sessions SET {set} WHERE hash = sha256(convert_to($1, 'UTF8'))");
        let changed = sqlx::query(sqlx::AssertSqlSafe(sql)).bind(secret).execute(&pool).await.unwrap();
        assert_eq!(changed.rows_affected(), 1);
    }
    assert!(store.authenticate_session(&idle, limits).await.unwrap().is_none(), "idle 13 hours");
    assert!(store.authenticate_session(&old, limits).await.unwrap().is_none(), "past its end");
    assert!(store.authenticate_session(&live, limits).await.unwrap().is_some());

    // Revoking the token ends its sessions.
    store.revoke_token(&name, None, &quiet()).await.unwrap();
    assert!(store.authenticate_session(&live, limits).await.unwrap().is_none());
}

/// Tests that make admins take turns: the "last admin" guard counts every admin in the database.
static ADMINS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn unique(prefix: &str) -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    format!("{prefix}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed))
}

/// The accounts a test makes, deleted when it ends (pass or fail).
fn people() -> TestUsers {
    TestUsers::new(&std::env::var("DUCTUS_TEST_DATABASE_URL").unwrap())
}

async fn make_user(store: &Store, people: &TestUsers, role: Role, permissions: &[&str]) -> ductus_store::User {
    let permissions: Vec<String> = permissions.iter().map(|p| p.to_string()).collect();
    store
        .create_user(
            &ductus_store::NewUser {
                username: &people.name("test-user"),
                display_name: "",
                email: "",
                role,
                permissions: &permissions,
                password_hash: Some("$argon2id$not-checked-here"),
                must_change_password: false,
                source: "local",
            },
            &quiet(),
        )
        .await
        .unwrap()
}

/// Accounts: names ignore case, wrong passwords lock an account for a while and a good sign-in
/// clears the count, and a disabled user's sessions and tokens stop at once.
#[tokio::test]
async fn users_lock_after_wrong_passwords_and_stop_when_disabled() {
    let Some(store) = store().await else { return };
    let limits = SessionLimits::default();
    let people = people();
    let user = make_user(&store, &people, Role::Operator, &["payloads"]).await;
    let err = store
        .create_user(
            &ductus_store::NewUser {
                username: &user.username.to_uppercase(),
                display_name: "",
                email: "",
                role: Role::Viewer,
                permissions: &[],
                password_hash: None,
                must_change_password: false,
                source: "local",
            },
            &quiet(),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("already a user"), "{err}");
    assert!(store.user_by_name(&user.username.to_uppercase()).await.unwrap().is_some(), "names ignore case");
    let unknown = ductus_store::clean_permissions(&["root".to_string()]).unwrap_err().to_string();
    assert!(unknown.contains("unknown permission"), "{unknown}");

    // Three wrong passwords within the window lock it; a right one (after an unlock) clears the count.
    let lockout = ductus_store::Lockout {
        attempts: 3,
        window: std::time::Duration::from_secs(900),
        lock_for: std::time::Duration::from_secs(600),
    };
    assert!(store.record_failed_login(user.id, lockout, &quiet()).await.unwrap().is_none());
    assert!(store.record_failed_login(user.id, lockout, &quiet()).await.unwrap().is_none());
    let until = store.record_failed_login(user.id, lockout, &quiet()).await.unwrap().expect("locked at the third");
    assert!((9..=10).contains(&(until - chrono::Utc::now()).num_minutes()), "{until}");
    let candidate = store.candidate(&user.username).await.unwrap().unwrap();
    assert_eq!(candidate.locked_until, Some(until));
    store.unlock_user(user.id, &quiet()).await.unwrap();
    assert!(store.candidate(&user.username).await.unwrap().unwrap().locked_until.is_none());
    store.record_failed_login(user.id, lockout, &quiet()).await.unwrap();
    store.record_sign_in(user.id).await.unwrap();
    assert!(store.record_failed_login(user.id, lockout, &quiet()).await.unwrap().is_none(), "the count started over");

    // A session acts as the user; their token never goes above them.
    let (secret, _) = store.create_session(SessionFor::User(user.id), limits, &quiet()).await.unwrap();
    let (who, _) = store.authenticate_session(&secret, limits).await.unwrap().expect("the session works");
    assert_eq!((who.user_id, who.token_id, who.role), (Some(user.id), None, Role::Operator));
    assert!(who.can("payloads") && !who.can("audit"));
    let tname = unique("test-user-token");
    let permissions = ["payloads".to_string(), "audit".to_string()];
    let (token, token_secret) =
        store.create_token(&tname, Role::Admin, &permissions, Some(user.id), &quiet()).await.unwrap();
    assert_eq!(token.owner.as_deref(), Some(user.username.as_str()));
    let acting = store.authenticate(&token_secret).await.unwrap().expect("the token works");
    assert_eq!(acting.role, Role::Operator, "capped at its owner's role");
    assert_eq!(acting.permissions, ["payloads"], "and at the owner's permissions");
    assert_eq!(store.tokens(false, Some(user.id)).await.unwrap().len(), 1);
    assert!(!store.revoke_token(&tname, Some(user.id + 1_000_000), &quiet()).await.unwrap(), "only the owner's own");

    // Disabling ends the sessions and stops the tokens.
    store
        .update_user(user.id, &ductus_store::UserChange { disabled: Some(true), ..Default::default() }, &quiet())
        .await
        .unwrap();
    assert!(store.authenticate_session(&secret, limits).await.unwrap().is_none());
    assert!(store.authenticate(&token_secret).await.unwrap().is_none(), "a disabled user's token stops");
    store
        .update_user(user.id, &ductus_store::UserChange { disabled: Some(false), ..Default::default() }, &quiet())
        .await
        .unwrap();
    assert!(store.authenticate(&token_secret).await.unwrap().is_some(), "and works again when they are back");
    store.revoke_token(&tname, None, &quiet()).await.unwrap();
    people.forget().await.unwrap();
}

/// Directory accounts: made at the first sign-in, kept as the directory says (each change logged
/// once), refused accounts stop at once (sessions and tokens) and come back when the directory
/// says yes again; a local account's name is never taken over.
#[tokio::test]
async fn directory_accounts_follow_the_directory() {
    use ductus_store::{DirectoryPerson, DirectoryUpdate};
    let Some(store) = store().await else { return };
    let url = std::env::var("DUCTUS_TEST_DATABASE_URL").unwrap();
    let pool = sqlx::PgPool::connect(&url).await.unwrap();
    let limits = SessionLimits::default();
    let people = people();
    let name = people.name("test-user-dir");
    let audit = || ductus_store::Audit::new(ductus_store::By::engine(), "user.directory_change");
    let viewer = DirectoryPerson {
        display_name: "Anna Smith".into(),
        email: "anna@h.org".into(),
        role: Role::Viewer,
        permissions: vec![],
        username: None,
        directory_id: None,
    };
    let actions = |name: String| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, String>("SELECT action FROM audit_log WHERE subject = $1 ORDER BY id")
                .bind(name)
                .fetch_all(&pool)
                .await
                .unwrap()
        }
    };

    let (user, update) = store.save_directory_user(&name, &viewer, &audit()).await.unwrap();
    assert_eq!((update, user.source.as_str(), user.role), (DirectoryUpdate::Created, "ldap", Role::Viewer));
    assert!(user.directory_checked_at.is_some() && user.directory_refused_at.is_none());
    let candidate = store.candidate(&name.to_uppercase()).await.unwrap().unwrap();
    assert_eq!((candidate.source.as_str(), candidate.password_hash), ("ldap", None), "no password kept here");
    let (_, update) = store.save_directory_user(&name, &viewer, &audit()).await.unwrap();
    assert_eq!(update, DirectoryUpdate::Unchanged);
    // A refresh's check of an account the directory says nothing new about writes nothing; the
    // round notes the time for all it asked about at once.
    let checked = |id: i32| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, chrono::DateTime<chrono::Utc>>(
                "SELECT directory_checked_at FROM users WHERE id = $1",
            )
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    let before = checked(user.id).await;
    let (same, update) = store.check_directory_user(&name, &viewer, &audit()).await.unwrap();
    assert_eq!((same.id, update), (user.id, DirectoryUpdate::Unchanged));
    assert_eq!(checked(user.id).await, before, "nothing written");
    store.directory_checked(&[user.id]).await.unwrap();
    assert!(checked(user.id).await > before);
    let moved = DirectoryPerson { email: "anna.smith@h.org".into(), ..viewer.clone() };
    let (changed, update) = store.check_directory_user(&name, &moved, &audit()).await.unwrap();
    assert_eq!((update, changed.email.as_str()), (DirectoryUpdate::Changed, "anna.smith@h.org"), "a change is saved");
    store.save_directory_user(&name, &viewer, &audit()).await.unwrap();

    let operator = DirectoryPerson { role: Role::Operator, permissions: vec!["payloads".into()], ..viewer.clone() };
    let (user, update) = store.save_directory_user(&name.to_uppercase(), &operator, &audit()).await.unwrap();
    assert_eq!(
        (update, user.role, user.permissions.as_slice()),
        (DirectoryUpdate::Changed, Role::Operator, &["payloads".to_string()][..])
    );
    assert_eq!(user.username, name, "the name keeps its first spelling");

    // Refused: sessions end, tokens stop, logged once however often it's seen.
    let (session, _) = store.create_session(SessionFor::User(user.id), limits, &quiet()).await.unwrap();
    let (_, token) =
        store.create_token(&unique("test-user-dirtoken"), Role::Operator, &[], Some(user.id), &quiet()).await.unwrap();
    let to_check = store.directory_users_to_check().await.unwrap();
    assert!(to_check.iter().any(|a| a.id == user.id && a.username == name), "has a session: checked at refresh");
    let refused = || ductus_store::Audit::new(ductus_store::By::engine(), "user.directory_refused");
    assert!(store.refuse_directory_user(user.id, "in no Ductus group", &refused()).await.unwrap());
    assert!(!store.refuse_directory_user(user.id, "in no Ductus group", &refused()).await.unwrap(), "already refused");
    assert!(store.authenticate_session(&session, limits).await.unwrap().is_none(), "the session ended");
    assert!(store.authenticate(&token).await.unwrap().is_none(), "the token stops");
    assert!(store.user(user.id).await.unwrap().unwrap().directory_refused_at.is_some());
    assert!(
        store.directory_users_to_check().await.unwrap().iter().any(|a| a.id == user.id),
        "still checked: it has a token, which comes back if the directory says yes"
    );

    // Back in a group: access comes back, logged as a change.
    let (user, update) = store.save_directory_user(&name, &operator, &audit()).await.unwrap();
    assert_eq!((update, user.directory_refused_at), (DirectoryUpdate::Changed, None));
    assert!(store.authenticate(&token).await.unwrap().is_some(), "the token works again");
    assert_eq!(
        actions(name.clone()).await,
        [
            "user.create",
            "user.directory_change", // the email, and back
            "user.directory_change",
            "user.directory_change", // operator
            "user.directory_refused",
            "user.directory_change"
        ]
    );

    // A local account's name is never taken over, and local accounts are never checked.
    let local = make_user(&store, &people, Role::Viewer, &[]).await;
    let err = store.save_directory_user(&local.username, &viewer, &audit()).await.unwrap_err().to_string();
    assert!(err.contains("is a local account"), "{err}");
    assert!(!store.refuse_directory_user(local.id, "x", &refused()).await.unwrap());
    assert!(store.user(local.id).await.unwrap().unwrap().directory_refused_at.is_none());
    store.create_session(SessionFor::User(local.id), limits, &quiet()).await.unwrap();
    assert!(!store.directory_users_to_check().await.unwrap().iter().any(|a| a.id == local.id));

    // The accounts go with `people`; their audit rows too, which the checks above read by name.
    for id in [user.id, local.id] {
        sqlx::query("DELETE FROM audit_log WHERE user_id = $1 OR subject = (SELECT username FROM users WHERE id = $1)")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
    }
    people.forget().await.unwrap();
}

/// A directory account is its person (0.6.7.1): found by directory id, renamed when the
/// directory's sign-in name changes, given its id at the first check when it had none; a name the
/// directory gives someone new gets them a new account, while the earlier one moves aside
/// (`<name>#<id>`), refused with its tokens. Every change of name or email keeps the old value,
/// in the change's transaction (a refused change keeps nothing).
#[tokio::test]
async fn directory_ids_follow_the_person_and_former_names_are_kept() {
    use ductus_store::{DirectoryPerson, DirectoryUpdate, FormerName, UserChange};
    let Some(store) = store().await else { return };
    let people = people();
    let audit = || ductus_store::Audit::new(ductus_store::By::engine(), "user.directory_change");
    let (first, renamed, legacy) =
        (people.name("test-user-dirid"), people.name("test-user-dirid-renamed"), people.name("test-user-dirid-old"));
    let uuid = |n: u32| format!("entryUUID:{}-{n}", std::process::id());
    let person = |name: &str, id: Option<String>| DirectoryPerson {
        display_name: "Anna Smith".into(),
        email: "anna@h.org".into(),
        role: Role::Viewer,
        permissions: vec![],
        username: Some(name.to_string()),
        directory_id: id,
    };
    let kinds = |names: Vec<FormerName>| names.into_iter().map(|n| (n.kind, n.value)).collect::<Vec<_>>();

    let (anna, update) = store.save_directory_user(&first, &person(&first, Some(uuid(1))), &audit()).await.unwrap();
    assert_eq!((update, anna.directory_id.clone()), (DirectoryUpdate::Created, Some(uuid(1))));

    // Married: a new sign-in name, display name and email. Found by id at the refresh (by her old
    // name), and at her next sign-in by her new one: one account.
    let married = DirectoryPerson {
        display_name: "Anna Jones".into(),
        email: "anna.jones@h.org".into(),
        ..person(&renamed, Some(uuid(1)))
    };
    let (user, update) = store.save_directory_user(&first, &married, &audit()).await.unwrap();
    assert_eq!((user.id, user.username.as_str(), update), (anna.id, renamed.as_str(), DirectoryUpdate::Changed));
    let (user, update) = store.save_directory_user(&renamed, &married, &audit()).await.unwrap();
    assert_eq!((user.id, update), (anna.id, DirectoryUpdate::Unchanged));
    assert_eq!(
        kinds(store.former_names(anna.id).await.unwrap()),
        [
            ("username".to_string(), first.clone()),
            ("display_name".to_string(), "Anna Smith".to_string()),
            ("email".to_string(), "anna@h.org".to_string())
        ]
    );

    // An account from before ids were kept is found by name and takes its id.
    let (old, _) = store.save_directory_user(&legacy, &person(&legacy, None), &audit()).await.unwrap();
    assert_eq!(old.directory_id, None);
    let (user, update) = store.save_directory_user(&legacy, &person(&legacy, Some(uuid(2))), &audit()).await.unwrap();
    assert_eq!((user.id, user.directory_id, update), (old.id, Some(uuid(2)), DirectoryUpdate::Unchanged));

    // Her old sign-in name given to someone new (another id): a fresh account; hers is untouched.
    let (newcomer, update) = store.save_directory_user(&first, &person(&first, Some(uuid(3))), &audit()).await.unwrap();
    assert_eq!(update, DirectoryUpdate::Created);
    assert_ne!(newcomer.id, anna.id);
    assert_eq!(store.user(anna.id).await.unwrap().unwrap().username, renamed);

    // Now the newcomer leaves and the name goes to a third person, before any refresh: the
    // newcomer's account moves aside, refused, its token with it.
    let (_, token) = store
        .create_token(&unique("test-user-dirid-token"), Role::Viewer, &[], Some(newcomer.id), &quiet())
        .await
        .unwrap();
    assert!(store.authenticate(&token).await.unwrap().is_some());
    let (third, update) = store.save_directory_user(&first, &person(&first, Some(uuid(4))), &audit()).await.unwrap();
    assert_eq!(
        (update, third.username.as_str(), third.directory_refused_at),
        (DirectoryUpdate::Created, first.as_str(), None)
    );
    let aside = store.user(newcomer.id).await.unwrap().unwrap();
    assert_eq!(aside.username, format!("{first}#{}", newcomer.id));
    assert!(aside.directory_refused_at.is_some());
    assert!(store.authenticate(&token).await.unwrap().is_none(), "the earlier person's token stays dead");
    assert_eq!(kinds(store.former_names(newcomer.id).await.unwrap()), [("username".to_string(), first.clone())]);
    // The earlier person seen again (under their id) gets their account back, under its new name.
    let (back, _) =
        store.save_directory_user(&aside.username, &person(&aside.username, Some(uuid(3))), &audit()).await.unwrap();
    assert_eq!((back.id, back.directory_refused_at), (newcomer.id, None));

    // A name a local account holds is never taken, by a rename either; nothing is kept then.
    let local = make_user(&store, &people, Role::Viewer, &[]).await;
    let err = store.save_directory_user(&renamed, &person(&local.username, Some(uuid(1))), &audit()).await.unwrap_err();
    assert!(err.to_string().contains("is a local account"), "{err}");
    assert_eq!(store.former_names(anna.id).await.unwrap().len(), 3, "the refused change kept nothing");
    assert_eq!(store.user(anna.id).await.unwrap().unwrap().username, renamed);

    // An admin's edit keeps the old values too (an empty one is nothing to remember).
    let change = |name: &str, email: &str| UserChange {
        display_name: Some(name.into()),
        email: Some(email.into()),
        ..Default::default()
    };
    store.update_user(local.id, &change("Vic Local", "vic@h.org"), &quiet()).await.unwrap();
    assert_eq!(store.former_names(local.id).await.unwrap(), [], "it had none");
    store.update_user(local.id, &change("Vic Jones", "vic@h.org"), &quiet()).await.unwrap();
    store.update_user(local.id, &change("Vic Jones", "vic@h.org"), &quiet()).await.unwrap();
    assert_eq!(
        kinds(store.former_names(local.id).await.unwrap()),
        [("display_name".to_string(), "Vic Local".to_string())],
        "only what changed"
    );

    // What owners are matched against: every account with its former names, each with its own.
    let accounts = store.accounts().await.unwrap();
    assert!(accounts.users.iter().any(|u| u.id == local.id));
    let theirs = |id: i32| -> Vec<String> {
        accounts.former.iter().filter(|n| n.user_id == id).map(|n| format!("{} {}", n.kind, n.value)).collect()
    };
    assert_eq!(theirs(local.id), ["display_name Vic Local"]);
    assert_eq!(theirs(anna.id).len(), 3);
    people.forget().await.unwrap();
}

/// Two engines making one person's account at once (0.6.7.3: an owner's at both engines' first
/// refresh round, or one new person signing in on two engines): one makes it, the others find it
/// and bring it up to date. Before, the second insert failed on the unique name.
#[tokio::test]
async fn two_engines_making_one_directory_account_at_once_both_succeed() {
    use ductus_store::{DirectoryPerson, DirectoryUpdate};
    let Some(store) = store().await else { return };
    let url = std::env::var("DUCTUS_TEST_DATABASE_URL").unwrap();
    let other = Store::connect(&url, 4).await.unwrap();
    let pool = sqlx::PgPool::connect(&url).await.unwrap();
    let people = people();
    let audit = || ductus_store::Audit {
        detail: Some(serde_json::json!({ "reason": "owner" })),
        ..ductus_store::Audit::new(ductus_store::By::engine(), "user.create")
    };
    for round in 0..6 {
        let name = people.name("test-user-race");
        // With an id and without one (a directory that gives none): found by id, or by name.
        let id = (round % 2 == 0).then(|| format!("entryUUID:race-{name}"));
        let person = DirectoryPerson {
            display_name: "Rae Race".into(),
            email: "rae@h.org".into(),
            role: Role::Operator,
            permissions: vec![],
            username: Some(name.clone()),
            directory_id: id.clone(),
        };
        let (a, b) = (audit(), audit());
        let save = |s: &'static str, store: &Store| {
            let (name, person, a) = (&name, &person, &a);
            let store = store.clone();
            async move { store.save_directory_user(name, person, a).await.map_err(|e| format!("{s}: {e:#}")) }
        };
        let (w, x, y, z) = tokio::join!(save("1", &store), save("2", &other), save("3", &store), save("4", &other));
        let saved: Vec<_> = [w, x, y, z].into_iter().map(|r| r.expect("every save succeeds")).collect();
        assert!(saved.iter().all(|(u, _)| u.id == saved[0].0.id), "one account, round {round}");
        let made = saved.iter().filter(|(_, update)| *update == DirectoryUpdate::Created).count();
        assert_eq!(made, 1, "made once, round {round}");
        let (user, _) = other.save_directory_user(&name, &person, &b).await.unwrap();
        assert_eq!((user.id, user.directory_id), (saved[0].0.id, id));
        let detail: Option<String> =
            sqlx::query_scalar("SELECT detail::text FROM audit_log WHERE action = 'user.create' AND subject = $1")
                .bind(&name)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(detail.as_deref().is_some_and(|d| d.contains(r#""reason": "owner""#)), "{detail:?}");
    }
    people.forget().await.unwrap();
}

/// Changing a password ends the user's other sessions but not the one changing it; people can end
/// their own sessions and nobody else's; and an admin-chosen password has to be changed.
#[tokio::test]
async fn password_changes_end_other_sessions() {
    let Some(store) = store().await else { return };
    let limits = SessionLimits::default();
    let people = people();
    let user = make_user(&store, &people, Role::Viewer, &[]).await;
    let other = make_user(&store, &people, Role::Viewer, &[]).await;
    let (mine, _) = store.create_session(SessionFor::User(user.id), limits, &quiet()).await.unwrap();
    let (elsewhere, _) = store.create_session(SessionFor::User(user.id), limits, &quiet()).await.unwrap();
    let (theirs, _) = store.create_session(SessionFor::User(other.id), limits, &quiet()).await.unwrap();

    let listed = store.user_sessions(user.id, Some(&mine), limits.idle).await.unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed.iter().filter(|s| s.current).count(), 1);
    let not_mine = store.user_sessions(other.id, None, limits.idle).await.unwrap()[0].id;
    assert!(!store.end_user_session(user.id, not_mine).await.unwrap(), "someone else's session");

    store.set_password(user.id, "$argon2id$new", false, Some(&mine), &quiet()).await.unwrap();
    assert!(store.authenticate_session(&mine, limits).await.unwrap().is_some(), "the one changing it stays");
    assert!(store.authenticate_session(&elsewhere, limits).await.unwrap().is_none(), "the others end");
    assert!(store.authenticate_session(&theirs, limits).await.unwrap().is_some(), "other people's are untouched");
    assert_eq!(store.password_hash(user.id).await.unwrap().as_deref(), Some("$argon2id$new"));

    store.set_password(user.id, "$argon2id$temporary", true, None, &quiet()).await.unwrap();
    assert!(store.authenticate_session(&mine, limits).await.unwrap().is_none(), "a reset ends every session");
    let (again, _) = store.create_session(SessionFor::User(user.id), limits, &quiet()).await.unwrap();
    let (who, _) = store.authenticate_session(&again, limits).await.unwrap().unwrap();
    assert!(who.must_change_password, "an admin-chosen password has to be changed");
    store.set_password(user.id, "$argon2id$mine", false, Some(&again), &quiet()).await.unwrap();
    let (who, _) = store.authenticate_session(&again, limits).await.unwrap().unwrap();
    assert!(!who.must_change_password);
    people.forget().await.unwrap();
}

/// Appearance is saved per user and replaced as a whole.
#[tokio::test]
async fn appearance_is_kept_per_user() {
    let Some(store) = store().await else { return };
    let people = people();
    let a = make_user(&store, &people, Role::Viewer, &[]).await;
    let b = make_user(&store, &people, Role::Viewer, &[]).await;
    assert!(store.user_settings(a.id).await.unwrap().is_none());
    store.save_user_settings(a.id, "custom", Some(r##"{"colors":{"accent":"#112233"}}"##)).await.unwrap();
    store.save_user_settings(b.id, "dark", None).await.unwrap();
    let saved = store.user_settings(a.id).await.unwrap().unwrap();
    assert_eq!(saved.theme, "custom");
    assert!(saved.colors.unwrap().contains("#112233"));
    store.save_user_settings(a.id, "light", None).await.unwrap();
    let saved = store.user_settings(a.id).await.unwrap().unwrap();
    assert_eq!((saved.theme.as_str(), saved.colors), ("light", None));
    assert_eq!(store.user_settings(b.id).await.unwrap().unwrap().theme, "dark");
    people.forget().await.unwrap();
}

/// Tests delete the accounts they make, with their sessions, tokens and settings; nothing but a
/// `test-user-*` account is ever deleted.
#[tokio::test]
async fn test_accounts_go_with_what_refers_to_them() {
    let Some(store) = store().await else { return };
    let err = store.forget_test_users(&["anna".into()]).await.unwrap_err().to_string();
    assert!(err.contains("isn't a test account"), "{err}");
    assert!(store.forget_test_users(&["%".into()]).await.is_err(), "not every account either");

    let people = people();
    let user = make_user(&store, &people, Role::Operator, &[]).await;
    let limits = SessionLimits::default();
    let (session, _) = store.create_session(SessionFor::User(user.id), limits, &quiet()).await.unwrap();
    let (_, token) =
        store.create_token(&unique("test-user-forget"), Role::Viewer, &[], Some(user.id), &quiet()).await.unwrap();
    let token_session = {
        let who = store.authenticate(&token).await.unwrap().unwrap();
        store.create_session(SessionFor::Token(who.token_id.unwrap()), limits, &quiet()).await.unwrap().0
    };
    store.save_user_settings(user.id, "dark", None).await.unwrap();

    assert_eq!(store.forget_test_users(std::slice::from_ref(&user.username)).await.unwrap(), 1);
    assert!(store.user(user.id).await.unwrap().is_none());
    assert!(store.authenticate(&token).await.unwrap().is_none());
    for s in [&session, &token_session] {
        assert!(store.authenticate_session(s, limits).await.unwrap().is_none());
    }
    assert_eq!(people.forget().await.unwrap(), 0, "already gone");
}

/// The last admin who can sign in can't be disabled or demoted (it would lock everyone out of
/// user management). Counts every admin in the database, so when the test database has others
/// already, the guard isn't reached and the test checks that the change goes through.
#[tokio::test]
async fn the_last_admin_stays() {
    let Some(store) = store().await else { return };
    let _serial = ADMINS.lock().await;
    // Admins tests made and left (a killed run) would hide the guard.
    forget_test_admins(&store).await;
    let others =
        store.users().await.unwrap().iter().filter(|u| u.role == Role::Admin && u.disabled_at.is_none()).count();
    let people = people();
    let admin = make_user(&store, &people, Role::Admin, &[]).await;
    let demote = ductus_store::UserChange { role: Some(Role::Viewer), ..Default::default() };
    let disable = ductus_store::UserChange { disabled: Some(true), ..Default::default() };
    if others == 0 {
        for change in [&demote, &disable] {
            let err = store.update_user(admin.id, change, &quiet()).await.unwrap_err().to_string();
            assert!(err.contains("no admin who can sign in"), "{err}");
        }
        let second = make_user(&store, &people, Role::Admin, &[]).await;
        store.update_user(admin.id, &demote, &quiet()).await.expect("another admin remains");
        let err = store.update_user(second.id, &disable, &quiet()).await.unwrap_err().to_string();
        assert!(err.contains("no admin who can sign in"), "{err}");
    } else {
        store.update_user(admin.id, &demote, &quiet()).await.expect("other admins remain");
    }
    people.forget().await.unwrap();
}

/// Delete the test admins (`test-user-*`) in the database, whoever made them: admins only, since
/// other tests running now use their viewers and operators (tests that make admins take `ADMINS`).
async fn forget_test_admins(store: &Store) {
    let admins: Vec<String> = store
        .users()
        .await
        .unwrap()
        .into_iter()
        .filter(|u| u.role == Role::Admin && u.username.starts_with("test-user-"))
        .map(|u| u.username)
        .collect();
    store.forget_test_users(&admins).await.unwrap();
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

/// Startup applies every interface's retention at once, with `apply_retention`'s rules: new ones
/// are added, unchanged ones only read, changes go through the one-by-one path.
#[tokio::test]
async fn retention_is_applied_to_every_interface_at_once() {
    let Some(store) = store().await else { return };
    let [new, same, longer, shorter] = ["ret-all-new", "ret-all-same", "ret-all-longer", "ret-all-shorter"].map(unique);
    // Stored by an older version without a retention (49 days): the first apply starts a change.
    let legacy = Iface::single(&store, "ret-all-legacy").await;
    store.insert(NewMessage { tier: 49, ..legacy.msg(0, b"old") }).await.unwrap();
    for (name, days) in [(&same, 21), (&longer, 7), (&shorter, 49)] {
        store.apply_retention(name, days).await.unwrap();
    }
    let wanted: Vec<(String, ductus_store::Tier)> = vec![
        (new.clone(), 21),
        (same.clone(), 21),
        (longer.clone(), 49),
        (shorter.clone(), 7),
        (legacy.name.clone(), 7),
    ];
    let states = store.apply_retention_all(&wanted).await.unwrap();
    let got: Vec<_> = states.iter().map(|s| (s.interface.clone(), s.days, s.changing_from, s.waiting)).collect();
    assert_eq!(
        got,
        [
            (new.clone(), 21, None, None),
            (same.clone(), 21, None, None),
            (longer.clone(), 49, Some(7), None),
            (shorter.clone(), 49, None, Some(7)),
            (legacy.name.clone(), 7, Some(49), None),
        ],
        "in the order asked"
    );
    assert!(states[3].needs_confirmation());
    assert!(store.apply_retention_all(&[(new, 30)]).await.is_err(), "not a tier");
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
    // Dismissing c, in the held area, is how its quarantine ends (a resend or an edit would too).
    let c: i64 = sqlx::query_scalar("SELECT id FROM messages WHERE interface = $1 AND payload = 'c quarantined'")
        .bind(&keep.name)
        .fetch_one(&pool)
        .await
        .unwrap();
    let dismissal = ductus_store::Resend { messages: &[c], destinations: &keep.ids, quarantined_only: true };
    assert!(!store.dismiss(dismissal, OPERATOR).await.unwrap().is_empty(), "c's quarantined deliveries");
    assert_eq!(status_by_id(&store, c).await, Status::Filtered);
    let tiers = tiers_of(&pool, &keep.name).await;
    assert_eq!(tiers.last().unwrap(), &("c quarantined".to_string(), ductus_store::HELD), "still held");
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

/// The documentation snapshot matches the project exactly: changed rows are rewritten, unchanged
/// ones keep their time, entries that left the project are removed. Each of those is a history
/// row (0.7.3.3); an unchanged entry adds none.
#[tokio::test]
async fn the_docs_snapshot_follows_the_project() {
    let Some(url) = std::env::var("DUCTUS_TEST_DATABASE_URL").ok() else { return };
    let store = store().await.unwrap();
    let pool = sqlx::PgPool::connect(&url).await.unwrap();
    let _docs = DOCS.lock().await;
    // Names of its own: other tests' engines write the snapshot too.
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let p = format!("test-docs-{}-{nanos}", std::process::id());
    let (adt, api, lab, orders) = (format!("{p}-adt"), format!("api {p}"), format!("{p}-lab"), format!("{p}-orders"));
    let row = |kind, name: &str, docs: &str| ductus_store::DocsRow { kind, name: name.into(), docs: docs.into() };
    let read = || async {
        sqlx::query_as::<_, (String, String, String, chrono::DateTime<chrono::Utc>)>(
            "SELECT kind, name, docs::text, updated_at FROM docs_snapshot WHERE name LIKE '%' || $1 || '%'
             ORDER BY kind, name",
        )
        .bind(&p)
        .fetch_all(&pool)
        .await
        .unwrap()
    };
    let history = || async {
        sqlx::query_as::<_, (String, Option<String>, Option<i32>)>(
            "SELECT name, docs::text, deployment_id FROM docs_history WHERE name LIKE '%' || $1 || '%' ORDER BY id",
        )
        .bind(&p)
        .fetch_all(&pool)
        .await
        .unwrap()
    };
    store
        .save_docs_snapshot(&[
            row("interface", &adt, r#"{"custom": {"baa_signed": "no"}}"#),
            row("resource", &api, r#"{"custom": {}}"#),
            row("interface", &lab, r#"{"custom": {"baa_signed": "yes"}}"#),
        ])
        .await
        .unwrap();
    let first = read().await;
    assert_eq!(first.len(), 3);
    assert_eq!(first[0].2, r#"{"custom": {"baa_signed": "no"}}"#, "stored as jsonb: {first:?}");
    assert_eq!(history().await.len(), 3, "each new entry starts its history");

    // adt changes, lab stays, the api is gone, a new interface appears.
    store
        .save_docs_snapshot(&[
            row("interface", &adt, r#"{"custom": {"baa_signed": "yes"}}"#),
            row("interface", &lab, r#"{"custom":   {"baa_signed": "yes"}}"#),
            row("interface", &orders, r#"{"custom": {}}"#),
        ])
        .await
        .unwrap();
    let second = read().await;
    let names: Vec<_> = second.iter().map(|r| r.1.as_str()).collect();
    assert_eq!(names, [adt.as_str(), lab.as_str(), orders.as_str()], "{second:?}");
    assert_eq!(second[0].2, r#"{"custom": {"baa_signed": "yes"}}"#);
    assert!(second[0].3 > first[0].3, "a changed row gets a new time");
    assert_eq!(second[1].3, first[2].3, "an unchanged row is left alone (same jsonb, spacing aside)");
    let changes = history().await;
    let added: Vec<(&str, Option<&str>)> = changes[3..].iter().map(|(n, d, _)| (n.as_str(), d.as_deref())).collect();
    assert_eq!(
        added,
        [
            (adt.as_str(), Some(r#"{"custom": {"baa_signed": "yes"}}"#)),
            (orders.as_str(), Some(r#"{"custom": {}}"#)),
            (api.as_str(), None),
        ],
        "changed and new, then gone; lab unchanged adds nothing"
    );
    assert!(changes.iter().all(|(.., deployment)| deployment.is_none()), "no deployment named: {changes:?}");
    // An interface another live engine runs stays (engines with projects of their own on one
    // database); once that engine is gone, a save without it removes it.
    let other = unique("docs-other");
    let held = lock(&store, &other, "host-other").await;
    assert!(store.claim(&other, std::slice::from_ref(&lab)).await.unwrap().is_empty());
    store.save_docs_snapshot(&[row("interface", &adt, r#"{"custom": {"baa_signed": "yes"}}"#)]).await.unwrap();
    let names: Vec<String> = read().await.into_iter().map(|r| r.1).collect();
    assert_eq!(names, [adt.clone(), lab.clone()], "orders went, lab is the other engine's");
    store.release_engine(&other).await.unwrap();
    held.release().await;
    store.forget_engine(&other).await.unwrap();
    // An empty project empties the table.
    store.save_docs_snapshot(&[]).await.unwrap();
    assert!(read().await.is_empty());
    sqlx::query("DELETE FROM docs_history WHERE name LIKE '%' || $1 || '%'").bind(&p).execute(&pool).await.unwrap();
}

/// A code version made up for a test: `(interface, [(path, text)])`, hashed as the engine would
/// (the version's hash only has to be unique here).
struct TestVersion {
    interface: String,
    hash: Vec<u8>,
    files: Vec<(String, Vec<u8>, String)>,
    /// `code_versions.parts`.
    parts: String,
}

impl TestVersion {
    fn new(interface: &str, files: &[(&str, &str)]) -> Self {
        use sha2::Digest;
        let files: Vec<(String, Vec<u8>, String)> = files
            .iter()
            .map(|(path, text)| (path.to_string(), sha2::Sha256::digest(text.as_bytes()).to_vec(), text.to_string()))
            .collect();
        let mut all = sha2::Sha256::new();
        all.update(interface.as_bytes());
        for (_, hash, _) in &files {
            all.update(hash);
        }
        let parts = r#"{"source": [], "destinations": {}}"#.into();
        Self { interface: interface.into(), hash: all.finalize().to_vec(), files, parts }
    }

    fn borrow(&self) -> ductus_store::NewVersion<'_> {
        ductus_store::NewVersion {
            interface: &self.interface,
            hash: &self.hash,
            files: self.files.iter().map(|(p, h, t)| (p.as_str(), h.as_slice(), t.as_str())).collect(),
            parts: self.parts.clone(),
        }
    }
}

/// Record a deployment of `versions` in the test environment `env`: the versions saved first, as
/// the engine saves them (its workers wait for their ids), then the deployment.
async fn deploy(
    store: &Store,
    env: &str,
    by: &str,
    versions: &[&TestVersion],
) -> anyhow::Result<ductus_store::Deployed> {
    deploy_with(store, env, by, versions, &Default::default(), None).await
}

/// `deploy`, with warnings for the interfaces it changes and the documentation saved with it.
async fn deploy_with(
    store: &Store,
    env: &str,
    by: &str,
    versions: &[&TestVersion],
    warnings: &std::collections::HashMap<String, String>,
    docs: Option<&[ductus_store::DocsRow]>,
) -> anyhow::Result<ductus_store::Deployed> {
    let versions: Vec<_> = versions.iter().map(|v| v.borrow()).collect();
    let d = ductus_store::NewDeployment {
        environment: env,
        instance: env,
        host: "test-host",
        version: "0.7.0-test",
        how: "start",
        by,
        reason: Some("testing"),
        git_commit: None,
        uncommitted: None,
        versions: &versions,
        warnings,
        docs,
        release: None,
        project_version: None,
        rollback: false,
    };
    let ids = store.save_versions(&versions).await?;
    store.record_deployment(&d, &ids).await
}

/// Delete what deployments in environments named `env`… recorded (tests share the database).
async fn forget_deployments(pool: &sqlx::PgPool, env: &str) {
    let like = format!("{env}%");
    for statement in [
        "DELETE FROM interface_versions WHERE deployment_id IN (SELECT id FROM deployments WHERE environment LIKE $1)",
        "DELETE FROM deployment_changes WHERE deployment_id IN (SELECT id FROM deployments WHERE environment LIKE $1)",
        "DELETE FROM docs_history WHERE deployment_id IN (SELECT id FROM deployments WHERE environment LIKE $1)
            OR name LIKE $1",
        "DELETE FROM docs_snapshot WHERE name LIKE $1",
        "DELETE FROM deployments WHERE environment LIKE $1",
        "DELETE FROM interface_sets s WHERE EXISTS (SELECT 1 FROM unnest(s.interfaces) n WHERE n LIKE $1)
            AND NOT EXISTS (SELECT 1 FROM deployments d WHERE d.interface_set = s.id)",
        "DELETE FROM code_files f USING code_versions v, jsonb_array_elements(v.files) e
         WHERE v.interface LIKE $1 AND f.hash = decode(e->>'hash', 'hex')",
        "DELETE FROM code_versions WHERE interface LIKE $1",
    ] {
        sqlx::query(statement).bind(&like).execute(pool).await.unwrap();
    }
}

/// A start records each interface's version once; one that changes nothing writes only its
/// deployment, and says which deployment each version came with; a change names what it replaced.
#[tokio::test]
async fn deployments_record_what_changed() {
    let Some(store) = store().await else { return };
    let pool = sqlx::PgPool::connect(&std::env::var("DUCTUS_TEST_DATABASE_URL").unwrap()).await.unwrap();
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let env = format!("test-deploy-{}-{nanos}", std::process::id());
    let (a, b) = (format!("{env}-a"), format!("{env}-b"));
    let shared = format!("// shared by {env}\n");
    let (ya, yb) = (format!("name: a # {env}\n"), format!("name: b # {env}\n"));
    let a1 = TestVersion::new(&a, &[("interfaces/a/interface.yaml", &ya), ("lib/shared.ts", &shared)]);
    let b1 = TestVersion::new(&b, &[("interfaces/b/interface.yaml", &yb), ("lib/shared.ts", &shared)]);

    let first = deploy(&store, &env, "todd", &[&a1, &b1]).await.unwrap();
    assert_eq!(first.changed(), 2);
    assert!(first.interfaces.iter().all(|i| i.previous.is_none() && i.since.deployment == first.id));
    let shared_rows: i64 = sqlx::query_scalar("SELECT count(*) FROM code_files WHERE content = $1")
        .bind(&shared)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(shared_rows, 1, "a file two versions share is kept once");

    let again = deploy(&store, &env, "ductus", &[&a1, &b1]).await.unwrap();
    assert_eq!(again.changed(), 0);
    assert!(again.interfaces.iter().all(|i| i.since.deployment == first.id && i.since.by == "todd"));

    let changed = format!("{ya}# changed\n");
    let a2 = TestVersion::new(&a, &[("interfaces/a/interface.yaml", &changed), ("lib/shared.ts", &shared)]);
    let third = deploy(&store, &env, "ann", &[&a2, &b1]).await.unwrap();
    assert_eq!(third.changed(), 1);
    assert_eq!(third.interfaces[0].previous.as_deref(), Some(a1.hash.as_slice()));
    assert_eq!(third.interfaces[0].since.deployment, third.id);
    assert_eq!(third.interfaces[1].since.deployment, first.id);
    let (changes, from): (i64, Option<i32>) = sqlx::query_as(
        "SELECT count(*) OVER (), from_version FROM deployment_changes WHERE interface = $1 ORDER BY deployment_id DESC LIMIT 1",
    )
    .bind(&a)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(changes, 2, "new, then changed");
    assert!(from.is_some());

    // Two engines recording the same new version at once (each start writes what it hasn't seen):
    // both succeed, and the version and its files are there once.
    let third_text = format!("{ya}# {nanos}\n");
    let a3 = TestVersion::new(&a, &[("interfaces/a/interface.yaml", &third_text)]);
    let versions = [&a3];
    let (x, y) = tokio::join!(deploy(&store, &env, "x", &versions), deploy(&store, &env, "y", &versions));
    x.unwrap();
    y.unwrap();
    let versions: i64 = sqlx::query_scalar("SELECT count(*) FROM code_versions WHERE hash = $1")
        .bind(&a3.hash)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(versions, 1);
    forget_deployments(&pool, &env).await;
}

/// What `ductus history` reads for one interface (0.7.3.3): the deployments that changed it (all
/// that ran it with `all`, never one that didn't run it), with what they replaced and any warning;
/// the audit log's changes naming it (reads and refusals with `all`); its documentation's changes
/// with what each replaced and the deployment that made it; the files its versions named; what it
/// runs now.
#[tokio::test]
async fn history_reads_one_interface() {
    use ductus_store::HistoryPage;
    let Some(store) = store().await else { return };
    let pool = sqlx::PgPool::connect(&std::env::var("DUCTUS_TEST_DATABASE_URL").unwrap()).await.unwrap();
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let env = format!("test-history-{}-{nanos}", std::process::id());
    let (a, b) = (format!("{env}-a"), format!("{env}-b"));
    let ya = format!("name: a # {env}\n");
    let a1 = TestVersion::new(&a, &[("interfaces/a/interface.yaml", &ya), ("lib/old.ts", &format!("// {env}\n"))]);
    let a2 = TestVersion::new(&a, &[("interfaces/a/interface.yaml", &ya), ("lib/new.ts", &format!("// {env} 2\n"))]);
    let b1 = TestVersion::new(&b, &[("interfaces/b/interface.yaml", &format!("name: b # {env}\n"))]);
    let docs = |x: &str| {
        [ductus_store::DocsRow { kind: "interface", name: a.clone(), docs: format!(r#"{{"custom": {{"x": "{x}"}}}}"#) }]
    };
    let none = Default::default();
    let _docs = DOCS.lock().await;

    let d1 = deploy_with(&store, &env, "ann", &[&a1, &b1], &none, Some(&docs("1"))).await.unwrap();
    let d2 = deploy_with(&store, &env, "ann", &[&a1, &b1], &none, Some(&docs("1"))).await.unwrap();
    let warned = std::collections::HashMap::from([(a.clone(), "replaces ann's change".to_string())]);
    let d3 = deploy_with(&store, &env, "bob", &[&a2, &b1], &warned, Some(&docs("2"))).await.unwrap();
    let d4 = deploy(&store, &env, "bob", &[&b1]).await.unwrap(); // `a` not run
    let page = HistoryPage { interface: &a, until: None, limit: 50, below: None };

    let changed = store.history_deployments(page, false).await.unwrap();
    let ids: Vec<i32> = changed.iter().map(|d| d.id).collect();
    assert_eq!(ids, [d3.id, d1.id], "only those that changed it, newest first");
    assert_eq!(changed[0].from_version.as_deref(), Some(a1.hash.as_slice()));
    assert_eq!(changed[0].to_version.as_deref(), Some(a2.hash.as_slice()));
    assert_eq!((changed[0].by_name.as_str(), changed[0].warning.as_deref()), ("bob", Some("replaces ann's change")));
    assert_eq!((changed[1].from_version.as_ref(), changed[1].warning.as_ref()), (None, None), "new here");
    let all = store.history_deployments(page, true).await.unwrap();
    let ids: Vec<(i32, bool)> = all.iter().map(|d| (d.id, d.changed)).collect();
    assert_eq!(ids, [(d3.id, true), (d2.id, false), (d1.id, true)], "{d4:?} didn't run it");
    let older = HistoryPage { until: Some(d2.at), ..page };
    let ids: Vec<i32> = store.history_deployments(older, true).await.unwrap().iter().map(|d| d.id).collect();
    assert_eq!(ids, [d2.id, d1.id], "up to a time, that time included");

    for (who, action, outcome) in
        [("ops", "destination.pause", "ok"), ("ops", "message.payload.read", "ok"), ("eve", "message.replay", "denied")]
    {
        sqlx::query(
            "INSERT INTO audit_log (actor_kind, actor_name, action, interface, outcome) VALUES ('user', $1, $2, $3, $4)",
        )
        .bind(who)
        .bind(action)
        .bind(&a)
        .bind(outcome)
        .execute(&pool)
        .await
        .unwrap();
    }
    let changes: Vec<String> =
        store.history_audit(page, false, None).await.unwrap().into_iter().map(|r| r.row.action).collect();
    assert_eq!(changes, ["destination.pause"], "no reads, nothing refused");
    assert_eq!(store.history_audit(page, true, None).await.unwrap().len(), 3);

    let docs = store.history_docs(page, None).await.unwrap();
    let seen: Vec<(Option<i32>, Option<&str>, Option<&str>)> =
        docs.iter().map(|d| (d.deployment_id, d.docs.as_deref(), d.before.as_deref())).collect();
    assert_eq!(
        seen,
        [
            (Some(d3.id), Some(r#"{"custom": {"x": "2"}}"#), Some(r#"{"custom": {"x": "1"}}"#)),
            (Some(d1.id), Some(r#"{"custom": {"x": "1"}}"#), None),
        ],
        "d2 changed nothing"
    );

    let paths: Vec<String> = store.version_paths(&a).await.unwrap().into_iter().collect();
    assert_eq!(paths, ["interfaces/a/interface.yaml", "lib/new.ts", "lib/old.ts"], "every version's files");
    let now = store.current_versions(&[a.as_str(), "nobody-ran-this"]).await.unwrap();
    assert_eq!(now.len(), 1);
    assert_eq!((now[0].version.as_slice(), now[0].deployment, now[0].by.as_str()), (a2.hash.as_slice(), d3.id, "bob"));
    assert_eq!(now[0].files, ["interfaces/a/interface.yaml", "lib/new.ts"]);

    sqlx::query("DELETE FROM audit_log WHERE interface = $1").bind(&a).execute(&pool).await.unwrap();
    forget_deployments(&pool, &env).await;
}

/// History by part (0.7.4.2): each version keeps which files each part runs (a version saved
/// before gets them the next time an engine runs it, once); a deployment's change comes with both
/// versions' files and parts; the audit log narrowed to a destination (a pause by its subject, a
/// resend by its message actions, one recorded without them under every destination) or to the
/// source (raised lanes); documentation narrowed to a destination's fields.
#[tokio::test]
async fn history_reads_one_part() {
    use ductus_store::{HistoryPage, Part};
    let Some(store) = store().await else { return };
    let pool = sqlx::PgPool::connect(&std::env::var("DUCTUS_TEST_DATABASE_URL").unwrap()).await.unwrap();
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let env = format!("test-parts-{}-{nanos}", std::process::id());
    let a = format!("{env}-a");
    let yaml = |port: u32| format!("name: a # {env}\ndestinations: {{ lab: {{ mllp: {{ port: {port} }} }} }}\n");
    let (y1, y2) = (yaml(1), yaml(2));
    let mut v1 =
        TestVersion::new(&a, &[("interfaces/a/interface.yaml", &y1), ("interfaces/a/lab.ts", &format!("// {env}\n"))]);
    let parts = r#"{"source": [], "destinations": {"lab": ["interfaces/a/lab.ts"]}}"#;
    v1.parts = parts.into();
    let mut v2 =
        TestVersion::new(&a, &[("interfaces/a/interface.yaml", &y2), ("interfaces/a/lab.ts", &format!("// {env}\n"))]);
    v2.parts = parts.into();
    let read_parts = |hash: Vec<u8>| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, Option<String>>("SELECT parts::text FROM code_versions WHERE hash = $1")
                .bind(hash)
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };

    // v1 as an engine before 0.7.4.2 saved it: no parts; the next save fills them in.
    deploy(&store, &env, "ann", &[&v1]).await.unwrap();
    sqlx::query("UPDATE code_versions SET parts = NULL WHERE hash = $1").bind(&v1.hash).execute(&pool).await.unwrap();
    assert_eq!(store.version_files(&a).await.unwrap()[0].parts, None);
    let d2 = deploy(&store, &env, "bob", &[&v2]).await.unwrap();
    assert!(read_parts(v1.hash.clone()).await.is_none(), "not run again: still none");
    assert!(read_parts(v2.hash.clone()).await.unwrap().contains("lab.ts"));
    deploy(&store, &env, "ann", &[&v1]).await.unwrap();
    assert!(read_parts(v1.hash.clone()).await.unwrap().contains("lab.ts"), "run again: filled in");

    let page = HistoryPage { interface: &a, until: None, limit: 50, below: None };
    let changes = store.history_deployments(page, false).await.unwrap();
    assert_eq!(changes.len(), 3);
    let second = changes.iter().find(|d| d.id == d2.id).unwrap();
    let (from, to) = (second.from.as_ref().unwrap(), second.to.as_ref().unwrap());
    assert_eq!(from.files.len(), 2);
    assert_ne!(from.files[0].1, to.files[0].1, "interface.yaml differs");
    assert!(to.parts.as_deref().unwrap().contains("lab.ts"));
    let below = HistoryPage { below: Some(i64::from(d2.id)), ..page };
    assert_eq!(store.history_deployments(below, true).await.unwrap().len(), 1, "the batch below, no unchanged starts");
    assert_eq!((second.from_yaml.as_ref(), second.to_yaml.as_ref()), (Some(&y1), Some(&y2)), "the two texts");
    let third = changes.iter().find(|d| d.id != d2.id && d.from.is_some()).unwrap();
    assert_eq!((third.from_yaml.as_ref(), third.to_yaml.as_ref()), (Some(&y2), Some(&y1)));
    let first = changes.iter().find(|d| d.from.is_none()).unwrap();
    assert_eq!(first.to_yaml, None, "new here: nothing to compare");
    assert_eq!(store.version_files(&a).await.unwrap().len(), 2);

    // The audit log, by part.
    let lab: i32 = sqlx::query_scalar("INSERT INTO destinations (interface, name) VALUES ($1, 'lab') RETURNING id")
        .bind(&a)
        .fetch_one(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO destinations (interface, name) VALUES ($1, 'billing')")
        .bind(&a)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(store.destination_names(&a).await.unwrap(), ["billing", "lab"]);
    let audit = |action: &'static str, subject: Option<String>| {
        let (pool, a) = (pool.clone(), a.clone());
        async move {
            sqlx::query_scalar::<_, i64>(
                "INSERT INTO audit_log (actor_kind, actor_name, action, subject, interface, outcome)
                 VALUES ('user', 'ops', $1, $2, $3, 'ok') RETURNING id",
            )
            .bind(action)
            .bind(subject)
            .bind(&a)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    audit("destination.pause", Some(format!("{a}/lab"))).await;
    audit("destination.pause", Some(format!("{a}/billing"))).await;
    let resend = audit("message.replay", None).await;
    sqlx::query(
        "INSERT INTO message_actions (action, actor, message_id, received_at, interface, destination_id, audit_id)
         VALUES ('resend', 'ops', 1, now(), $1, $2, $3)",
    )
    .bind(&a)
    .bind(lab)
    .bind(resend)
    .execute(&pool)
    .await
    .unwrap();
    audit("message.dismiss", None).await; // recorded without its actions
    audit("interface.partitions_raised", None).await;
    audit("retention.confirm", None).await;
    let of = |part| {
        let store = &store;
        async move {
            store
                .history_audit(page, false, part)
                .await
                .unwrap()
                .into_iter()
                .map(|r| (r.row.action, r.row.subject.unwrap_or_default(), r.acted_on))
                .collect::<Vec<_>>()
        }
    };
    let lab_rows = of(Some(Part::Destination("lab"))).await;
    let actions: Vec<&str> = lab_rows.iter().map(|(a, ..)| a.as_str()).collect();
    assert_eq!(actions, ["message.dismiss", "message.replay", "destination.pause"]);
    assert_eq!(lab_rows[1].2, ["lab"], "the resend's destinations");
    let billing: Vec<String> = of(Some(Part::Destination("billing"))).await.into_iter().map(|r| r.0).collect();
    assert_eq!(billing, ["message.dismiss", "destination.pause"]);
    let source: Vec<String> = of(Some(Part::Source)).await.into_iter().map(|r| r.0).collect();
    assert_eq!(source, ["interface.partitions_raised"]);
    assert_eq!(of(None).await.len(), 6);

    // Documentation, by part.
    let _docs = DOCS.lock().await;
    for docs in [
        r#"{"owner": "ann", "destinations": {"lab": {"custom": {"x": "1"}}}}"#,
        r#"{"owner": "bob", "destinations": {"lab": {"custom": {"x": "1"}}}}"#,
        r#"{"owner": "bob", "destinations": {"lab": {"custom": {"x": "2"}}}}"#,
    ] {
        sqlx::query("INSERT INTO docs_history (kind, name, docs) VALUES ('interface', $1, $2::jsonb)")
            .bind(&a)
            .bind(docs)
            .execute(&pool)
            .await
            .unwrap();
    }
    assert_eq!(store.history_docs(page, None).await.unwrap().len(), 3);
    let lab_docs = store.history_docs(page, Some(Part::Destination("lab"))).await.unwrap();
    assert_eq!(lab_docs.len(), 2, "the first (lab's fields appear) and the third: not the owner's change");
    assert!(store.history_docs(page, Some(Part::Source)).await.unwrap().is_empty());

    sqlx::query("DELETE FROM docs_history WHERE name = $1").bind(&a).execute(&pool).await.unwrap();
    sqlx::query("DELETE FROM message_actions WHERE interface = $1").bind(&a).execute(&pool).await.unwrap();
    sqlx::query("DELETE FROM audit_log WHERE interface = $1").bind(&a).execute(&pool).await.unwrap();
    sqlx::query("DELETE FROM destinations WHERE interface = $1").bind(&a).execute(&pool).await.unwrap();
    forget_deployments(&pool, &env).await;
}

/// A start records its deployment beside it (0.7.4.2), so its first decisions can be saved before
/// the deployment's row is: such a delivery still names the deployment that brought its version
/// (Windows CI found one that named none).
#[tokio::test]
async fn a_delivery_decided_before_its_deployment_was_recorded_names_it() {
    let Some(store) = store().await else { return };
    let t = Iface::new(&store, "early", &["lab"]).await;
    let lab = t.id("lab");
    let env = t.name.clone();
    let v = TestVersion::new(&t.name, &[("interfaces/x/interface.yaml", &format!("name: {}\n", t.name))]);
    let ids = store.save_versions(&[v.borrow()]).await.unwrap();
    let id = ids[v.hash.as_slice()];
    let a = store.insert(t.msg(0, b"a")).await.unwrap();
    store.finish(lab, &[sent(a).by(Some(id))]).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    let deployed = deploy(&store, &env, "todd", &[&v]).await.unwrap();
    let decided = store.deliveries(a).await.unwrap();
    assert_eq!(decided[0].code_version.as_deref(), Some(v.hash.as_slice()));
    assert_eq!(decided[0].deployed_by.as_deref(), Some("todd"), "the deployment recorded just after");
    assert_eq!(decided[0].deployed_at, Some(deployed.at));
    let pool = sqlx::PgPool::connect(&std::env::var("DUCTUS_TEST_DATABASE_URL").unwrap()).await.unwrap();
    forget_deployments(&pool, &env).await;
}

/// A delivery says which code version decided it, with the deployment that brought that version
/// (0.7.3.2). Sending it again clears it and the resend's row keeps it; a person's decision (an
/// edited copy, a dismissal) leaves none, its row keeping what decided it before.
#[tokio::test]
async fn a_delivery_names_the_code_that_decided_it() {
    use ductus_store::{Copy, Edited, Resend};
    let Some(store) = store().await else { return };
    let pool = sqlx::PgPool::connect(&std::env::var("DUCTUS_TEST_DATABASE_URL").unwrap()).await.unwrap();
    let t = Iface::new(&store, "decided", &["billing", "lab"]).await;
    let (lab, billing) = (t.id("lab"), t.id("billing"));
    // Named as the interface, so `forget_deployments` finds its version too.
    let env = t.name.clone();
    let v = TestVersion::new(&t.name, &[("interfaces/x/interface.yaml", &format!("name: {}\n", t.name))]);
    let deployed = deploy(&store, &env, "todd", &[&v]).await.unwrap();
    let id: i32 = sqlx::query_scalar("SELECT id FROM code_versions WHERE hash = $1")
        .bind(&v.hash)
        .fetch_one(&pool)
        .await
        .unwrap();

    let a = store.insert(t.msg(0, b"a")).await.unwrap();
    let b = store.insert(t.msg(0, b"b")).await.unwrap();
    let pending = store.deliveries(a).await.unwrap();
    assert!(pending.iter().all(|d| d.code_version.is_none()), "nothing decided a pending delivery");
    store
        .finish(lab, &[sent(a).by(Some(id)), Done::with(b, Status::Quarantined, Some("AR")).by(Some(id))])
        .await
        .unwrap();
    store.finish(billing, &[sent(a), sent(b).by(Some(id))]).await.unwrap();
    let decided = store.deliveries(a).await.unwrap();
    let by_lab = decided.iter().find(|d| d.destination == "lab").unwrap();
    assert_eq!(by_lab.code_version.as_deref(), Some(v.hash.as_slice()));
    assert_eq!((by_lab.deployed_by.as_deref(), by_lab.deployed_reason.as_deref()), (Some("todd"), Some("testing")));
    assert_eq!(by_lab.deployed_at, Some(deployed.at));
    let unknown = decided.iter().find(|d| d.destination == "billing").unwrap();
    assert_eq!((unknown.code_version.as_ref(), unknown.deployed_by.as_ref()), (None, None), "not known: none");

    // Sent again: pending, decided by nothing yet; the resend's row keeps the version.
    let again = Resend { messages: &[a.id], destinations: &[lab], quarantined_only: false };
    assert_eq!(store.resend(again, OPERATOR).await.unwrap().len(), 1);
    let lab_now = store.deliveries(a).await.unwrap().into_iter().find(|d| d.destination == "lab").unwrap();
    assert_eq!((lab_now.status, lab_now.code_version), (Status::Pending, None));
    let actions = store.actions(a.id).await.unwrap();
    assert_eq!(actions[0].previous_code_version.as_deref(), Some(v.hash.as_slice()));

    // b's quarantine replaced by an edited copy: a person decided; the edit's row keeps the version.
    let lab_only = [lab];
    let copy = Copy {
        original: b.id,
        interface: &t.name,
        partition_key: 0,
        ordering_key: None,
        control_id: None,
        payload: b"b fixed",
        tier: TIER,
        destinations: &lab_only,
    };
    assert!(matches!(store.copy_edited(copy, OPERATOR).await.unwrap(), Edited::Copied { .. }));
    let replaced = store.deliveries(b).await.unwrap().into_iter().find(|d| d.destination == "lab").unwrap();
    assert_eq!((replaced.status, replaced.code_version), (Status::Filtered, None));
    assert_eq!(store.actions(b.id).await.unwrap()[0].previous_code_version.as_deref(), Some(v.hash.as_slice()));

    // A dismissal: the same.
    store.finish(lab, &[Done::with(a, Status::Quarantined, Some("AR again")).by(Some(id))]).await.unwrap();
    let dismissal = Resend { messages: &[a.id], destinations: &[lab], quarantined_only: true };
    assert_eq!(store.dismiss(dismissal, OPERATOR).await.unwrap().len(), 1);
    let dismissed = store.deliveries(a).await.unwrap().into_iter().find(|d| d.destination == "lab").unwrap();
    assert_eq!((dismissed.status, dismissed.code_version), (Status::Filtered, None));
    let last = store.actions(a.id).await.unwrap().pop().unwrap();
    assert_eq!((last.action.as_str(), last.previous_code_version.as_deref()), ("dismiss", Some(v.hash.as_slice())));
    forget_deployments(&pool, &env).await;
}

/// A test release's files: unique texts (a `code_files` row is shared by content), a binary file,
/// files in two interface folders, `lib/` and the top.
fn release_files(tag: &str, lab: &str) -> Vec<(String, Vec<u8>, bool)> {
    vec![
        ("ductus.yaml".into(), format!("# {tag}\n").into_bytes(), false),
        ("lib/codes.ts".into(), format!("export const tag = \"{tag}\";\n").into_bytes(), false),
        ("interfaces/adt/interface.yaml".into(), format!("name: adt-{tag}\n").into_bytes(), false),
        ("interfaces/adt/fixtures/logo.png".into(), [b"\x89PNG\0".as_slice(), tag.as_bytes()].concat(), true),
        ("interfaces/lab/lab.ts".into(), format!("// {tag} {lab}\n").into_bytes(), false),
    ]
}

async fn store_test_release(
    store: &Store,
    id: &[u8],
    files: &[(String, Vec<u8>, bool)],
    reason: &str,
) -> ductus_store::ReleaseStored {
    use sha2::Digest;
    let hashes: Vec<Vec<u8>> = files.iter().map(|(_, b, _)| sha2::Sha256::digest(b).to_vec()).collect();
    let files: Vec<ductus_store::ReleaseFile> = files
        .iter()
        .zip(&hashes)
        .map(|((path, bytes, binary), hash)| ductus_store::ReleaseFile { path, hash, bytes, binary: *binary })
        .collect();
    let manifest = serde_json::json!({ "id": "test", "made": { "reason": reason } });
    let new = ductus_store::NewRelease {
        id,
        project: "integrations",
        manifest: &manifest,
        made_at: Some(chrono::Utc::now()),
        made_by: "ann",
        reason: Some(reason),
        git_commit: None,
        branch: None,
        interfaces: 2,
        ancestry: &[7u8; 40],
        files: &files,
        by: "test-ci",
    };
    store.store_release(&new, &quiet()).await.unwrap()
}

async fn forget_release(pool: &sqlx::PgPool, id: &[u8]) {
    sqlx::query("DELETE FROM release_checks WHERE release_id = $1").bind(id).execute(pool).await.unwrap();
    sqlx::query("DELETE FROM release_places WHERE release_id = $1").bind(id).execute(pool).await.unwrap();
    sqlx::query("DELETE FROM releases WHERE id = $1").bind(id).execute(pool).await.unwrap();
}

/// A release is stored once, by folder: a second release that changes one interface adds one
/// folder and one file; reading it back gives every file as it was, binary ones byte for byte.
#[tokio::test]
async fn releases_are_stored_once_by_folder_and_read_back_whole() {
    let Some(store) = store().await else { return };
    let pool = sqlx::PgPool::connect(&std::env::var("DUCTUS_TEST_DATABASE_URL").unwrap()).await.unwrap();
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let tag = format!("{}-{nanos}", std::process::id());
    let (first, second) = ([1u8; 32].map(|b| b ^ (nanos as u8)), [2u8; 32].map(|b| b ^ (nanos as u8)));
    forget_release(&pool, &first).await;
    forget_release(&pool, &second).await;

    let files = release_files(&tag, "v1");
    let stored = store_test_release(&store, &first, &files, "first").await;
    assert!(stored.new);
    assert_eq!((stored.new_folders, stored.new_files, stored.by.as_str()), (4, 5, "test-ci"));
    let again = store_test_release(&store, &first, &files, "made again").await;
    assert!(!again.new, "the same id is stored once");
    assert_eq!((again.at, again.by.as_str(), again.new_folders), (stored.at, "test-ci", 0));

    let changed = release_files(&tag, "v2");
    let next = store_test_release(&store, &second, &changed, "second").await;
    assert_eq!((next.new, next.new_folders, next.new_files), (true, 1, 1), "only interfaces/lab changed");

    let back = store.release_contents(&first).await.unwrap().unwrap();
    let mut expected = files.clone();
    expected.sort();
    assert_eq!(back.files, expected);
    assert_eq!(back.ancestry, vec![7u8; 40]);
    assert_eq!(back.manifest["made"]["reason"], "first");

    let found = store.find_release(&ductus_store_hex(&second)[..12]).await.unwrap();
    assert_eq!(found.as_deref(), Some(second.as_slice()));
    assert!(store.find_release("0g").await.is_err(), "not hex");

    let report = serde_json::json!({ "ok": true, "checks": [] });
    let check = ductus_store::NewCheck {
        release: &second,
        environment: "prod",
        instance: "i-1",
        host: "box",
        by: "test-ci",
        ok: true,
        problems: 0,
        seconds: 1.5,
        report: &report,
        ductus_version: "0.7.0-test",
    };
    store.record_check(&check, &quiet()).await.unwrap();
    store.record_check(&ductus_store::NewCheck { ok: false, problems: 2, ..check.clone() }, &quiet()).await.unwrap();
    let listed = store.releases(None, 500).await.unwrap();
    let row = listed.iter().find(|r| r.id == second).expect("listed");
    assert_eq!((row.files, row.interfaces, row.reason.as_deref()), (5, 2, Some("second")));
    assert_eq!(row.checks.len(), 1, "the last check per environment");
    assert!(!row.checks[0].ok && row.checks[0].report.is_null(), "lists leave reports out");
    let (one, manifest) = store.release(&second).await.unwrap().unwrap();
    assert_eq!(one.checks.len(), 2);
    assert!(one.checks[0].report.is_object(), "the newest of an environment with its report");
    assert!(one.checks[1].report.is_null(), "older ones without (0.7.4.5: hundreds of KB each at scale)");
    let older = store.release_check(&second, one.checks[1].id).await.unwrap().unwrap();
    assert_eq!((older.ok, &older.report["ok"]), (true, &serde_json::json!(true)), "read one by one");
    assert!(store.release_check(&first, one.checks[1].id).await.unwrap().is_none(), "another release's");
    assert_eq!(manifest["made"]["reason"], "second");
    assert_eq!(store.release_summary(&first).await.unwrap().unwrap().checks.len(), 0);

    forget_release(&pool, &first).await;
    forget_release(&pool, &second).await;
}

/// The pre-run (0.7.4.4): a deploy reuses a check only when it passed, is of that release against
/// that environment by this Ductus version, is recent enough, and nothing was deployed there since.
/// And the release an engine in release mode runs: the last one deployed to its environment.
#[tokio::test]
async fn a_deploy_reuses_only_a_recent_passing_check_with_nothing_deployed_since() {
    let Some(store) = store().await else { return };
    let pool = sqlx::PgPool::connect(&std::env::var("DUCTUS_TEST_DATABASE_URL").unwrap()).await.unwrap();
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let tag = format!("{}-{nanos}", std::process::id());
    let env = format!("test-prerun-{tag}");
    let (first, second) = ([3u8; 32].map(|b| b ^ (nanos as u8)), [4u8; 32].map(|b| b ^ (nanos as u8)));
    forget_deployments(&pool, &env).await;
    store_test_release(&store, &first, &release_files(&tag, "a"), "first").await;
    store_test_release(&store, &second, &release_files(&tag, "b"), "second").await;
    let report = serde_json::json!({ "ok": true, "checks": [] });
    let check = |ok, version| ductus_store::NewCheck {
        release: &first,
        environment: &env,
        instance: "i-1",
        host: "box",
        by: "test-ci",
        ok,
        problems: if ok { 0 } else { 1 },
        seconds: 1.0,
        report: &report,
        ductus_version: version,
    };
    let hour_ago = chrono::Utc::now() - chrono::Duration::hours(1);
    let reuse = |version: &'static str, since| {
        let (store, env) = (store.clone(), env.clone());
        async move { store.reusable_check(&first, &env, version, since).await.unwrap().map(|c| c.id) }
    };
    assert_eq!(reuse("0.7.0-test", hour_ago).await, None, "no check yet");
    store.record_check(&check(false, "0.7.0-test"), &quiet()).await.unwrap();
    assert_eq!(reuse("0.7.0-test", hour_ago).await, None, "a failed check is never reused");
    let passed = store.record_check(&check(true, "0.7.0-test"), &quiet()).await.unwrap();
    assert_eq!(reuse("0.7.0-test", hour_ago).await, Some(passed));
    assert_eq!(reuse("0.7.1-test", hour_ago).await, None, "another Ductus version ran it");
    let later = chrono::Utc::now() + chrono::Duration::seconds(1);
    assert_eq!(reuse("0.7.0-test", later).await, None, "older than the window");
    assert_eq!(
        store.reusable_check(&first, "another-env", "0.7.0-test", hour_ago).await.unwrap().map(|c| c.id),
        None,
        "another environment's check"
    );

    // A deployment of any kind in that environment since the check: not reusable any more.
    let yaml = format!("name: {env}\n");
    let v = TestVersion::new(&env, &[("interfaces/a/interface.yaml", &yaml)]);
    let deployed = deploy_with(&store, &env, "ann", &[&v], &Default::default(), None).await.unwrap();
    assert_eq!(reuse("0.7.0-test", hour_ago).await, None, "something was deployed since");
    assert!(store.last_deployed(&env).await.unwrap().is_empty(), "folder mode records no release");

    // Release mode: the deployments name their release; the last one per project is what runs.
    for (release, by) in [(&first, "ann"), (&second, "bob")] {
        let versions = [v.borrow()];
        let d = ductus_store::NewDeployment {
            environment: &env,
            instance: &env,
            host: "test-host",
            version: "0.7.0-test",
            how: "handover",
            by,
            reason: Some("deploying"),
            git_commit: Some("abc"),
            uncommitted: None,
            versions: &versions,
            warnings: &Default::default(),
            docs: None,
            release: Some(release.as_slice()),
            project_version: Some([9u8; 32].as_slice()),
            rollback: false,
        };
        let ids = store.save_versions(&versions).await.unwrap();
        store.record_deployment(&d, &ids).await.unwrap();
    }
    let last = store.last_deployed(&env).await.unwrap();
    assert_eq!(last.len(), 1, "one project");
    assert_eq!(
        (last[0].project.as_str(), last[0].release.as_slice(), last[0].by.as_str()),
        ("integrations", second.as_slice(), "bob")
    );
    assert!(last[0].deployment > deployed.id);
    // The project version runs since the first deployment that brought it, not the restart after.
    let project = store.project_running(&env, "integrations").await.unwrap().unwrap();
    assert!(store.project_running(&env, "another").await.unwrap().is_none(), "by project");
    assert_eq!((project.version, project.by.as_str()), (vec![9u8; 32], "ann"));
    assert_eq!(project.release.as_deref(), Some(first.as_slice()));
    let current = store.current_versions(&[v.interface.as_str()]).await.unwrap();
    assert_eq!(current[0].release, None, "unchanged since the folder-mode start that brought it");

    forget_deployments(&pool, &env).await;
    forget_release(&pool, &first).await;
    forget_release(&pool, &second).await;
}

/// Promotion and rollback (0.7.4.4.3): where a release went is this database's deployments plus
/// what files said about others (kept once, merged on every upload, a record that came home listed
/// once); an environment's runs are one entry per release run, the rollbacks marked.
#[tokio::test]
async fn where_a_release_went_and_the_runs_a_rollback_chooses_from() {
    let Some(store) = store().await else { return };
    let pool = sqlx::PgPool::connect(&std::env::var("DUCTUS_TEST_DATABASE_URL").unwrap()).await.unwrap();
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let tag = format!("{}-{nanos}", std::process::id());
    let env = format!("test-promote-{tag}");
    let (a, b) = ([5u8; 32].map(|x| x ^ (nanos as u8)), [6u8; 32].map(|x| x ^ (nanos as u8)));
    forget_deployments(&pool, &env).await;
    store_test_release(&store, &a, &release_files(&tag, "a"), "a").await;
    store_test_release(&store, &b, &release_files(&tag, "b"), "b").await;
    let yaml = format!("name: {env}\n");
    let v = TestVersion::new(&env, &[("interfaces/a/interface.yaml", &yaml)]);
    // a, a again (a restart), b, then a rolled back to.
    let mut ids = Vec::new();
    for (release, rollback) in [(&a, false), (&a, false), (&b, false), (&a, true)] {
        let versions = [v.borrow()];
        let d = ductus_store::NewDeployment {
            environment: &env,
            instance: &env,
            host: "test-host",
            version: "0.7.0-test",
            how: "handover",
            by: "ann",
            reason: Some("deploying"),
            git_commit: None,
            uncommitted: None,
            versions: &versions,
            warnings: &Default::default(),
            docs: None,
            release: Some(release.as_slice()),
            project_version: None,
            rollback,
        };
        let saved = store.save_versions(&versions).await.unwrap();
        ids.push(store.record_deployment(&d, &saved).await.unwrap().id);
    }
    let runs = store.release_runs(&env, "integrations").await.unwrap();
    let seen: Vec<(&[u8], i32, bool)> = runs.iter().map(|r| (r.release.as_slice(), r.deployment, r.rollback)).collect();
    assert_eq!(seen, [(a.as_slice(), ids[0], false), (b.as_slice(), ids[2], false), (a.as_slice(), ids[3], true)]);
    assert!(store.release_runs(&env, "another").await.unwrap().is_empty(), "by project");

    // Where it went: here (each environment's first deployment), and what a file said.
    let here = store.release_places(&a).await.unwrap();
    let mine: Vec<_> = here.iter().filter(|p| p.environment == env).collect();
    assert_eq!(mine.len(), 1, "one per environment: {here:?}");
    assert!(mine[0].here && mine[0].deployment == ids[0] && mine[0].host == "test-host");
    let elsewhere = ductus_store::Place {
        environment: format!("{env}-test"),
        host: "test-box".into(),
        deployment: 41,
        at: chrono::Utc::now() - chrono::Duration::hours(2),
        by: "ci".into(),
        ductus_version: "0.7.0-test".into(),
        here: false,
    };
    // A record that came home (the file went back to the database it was made on): listed once.
    let home = ductus_store::Place { here: false, ..mine[0].clone() };
    assert_eq!(store.add_release_places(&a, &[elsewhere.clone(), home], "test-ci").await.unwrap(), 2);
    assert_eq!(
        store.add_release_places(&a, std::slice::from_ref(&elsewhere), "test-ci").await.unwrap(),
        0,
        "kept once"
    );
    let places: Vec<_> =
        store.release_places(&a).await.unwrap().into_iter().filter(|p| p.environment.starts_with(&env)).collect();
    assert_eq!(places.len(), 2, "{places:?}");
    assert_eq!(
        (places[0].environment.as_str(), places[0].here),
        (elsewhere.environment.as_str(), false),
        "oldest first"
    );
    assert!(places[1].here);

    forget_deployments(&pool, &env).await;
    forget_release(&pool, &a).await;
    forget_release(&pool, &b).await;
}

fn ductus_store_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `deploy` is for operators and above: refused on a viewer when given, and worth nothing to a
/// viewer however it got there.
#[test]
fn deploy_is_for_operators_and_admins() {
    let deploy = vec!["deploy".to_string()];
    let refused = ductus_store::allowed_for(Role::Viewer, &deploy).unwrap_err().to_string();
    assert!(refused.contains("operators and admins, not viewers"), "{refused}");
    ductus_store::allowed_for(Role::Operator, &deploy).unwrap();
    ductus_store::allowed_for(Role::Viewer, &["payloads".to_string()]).unwrap();
    let who = |role| ductus_store::Principal {
        user_id: None,
        token_id: Some(1),
        name: "t".into(),
        role,
        permissions: deploy.clone(),
        must_change_password: false,
    };
    assert!(!who(Role::Viewer).can("deploy"));
    assert!(who(Role::Operator).can("deploy"));
    assert!(ductus_store::Principal { permissions: vec![], ..who(Role::Admin) }.can("deploy"));
    assert!(!ductus_store::Principal { permissions: vec![], ..who(Role::Operator) }.can("deploy"));
}
