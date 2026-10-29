//! The handover (Linux): `systemctl reload ductus` replaces the running engine with the binary
//! now on disk and the project as it is now, without refusing a connection. An upgrade and a
//! project deploy are the same thing to it.
//!
//! `ductus handover` (the unit's `ExecReload`) asks the running engine over its control socket
//! (`Control`, `/run/ductus/control`). The engine starts the binary at the path it was started
//! from, with the same arguments, its listening sockets inherited (`systemd::handover_sockets`)
//! and a socket pair to talk over (`DUCTUS_TAKEOVER`). The new engine starts as any start does
//! (the project, the database and its migrations, a lock of its own, every interface's code,
//! intake on the inherited sockets) but claims nothing: from then on both engines store what
//! arrives (any number may), and only the old one sends. The new one says it's ready; the old
//! one stops as a clean stop does (each MLLP connection closed after its last ACK, the messages
//! in hand finished and saved), lets go of its interfaces, tells systemd the new process is the
//! service's now (`MAINPID`), says so and exits. The new one claims, reconciles, applies
//! retention and sends. Receiving never stops; sending pauses from the old engine's stop until
//! the new one holds the interfaces.
//!
//! Anything that keeps the new engine from getting ready (a broken project, a failed migration,
//! a start it would refuse, the time limit) leaves the old one running as it was: the new one
//! exits, and `ductus handover` exits 1 saying why.

use crate::control::Engine;
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long the new engine has, once the old one let go, to say it sends (claims, reconcile,
/// retention: well under a second at a thousand interfaces).
#[cfg(target_os = "linux")]
const RUNNING_WITHIN: Duration = Duration::from_secs(60);

/// Where the engine listens for `ductus handover`: `DUCTUS_CONTROL`, else
/// `$RUNTIME_DIRECTORY/control` (systemd sets it for the package's unit: `/run/ductus`). None:
/// no control socket, so no handover (`ductus dev`, containers, a unit of one's own without
/// `RuntimeDirectory=`).
pub fn control_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("DUCTUS_CONTROL").filter(|p| !p.is_empty()) {
        return Some(path.into());
    }
    let dir = std::env::var("RUNTIME_DIRECTORY").ok().filter(|d| !d.is_empty())?;
    // systemd separates several directories with `:` when a unit names several.
    Some(Path::new(dir.split(':').next().unwrap_or_default()).join("control"))
}

/// `ductus handover` → the running engine (one JSON line).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    /// How long the new engine may take to be ready (it migrates the database first).
    pub timeout_secs: u64,
    /// Start the new engine with this environment (`--with-environment`: systemd's, with
    /// ductus.env read again) instead of the running engine's.
    pub environment: Option<Vec<(String, String)>>,
}

/// The running engine → `ductus handover`: what it does as it goes, then how it ended.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Answer {
    Progress(String),
    Done { ok: bool, text: String },
}

/// The engine a takeover takes over from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Predecessor {
    pub pid: u32,
    pub version: String,
    pub instance: String,
}

/// How this engine took over (`GET /engine`, the audit log's `engine.handover`).
#[derive(Debug, Clone)]
pub struct HandedOver {
    pub from: Predecessor,
    pub at: DateTime<Utc>,
    /// From the old engine's stop until this one held the interfaces.
    pub sending_paused: Duration,
}

#[cfg(target_os = "linux")]
/// The new engine → the old one (JSON lines over the socket pair).
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ToOld {
    /// Loaded, intake open on the inherited sockets: the old engine may let go.
    Ready { pid: u32, version: String, took_ms: u64 },
    /// It can't take over, and exits: why.
    Failed(String),
    /// It holds the interfaces and sends.
    Running { sending_paused_ms: u64 },
}

#[cfg(target_os = "linux")]
/// The old engine → the new one.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ToNew {
    /// Its first word: who is being taken over from.
    From(Predecessor),
    /// It stopped (`stop_ms`: how long since its workers began to stop) and let go of its interfaces.
    Released { stop_ms: u64 },
}

/// "180 ms", "2.4 s".
#[cfg(target_os = "linux")]
fn took(d: Duration) -> String {
    if d < Duration::from_secs(1) { format!("{} ms", d.as_millis()) } else { format!("{:.1} s", d.as_secs_f64()) }
}

/// The path to start the new engine from: the one this engine was started as, remembered at its
/// start. Not `/proc/self/exe`: after a package upgrade that's the old, deleted file, and through
/// a symbolic link (`/opt/ductus/current/ductus`) it names the version the link pointed to then.
static EXECUTABLE: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();

