//! The handover: the running engine is replaced by a new one, without refusing a connection. An
//! upgrade, a project deploy and a reload are the same thing to it.
//!
//! A handover is asked for by `ductus handover` over the engine's control socket (Linux:
//! `Control`, `/run/ductus/control`; the package's `systemctl reload ductus`), by a deploy
//! (`POST /deployments`, from inside the engine: `Asker`), or by the system: `SIGHUP` (Linux,
//! containers) and the Windows service's `paramchange` (`reload`).
//!
//! **Two ways to start the new engine, one conversation.**
//! - **A new process** (Linux with a control socket, under the package's unit): the engine starts
//!   the binary at the path it was started from, with the same arguments, its listening sockets
//!   inherited (`systemd::handover_sockets`) and a socket pair to talk over (`DUCTUS_TAKEOVER`).
//!   This one picks up a new binary (a package upgrade) and, with `--with-environment`, ductus.env
//!   as it is now.
//! - **In this process** (Windows, containers, a plain `ductus run` without a control socket, and
//!   always as PID 1, whose exit would end the container): the new engine runs as a task beside
//!   this one, listening on copies of the same sockets (`systemd`), with its settings read again
//!   (`Rebuild`: the CLI's, which reads `--env-file` again) and a channel to talk over. Same
//!   binary: a new one comes with the installer or a new image.
//!
//! Either way the new engine starts as any start does (the project, the database and its
//! migrations, a lock of its own, every interface's code, intake on the shared sockets) but claims
//! nothing: from then on both engines store what arrives (any number may), and only the old one
//! sends. The new one says it's ready; the old one stops as a clean stop does (each MLLP
//! connection closed after its last ACK, the messages in hand finished and saved), lets go of its
//! interfaces, says so (a new process: tells systemd it's the service's now, `MAINPID`), and ends.
//! The new one claims, reconciles, applies retention and sends. Receiving never stops; sending
//! pauses from the old engine's stop until the new one holds the interfaces.
//!
//! Anything that keeps the new engine from getting ready (a broken project, a failed migration,
//! a start it would refuse, the time limit) leaves the old one running as it was: the new one
//! ends, and whoever asked is told why. A new engine that fails after the old one let go (rare: it
//! was ready) is started again by systemd (a new process), or by `crate::run` (in this process: a
//! plain start of what ran before, on the same sockets).

use crate::control::Engine;
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// How long the new engine has, once the old one let go, to say it sends (claims, reconcile,
/// retention: well under a second at a thousand interfaces).
const RUNNING_WITHIN: Duration = Duration::from_secs(60);

/// How long a reload asked by the system (`reload`) gives the new engine to be ready, as
/// `ductus handover`'s default.
const RELOAD_TIMEOUT: Duration = Duration::from_secs(300);

/// Where the engine listens for `ductus handover`: `DUCTUS_CONTROL`, else
/// `$RUNTIME_DIRECTORY/control` (systemd sets it for the package's unit: `/run/ductus`). None:
/// no control socket, so handovers run in this process (`ductus dev` has none at all).
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
    /// ductus.env read again) instead of the running engine's. A new process only.
    pub environment: Option<Vec<(String, String)>>,
    /// Who asks and why, for the new engine's deployment record (0.7.3; engines before ignore it).
    #[serde(default)]
    pub asked: Asked,
}

/// Who asked for a handover and why (`ductus handover -m`): the old engine passes it to the new
/// one, which records it with its deployment.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Asked {
    pub by: Option<String>,
    pub reason: Option<String>,
    /// A deploy (`POST /deployments`, 0.7.4.4): the release the new engine runs, in hex (release
    /// mode). None: the release that runs now (a plain `ductus handover`). Engines before ignore it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<String>,
    /// That deploy is a rollback (`ductus deploy --rollback`, 0.7.4.4.3): the new engine's
    /// deployment says so. Engines before ignore it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub rollback: bool,
}

