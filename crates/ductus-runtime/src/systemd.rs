//! Every listening socket the engine opens goes through here (`listen`), so it outlives the
//! engine that opened it:
//!
//! **This process keeps a copy of each** (every system): another handle to the same socket (`dup`
//! on Linux, `WSADuplicateSocketW` on Windows), which listens until every copy is closed. An
//! engine started in the same process (an in-process handover, `crate::handover`; a start again
//! after a failed one) listens on that copy instead of binding: the socket never closes in
//! between, so senders' connections wait in the kernel's backlog until it accepts them, and two
//! engines may accept on it at once while one takes over from the other.
//!
//! **systemd keeps them** (Linux, under the package's `Type=notify` unit). A restart used to refuse
//! connections: between the old process closing its listeners and the new one binding them again
//! (the start takes ~0.5–1 s) nothing listened on the port. Now every listening socket is also
//! handed to systemd's file descriptor store when it's opened (`FDSTORE=1`, named after its
//! configured address). systemd holds its copy while the service restarts or after a crash and
//! hands it to the next start (`LISTEN_FDS`, `LISTEN_FDNAMES`), which listens on it instead of
//! binding: nothing is refused, senders wait for the start.
//!
//! **A handover to a new process passes them on** (Linux, `crate::handover`): the new engine
//! inherits this process's copies (`DUCTUS_LISTEN_FDS`) and listens on them as on systemd's,
//! while the old one still does. Until the old engine hands it the service (`MAINPID`), systemd
//! listens only to the old one (`NotifyAccess=main`): the new one stores nothing and gives nothing
//! back until then (`promote`).
//!
//! Closing a socket on purpose takes it out of systemd's store first (`FDSTOREREMOVE=1`), or
//! systemd's copy would keep it listening with nobody behind it; and out of this process's copies,
//! for the same reason:
//! - a drain closes MLLP listeners so senders see the engine down (`release`; a stop and a
//!   handover keep them: `stopping`, `handing_over`);
//! - a socket no longer configured is dropped at start, before anything binds (`keep_only`: it
//!   would hold its port), and once a takeover took over (`promote`: the engine taken over from
//!   listened on it);
//! - a start that fails (`abandon`): a broken engine is refused as before, not a port that takes
//!   connections and never answers; a takeover in this process that fails closes only what it
//!   opened itself (`abandon_takeover`: the engine it would have taken over from goes on).
//!
//! Also `READY=1` once every interface is up (`systemctl start` waits for it) and `STOPPING=1`.
//! Outside systemd (no `NOTIFY_SOCKET`: `ductus dev`, containers, Windows) nothing is stored.

use std::collections::{HashMap, HashSet};
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};
use tokio::net::TcpListener;

static SOCKETS: LazyLock<Sockets> = LazyLock::new(Sockets::from_env);

/// The name a listening socket is stored under: its configured address (`tcp-0.0.0.0-2575`).
/// The protocol isn't part of it: a port moved from MLLP to HTTP keeps its socket. systemd takes
/// printable ASCII without `:` (`LISTEN_FDNAMES` is colon-separated), up to 255 characters.
pub fn name(host: &str, port: u16) -> String {
    let host: String =
        host.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect();
    let mut name = format!("tcp-{host}-{port}");
    name.truncate(255);
    name
}

/// Read what systemd or the engine taken over from passed to this process (call at start,
/// before anything else could start a process: those descriptors are made close-on-exec here).
pub fn init() {
    LazyLock::force(&SOCKETS);
}

/// Listen on `host:port`: on the socket kept for it when there is one (systemd's, the engine
/// taken over from's, a copy this process holds), else on a new one (which systemd is then given
/// to keep).
pub async fn listen(host: &str, port: u16) -> io::Result<TcpListener> {
    SOCKETS.listen(host, port).await.map_err(|e| in_use(e, port))
}

/// "Address already in use" says nothing about what to do: name the port and how to find what
/// holds it.
fn in_use(e: io::Error, port: u16) -> io::Error {
    if e.kind() != io::ErrorKind::AddrInUse {
        return e;
    }
    let find = if cfg!(windows) {
        format!("`netstat -ano | findstr :{port}` (the last column is its process id)")
    } else {
        format!("`sudo ss -ltnp 'sport = :{port}'`")
    };
    io::Error::new(
        io::ErrorKind::AddrInUse,
        format!(
            "port {port} is in use: another program listens on it (another engine, or a test tool); find it with \
             {find}, or give this one another port"
        ),
    )
}

/// The listener on `host:port` is about to close for a while (a drain): nothing may keep it
/// listening (systemd, this process's copy) nor pass it on. Nothing while the engine stops (the
/// next start, or the engine taking over, listens on it).
pub fn release(host: &str, port: u16) {
    SOCKETS.release(&name(host, port));
}

/// Before anything listens: close the sockets kept for addresses no longer configured, and
/// remember what this engine listens on (what a takeover keeps once it took over).
pub fn keep_only<'a>(addresses: impl IntoIterator<Item = (&'a str, u16)>) {
    SOCKETS.keep_only(addresses.into_iter().map(|(host, port)| name(host, port)).collect());
}

/// Every interface and the API are up: `systemctl start` returns.
pub fn ready(status: &str) {
    SOCKETS.notify_main(&format!("READY=1\nSTATUS={status}"));
}

