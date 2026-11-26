//! The in-process handover (0.7.4.4.2), run inside this test's process as the engine runs it in
//! the Windows service, a container or a plain `ductus run`: a reload starts a new engine beside
//! the running one, which takes over; a reload of a project that can't start leaves the running
//! one as it was; and an engine that ended leaves nothing behind (tasks, threads), however many
//! handovers ran. Needs `DUCTUS_TEST_DATABASE_URL`.

use ductus_runtime::{EngineOptions, handover};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// One test's engines at a time: a reload is asked of the process, and an engine running for
/// another test beside the handover's made the handover test fail.
static ONE_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// The engine's settings, made again for each engine as the CLI does (`rebuild`).
fn options(stop: &tokio_util::sync::CancellationToken, root: &Path, db: &str) -> EngineOptions {
    let (again_stop, again_root, again_db) = (stop.clone(), root.to_path_buf(), db.to_string());
    EngineOptions {
        pool_threads: 1,
        db_connections: 4,
        api_listen: Some("off".into()),
        stop_request: Some(stop.clone()),
        control: true,
        rebuild: Some(Arc::new(move || {
            Ok(handover::Restart {
                root: again_root.clone(),
                database_url: again_db.clone(),
                options: options(&again_stop, &again_root, &again_db),
            })
        })),
        ..Default::default()
    }
}