/// Remember where this binary was started from (call once, at start).
pub fn remember_executable() {
    EXECUTABLE.get_or_init(|| {
        let argv0 = PathBuf::from(std::env::args_os().next()?);
        let path = if argv0.components().count() > 1 {
            std::env::current_dir().ok()?.join(argv0)
        } else {
            // Found through PATH, as a shell found it.
            std::env::split_paths(&std::env::var_os("PATH")?).map(|dir| dir.join(&argv0)).find(|p| p.is_file())?
        };
        Some(path)
    });
}

#[cfg(target_os = "linux")]
fn executable() -> Result<PathBuf> {
    use anyhow::Context;
    match EXECUTABLE.get().cloned().flatten() {
        Some(path) => Ok(path),
        None => std::env::current_exe().context("finding this binary's path"),
    }
}

/// The engine's control socket: where `ductus handover` asks for a handover.
pub struct Control {
    #[cfg(target_os = "linux")]
    requests: Option<tokio::sync::mpsc::Receiver<linux::Requested>>,
    /// A handover is under way: a request meanwhile is told so.
    #[cfg(target_os = "linux")]
    busy: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Control {
    /// Listen at `control_path()` when `enabled` (no socket and no handover when there's none, or
    /// outside Linux). One that can't be opened is a warning: the engine runs without.
    pub fn listen(enabled: bool) -> Self {
        #[cfg(target_os = "linux")]
        {
            let busy = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let requests = control_path().filter(|_| enabled).and_then(|path| match linux::listen(&path, busy.clone()) {
                Ok(requests) => Some(requests),
                Err(e) => {
                    tracing::warn!(error = %format!("{e:#}"), "no handover possible (`systemctl reload` fails; restart instead)");
                    None
                }
            });
            Self { requests, busy }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = enabled;
            Self {}
        }
    }

    /// Resolves when a new engine is ready to take over: this one must then stop, let go of its
    /// interfaces and call `Successor::released`. Requests that fail are answered here (the
    /// engine goes on), and the next one is waited for.
    pub async fn next(&mut self, engine: &Engine) -> Successor {
        #[cfg(target_os = "linux")]
        if let Some(requests) = self.requests.as_mut() {
            use std::sync::atomic::Ordering::SeqCst;
            while let Some(requested) = requests.recv().await {
                self.busy.store(true, SeqCst);
                if let Some(successor) = linux::start(requested, engine).await {
                    return successor; // busy for good: this engine stops
                }
                self.busy.store(false, SeqCst);
            }
        }
        let _ = engine;
        std::future::pending().await
    }
}

/// The new engine, ready: what the old one tells once it let go.
#[cfg(target_os = "linux")]
pub use linux::Successor;

/// There's no handover outside Linux.
#[cfg(not(target_os = "linux"))]
pub enum Successor {}

#[cfg(not(target_os = "linux"))]
impl Successor {
    pub fn pid(&self) -> u32 {
        match *self {}
    }