/// The engine is stopping: listeners that close from now on stay kept.
pub fn stopping() {
    SOCKETS.stopping(true);
}

/// The engine stops for a handover: as `stopping`, but systemd isn't told the service stops (it
/// goes on in the new engine).
pub fn handing_over() {
    SOCKETS.stopping(false);
}

/// The start failed: give every socket back, so senders are refused while the engine is down
/// (systemd tries the start again in a few seconds; the next one binds them anew). Nothing in a
/// takeover from another process that hasn't taken over: the sockets are still the old engine's.
pub fn abandon() {
    SOCKETS.abandon();
}

/// The last engine in this process ended: close this process's copies, so the ports are free
/// before the process exits (systemd's store keeps its own, for the next start).
pub fn close_all() {
    SOCKETS.close_all();
}

/// A copy of every socket listening now, by name: what a handover to a new process passes on.
#[cfg(target_os = "linux")]
pub fn handover_sockets() -> io::Result<Vec<(String, std::os::fd::OwnedFd)>> {
    SOCKETS.copies()
}

/// A handover: the service's process is `pid` from now on (this one stops telling systemd anything).
pub fn main_pid(pid: u32) {
    SOCKETS.hand_to(pid);
}

/// A takeover took over: this engine is the service's now. It keeps the sockets it listens on;
/// the ones only the engine taken over from listened on close (in this process: that engine's
/// copies), and systemd's store follows (`ready` follows once it sends).
pub fn promote() {
    SOCKETS.promote();
}

/// An engine starts beside the running one in this process (an in-process handover): note the
/// sockets it binds anew, which close again if it fails (`abandon_takeover`).
pub fn begin_takeover() {
    SOCKETS.begin_takeover();
}

/// The in-process takeover failed: close what it bound anew; the running engine's sockets stay.
pub fn abandon_takeover() {
    SOCKETS.abandon_takeover();
}

/// Another handle to the same listening socket: it listens until every handle is closed.
fn copy(listener: &TcpListener) -> io::Result<std::net::TcpListener> {
    #[cfg(unix)]
    let copy = std::os::fd::AsFd::as_fd(listener).try_clone_to_owned()?.into();
    #[cfg(windows)]
    let copy = std::os::windows::io::AsSocket::as_socket(listener).try_clone_to_owned()?.into();
    Ok(copy)
}

struct Sockets {
    /// systemd's notify socket (`NOTIFY_SOCKET`: a path, or `@name` for an abstract one). None
    /// outside systemd (always, off Linux).
    notify: Option<Vec<u8>>,
    /// Sockets handed to this start that nothing has listened on yet, by name.
    kept: Mutex<HashMap<String, std::net::TcpListener>>,
    /// A copy of every socket listening now, by name: a handover passes them on, an engine started
    /// again in this process listens on them.
    open: Mutex<HashMap<String, std::net::TcpListener>>,
    /// Every name in systemd's store as far as this process knows (for `abandon`, `promote`).
    stored: Mutex<HashSet<String>>,
    stopping: AtomicBool,
    /// systemd listens to this process (`NotifyAccess=main`). False in a takeover from another
    /// process until the old engine hands it the service, and in the old engine once it did.
    main: AtomicBool,
    /// What the engine that started last listens on (`keep_only`): a takeover keeps only these.
    wanted: Mutex<Option<HashSet<String>>>,
    /// While an engine starts beside another in this process: the sockets it bound anew.
    fresh: Mutex<Option<HashSet<String>>>,
}

impl Sockets {
    #[cfg(target_os = "linux")]
    fn from_env() -> Self {
        linux::from_env()
    }

    #[cfg(not(target_os = "linux"))]
    fn from_env() -> Self {
        Self::new(None, Vec::new(), true)
    }

    fn new(notify: Option<&[u8]>, kept: Vec<(String, std::net::TcpListener)>, main: bool) -> Self {
        // The ones handed over are in systemd's store already (the engine taken over from
        // stored them): a takeover that drops one takes it out once it's the service's.
        let stored =
            if notify.is_some() { kept.iter().map(|(name, _)| name.clone()).collect() } else { HashSet::new() };
        if !kept.is_empty() {
            let from = if main {
                "systemd kept the listening sockets"
            } else {
                "listening on the sockets of the engine it takes over from"
            };
            tracing::info!(sockets = kept.len(), "{from}: senders waited for this start");
        }
        Self {
            notify: notify.map(<[u8]>::to_vec),
            kept: Mutex::new(kept.into_iter().collect()),
            open: Mutex::new(HashMap::new()),
            stored: Mutex::new(stored),
            stopping: AtomicBool::new(false),
            main: AtomicBool::new(main),
            wanted: Mutex::new(None),
            fresh: Mutex::new(None),
        }
    }

    fn is_main(&self) -> bool {
        self.main.load(Ordering::SeqCst)
    }