impl Asked {
    /// Asked from here: the person behind `sudo` (`SUDO_USER`); `systemd (systemctl reload)` when
    /// systemd runs it (the unit's `ExecReload`, which gets `MAINPID`), since that can't tell who
    /// typed `systemctl reload`; else this account.
    pub fn here(reason: Option<String>) -> Self {
        let set = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        let by = match (set("SUDO_USER"), set("MAINPID")) {
            (Some(person), _) => person,
            (None, Some(_)) => "systemd (systemctl reload)".into(),
            (None, None) => crate::deployment::account(),
        };
        Self { by: Some(by), reason: reason.filter(|r| !r.trim().is_empty()), release: None, rollback: false }
    }
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
    /// Who asked for this handover and why (absent from engines before 0.7.3).
    #[serde(default)]
    pub asked: Asked,
}

/// How this engine took over (`GET /engine`, the audit log's `engine.handover`).
#[derive(Debug, Clone)]
pub struct HandedOver {
    pub from: Predecessor,
    pub at: DateTime<Utc>,
    /// From the old engine's stop until this one held the interfaces.
    pub sending_paused: Duration,
    /// In the same process as the engine taken over from (`from.pid` is this one's).
    pub in_process: bool,
}

/// The new engine → the old one (JSON lines over the socket pair; values over the channel).
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ToOld {
    /// Loaded, intake open on the shared sockets: the old engine may let go. `warnings`: changes
    /// that may undo someone else's (0.7.3.3; absent from engines before), for the asker.
    Ready {
        pid: u32,
        version: String,
        took_ms: u64,
        #[serde(default)]
        warnings: Vec<String>,
    },
    /// It can't take over, and ends: why.
    Failed(String),
    /// It holds the interfaces and sends.
    Running { sending_paused_ms: u64 },
}

/// The old engine → the new one.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ToNew {
    /// Its first word: who is being taken over from.
    From(Predecessor),
    /// It stopped (`stop_ms`: how long since its workers began to stop) and let go of its interfaces.
    Released { stop_ms: u64 },
}

/// The conversation between the two engines: over the socket pair (another process) or a channel
/// (this one).
enum Link<Out, In> {
    #[cfg(target_os = "linux")]
    Socket(linux::Lined),
    Channel(mpsc::UnboundedSender<Out>, mpsc::UnboundedReceiver<In>),
}

impl<Out: Serialize, In: DeserializeOwned> Link<Out, In> {
    async fn send(&mut self, value: Out) -> io::Result<()> {
        match self {
            #[cfg(target_os = "linux")]
            Link::Socket(lined) => lined.send(&value).await,
            Link::Channel(tx, _) => tx.send(value).map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe)),
        }
    }

    /// The next value; None once the other side is gone.
    async fn receive(&mut self) -> io::Result<Option<In>> {
        match self {
            #[cfg(target_os = "linux")]
            Link::Socket(lined) => lined.receive().await,
            Link::Channel(_, rx) => Ok(rx.recv().await),
        }
    }
}

/// "180 ms", "2.4 s".
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

/// This binary's path, as it was started (after an upgrade replaced the file: the new binary,
/// what the next start runs). Also what the API's release check runs (`api::releases`), on every
/// system: on Windows a name found through PATH has no `.exe` to match, so it's `current_exe`.
pub fn executable() -> Result<PathBuf> {
    match EXECUTABLE.get().cloned().flatten() {
        Some(path) => Ok(path),
        None => std::env::current_exe().context("finding this binary's path"),
    }
}

/// What an engine started in this process runs with: its settings read again (the CLI reads
/// `--env-file` again), so a reload picks up what changed there.
pub type Rebuild = Arc<dyn Fn() -> Result<Restart> + Send + Sync>;

/// An engine's settings, as `crate::run` takes them.
pub struct Restart {
    pub root: PathBuf,
    pub database_url: String,
    pub options: crate::EngineOptions,
}

/// Who to answer: `ductus handover` over the socket, a deploy inside the engine, or nobody (a
/// reload the system asked for: the log says how it went).
enum Reply {
    #[cfg(target_os = "linux")]
    Socket(linux::Lined),
    InProcess(mpsc::UnboundedSender<Answer>),
}

impl Reply {
    async fn send(&mut self, answer: Answer) {
        // The asker may have gone (Ctrl-C): the handover goes on all the same.
        match self {
            #[cfg(target_os = "linux")]
            Reply::Socket(lined) => drop(lined.send(&answer).await),
            Reply::InProcess(tx) => drop(tx.send(answer)),
        }
    }
}

/// A request for a handover, with whom to answer.
pub(crate) struct Requested {
    request: Request,
    asker: Option<Reply>,
}