/// The instance running `name` now, once one does.
async fn running(store: &ductus_store::Store, name: &str, not: Option<&str>) -> String {
    for _ in 0..300 {
        let engines = store.live_engines().await.unwrap();
        if let Some(e) =
            engines.iter().find(|e| e.interfaces.iter().any(|i| i == name) && Some(e.instance.as_str()) != not)
        {
            return e.instance.clone();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("no engine (other than {not:?}) runs {name}");
}

/// This process's threads (Linux; elsewhere not counted).
fn threads() -> usize {
    std::fs::read_dir("/proc/self/task").map(|d| d.count()).unwrap_or(0)
}

/// Ask for a handover until one is taken (the new engine takes requests once it runs). The full
/// handover, whatever changed (since 0.7.5.3 a plain reload applies the project in place).
async fn reload() {
    for _ in 0..100 {
        if handover::handover("the in-process test") {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("no reload was taken");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_in_process_handover_takes_over_and_leaves_nothing_behind() {
    let Ok(db) = std::env::var("DUCTUS_TEST_DATABASE_URL") else { return };
    let _alone = ONE_AT_A_TIME.lock().await;
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let name = format!("in-process-{nanos}");
    let port = free_port();
    let dir = tempfile::tempdir().unwrap();
    let root: PathBuf = dir.path().to_path_buf();
    let yaml = root.join(format!("interfaces/{name}/interface.yaml"));
    std::fs::create_dir_all(yaml.parent().unwrap()).unwrap();
    std::fs::create_dir_all(root.join("environments")).unwrap();
    std::fs::write(root.join("ductus.yaml"), "{}\n").unwrap();
    std::fs::write(root.join("environments/dev.yaml"), "variables: {}\n").unwrap();
    let config = format!(
        "name: {name}\nsource: {{ mllp: {{ host: 127.0.0.1, port: {port} }} }}\ndestinations: {{ out: {{ file: {{ dir: out }} }} }}\n"
    );
    std::fs::write(&yaml, &config).unwrap();
    let store = ductus_store::Store::connect(&db, 2).await.unwrap();
    let before = tokio::runtime::Handle::current().metrics().num_alive_tasks();

    let stop = tokio_util::sync::CancellationToken::new();
    let engine = tokio::spawn({
        let (root, db, options) = (root.clone(), db.clone(), options(&stop, &root, &db));
        async move { ductus_runtime::run(&root, &db, options).await }
    });
    let mut instance = running(&store, &name, None).await;

    // Each reload: a new engine in this process holds the interface; what the old one started
    // ends with it. Counted once things settle (the first reload's numbers are the baseline: the
    // first start may have made things that stay for the process, as the SDK's transpiled code).
    let mut counts = Vec::new();
    for _ in 0..3 {
        reload().await;
        instance = running(&store, &name, Some(&instance)).await;
        tokio::time::sleep(Duration::from_secs(3)).await;
        counts.push((tokio::runtime::Handle::current().metrics().num_alive_tasks(), threads()));
        // Sends to it still work: the socket the first engine opened is the one this one listens on.
        let mut sender = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.expect("the port listens");
        let message = b"\x0bMSH|^~\\&|A|B|C|D|20260101||ADT^A01|1|P|2.5\rPID|1||42\r\x1c\r";
        tokio::io::AsyncWriteExt::write_all(&mut sender, message).await.unwrap();
        let mut ack = vec![0u8; 512];
        let n = tokio::time::timeout(Duration::from_secs(10), tokio::io::AsyncReadExt::read(&mut sender, &mut ack))
            .await
            .expect("an ACK")
            .unwrap();
        assert!(String::from_utf8_lossy(&ack[..n]).contains("MSA|AA"), "{}", String::from_utf8_lossy(&ack[..n]));
    }
    let (first, last) = (counts[0], counts[2]);
    assert!(last.0 <= first.0 + 2, "tasks grow with each handover: {counts:?}");
    assert!(last.1 <= first.1 + 1, "threads grow with each handover: {counts:?}");

    // A project that can't start: the running engine goes on, and the reload's new engine is gone.
    std::fs::write(&yaml, format!("{config}bogus: true\n")).unwrap();
    reload().await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(running(&store, &name, None).await, instance, "the running engine goes on");
    let failed = tokio::runtime::Handle::current().metrics().num_alive_tasks();
    assert!(failed <= first.0 + 2, "the failed engine left tasks behind: {failed} vs {counts:?}");
    std::fs::write(&yaml, &config).unwrap();

    // The service manager's stop reaches the engine running now; the run ends with it.
    stop.cancel();
    let ended = tokio::time::timeout(Duration::from_secs(60), engine).await.expect("the run ends").unwrap();
    assert!(ended.is_ok(), "{ended:?}");
    tokio::time::sleep(Duration::from_secs(2)).await;
    let after = tokio::runtime::Handle::current().metrics().num_alive_tasks();
    assert!(after <= before + 2, "tasks left once every engine ended: {after} (before: {before})");
}

/// An engine with a respond-mode interface stops at once, also with a caller's kept-alive
/// connection open (which it closes). Until 0.8.1.2 its HTTP servers were kept to the end of
/// the stop, so the responder they feed never ended and every stop took its whole time limit
/// (30 s), saying messages in hand were left pending.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_engine_answering_http_requests_stops_at_once() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let Ok(db) = std::env::var("DUCTUS_TEST_DATABASE_URL") else { return };
    let _alone = ONE_AT_A_TIME.lock().await;
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let name = format!("in-process-respond-{nanos}");
    let port = free_port();
    let dir = tempfile::tempdir().unwrap();
    let root: PathBuf = dir.path().to_path_buf();
    let folder = root.join(format!("interfaces/{name}"));
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::create_dir_all(root.join("environments")).unwrap();
    std::fs::write(root.join("ductus.yaml"), "{}\n").unwrap();
    std::fs::write(root.join("environments/dev.yaml"), "variables: {}\n").unwrap();
    std::fs::write(
        folder.join("interface.yaml"),
        format!(
            "name: {name}\nsource: {{ http: {{ host: 127.0.0.1, port: {port}, path: \"/p/{{id}}\", method: GET, mode: respond }} }}\nformat: json\nscript: transform.ts\n"
        ),
    )
    .unwrap();
    std::fs::write(
        folder.join("transform.ts"),
        "export function transform(req: { params: { id: string } }) {\n  return { id: req.params.id };\n}\n",
    )
    .unwrap();

    let stop = tokio_util::sync::CancellationToken::new();
    let (started, up) = tokio::sync::oneshot::channel();
    let options = EngineOptions {
        stop_timeout: Duration::from_secs(30),
        on_started: Some(Box::new(move || {
            let _ = started.send(());
        })),
        ..options(&stop, &root, &db)
    };
    let engine = tokio::spawn({
        let (root, db) = (root.clone(), db.clone());
        async move { ductus_runtime::run(&root, &db, options).await }
    });
    tokio::time::timeout(Duration::from_secs(60), up).await.expect("the engine starts").unwrap();

    // A caller that keeps its connection after its answer, as callers are told to.
    let mut caller = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    caller.write_all(b"GET /p/42 HTTP/1.1\r\nHost: engine\r\n\r\n").await.unwrap();
    let mut answer = vec![0u8; 1024];
    let n = tokio::time::timeout(Duration::from_secs(10), caller.read(&mut answer)).await.expect("an answer").unwrap();
    let answer = String::from_utf8_lossy(&answer[..n]).to_string();
    assert!(answer.starts_with("HTTP/1.1 200") && answer.contains(r#"{"id":"42"}"#), "{answer}");

    let asked = std::time::Instant::now();
    stop.cancel();
    let ended = tokio::time::timeout(Duration::from_secs(60), engine).await.expect("the run ends").unwrap();
    let took = asked.elapsed();
    assert!(ended.is_ok(), "{ended:?}");
    assert!(took < Duration::from_secs(5), "the stop took {took:?} (its limit: 30 s)");
    let closed = tokio::time::timeout(Duration::from_secs(5), caller.read(&mut [0u8; 64])).await;
    assert!(matches!(closed, Ok(Ok(0)) | Ok(Err(_))), "the kept-alive connection was closed: {closed:?}");
}