    async fn listen(&self, host: &str, port: u16) -> io::Result<TcpListener> {
        let name = name(host, port);
        let kept = self.kept.lock().unwrap().remove(&name);
        // A socket still open (another engine in this process listens on it, or this one did
        // before an accept error): its copy.
        let kept = match kept {
            Some(socket) => Some(socket),
            None => self.open.lock().unwrap().get(&name).map(std::net::TcpListener::try_clone).transpose()?,
        };
        let mut listener = None;
        if let Some(std) = kept {
            if std.local_addr().is_ok_and(|a| a.port() == port) {
                std.set_nonblocking(true)?;
                tracing::debug!(%host, port, "listening on a socket kept for it");
                listener = Some(TcpListener::from_std(std)?);
            } else {
                // Not the port its name says (nothing of ours stores one like that): close it.
                tracing::warn!(socket = %name, "a socket handed to this start isn't on its port: closing it");
                drop(std);
                self.remove(&name);
            }
        }
        let (listener, bound) = match listener {
            Some(l) => (l, false),
            None => (TcpListener::bind((host, port)).await?, true),
        };
        let copy = copy(&listener)?;
        if bound {
            self.store(&name, &copy);
            if let Some(fresh) = self.fresh.lock().unwrap().as_mut() {
                fresh.insert(name.clone());
            }
        }
        self.open.lock().unwrap().insert(name, copy);
        Ok(listener)
    }

    /// Give `listener` to systemd's store (when this process is the service's; a takeover's are
    /// stored when it is: `promote`).
    fn store(&self, name: &str, listener: &std::net::TcpListener) {
        if self.notify.is_none() || !self.is_main() {
            return;
        }
        #[cfg(target_os = "linux")]
        match self.send(&format!("FDSTORE=1\nFDNAME={name}"), Some(std::os::fd::AsFd::as_fd(listener))) {
            Ok(()) => {
                self.stored.lock().unwrap().insert(name.to_string());
            }
            Err(e) => tracing::warn!(
                socket = %name,
                error = %e,
                "systemd can't keep this listening socket (`FileDescriptorStoreMax=` in the unit): a restart refuses connections to it for a moment"
            ),
        }
        #[cfg(not(target_os = "linux"))]
        let _ = (name, listener);
    }

    fn release(&self, name: &str) {
        if !self.stopping.load(Ordering::SeqCst) {
            self.open.lock().unwrap().remove(name);
            self.remove(name);
        }
    }

    fn keep_only(&self, wanted: HashSet<String>) {
        let gone: Vec<(String, std::net::TcpListener)> = {
            let mut kept = self.kept.lock().unwrap();
            let names: Vec<String> = kept.keys().filter(|n| !wanted.contains(*n)).cloned().collect();
            names.into_iter().filter_map(|n| kept.remove_entry(&n)).collect()
        };
        for (name, socket) in gone {
            tracing::info!(socket = %name, "no longer configured: closing the listening socket kept for it");
            drop(socket);
            self.remove(&name);
        }
        *self.wanted.lock().unwrap() = Some(wanted);
        // An engine starting again in this process after another stopped: its drains close
        // listeners for real again.
        self.stopping.store(false, Ordering::SeqCst);
    }

    /// `tell`: say so to systemd (not when a handover stops the engine: the service goes on).
    fn stopping(&self, tell: bool) {
        self.stopping.store(true, Ordering::SeqCst);
        if tell {
            self.notify_main("STOPPING=1\nSTATUS=stopping: finishing the messages in hand");
        }
    }

    fn abandon(&self) {
        if !self.is_main() {
            return;
        }
        self.kept.lock().unwrap().clear();
        let names: Vec<String> = self.stored.lock().unwrap().iter().cloned().collect();
        for name in names {
            self.remove(&name);
        }
    }

    #[cfg(target_os = "linux")]
    fn copies(&self) -> io::Result<Vec<(String, std::os::fd::OwnedFd)>> {
        self.open.lock().unwrap().iter().map(|(name, socket)| Ok((name.clone(), socket.try_clone()?.into()))).collect()
    }

    fn hand_to(&self, pid: u32) {
        self.notify_main(&format!("MAINPID={pid}"));
        self.main.store(false, Ordering::SeqCst);
    }

    fn promote(&self) {
        self.main.store(true, Ordering::SeqCst);
        *self.fresh.lock().unwrap() = None;
        self.stopping.store(false, Ordering::SeqCst);
        // In this process, the engine taken over from had copies of its own: those this engine
        // doesn't listen on close now (a port the new release dropped).
        if let Some(wanted) = self.wanted.lock().unwrap().clone() {
            let dropped: Vec<String> = {
                let mut open = self.open.lock().unwrap();
                let names: Vec<String> = open.keys().filter(|n| !wanted.contains(*n)).cloned().collect();
                names.into_iter().filter(|n| open.remove(n).is_some()).collect()
            };
            for name in dropped {
                tracing::info!(socket = %name, "no longer configured: closing the listening socket the engine taken over from used");
                self.remove(&name);
            }
        }
        if self.notify.is_none() {
            return;
        }
        let open: Vec<(String, std::net::TcpListener)> = self
            .open
            .lock()
            .unwrap()
            .iter()
            .filter_map(|(n, socket)| Some((n.clone(), socket.try_clone().ok()?)))
            .collect();
        let stored = self.stored.lock().unwrap().clone();
        for (name, socket) in &open {
            if !stored.contains(name) {
                self.store(name, socket);
            }
        }
        for name in stored {
            if !open.iter().any(|(n, _)| *n == name) {
                self.remove(&name);
            }
        }
    }

    fn close_all(&self) {
        self.kept.lock().unwrap().clear();
        self.open.lock().unwrap().clear();
    }