impl Requested {
    async fn say(&mut self, text: impl Into<String>) {
        let text = text.into();
        tracing::info!("handover: {text}");
        if let Some(asker) = self.asker.as_mut() {
            asker.send(Answer::Progress(text)).await;
        }
    }

    async fn done(mut self, ok: bool, text: String) {
        if ok {
            tracing::info!("handover: {text}");
        } else {
            tracing::error!("handover: {text}");
        }
        if let Some(mut asker) = self.asker.take() {
            asker.send(Answer::Done { ok, text }).await;
        }
    }
}

impl Drop for Requested {
    /// Not answered: this engine stopped while the new one got ready (a SIGTERM, a drain that
    /// stops), and the new one was stopped with it (`Unready`).
    fn drop(&mut self) {
        let text = "this engine stopped before the handover finished (it was asked to stop): a new engine it had started was stopped too";
        let done = Answer::Done { ok: false, text: text.into() };
        match self.asker.take() {
            #[cfg(target_os = "linux")]
            Some(Reply::Socket(asker)) => asker.try_send(&done),
            Some(Reply::InProcess(tx)) => drop(tx.send(done)),
            None => {}
        }
    }
}

/// Where this process's current engine takes requests the system asks for (`reload`).
static RELOADS: std::sync::Mutex<Option<(mpsc::Sender<Requested>, Arc<AtomicBool>)>> = std::sync::Mutex::new(None);

/// The system asks for a reload (`SIGHUP`; the Windows service's `paramchange`): a handover to
/// the project or release as it is now, with the settings read again. Nobody waits for the
/// answer: the log (and the Event Log) says how it went. False when it can't be asked now (no
/// engine takes handovers, or one is under way), said in the log too.
pub fn reload(by: &str) -> bool {
    let current = RELOADS.lock().unwrap().clone();
    let Some((tx, busy)) = current else {
        tracing::warn!(%by, "a reload was asked, but this engine takes no handovers: restart it instead");
        return false;
    };
    let request = Request {
        timeout_secs: RELOAD_TIMEOUT.as_secs(),
        environment: None,
        asked: Asked { by: Some(by.to_string()), ..Asked::default() },
    };
    if busy.load(Ordering::SeqCst) || tx.try_send(Requested { request, asker: None }).is_err() {
        tracing::warn!(%by, "a reload was asked while another handover is under way: ignored (ask again once it's done)");
        return false;
    }
    tracing::info!(%by, "reload asked: a new engine starts beside this one on the project or release as it is now");
    true
}

/// How a handover starts the new engine (see the module).
#[derive(Clone)]
enum How {
    #[cfg(target_os = "linux")]
    Process,
    InProcess {
        rebuild: Rebuild,
        stop: Option<tokio_util::sync::CancellationToken>,
    },
}

/// Where an engine takes handover requests: its control socket (Linux), a deploy from inside it,
/// the system's reload.
pub struct Control {
    requests: Option<mpsc::Receiver<Requested>>,
    /// A handover is under way: a request meanwhile is told so.
    busy: Arc<AtomicBool>,
    /// Where a request from inside the engine goes (a deploy: `POST /deployments`).
    asker: Option<mpsc::Sender<Requested>>,
    how: Option<How>,
}

impl Control {
    /// Take handover requests when `enabled` (`ductus run`, the service; not `ductus dev`): over
    /// the control socket and in a new process when there is one (Linux, not PID 1), else in this
    /// process when `rebuild` says how to start an engine again (None: no handover at all). `stop`
    /// is the service manager's stop, which the new engine heeds too.
    pub fn listen(enabled: bool, rebuild: Option<Rebuild>, stop: Option<tokio_util::sync::CancellationToken>) -> Self {
        let busy = Arc::new(AtomicBool::new(false));
        let none = || Self { requests: None, busy: busy.clone(), asker: None, how: None };
        if !enabled {
            return none();
        }
        let in_process = rebuild.map(|rebuild| How::InProcess { rebuild, stop });
        #[cfg(target_os = "linux")]
        let (channel, how) = {
            // As PID 1 (a container) a new process would end the container when this one exits:
            // the socket still takes requests (`docker exec … ductus handover`), served here.
            let pid1 = std::process::id() == 1;
            let opened = control_path().and_then(|path| match linux::listen(&path, busy.clone()) {
                Ok(opened) => Some(opened),
                Err(e) => {
                    tracing::warn!(error = %format!("{e:#}"), "no control socket: `ductus handover` can't reach this engine");
                    None
                }
            });
            match (opened, in_process) {
                (Some(channel), _) if !pid1 => (Some(channel), Some(How::Process)),
                (Some(channel), Some(how)) => (Some(channel), Some(how)),
                (None, Some(how)) => (Some(mpsc::channel(1)), Some(how)),
                (_, None) => (None, None),
            }
        };
        #[cfg(not(target_os = "linux"))]
        let (channel, how) = match in_process {
            Some(how) => (Some(mpsc::channel(1)), Some(how)),
            None => (None, None),
        };
        let Some((asker, requests)) = channel else { return none() };
        *RELOADS.lock().unwrap() = Some((asker.clone(), busy.clone()));
        listen_for_hangup();
        Self { requests: Some(requests), busy, asker: Some(asker), how }
    }