    pub async fn released(self, _stop_took: Duration) {
        match self {}
    }
}

/// The new engine's side: the socket pair the old one passed (`DUCTUS_TAKEOVER`).
#[cfg(target_os = "linux")]
pub use linux::Takeover;

/// There's no handover outside Linux.
#[cfg(not(target_os = "linux"))]
pub enum Takeover {}

#[cfg(not(target_os = "linux"))]
impl Takeover {
    pub fn new(_fd: i32) -> Result<std::sync::Arc<Self>> {
        anyhow::bail!("the handover is for Linux")
    }
    pub async fn predecessor(&self) -> Result<Predecessor> {
        match *self {}
    }
    pub async fn ready(&self, _took: Duration) -> Result<()> {
        match *self {}
    }
    pub async fn released(&self) -> Option<Duration> {
        match *self {}
    }
    pub async fn running(&self, _sending_paused: Duration) {
        match *self {}
    }
    pub async fn failed(&self, _error: &str) {
        match *self {}
    }
}

#[cfg(not(target_os = "linux"))]
impl Successor {
    pub fn version(&self) -> &str {
        match *self {}
    }
}

/// `ductus handover`: ask the engine listening at `path`, passing each line it says as it goes to
/// `say`. Ok((true, text)) once the new engine runs; (false, why) when it couldn't take over (the
/// running engine goes on).
pub async fn ask(path: &Path, request: &Request, say: impl FnMut(&str)) -> Result<(bool, String)> {
    #[cfg(target_os = "linux")]
    return linux::ask(path, request, say).await;
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (path, request, say);
        anyhow::bail!(
            "the handover is for Linux under systemd; on Windows `Restart-Service Ductus` restarts the engine"
        )
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use anyhow::{Context, bail};
    use serde::de::DeserializeOwned;
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Instant;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
    use tokio::net::UnixStream;
    use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
    use tokio::sync::mpsc;

    /// One JSON value per line, both ways.
    struct Lined {
        read: Lines<BufReader<OwnedReadHalf>>,
        write: OwnedWriteHalf,
    }

    impl Lined {
        fn new(stream: UnixStream) -> Self {
            let (read, write) = stream.into_split();
            Self { read: BufReader::new(read).lines(), write }
        }

        async fn send<T: Serialize>(&mut self, value: &T) -> io::Result<()> {
            let mut line = serde_json::to_vec(value).map_err(io::Error::other)?;
            line.push(b'\n');
            self.write.write_all(&line).await
        }

        /// The next value; None once the other side closed.
        async fn receive<T: DeserializeOwned>(&mut self) -> io::Result<Option<T>> {
            match self.read.next_line().await? {
                None => Ok(None),
                Some(line) => serde_json::from_str(&line).map(Some).map_err(io::Error::other),
            }
        }
    }

    /// A request for a handover, with the connection to answer it on.
    pub(super) struct Requested {
        request: Request,
        asker: Option<Lined>,
    }

    impl Requested {
        async fn say(&mut self, text: impl Into<String>) {
            let text = text.into();
            tracing::info!("handover: {text}");
            if let Some(asker) = self.asker.as_mut() {
                // The asker may have gone (Ctrl-C): the handover goes on all the same.
                let _ = asker.send(&Answer::Progress(text)).await;
            }
        }

        async fn done(mut self, ok: bool, text: String) {
            if ok {
                tracing::info!("handover: {text}");
            } else {
                tracing::error!("handover: {text}");
            }
            if let Some(mut asker) = self.asker.take() {
                let _ = asker.send(&Answer::Done { ok, text }).await;
            }
        }
    }

    impl Drop for Requested {
        /// Not answered: this engine stopped while the new one got ready (a SIGTERM, a drain
        /// that stops), and the new one was stopped with it (`Spawned`).
        fn drop(&mut self) {
            if let Some(asker) = self.asker.take() {
                let text = "this engine stopped before the handover finished (it was asked to stop): a new engine it had started was stopped too";
                let mut line = serde_json::to_vec(&Answer::Done { ok: false, text: text.into() }).unwrap_or_default();
                line.push(b'\n');
                let _ = asker.write.try_write(&line);
            }
        }
    }

    /// Listen at `path` (a stale socket left by an engine that's gone is replaced). Only this
    /// engine's own user (or root) may ask: the socket is 0600, and each asker's user is checked.
    pub(super) fn listen(path: &Path, busy: Arc<AtomicBool>) -> Result<mpsc::Receiver<Requested>> {
        let _ = std::fs::remove_file(path);
        let listener = tokio::net::UnixListener::bind(path)
            .with_context(|| format!("listening for `ductus handover` at {}", path.display()))?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("making {} private", path.display()))?;
        // One at a time: a request while another is under way is told so.
        let (tx, rx) = mpsc::channel(1);
        // SAFETY: geteuid has no preconditions.
        let me = unsafe { libc::geteuid() };
        tokio::spawn(async move {
            loop {
                let stream = match listener.accept().await {
                    Ok((stream, _)) => stream,
                    Err(e) => {
                        tracing::warn!(error = %e, "the control socket couldn't take a connection");
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        continue;
                    }
                };
                if !stream.peer_cred().is_ok_and(|c| c.uid() == me || c.uid() == 0) {
                    continue; // dropped: closed without a word
                }
                let (tx, busy) = (tx.clone(), busy.clone());
                tokio::spawn(async move {
                    let mut asker = Lined::new(stream);
                    let request = match tokio::time::timeout(Duration::from_secs(5), asker.receive::<Request>()).await {
                        Ok(Ok(Some(request))) => request,
                        _ => return,
                    };
                    let requested = Requested { request, asker: Some(asker) };
                    if busy.load(Ordering::SeqCst) {
                        return requested.done(false, "another handover is under way".into()).await;
                    }
                    if let Err(mpsc::error::TrySendError::Full(r) | mpsc::error::TrySendError::Closed(r)) =
                        tx.try_send(requested)
                    {
                        r.done(false, "another handover is under way".into()).await;
                    }
                });
            }
        });
        Ok(rx)
    }