    fn begin_takeover(&self) {
        *self.fresh.lock().unwrap() = Some(HashSet::new());
    }

    fn abandon_takeover(&self) {
        let Some(fresh) = self.fresh.lock().unwrap().take() else { return };
        for name in fresh {
            self.open.lock().unwrap().remove(&name);
            self.remove(&name);
        }
    }

    /// Tell systemd `state`, when this process is the service's.
    fn notify_main(&self, state: &str) {
        if self.notify.is_none() || !self.is_main() {
            return;
        }
        #[cfg(target_os = "linux")]
        if let Err(e) = self.send(state, None) {
            tracing::warn!(error = %e, state, "couldn't tell systemd");
        }
        #[cfg(not(target_os = "linux"))]
        let _ = state;
    }

    fn remove(&self, name: &str) {
        let stored = self.notify.is_some() && self.is_main() && self.stored.lock().unwrap().remove(name);
        #[cfg(target_os = "linux")]
        if stored && let Err(e) = self.send(&format!("FDSTOREREMOVE=1\nFDNAME={name}"), None) {
            tracing::warn!(socket = %name, error = %e, "couldn't take the listening socket out of systemd's store");
        }
        #[cfg(not(target_os = "linux"))]
        let _ = stored;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
    }

    /// Nothing in this process listens on `port` any more. On Linux it's read from `/proc`, not
    /// tried by binding: under a parallel test run another test can take a port the moment it's
    /// free (`free_port` hands out ports the system just gave back), and the bind then fails.
    pub(super) fn free(port: u16) -> bool {
        #[cfg(target_os = "linux")]
        {
            !listens_here(port)
        }
        #[cfg(not(target_os = "linux"))]
        {
            std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
        }
    }

    /// One of this process's descriptors is a socket listening on `port`: `/proc/self/fd` names
    /// each socket's inode, `/proc/self/net/tcp{,6}` each socket's port and state (0A: listening).
    #[cfg(target_os = "linux")]
    fn listens_here(port: u16) -> bool {
        let ours: std::collections::HashSet<String> = std::fs::read_dir("/proc/self/fd")
            .unwrap()
            .filter_map(|entry| std::fs::read_link(entry.ok()?.path()).ok())
            .filter_map(|link| Some(link.to_str()?.strip_prefix("socket:[")?.strip_suffix(']')?.to_string()))
            .collect();
        ["/proc/self/net/tcp", "/proc/self/net/tcp6"].iter().any(|table| {
            std::fs::read_to_string(table).unwrap_or_default().lines().skip(1).any(|line| {
                let f: Vec<&str> = line.split_whitespace().collect();
                let local = f.get(1).and_then(|a| a.rsplit(':').next()).and_then(|p| u16::from_str_radix(p, 16).ok());
                f.len() > 9 && f[3] == "0A" && local == Some(port) && ours.contains(f[9])
            })
        })
    }