    /// Asks for a handover from inside the engine (a deploy), as `ductus handover` asks over the
    /// socket: None when this engine takes no handovers.
    pub fn asker(&self) -> Option<Asker> {
        self.asker.clone().map(|tx| Asker { tx, busy: self.busy.clone() })
    }

    /// Resolves when a new engine is ready to take over: this one must then stop, let go of its
    /// interfaces and call `Successor::released`. Requests that fail are answered here (the
    /// engine goes on), and the next one is waited for.
    pub async fn next(&mut self, engine: &Engine) -> Successor {
        if let (Some(requests), Some(how)) = (self.requests.as_mut(), self.how.as_ref()) {
            while let Some(requested) = requests.recv().await {
                self.busy.store(true, Ordering::SeqCst);
                if let Some(successor) = start(requested, engine, how).await {
                    return successor; // busy for good: this engine stops
                }
                self.busy.store(false, Ordering::SeqCst);
            }
        }
        std::future::pending().await
    }
}

/// `SIGHUP` asks for a reload (once per process; before, its default ended the engine).
fn listen_for_hangup() {
    #[cfg(unix)]
    {
        static LISTENING: std::sync::Once = std::sync::Once::new();
        LISTENING.call_once(|| {
            use tokio::signal::unix::{SignalKind, signal};
            match signal(SignalKind::hangup()) {
                Ok(mut hangup) => drop(tokio::spawn(async move {
                    while hangup.recv().await.is_some() {
                        reload("a signal (SIGHUP)");
                    }
                })),
                Err(e) => tracing::warn!(error = %e, "can't listen for SIGHUP: reload with `ductus handover` instead"),
            }
        });
    }
}

/// Asks this engine for a handover from inside it: what `POST /deployments` uses.
#[derive(Clone)]
pub struct Asker {
    tx: mpsc::Sender<Requested>,
    busy: Arc<AtomicBool>,
}

impl Asker {
    /// Ask, passing each line said as it goes to `say`: Ok((true, text)) once the new engine runs,
    /// (false, why) when it couldn't take over (this engine goes on), as `ask` over the socket.
    /// The answer comes once this engine stopped and let go (it answers before it ends).
    pub async fn ask(&self, request: Request, mut say: impl FnMut(&str)) -> Result<(bool, String)> {
        if self.busy.load(Ordering::SeqCst) {
            return Ok((false, "another handover is under way".into()));
        }
        let (answers, mut rx) = mpsc::unbounded_channel();
        let requested = Requested { request, asker: Some(Reply::InProcess(answers)) };
        if self.tx.try_send(requested).is_err() {
            return Ok((false, "another handover is under way".into()));
        }
        while let Some(answer) = rx.recv().await {
            match answer {
                Answer::Progress(text) => say(&text),
                Answer::Done { ok, text } => return Ok((ok, text)),
            }
        }
        bail!("the handover ended without an answer: see the engine's log")
    }
}

/// The new engine while it gets ready: stopped if this is dropped before (this engine stops
/// meanwhile, or the handover failed).
enum Unready {
    #[cfg(target_os = "linux")]
    Process(Option<tokio::process::Child>),
    InProcess(Option<tokio::task::JoinHandle<Result<crate::Ended>>>),
}