    /// The new engine's process until it's ready: stopped if it's dropped before (this engine
    /// stops meanwhile, or the handover failed).
    struct Unready(Option<tokio::process::Child>);

    impl Drop for Unready {
        fn drop(&mut self) {
            if let Some(child) = self.0.as_mut() {
                let _ = child.start_kill();
            }
        }
    }

    struct Spawned {
        child: Unready,
        pid: u32,
        path: PathBuf,
        pair: Lined,
    }

    impl Spawned {
        /// After a failure: wait a little for it to exit (it says why first), else kill it.
        /// How it ended, in words.
        async fn end(&mut self) -> String {
            let Some(mut child) = self.child.0.take() else { return "gone".into() };
            let status = match tokio::time::timeout(Duration::from_secs(10), child.wait()).await {
                Ok(status) => status,
                Err(_) => {
                    let _ = child.start_kill();
                    child.wait().await
                }
            };
            match status {
                Ok(s) => match (s.code(), std::os::unix::process::ExitStatusExt::signal(&s)) {
                    (Some(code), _) => format!("exit code {code}"),
                    (None, Some(signal)) => format!("signal {signal}"),
                    _ => s.to_string(),
                },
                Err(e) => e.to_string(),
            }
        }
    }

    /// Start the binary now on disk with this engine's arguments, its listening sockets and a
    /// socket pair, and tell it who it takes over from.
    async fn spawn(request: &Request, me: &Predecessor) -> Result<Spawned> {
        let path = executable()?;
        let (ours, theirs) = std::os::unix::net::UnixStream::pair().context("making the socket pair")?;
        let sockets = crate::systemd::handover_sockets().context("copying the listening sockets")?;
        let listen: Vec<String> = sockets.iter().map(|(name, fd)| format!("{name}={}", fd.as_raw_fd())).collect();
        let mut command = tokio::process::Command::new(&path);
        command.args(std::env::args_os().skip(1)).stdin(std::process::Stdio::null());
        if let Some(environment) = &request.environment {
            command.env_clear().envs(environment.iter().cloned());
        }
        // systemd's: these were for this process (MAINPID, the sockets it passed), and its notify
        // socket is the one the service was given.
        for name in ["MAINPID", "LISTEN_PID", "LISTEN_FDS", "LISTEN_FDNAMES", "NOTIFY_SOCKET"] {
            command.env_remove(name);
        }
        if let Some(notify) = std::env::var_os("NOTIFY_SOCKET") {
            command.env("NOTIFY_SOCKET", notify);
        }
        command.env("DUCTUS_TAKEOVER", theirs.as_raw_fd().to_string()).env("DUCTUS_LISTEN_FDS", listen.join(" "));
        let inherited: Vec<RawFd> = sockets.iter().map(|(_, fd)| fd.as_raw_fd()).chain([theirs.as_raw_fd()]).collect();
        // SAFETY: runs in the child between fork and exec: fcntl is async-signal-safe, and the
        // vector was made before the fork (iterating it allocates nothing).
        unsafe {
            command.pre_exec(move || {
                for fd in &inherited {
                    if libc::fcntl(*fd, libc::F_SETFD, 0) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        let child = command.spawn().with_context(|| format!("starting {}", path.display()))?;
        drop((sockets, theirs)); // the child has its own copies now
        let pid = child.id().unwrap_or_default();
        ours.set_nonblocking(true)?;
        let mut spawned =
            Spawned { child: Unready(Some(child)), pid, path, pair: Lined::new(UnixStream::from_std(ours)?) };
        spawned.pair.send(&ToNew::From(me.clone())).await.context("telling the new engine who it takes over from")?;
        Ok(spawned)
    }

    /// The new engine, ready: this engine stops now.
    pub struct Successor {
        pid: u32,
        version: String,
        pair: Lined,
        requested: Requested,
    }

    impl Successor {
        pub fn pid(&self) -> u32 {
            self.pid
        }

        pub fn version(&self) -> &str {
            &self.version
        }

        /// After this engine stopped and let go of its interfaces (`stop_took`: since its
        /// workers began to stop): make the new process the service's, tell it, and wait until it
        /// sends; then answer `ductus handover`.
        pub async fn released(mut self, stop_took: Duration) {
            crate::systemd::main_pid(self.pid);
            let released = ToNew::Released { stop_ms: stop_took.as_millis() as u64 };
            if let Err(e) = self.pair.send(&released).await {
                let text = format!("the new engine (pid {}) went away before this one let go: {e}", self.pid);
                return self.requested.done(false, text).await;
            }
            self.requested
                .say(format!(
                    "this engine stopped in {} and let go of its interfaces: the new one takes them",
                    took(stop_took)
                ))
                .await;
            let (pid, version) = (self.pid, self.version.clone());
            let text = match tokio::time::timeout(RUNNING_WITHIN, self.pair.receive::<ToOld>()).await {
                Ok(Ok(Some(ToOld::Running { sending_paused_ms }))) => {
                    let paused = took(Duration::from_millis(sending_paused_ms));
                    let text = format!(
                        "handed over to ductus {version} (pid {pid}): receiving never stopped, sending paused {paused}"
                    );
                    return self.requested.done(true, text).await;
                }
                Ok(Ok(Some(ToOld::Failed(error)))) => format!(
                    "the new engine (pid {pid}) couldn't take the interfaces after this one let go:\n{error}\nsystemd restarts it if it exits with an error (`journalctl -u ductus`)"
                ),
                Ok(_) => format!(
                    "the new engine (pid {pid}) went away after this one let go: systemd restarts it (`journalctl -u ductus`)"
                ),
                Err(_) => format!(
                    "the new engine (pid {pid}) didn't say it runs within {RUNNING_WITHIN:?} of this one letting go: see `journalctl -u ductus`"
                ),
            };
            self.requested.done(false, text).await
        }
    }

    /// Run a handover request up to the point of no return: the new engine is ready (Some), or
    /// it failed and this engine goes on (None, the asker told why).
    pub(super) async fn start(mut requested: Requested, engine: &Engine) -> Option<Successor> {
        let here = format!("This engine (pid {}, ductus {}) goes on.", std::process::id(), env!("CARGO_PKG_VERSION"));
        if !matches!(engine.drain_state(), crate::control::DrainState::Accepting) {
            let text = format!(
                "the engine is draining: a new engine would take messages again. Resume it (`ductus resume`) or stop it first. {here}"
            );
            requested.done(false, text).await;
            return None;
        }
        let me = Predecessor {
            pid: std::process::id(),
            version: env!("CARGO_PKG_VERSION").into(),
            instance: engine.instance.clone(),
        };
        let started = Instant::now();
        let mut spawned = match spawn(&requested.request, &me).await {
            Ok(s) => s,
            Err(e) => {
                requested.done(false, format!("couldn't start the new engine: {e:#}. {here}")).await;
                return None;
            }
        };
        requested
            .say(format!(
                "started {} (pid {}): it reads the project, migrates the database and loads every interface's code while this one runs",
                spawned.path.display(),
                spawned.pid
            ))
            .await;
        let limit = Duration::from_secs(requested.request.timeout_secs.max(1));
        let said = tokio::time::timeout(limit, spawned.pair.receive::<ToOld>()).await;
        let failure = match said {
            Ok(Ok(Some(ToOld::Ready { pid, version, .. }))) => {
                requested
                    .say(format!(
                        "ductus {version} (pid {pid}) is ready after {} and receives beside this one: stopping this one",
                        took(started.elapsed())
                    ))
                    .await;
                // From here it's the service's process to be: never stopped with this one.
                drop(spawned.child.0.take());
                return Some(Successor { pid, version, pair: spawned.pair, requested });
            }
            Ok(Ok(Some(ToOld::Failed(error)))) => {
                spawned.end().await;
                format!("the new engine can't take over:\n{error}")
            }
            Ok(Ok(Some(ToOld::Running { .. }))) | Ok(Err(_)) | Ok(Ok(None)) => {
                let how = spawned.end().await;
                format!("the new engine exited ({how}) before it was ready: its log (`journalctl -u ductus`) says why")
            }
            Err(_) => {
                drop(spawned);
                format!(
                    "the new engine wasn't ready within {limit:?} and was stopped (a long migration needs `ductus handover --timeout`)"
                )
            }
        };
        requested.done(false, format!("{failure}\n{here}")).await;
        None
    }

    /// The new engine's side of the conversation.
    pub struct Takeover {
        pair: tokio::sync::Mutex<Lined>,
        from: tokio::sync::OnceCell<Predecessor>,
        /// It said it runs: a later failure is its own (systemd's to restart), not the takeover's.
        running: AtomicBool,
    }

    impl Takeover {
        /// From the descriptor in `DUCTUS_TAKEOVER`.
        pub fn new(fd: RawFd) -> Result<Arc<Self>> {
            // SAFETY: the old engine passed this descriptor (one end of its socket pair) to this
            // process and named it in DUCTUS_TAKEOVER; nothing else here knows it.
            let fd = unsafe { OwnedFd::from_raw_fd(fd) };
            let stream = std::os::unix::net::UnixStream::from(fd);
            stream.set_nonblocking(true).context("the takeover's socket pair")?;
            // SAFETY: fcntl on a descriptor this process owns.
            let flags = unsafe { libc::fcntl(stream.as_raw_fd(), libc::F_GETFD) };
            if flags < 0 {
                bail!(
                    "DUCTUS_TAKEOVER doesn't name an open descriptor: `run` takes it only from a running engine's handover"
                );
            }
            // SAFETY: as above; not passed on to any later child.
            unsafe { libc::fcntl(stream.as_raw_fd(), libc::F_SETFD, flags | libc::FD_CLOEXEC) };
            Ok(Arc::new(Self {
                pair: tokio::sync::Mutex::new(Lined::new(UnixStream::from_std(stream)?)),
                from: tokio::sync::OnceCell::new(),
                running: AtomicBool::new(false),
            }))
        }

        /// Who is being taken over from (the old engine's first word).
        pub async fn predecessor(&self) -> Result<Predecessor> {
            self.from
                .get_or_try_init(|| async {
                    match self.pair.lock().await.receive::<ToNew>().await? {
                        Some(ToNew::From(p)) => Ok(p),
                        _ => bail!("the engine to take over from didn't say who it is"),
                    }
                })
                .await
                .cloned()
        }

        /// Ready: the old engine stops now.
        pub async fn ready(&self, took: Duration) -> Result<()> {
            let ready = ToOld::Ready {
                pid: std::process::id(),
                version: env!("CARGO_PKG_VERSION").into(),
                took_ms: took.as_millis() as u64,
            };
            self.pair.lock().await.send(&ready).await.context("telling the engine to take over from")
        }

        /// Wait until the old engine let go: how long its stop took (None: it went away without
        /// a word; its claims went with its lock).
        pub async fn released(&self) -> Option<Duration> {
            match self.pair.lock().await.receive::<ToNew>().await {
                Ok(Some(ToNew::Released { stop_ms })) => Some(Duration::from_millis(stop_ms)),
                _ => None,
            }
        }

        /// It holds the interfaces and sends.
        pub async fn running(&self, sending_paused: Duration) {
            self.running.store(true, Ordering::SeqCst);
            let running = ToOld::Running { sending_paused_ms: sending_paused.as_millis() as u64 };
            let _ = self.pair.lock().await.send(&running).await;
        }

        /// The takeover failed (this engine exits): why, for the old engine to report. Nothing
        /// once it said it runs.
        pub async fn failed(&self, error: &str) {
            if !self.running.load(Ordering::SeqCst) {
                let _ = self.pair.lock().await.send(&ToOld::Failed(error.to_string())).await;
            }
        }
    }

    pub(super) async fn ask(path: &Path, request: &Request, mut say: impl FnMut(&str)) -> Result<(bool, String)> {
        let stream = UnixStream::connect(path).await.map_err(|e| match e.kind() {
            io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => anyhow::anyhow!(
                "no engine answers at {} ({e}): is it running? (Engines before 0.6.6.3 have no control socket: restart those.)",
                path.display()
            ),
            io::ErrorKind::PermissionDenied => {
                anyhow::anyhow!("{}: {e}: run it as the engine's user (ductus) or root", path.display())
            }
            _ => anyhow::anyhow!("connecting to {}: {e}", path.display()),
        })?;
        let mut engine = Lined::new(stream);
        engine.send(request).await.context("asking the engine")?;
        loop {
            match engine.receive::<Answer>().await.context("reading the engine's answer")? {
                Some(Answer::Progress(text)) => say(&text),
                Some(Answer::Done { ok, text }) => return Ok((ok, text)),
                None => {
                    bail!("the engine closed the connection without an answer: see its log (`journalctl -u ductus`)")
                }
            }
        }
    }
}