    async fn takes_connections(listener: &TcpListener, port: u16) -> bool {
        let _sender = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), listener.accept()).await.is_ok_and(|a| a.is_ok())
    }

    #[tokio::test]
    async fn a_port_in_use_says_how_to_find_what_holds_it() {
        let holder = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = holder.local_addr().unwrap().port();
        let e = listen("127.0.0.1", port).await.unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::AddrInUse);
        assert!(e.to_string().starts_with(&format!("port {port} is in use: another program listens on it")), "{e}");
    }

    /// What an in-process handover relies on, on every system: the new engine listens on the old
    /// one's sockets beside it; whatever it bound anew closes if it fails; once it took over, the
    /// ports only the old engine used close and the rest listen on.
    #[tokio::test]
    async fn an_engine_started_in_this_process_shares_the_running_one_s_sockets() {
        let sockets = Sockets::new(None, Vec::new(), true);
        let (a, b, c) = (free_port(), free_port(), free_port());
        sockets.keep_only([name("127.0.0.1", a), name("127.0.0.1", b)].into());
        let old_a = sockets.listen("127.0.0.1", a).await.unwrap();
        let old_b = sockets.listen("127.0.0.1", b).await.unwrap();

        // A takeover that fails: it listened on a beside the old engine and bound c anew.
        sockets.begin_takeover();
        let new_a = sockets.listen("127.0.0.1", a).await.expect("a copy of the old engine's socket, not a bind");
        let new_c = sockets.listen("127.0.0.1", c).await.unwrap();
        sockets.keep_only([name("127.0.0.1", a), name("127.0.0.1", c)].into());
        assert!(takes_connections(&new_a, a).await && takes_connections(&old_a, a).await, "both accept on a");
        drop((new_a, new_c));
        sockets.abandon_takeover();
        assert!(free(c), "what the failed takeover bound is closed");
        assert!(takes_connections(&old_a, a).await && takes_connections(&old_b, b).await, "the old engine goes on");

        // One that takes over: the old engine stops (its listeners close, the copies stay).
        sockets.begin_takeover();
        let new_a = sockets.listen("127.0.0.1", a).await.unwrap();
        let new_c = sockets.listen("127.0.0.1", c).await.unwrap();
        sockets.keep_only([name("127.0.0.1", a), name("127.0.0.1", c)].into());
        sockets.stopping(false);
        sockets.release(&name("127.0.0.1", a));
        drop((old_a, old_b));
        assert!(!free(b), "b is still kept until the new engine took over");
        sockets.promote();
        assert!(free(b), "b, which the new release doesn't configure, closed");
        assert!(takes_connections(&new_a, a).await && takes_connections(&new_c, c).await);
        // And its drains close for real again (`stopping` was the old engine's).
        sockets.release(&name("127.0.0.1", c));
        drop(new_c);
        assert!(free(c), "a drain after the takeover closes the socket");
    }

    /// A plain stop keeps the copies (a start again may follow); once the last engine ended they
    /// close, so the ports are free before the process exits (a Windows service's restart).
    #[tokio::test]
    async fn the_ports_close_once_the_last_engine_ended() {
        let sockets = Sockets::new(None, Vec::new(), true);
        let port = free_port();
        sockets.keep_only([name("127.0.0.1", port)].into());
        let listener = sockets.listen("127.0.0.1", port).await.unwrap();
        sockets.stopping(true);
        sockets.release(&name("127.0.0.1", port));
        drop(listener);
        assert!(!free(port), "kept while a start again may follow");
        sockets.close_all();
        assert!(free(port), "free once the last engine ended");
    }

    /// The start again after a failed takeover (or any start in this process after a stop):
    /// nothing listens for a moment, and senders wait instead of being refused.
    #[tokio::test]
    async fn a_start_again_in_this_process_listens_on_the_kept_socket() {
        let sockets = Sockets::new(None, Vec::new(), true);
        let port = free_port();
        sockets.keep_only([name("127.0.0.1", port)].into());
        let first = sockets.listen("127.0.0.1", port).await.unwrap();
        sockets.stopping(true);
        sockets.release(&name("127.0.0.1", port)); // a stop keeps it
        drop(first);
        let sender = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.expect("waits in the backlog");
        sockets.keep_only([name("127.0.0.1", port)].into());
        let again = sockets.listen("127.0.0.1", port).await.unwrap();
        let (accepted, _) =
            tokio::time::timeout(std::time::Duration::from_secs(2), again.accept()).await.unwrap().unwrap();
        assert_eq!(accepted.peer_addr().unwrap(), sender.local_addr().unwrap(), "the sender that waited");
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::net::{SocketAddr, UnixDatagram};

    /// The first descriptor systemd passes (`SD_LISTEN_FDS_START`).
    const FIRST_FD: RawFd = 3;

    pub(super) fn from_env() -> Sockets {
        let var = |name: &str| std::env::var(name).ok();
        let notify = std::env::var_os("NOTIFY_SOCKET").map(|a| a.as_bytes().to_vec());
        let takeover = std::env::var_os("DUCTUS_TAKEOVER").is_some();
        let passed = if takeover {
            inherited(var("DUCTUS_LISTEN_FDS").as_deref())
        } else if notify.is_some() {
            passed(
                var("LISTEN_PID").as_deref(),
                var("LISTEN_FDS").as_deref(),
                var("LISTEN_FDNAMES").as_deref(),
                std::process::id(),
            )
        } else {
            Vec::new()
        };
        let kept = passed
            .into_iter()
            .map(|(name, fd)| {
                // SAFETY: systemd (LISTEN_PID is ours) or the engine taken over from (in
                // DUCTUS_LISTEN_FDS, with DUCTUS_TAKEOVER) passed these descriptors to this
                // process, and nothing else in it knows them: they're taken once, here.
                let fd = unsafe { OwnedFd::from_raw_fd(fd) };
                close_on_exec(fd.as_fd());
                (name, fd)
            })
            .collect();
        Sockets::with_fds(notify.as_deref(), kept, !takeover)
    }

    impl Sockets {
        pub(super) fn with_fds(notify: Option<&[u8]>, kept: Vec<(String, OwnedFd)>, main: bool) -> Self {
            Self::new(
                notify,
                kept.into_iter().map(|(name, fd)| (name, std::net::TcpListener::from(fd))).collect(),
                main,
            )
        }

        /// One datagram to systemd's notify socket, with `fd` attached (`SCM_RIGHTS`). Connected
        /// anew each time, as `sd_notify` does: the socket is new after `systemctl daemon-reexec`.
        pub(super) fn send(&self, state: &str, fd: Option<BorrowedFd>) -> io::Result<()> {
            let Some(address) = self.notify.as_deref() else { return Ok(()) };
            let socket = UnixDatagram::unbound()?;
            match address.strip_prefix(b"@") {
                Some(abstract_name) => socket.connect_addr(&SocketAddr::from_abstract_name(abstract_name)?)?,
                None => socket.connect(std::ffi::OsStr::from_bytes(address))?,
            }
            let Some(fd) = fd else {
                socket.send(state.as_bytes())?;
                return Ok(());
            };
            let mut iov = libc::iovec { iov_base: state.as_ptr() as *mut libc::c_void, iov_len: state.len() };
            // u64s: the control buffer must be aligned for `cmsghdr`.
            // SAFETY: CMSG_SPACE only computes a size.
            let space = unsafe { libc::CMSG_SPACE(size_of::<RawFd>() as u32) } as usize;
            let mut control = vec![0u64; space.div_ceil(size_of::<u64>())];
            // SAFETY: an all-zero msghdr is valid (no name, no buffers); filled in below.
            let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
            message.msg_iov = &mut iov;
            message.msg_iovlen = 1;
            message.msg_control = control.as_mut_ptr().cast();
            message.msg_controllen = space as _;
            // SAFETY: the control buffer is `space` bytes, aligned, and has room for one header and
            // one descriptor (CMSG_SPACE of it); the header is the buffer's first.
            unsafe {
                let header = libc::CMSG_FIRSTHDR(&message);
                (*header).cmsg_level = libc::SOL_SOCKET;
                (*header).cmsg_type = libc::SCM_RIGHTS;
                (*header).cmsg_len = libc::CMSG_LEN(size_of::<RawFd>() as u32) as _;
                std::ptr::write_unaligned(libc::CMSG_DATA(header).cast::<RawFd>(), fd.as_raw_fd());
            }
            // SAFETY: `message` points at `iov` (the state's bytes) and `control`, both alive here.
            let sent = unsafe { libc::sendmsg(socket.as_raw_fd(), &message, libc::MSG_NOSIGNAL) };
            if sent < 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
        }
    }

    /// The descriptors systemd passed to this process and their names: `LISTEN_FDS` of them from 3
    /// on, when `LISTEN_PID` is this process (a child inheriting the variables isn't meant).
    pub(super) fn passed(pid: Option<&str>, fds: Option<&str>, names: Option<&str>, me: u32) -> Vec<(String, RawFd)> {
        if pid.and_then(|p| p.parse::<u32>().ok()) != Some(me) {
            return Vec::new();
        }
        let count = fds.and_then(|n| n.parse::<RawFd>().ok()).unwrap_or(0).max(0);
        let mut names = names.unwrap_or_default().split(':').filter(|n| !n.is_empty());
        (0..count).map(|i| (names.next().unwrap_or("unknown").to_string(), FIRST_FD + i)).collect()
    }

    /// The descriptors the engine taken over from passed: `DUCTUS_LISTEN_FDS`, `name=fd` pairs
    /// separated by spaces (names never hold either).
    pub(super) fn inherited(list: Option<&str>) -> Vec<(String, RawFd)> {
        list.unwrap_or_default()
            .split(' ')
            .filter_map(|pair| {
                let (name, fd) = pair.split_once('=')?;
                Some((name.to_string(), fd.parse::<RawFd>().ok().filter(|fd| *fd > 2)?))
            })
            .collect()
    }

    fn close_on_exec(fd: BorrowedFd) {
        // SAFETY: fcntl on a descriptor we own; F_SETFD takes an int.
        unsafe {
            let flags = libc::fcntl(fd.as_raw_fd(), libc::F_GETFD);
            if flags >= 0 {
                libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, flags | libc::FD_CLOEXEC);
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::time::Duration;

        /// A stand-in for systemd's notify socket.
        struct FakeSystemd {
            socket: UnixDatagram,
            _dir: tempfile::TempDir,
            path: std::path::PathBuf,
        }

        impl FakeSystemd {
            fn new() -> Self {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("notify");
                let socket = UnixDatagram::bind(&path).unwrap();
                socket.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                Self { socket, _dir: dir, path }
            }

            /// The service's process, started with `kept`.
            fn systemd(&self, kept: Vec<(String, OwnedFd)>) -> Sockets {
                Sockets::with_fds(Some(self.path.as_os_str().as_bytes()), kept, true)
            }

            /// A takeover, with the sockets the engine it takes over from passed it.
            fn taking_over(&self, inherited: Vec<(String, OwnedFd)>) -> Sockets {
                Sockets::with_fds(Some(self.path.as_os_str().as_bytes()), inherited, false)
            }

            /// The next datagram and the descriptor it carried.
            fn receive(&self) -> (String, Option<OwnedFd>) {
                let mut buffer = [0u8; 512];
                let mut iov = libc::iovec { iov_base: buffer.as_mut_ptr().cast(), iov_len: buffer.len() };
                let mut control = [0u64; 8];
                // SAFETY: as in `send`, the other way round: buffers alive and sized for recvmsg.
                unsafe {
                    let mut message: libc::msghdr = std::mem::zeroed();
                    message.msg_iov = &mut iov;
                    message.msg_iovlen = 1;
                    message.msg_control = control.as_mut_ptr().cast();
                    message.msg_controllen = size_of_val(&control) as _;
                    let n = libc::recvmsg(self.socket.as_raw_fd(), &mut message, 0);
                    assert!(n >= 0, "nothing from the engine: {}", io::Error::last_os_error());
                    let header = libc::CMSG_FIRSTHDR(&message);
                    let fd = (!header.is_null() && (*header).cmsg_type == libc::SCM_RIGHTS).then(|| {
                        OwnedFd::from_raw_fd(std::ptr::read_unaligned(libc::CMSG_DATA(header).cast::<RawFd>()))
                    });
                    (String::from_utf8_lossy(&buffer[..n as usize]).into_owned(), fd)
                }
            }

            fn nothing_more(&self) -> bool {
                self.socket.set_nonblocking(true).unwrap();
                let quiet = self.socket.recv(&mut [0u8; 512]).is_err();
                self.socket.set_nonblocking(false).unwrap();
                quiet
            }
        }

        fn free_port() -> u16 {
            std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
        }

        #[test]
        fn names_are_what_systemd_accepts() {
            assert_eq!(name("0.0.0.0", 2575), "tcp-0.0.0.0-2575");
            assert_eq!(name("::", 7700), "tcp-__-7700");
            assert_eq!(name("fe80::1%eth0", 80), "tcp-fe80__1_eth0-80");
            assert_eq!(name(&"h".repeat(300), 1).len(), 255);
        }

        #[test]
        fn only_this_process_s_descriptors_are_taken() {
            let me = std::process::id();
            let pid = me.to_string();
            assert_eq!(
                passed(Some(&pid), Some("2"), Some("tcp-a-1:tcp-b-2"), me),
                vec![("tcp-a-1".to_string(), 3), ("tcp-b-2".to_string(), 4)]
            );
            assert_eq!(passed(Some(&pid), Some("1"), None, me), vec![("unknown".to_string(), 3)]);
            assert!(passed(Some("1"), Some("2"), Some("a:b"), me).is_empty(), "another process's");
            assert!(passed(None, Some("2"), Some("a:b"), me).is_empty());
            assert!(passed(Some(&pid), Some("x"), None, me).is_empty());
        }

        #[tokio::test]
        async fn a_new_listener_is_given_to_systemd_by_its_address() {
            let fake = FakeSystemd::new();
            let systemd = fake.systemd(Vec::new());
            let port = free_port();
            let listener = systemd.listen("127.0.0.1", port).await.unwrap();
            let (state, fd) = fake.receive();
            assert_eq!(state, format!("FDSTORE=1\nFDNAME=tcp-127.0.0.1-{port}"));
            // systemd's copy is the same listening socket: it takes connections after ours closes.
            let kept = std::net::TcpListener::from(fd.expect("the socket"));
            assert_eq!(kept.local_addr().unwrap().port(), port);
            drop(listener);
            let _sender = std::net::TcpStream::connect(("127.0.0.1", port)).expect("waits in the backlog");
            assert!(kept.accept().is_ok());
        }

        #[tokio::test]
        async fn a_kept_socket_is_listened_on_without_binding() {
            let fake = FakeSystemd::new();
            let original = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = original.local_addr().unwrap().port();
            let kept = OwnedFd::from(original.try_clone().unwrap());
            let systemd = fake.systemd(vec![(name("127.0.0.1", port), kept)]);
            // Binding would fail: `original` still listens there.
            let listener = systemd.listen("127.0.0.1", port).await.unwrap();
            let _sender = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
            tokio::time::timeout(Duration::from_secs(2), listener.accept()).await.unwrap().unwrap();
            assert!(fake.nothing_more(), "already in the store: not stored again");
        }

        #[tokio::test]
        async fn sockets_no_longer_configured_are_closed_and_removed() {
            let fake = FakeSystemd::new();
            let (a, b) = (free_port(), free_port());
            let kept = |port: u16| {
                let listener = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
                (name("127.0.0.1", port), OwnedFd::from(listener))
            };
            let systemd = fake.systemd(vec![kept(a), kept(b)]);
            systemd.keep_only([name("127.0.0.1", a)].into());
            assert_eq!(fake.receive().0, format!("FDSTOREREMOVE=1\nFDNAME=tcp-127.0.0.1-{b}"));
            assert!(fake.nothing_more());
            // Ours is closed (the port is free again for whatever is configured now) ...
            assert!(super::super::tests::free(b), "b's port is free");
            // ... and the configured one is still there to listen on.
            let listener = systemd.listen("127.0.0.1", a).await.unwrap();
            assert_eq!(listener.local_addr().unwrap().port(), a);
        }

        #[tokio::test]
        async fn a_drain_removes_the_socket_a_stop_keeps_it() {
            let fake = FakeSystemd::new();
            let systemd = fake.systemd(Vec::new());
            let port = free_port();
            let listener = systemd.listen("127.0.0.1", port).await.unwrap();
            fake.receive();
            systemd.release(&name("127.0.0.1", port));
            assert_eq!(fake.receive().0, format!("FDSTOREREMOVE=1\nFDNAME=tcp-127.0.0.1-{port}"));
            drop(listener);

            let _listener = systemd.listen("127.0.0.1", port).await.unwrap();
            assert!(fake.receive().0.starts_with("FDSTORE=1\n"), "stored again when it opens again");
            systemd.stopping(true);
            assert_eq!(fake.receive().0, "STOPPING=1\nSTATUS=stopping: finishing the messages in hand");
            systemd.release(&name("127.0.0.1", port));
            assert!(fake.nothing_more(), "kept for the next start");
            assert_eq!(systemd.copies().unwrap().len(), 1, "and for a handover");
        }

        #[tokio::test]
        async fn listening_again_takes_the_socket_still_open() {
            let fake = FakeSystemd::new();
            let systemd = fake.systemd(Vec::new());
            let port = free_port();
            drop(systemd.listen("127.0.0.1", port).await.unwrap());
            fake.receive();
            // Still open (systemd's copy, our handover copy): binding would fail with "in use".
            let listener = systemd.listen("127.0.0.1", port).await.unwrap();
            assert_eq!(listener.local_addr().unwrap().port(), port);
            assert!(fake.nothing_more(), "already in the store");
        }

        #[tokio::test]
        async fn a_handover_passes_on_every_listening_socket_but_a_drained_one() {
            let systemd = Sockets::with_fds(None, Vec::new(), true); // outside systemd: the same
            let (a, b) = (free_port(), free_port());
            let _a = systemd.listen("127.0.0.1", a).await.unwrap();
            let held_b = systemd.listen("127.0.0.1", b).await.unwrap();
            let names = |copies: Vec<(String, OwnedFd)>| {
                let mut names: Vec<String> = copies
                    .into_iter()
                    .map(|(name, fd)| {
                        let port = std::net::TcpListener::from(fd).local_addr().unwrap().port();
                        assert!(name.ends_with(&format!("-{port}")), "{name} is on {port}");
                        name
                    })
                    .collect();
                names.sort();
                names
            };
            let mut both = vec![name("127.0.0.1", a), name("127.0.0.1", b)];
            both.sort();
            assert_eq!(names(systemd.copies().unwrap()), both);
            systemd.release(&name("127.0.0.1", b));
            drop(held_b);
            assert_eq!(names(systemd.copies().unwrap()), vec![name("127.0.0.1", a)]);
            assert!(super::super::tests::free(b), "the drained one is closed for real");
        }

        #[tokio::test]
        async fn a_takeover_tells_systemd_nothing_until_it_is_the_service_s() {
            let fake = FakeSystemd::new();
            let (a, b, c) = (free_port(), free_port(), free_port());
            let inherited = |port: u16| {
                let listener = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
                (name("127.0.0.1", port), OwnedFd::from(listener))
            };
            let new = fake.taking_over(vec![inherited(a), inherited(b)]);
            let _a = new.listen("127.0.0.1", a).await.unwrap();
            let _c = new.listen("127.0.0.1", c).await.unwrap();
            new.keep_only([name("127.0.0.1", a), name("127.0.0.1", c)].into());
            new.notify_main("READY=1");
            new.abandon();
            assert!(fake.nothing_more(), "systemd hears only the old engine until it hands over");

            // The old engine handed it the service: c is stored, b (no longer configured) goes.
            new.promote();
            let (stored, fd) = fake.receive();
            assert_eq!(stored, format!("FDSTORE=1\nFDNAME=tcp-127.0.0.1-{c}"));
            assert!(fd.is_some());
            assert_eq!(fake.receive().0, format!("FDSTOREREMOVE=1\nFDNAME=tcp-127.0.0.1-{b}"));
            assert!(fake.nothing_more(), "a was stored by the old engine");
            new.notify_main("READY=1");
            assert_eq!(fake.receive().0, "READY=1");
        }

        #[tokio::test]
        async fn the_old_engine_hands_over_quietly() {
            let fake = FakeSystemd::new();
            let old = fake.systemd(Vec::new());
            let port = free_port();
            let listener = old.listen("127.0.0.1", port).await.unwrap();
            fake.receive();
            old.stopping(false); // a handover's stop: systemd isn't told the service stops
            old.release(&name("127.0.0.1", port));
            drop(listener);
            assert!(fake.nothing_more(), "the socket stays: the new engine listens on it");
            old.hand_to(4242);
            assert_eq!(fake.receive().0, "MAINPID=4242");
            old.notify_main("STOPPING=1");
            old.abandon();
            assert!(fake.nothing_more(), "the service is the new process's now");
        }

        #[test]
        fn handed_over_sockets_are_read_from_their_list() {
            assert_eq!(
                inherited(Some("tcp-0.0.0.0-2575=5 tcp-127.0.0.1-7700=6")),
                vec![("tcp-0.0.0.0-2575".to_string(), 5), ("tcp-127.0.0.1-7700".to_string(), 6)]
            );
            assert!(inherited(None).is_empty());
            assert!(inherited(Some("tcp-a-1=x tcp-b-2 tcp-c-3=1")).is_empty(), "not a descriptor, none, stdout");
        }

        #[tokio::test]
        async fn a_failed_start_gives_every_socket_back() {
            let fake = FakeSystemd::new();
            let unused = free_port();
            let kept = std::net::TcpListener::bind(("127.0.0.1", unused)).unwrap();
            let systemd = fake.systemd(vec![(name("127.0.0.1", unused), OwnedFd::from(kept))]);
            let port = free_port();
            let _listener = systemd.listen("127.0.0.1", port).await.unwrap();
            fake.receive();
            systemd.abandon();
            let mut removed = vec![fake.receive().0, fake.receive().0];
            removed.sort();
            let mut expected = vec![
                format!("FDSTOREREMOVE=1\nFDNAME=tcp-127.0.0.1-{port}"),
                format!("FDSTOREREMOVE=1\nFDNAME=tcp-127.0.0.1-{unused}"),
            ];
            expected.sort();
            assert_eq!(removed, expected);
        }

        #[test]
        fn an_abstract_notify_socket_is_reached() {
            let name = format!("ductus-test-{}", std::process::id());
            let fake = UnixDatagram::bind_addr(&SocketAddr::from_abstract_name(name.as_bytes()).unwrap()).unwrap();
            fake.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            Sockets::with_fds(Some(format!("@{name}").as_bytes()), Vec::new(), true).notify_main("READY=1");
            let mut buffer = [0u8; 64];
            let n = fake.recv(&mut buffer).unwrap();
            assert_eq!(&buffer[..n], b"READY=1");
        }
    }
}