impl Unready {
    /// After a failure: wait a little for it to end (it says why first), else stop it. How it
    /// ended, in words.
    async fn end(&mut self) -> String {
        match self {
            #[cfg(target_os = "linux")]
            Unready::Process(child) => linux::end(child.take()).await,
            Unready::InProcess(task) => {
                let Some(mut task) = task.take() else { return "gone".into() };
                let ended = match tokio::time::timeout(Duration::from_secs(10), &mut task).await {
                    Ok(ended) => ended,
                    Err(_) => {
                        task.abort();
                        task.await
                    }
                };
                // Whatever it bound anew closes; the sockets this engine listens on stay.
                crate::systemd::abandon_takeover();
                match ended {
                    Ok(Ok(_)) => "ended".into(),
                    Ok(Err(e)) => format!("{e:#}"),
                    Err(e) if e.is_cancelled() => "stopped".into(),
                    Err(e) => format!("it panicked: {e}"),
                }
            }
        }
    }
}

impl Drop for Unready {
    fn drop(&mut self) {
        match self {
            #[cfg(target_os = "linux")]
            Unready::Process(Some(child)) => drop(child.start_kill()),
            Unready::InProcess(Some(task)) => {
                task.abort();
                crate::systemd::abandon_takeover();
            }
            _ => {}
        }
    }
}

/// The new engine as it starts, before it's ready.
struct Started {
    unready: Unready,
    /// What it is, for the asker: `/usr/bin/ductus (pid 4242)`, `a new engine in this process`.
    what: String,
    link: Link<ToNew, ToOld>,
    /// In this process: whether it runs releases (Some), for the words if it fails late.
    in_process: Option<bool>,
}

/// The new engine, ready: this engine stops now.
pub struct Successor {
    pid: u32,
    version: String,
    link: Link<ToNew, ToOld>,
    requested: Requested,
    /// In this process: the new engine's task.
    next: Option<Next>,
}

/// An engine started in this process that took over: `crate::run` waits for it once the engine it
/// took over from ended.
pub(crate) struct Next {
    pub(crate) task: tokio::task::JoinHandle<Result<crate::Ended>>,
    /// Folder mode or release mode, for the words if it fails.
    pub(crate) releases: bool,
}

impl Successor {
    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    /// In this process (an in-process handover).
    pub fn in_process(&self) -> bool {
        self.next.is_some()
    }

    /// After this engine stopped and let go of its interfaces (`stop_took`: since its workers
    /// began to stop): tell the new engine (a new process: make it the service's first), and wait
    /// until it sends; then answer whoever asked. In this process: the new engine, for `crate::run`.
    pub(crate) async fn released(mut self, stop_took: Duration) -> Option<Next> {
        let next = self.next.take();
        let new = match &next {
            None => {
                crate::systemd::main_pid(self.pid);
                format!("the new engine (pid {})", self.pid)
            }
            Some(_) => "the new engine (in this process)".to_string(),
        };
        let released = ToNew::Released { stop_ms: stop_took.as_millis() as u64 };
        if let Err(e) = self.link.send(released).await {
            let text = format!("{new} went away before this one let go: {e}");
            self.requested.done(false, text).await;
            return next;
        }
        self.requested
            .say(format!(
                "this engine stopped in {} and let go of its interfaces: the new one takes them",
                took(stop_took)
            ))
            .await;
        let again = match &next {
            None => "systemd restarts it if it exits with an error (`journalctl -u ductus`)",
            Some(Next { releases: true, .. }) => "the release that ran before starts again in this process",
            Some(Next { releases: false, .. }) => "the engine starts again in this process on the project as it is",
        };
        let version = self.version.clone();
        let text = match tokio::time::timeout(RUNNING_WITHIN, self.link.receive()).await {
            Ok(Ok(Some(ToOld::Running { sending_paused_ms }))) => {
                let paused = took(Duration::from_millis(sending_paused_ms));
                let to = match &next {
                    None => format!("ductus {version} (pid {})", self.pid),
                    Some(_) => format!("a new engine in this process (ductus {version})"),
                };
                let text = format!("handed over to {to}: receiving never stopped, sending paused {paused}");
                self.requested.done(true, text).await;
                return next;
            }
            Ok(Ok(Some(ToOld::Failed(error)))) => {
                format!("{new} couldn't take the interfaces after this one let go:\n{error}\n{again}")
            }
            Ok(_) => format!("{new} went away after this one let go: {again}"),
            Err(_) => format!("{new} didn't say it runs within {RUNNING_WITHIN:?} of this one letting go: see its log"),
        };
        self.requested.done(false, text).await;
        next
    }
}

/// Run a handover request up to the point of no return: the new engine is ready (Some), or it
/// failed and this engine goes on (None, the asker told why).
async fn start(mut requested: Requested, engine: &Engine, how: &How) -> Option<Successor> {
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
        asked: requested.request.asked.clone(),
    };
    let began = Instant::now();
    let started = match how {
        #[cfg(target_os = "linux")]
        How::Process => linux::spawn(&requested.request, &me).await,
        How::InProcess { rebuild, stop } => in_process(rebuild, stop.clone(), me, engine.releases.is_some()).await,
    };
    let mut started = match started {
        Ok(s) => s,
        Err(e) => {
            requested.done(false, format!("couldn't start the new engine: {e:#}. {here}")).await;
            return None;
        }
    };
    let reads = "it reads the project, migrates the database and loads every interface's code while this one runs";
    requested.say(format!("started {}: {reads}", started.what)).await;
    let limit = Duration::from_secs(requested.request.timeout_secs.max(1));
    let said = tokio::time::timeout(limit, started.link.receive()).await;
    let failure = match said {
        Ok(Ok(Some(ToOld::Ready { pid, version, warnings, .. }))) => {
            for warning in warnings {
                requested.say(warning).await;
            }
            let who = match started.in_process {
                None => format!("ductus {version} (pid {pid})"),
                Some(_) => format!("the new engine (ductus {version}, in this process)"),
            };
            requested
                .say(format!(
                    "{who} is ready after {} and receives beside this one: stopping this one",
                    took(began.elapsed())
                ))
                .await;
            // From here it's the service's engine to be: never stopped with this one.
            let next = match &mut started.unready {
                #[cfg(target_os = "linux")]
                Unready::Process(child) => {
                    drop(child.take());
                    None
                }
                Unready::InProcess(task) => {
                    let releases = started.in_process.unwrap_or_default();
                    task.take().map(|task| Next { task, releases })
                }
            };
            return Some(Successor { pid, version, link: started.link, requested, next });
        }
        Ok(Ok(Some(ToOld::Failed(error)))) => {
            started.unready.end().await;
            format!("the new engine can't take over:\n{error}")
        }
        Ok(Ok(Some(ToOld::Running { .. }))) | Ok(Err(_)) | Ok(Ok(None)) => {
            let how = started.unready.end().await;
            let log =
                if started.in_process.is_some() { "this engine's log" } else { "its log (`journalctl -u ductus`)" };
            format!("the new engine ended ({how}) before it was ready: {log} says why")
        }
        Err(_) => {
            drop(started);
            format!(
                "the new engine wasn't ready within {limit:?} and was stopped (a long migration needs `ductus handover --timeout`)"
            )
        }
    };
    // Lanes the new engine added for a change of `ordering`: gone again where it stored nothing,
    // so this engine's resends don't go to lanes it doesn't read.
    let queued: Vec<(String, &crate::lanes::Lanes)> = engine
        .interfaces
        .iter()
        .filter(|i| i.intake.is_some())
        .map(|i| (i.config.name.clone(), i.lanes.as_ref()))
        .collect();
    let kept = crate::lanes::take_back(&engine.store, &queued).await.map(|k| format!("\n{k}")).unwrap_or_default();
    requested.done(false, format!("{failure}{kept}\n{here}")).await;
    None
}

/// Start a new engine as a task in this process, with its settings read again, on copies of
/// this engine's sockets, and tell it who it takes over from.
async fn in_process(
    rebuild: &Rebuild,
    stop: Option<tokio_util::sync::CancellationToken>,
    me: Predecessor,
    releases: bool,
) -> Result<Started> {
    let Restart { root, database_url, mut options } = rebuild().context("reading the engine's settings again")?;
    let (to_old, from_new) = mpsc::unbounded_channel();
    let (to_new, from_old) = mpsc::unbounded_channel();
    let takeover = Takeover::with(Link::Channel(to_old, from_old), true);
    options.takeover = Some(takeover.clone());
    // The service manager's stop reaches whichever engine runs.
    options.stop_request = stop;
    crate::systemd::begin_takeover();
    let task = tokio::spawn(crate::run_one(root, database_url, options));
    let mut link = Link::Channel(to_new, from_new);
    // It can't have gone yet unless it failed at once: what it said is still in the channel.
    let _ = link.send(ToNew::From(me)).await;
    Ok(Started {
        unready: Unready::InProcess(Some(task)),
        what: "a new engine in this process".into(),
        link,
        in_process: Some(releases),
    })
}

/// The new engine's side of the conversation.
pub struct Takeover {
    link: tokio::sync::Mutex<Link<ToOld, ToNew>>,
    from: tokio::sync::OnceCell<Predecessor>,
    /// It said it runs: a later failure is its own (systemd's or `crate::run`'s to start again),
    /// not the takeover's.
    running: AtomicBool,
    in_process: bool,
}

impl Takeover {
    fn with(link: Link<ToOld, ToNew>, in_process: bool) -> Arc<Self> {
        Arc::new(Self {
            link: tokio::sync::Mutex::new(link),
            from: Default::default(),
            running: false.into(),
            in_process,
        })
    }

    /// From the descriptor in `DUCTUS_TAKEOVER` (a new process the old engine started).
    pub fn new(fd: i32) -> Result<Arc<Self>> {
        #[cfg(target_os = "linux")]
        return Ok(Self::with(Link::Socket(linux::takeover_pair(fd)?), false));
        #[cfg(not(target_os = "linux"))]
        {
            let _ = fd;
            bail!("`DUCTUS_TAKEOVER` is for a handover to a new process, which is Linux's")
        }
    }

    /// In the same process as the engine it takes over from.
    pub fn in_process(&self) -> bool {
        self.in_process
    }

    /// Who is being taken over from (the old engine's first word).
    pub async fn predecessor(&self) -> Result<Predecessor> {
        self.from
            .get_or_try_init(|| async {
                match self.link.lock().await.receive().await? {
                    Some(ToNew::From(p)) => Ok(p),
                    _ => bail!("the engine to take over from didn't say who it is"),
                }
            })
            .await
            .cloned()
    }

    /// Ready: the old engine stops now (and tells the asker `warnings` first).
    pub async fn ready(&self, took: Duration, warnings: Vec<String>) -> Result<()> {
        let ready = ToOld::Ready {
            pid: std::process::id(),
            version: env!("CARGO_PKG_VERSION").into(),
            took_ms: took.as_millis() as u64,
            warnings,
        };
        self.link.lock().await.send(ready).await.context("telling the engine to take over from")
    }

    /// Wait until the old engine let go: how long its stop took (None: it went away without a
    /// word; its claims went with its lock).
    pub async fn released(&self) -> Option<Duration> {
        match self.link.lock().await.receive().await {
            Ok(Some(ToNew::Released { stop_ms })) => Some(Duration::from_millis(stop_ms)),
            _ => None,
        }
    }

    /// It holds the interfaces and sends.
    pub async fn running(&self, sending_paused: Duration) {
        self.running.store(true, Ordering::SeqCst);
        let running = ToOld::Running { sending_paused_ms: sending_paused.as_millis() as u64 };
        let _ = self.link.lock().await.send(running).await;
    }

    /// The takeover failed (this engine ends): why, for the old engine to report. Nothing once it
    /// said it runs.
    pub async fn failed(&self, error: &str) {
        if !self.running.load(Ordering::SeqCst) {
            let _ = self.link.lock().await.send(ToOld::Failed(error.to_string())).await;
        }
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
        bail!(
            "the control socket is Linux's: on Windows `ductus handover` asks the service (`sc.exe control Ductus paramchange`)"
        )
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    use std::os::unix::fs::PermissionsExt;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
    use tokio::net::UnixStream;
    use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

    /// One JSON value per line, both ways.
    pub(super) struct Lined {
        read: Lines<BufReader<OwnedReadHalf>>,
        write: OwnedWriteHalf,
    }

    impl Lined {
        fn new(stream: UnixStream) -> Self {
            let (read, write) = stream.into_split();
            Self { read: BufReader::new(read).lines(), write }
        }

        pub(super) async fn send<T: Serialize>(&mut self, value: &T) -> io::Result<()> {
            let mut line = serde_json::to_vec(value).map_err(io::Error::other)?;
            line.push(b'\n');
            self.write.write_all(&line).await
        }

        /// The next value; None once the other side closed.
        pub(super) async fn receive<T: DeserializeOwned>(&mut self) -> io::Result<Option<T>> {
            match self.read.next_line().await? {
                None => Ok(None),
                Some(line) => serde_json::from_str(&line).map(Some).map_err(io::Error::other),
            }
        }

        /// Without waiting (from a `Drop`): what fits in the socket's buffer now.
        pub(super) fn try_send<T: Serialize>(&self, value: &T) {
            let mut line = serde_json::to_vec(value).unwrap_or_default();
            line.push(b'\n');
            let _ = self.write.try_write(&line);
        }
    }

    /// Listen at `path` (a stale socket left by an engine that's gone is replaced). Only this
    /// engine's own user (or root) may ask: the socket is 0600, and each asker's user is checked.
    /// Returns a sender for requests from inside the engine, and where all requests arrive.
    pub(super) fn listen(
        path: &Path,
        busy: Arc<AtomicBool>,
    ) -> Result<(mpsc::Sender<Requested>, mpsc::Receiver<Requested>)> {
        let _ = std::fs::remove_file(path);
        let listener = tokio::net::UnixListener::bind(path)
            .with_context(|| format!("listening for `ductus handover` at {}", path.display()))?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("making {} private", path.display()))?;
        // One at a time: a request while another is under way is told so.
        let (tx, rx) = mpsc::channel(1);
        let inside = tx.clone();
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
                    let requested = Requested { request, asker: Some(Reply::Socket(asker)) };
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
        Ok((inside, rx))
    }

    /// After a failure: wait a little for the new process to exit (it says why first), else kill
    /// it. How it ended, in words.
    pub(super) async fn end(child: Option<tokio::process::Child>) -> String {
        let Some(mut child) = child else { return "gone".into() };
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

    /// Start the binary now on disk with this engine's arguments, its listening sockets and a
    /// socket pair, and tell it who it takes over from.
    pub(super) async fn spawn(request: &Request, me: &Predecessor) -> Result<Started> {
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
        let what = format!("{} (pid {})", path.display(), child.id().unwrap_or_default());
        // Stopped if anything below fails.
        let unready = Unready::Process(Some(child));
        ours.set_nonblocking(true)?;
        let mut link = Link::Socket(Lined::new(UnixStream::from_std(ours)?));
        // A new engine whose project doesn't load says why and exits at once, possibly before this
        // line is written (CI saw "Broken pipe" instead of the reason): what it said is still in the
        // pair for `start` to read, so a closed pair isn't the failure to report here.
        if let Err(e) = link.send(ToNew::From(me.clone())).await
            && !matches!(e.kind(), io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset)
        {
            return Err(e).context("telling the new engine who it takes over from");
        }
        Ok(Started { unready, what, link, in_process: None })
    }

    /// The new engine's end of the socket pair, from the descriptor in `DUCTUS_TAKEOVER`.
    pub(super) fn takeover_pair(fd: RawFd) -> Result<Lined> {
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
        Ok(Lined::new(UnixStream::from_std(stream)?))
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

    #[cfg(test)]
    mod tests {
        use super::*;

        /// What `spawn` relies on when a new engine fails at once: it said why and closed before
        /// the old engine's first line, so that line meets a closed pair, and the reason can still
        /// be read after it.
        #[tokio::test]
        async fn a_new_engine_that_failed_at_once_is_still_heard() {
            let (old, new) = UnixStream::pair().unwrap();
            let mut new = Lined::new(new);
            new.send(&ToOld::Failed("unknown field `bogus`".into())).await.unwrap();
            drop(new);
            let mut old = Lined::new(old);
            let me = Predecessor { pid: 1, version: "0".into(), instance: "i".into(), asked: Asked::default() };
            let sent = old.send(&ToNew::From(me)).await;
            assert!(sent.as_ref().is_err_and(|e| e.kind() == io::ErrorKind::BrokenPipe), "{sent:?}");
            let said = old.receive::<ToOld>().await.unwrap();
            assert!(matches!(said, Some(ToOld::Failed(why)) if why.contains("bogus")));
        }
    }
}
