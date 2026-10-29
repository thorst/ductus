//! Every listening socket the engine opens goes through here (`listen`), so it outlives the
//! process that opened it (Linux):
//!
//! **systemd keeps them** (under the package's `Type=notify` unit). A restart used to refuse
//! connections: between the old process closing its listeners and the new one binding them again
//! (the start takes ~0.5–1 s) nothing listened on the port. Now every listening socket is also
//! handed to systemd's file descriptor store when it's opened (`FDSTORE=1`, named after its
//! configured address). systemd holds its copy while the service restarts or after a crash and
//! hands it to the next start (`LISTEN_FDS`, `LISTEN_FDNAMES`), which listens on it instead of
//! binding. The socket never closes in between, so senders' connections wait in the kernel's
//! backlog until the new process accepts them: nothing is refused, senders wait for the start.
//!
//! **A handover passes them on** (`crate::handover`): the engine keeps a copy of each socket it
//! listens on (`handover_sockets`); the new engine inherits them (`DUCTUS_LISTEN_FDS`) and
//! listens on them as on systemd's, while the old one still does. Until the old engine hands it
//! the service (`MAINPID`), systemd listens only to the old one (`NotifyAccess=main`): the new
//! one stores nothing and gives nothing back until then (`promote`).
//!
//! Three things close a socket on purpose, and each takes it out of the store first
//! (`FDSTOREREMOVE=1`), or systemd's copy would keep it listening with nobody behind it:
//! - a drain closes MLLP listeners so senders see the engine down (`release`; a stop and a
//!   handover keep them: `stopping`, `handing_over`);
//! - a socket no longer configured is dropped at start, before anything binds (`keep_only`: it
//!   would hold its port);
//! - a start that fails (`abandon`): a broken engine is refused as before, not a port that takes
//!   connections and never answers. systemd itself closes the store when the service stops.
//!
//! Also `READY=1` once every interface is up (`systemctl start` waits for it) and `STOPPING=1`.
//! Outside systemd (no `NOTIFY_SOCKET`: `ductus dev`, containers) nothing is stored, and outside
//! Linux `listen` binds as before and the rest does nothing.

use std::io;
use tokio::net::TcpListener;

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
    #[cfg(target_os = "linux")]
    std::sync::LazyLock::force(&linux::SOCKETS);
}

/// Listen on `host:port`: on the socket kept for it when there is one (systemd's, or the engine
/// taken over from), else on a new one (which systemd is then given to keep).
pub async fn listen(host: &str, port: u16) -> io::Result<TcpListener> {
    #[cfg(target_os = "linux")]
    return linux::SOCKETS.listen(host, port).await;
    #[cfg(not(target_os = "linux"))]
    TcpListener::bind((host, port)).await
}

/// The listener on `host:port` is about to close for a while (a drain): systemd mustn't keep it
/// listening, nor a handover pass it on. Nothing while the engine stops (the next start, or the
/// engine taking over, listens on it).
pub fn release(host: &str, port: u16) {
    #[cfg(target_os = "linux")]
    linux::SOCKETS.release(&name(host, port));
    #[cfg(not(target_os = "linux"))]
    let _ = (host, port);
}

/// Before anything listens: close the sockets kept for addresses no longer configured.
pub fn keep_only<'a>(addresses: impl IntoIterator<Item = (&'a str, u16)>) {
    #[cfg(target_os = "linux")]
    linux::SOCKETS.keep_only(addresses.into_iter().map(|(host, port)| name(host, port)).collect());
    #[cfg(not(target_os = "linux"))]
    let _ = addresses.into_iter().count();
}

/// Every interface and the API are up: `systemctl start` returns.
pub fn ready(status: &str) {
    #[cfg(target_os = "linux")]
    linux::SOCKETS.notify_main(&format!("READY=1\nSTATUS={status}"));
    #[cfg(not(target_os = "linux"))]
    let _ = status;
}

/// The engine is stopping: listeners that close from now on stay in the store.
pub fn stopping() {
    #[cfg(target_os = "linux")]
    linux::SOCKETS.stopping(true);
}

/// The engine stops for a handover: as `stopping`, but systemd isn't told the service stops (it
/// goes on in the new process).
pub fn handing_over() {
    #[cfg(target_os = "linux")]
    linux::SOCKETS.stopping(false);
}

/// The start failed: give every socket back, so senders are refused while the engine is down
/// (systemd tries the start again in a few seconds; the next one binds them anew). Nothing in a
/// takeover that hasn't taken over: the sockets are still the old engine's.
pub fn abandon() {
    #[cfg(target_os = "linux")]
    linux::SOCKETS.abandon();
}

/// A copy of every socket listening now, by name: what a handover passes on.
#[cfg(target_os = "linux")]
pub fn handover_sockets() -> io::Result<Vec<(String, std::os::fd::OwnedFd)>> {
    linux::SOCKETS.copies()
}

/// A handover: the service's process is `pid` from now on (this one stops telling systemd anything).
pub fn main_pid(pid: u32) {
    #[cfg(target_os = "linux")]
    linux::SOCKETS.hand_to(pid);
    #[cfg(not(target_os = "linux"))]
    let _ = pid;
}

/// A takeover took over: this process is the service's now. systemd gets the sockets this engine
/// opened and loses the ones it no longer uses (`ready` follows once it sends).
pub fn promote() {
    #[cfg(target_os = "linux")]
    linux::SOCKETS.promote();
}

#[cfg(target_os = "linux")]
mod linux {
    use super::name;
    use std::collections::{HashMap, HashSet};
    use std::io;
    use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::net::{SocketAddr, UnixDatagram};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{LazyLock, Mutex};
    use tokio::net::TcpListener;

    pub(super) static SOCKETS: LazyLock<Sockets> = LazyLock::new(Sockets::from_env);

    /// The first descriptor systemd passes (`SD_LISTEN_FDS_START`).
    const FIRST_FD: RawFd = 3;

    pub(super) struct Sockets {
        /// systemd's notify socket (`NOTIFY_SOCKET`: a path, or `@name` for an abstract one).
        /// None outside systemd.
        notify: Option<Vec<u8>>,
        /// Sockets handed to this start that nothing has listened on yet, by name.
        kept: Mutex<HashMap<String, OwnedFd>>,
        /// A copy of every socket listening now, by name (a handover passes them on).
        open: Mutex<HashMap<String, OwnedFd>>,
        /// Every name in systemd's store as far as this process knows (for `abandon`, `promote`).
        stored: Mutex<HashSet<String>>,
        stopping: AtomicBool,
        /// systemd listens to this process (`NotifyAccess=main`). False in a takeover until the
        /// old engine hands it the service, and in the old engine once it did.
        main: AtomicBool,
    }

    impl Sockets {
        fn from_env() -> Self {
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
            Self::new(notify.as_deref(), kept, !takeover)
        }

        pub(super) fn new(notify: Option<&[u8]>, kept: Vec<(String, OwnedFd)>, main: bool) -> Self {
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
            }
        }

        fn is_main(&self) -> bool {
            self.main.load(Ordering::SeqCst)
        }

        pub(super) async fn listen(&self, host: &str, port: u16) -> io::Result<TcpListener> {
            let name = name(host, port);
            let kept = self.kept.lock().unwrap().remove(&name);
            // Listening again on a socket that's still open (after an accept error): its copy.
            let kept = match kept {
                Some(fd) => Some(fd),
                None => self.open.lock().unwrap().get(&name).map(|fd| fd.try_clone()).transpose()?,
            };
            let mut listener = None;
            if let Some(fd) = kept {
                let std = std::net::TcpListener::from(fd);
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
            let listener = match listener {
                Some(l) => l,
                None => {
                    let l = TcpListener::bind((host, port)).await?;
                    self.store(&name, l.as_fd());
                    l
                }
            };
            self.open.lock().unwrap().insert(name, listener.as_fd().try_clone_to_owned()?);
            Ok(listener)
        }

        /// Give `fd` to systemd's store (when this process is the service's; a takeover's are
        /// stored when it is: `promote`).
        fn store(&self, name: &str, fd: BorrowedFd) {
            if self.notify.is_none() || !self.is_main() {
                return;
            }
            match self.send(&format!("FDSTORE=1\nFDNAME={name}"), Some(fd)) {
                Ok(()) => {
                    self.stored.lock().unwrap().insert(name.to_string());
                }
                Err(e) => tracing::warn!(
                    socket = %name,
                    error = %e,
                    "systemd can't keep this listening socket (`FileDescriptorStoreMax=` in the unit): a restart refuses connections to it for a moment"
                ),
            }
        }

        pub(super) fn release(&self, name: &str) {
            if !self.stopping.load(Ordering::SeqCst) {
                self.open.lock().unwrap().remove(name);
                self.remove(name);
            }
        }

        pub(super) fn keep_only(&self, wanted: HashSet<String>) {
            let gone: Vec<(String, OwnedFd)> = {
                let mut kept = self.kept.lock().unwrap();
                let names: Vec<String> = kept.keys().filter(|n| !wanted.contains(*n)).cloned().collect();
                names.into_iter().filter_map(|n| kept.remove_entry(&n)).collect()
            };
            for (name, fd) in gone {
                tracing::info!(socket = %name, "no longer configured: closing the listening socket kept for it");
                drop(fd);
                self.remove(&name);
            }
        }

        /// `tell`: say so to systemd (not when a handover stops the engine: the service goes on).
        pub(super) fn stopping(&self, tell: bool) {
            self.stopping.store(true, Ordering::SeqCst);
            if tell {
                self.notify_main("STOPPING=1\nSTATUS=stopping: finishing the messages in hand");
            }
        }

        pub(super) fn abandon(&self) {
            if !self.is_main() {
                return;
            }
            self.kept.lock().unwrap().clear();
            let names: Vec<String> = self.stored.lock().unwrap().iter().cloned().collect();
            for name in names {
                self.remove(&name);
            }
        }

        pub(super) fn copies(&self) -> io::Result<Vec<(String, OwnedFd)>> {
            self.open.lock().unwrap().iter().map(|(name, fd)| Ok((name.clone(), fd.try_clone()?))).collect()
        }

        pub(super) fn hand_to(&self, pid: u32) {
            self.notify_main(&format!("MAINPID={pid}"));
            self.main.store(false, Ordering::SeqCst);
        }

        pub(super) fn promote(&self) {
            self.main.store(true, Ordering::SeqCst);
            if self.notify.is_none() {
                return;
            }
            let open: Vec<(String, OwnedFd)> = self
                .open
                .lock()
                .unwrap()
                .iter()
                .filter_map(|(n, fd)| Some((n.clone(), fd.try_clone().ok()?)))
                .collect();
            let stored = self.stored.lock().unwrap().clone();
            for (name, fd) in &open {
                if !stored.contains(name) {
                    self.store(name, fd.as_fd());
                }
            }
            for name in stored {
                if !open.iter().any(|(n, _)| *n == name) {
                    self.remove(&name);
                }
            }
        }

        /// Tell systemd `state`, when this process is the service's.
        pub(super) fn notify_main(&self, state: &str) {
            if self.notify.is_none() || !self.is_main() {
                return;
            }
            if let Err(e) = self.send(state, None) {
                tracing::warn!(error = %e, state, "couldn't tell systemd");
            }
        }

        fn remove(&self, name: &str) {
            if self.notify.is_none() || !self.is_main() {
                return;
            }
            self.stored.lock().unwrap().remove(name);
            if let Err(e) = self.send(&format!("FDSTOREREMOVE=1\nFDNAME={name}"), None) {
                tracing::warn!(socket = %name, error = %e, "couldn't take the listening socket out of systemd's store");
            }
        }

        /// One datagram to systemd's notify socket, with `fd` attached (`SCM_RIGHTS`). Connected
        /// anew each time, as `sd_notify` does: the socket is new after `systemctl daemon-reexec`.
        fn send(&self, state: &str, fd: Option<BorrowedFd>) -> io::Result<()> {
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
                Sockets::new(Some(self.path.as_os_str().as_bytes()), kept, true)
            }

            /// A takeover, with the sockets the engine it takes over from passed it.
            fn taking_over(&self, inherited: Vec<(String, OwnedFd)>) -> Sockets {
                Sockets::new(Some(self.path.as_os_str().as_bytes()), inherited, false)
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
            drop(std::net::TcpListener::bind(("127.0.0.1", b)).expect("b's port is free"));
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
            let systemd = Sockets::new(None, Vec::new(), true); // outside systemd: the same
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
            drop(std::net::TcpListener::bind(("127.0.0.1", b)).expect("the drained one is closed for real"));
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
            Sockets::new(Some(format!("@{name}").as_bytes()), Vec::new(), true).notify_main("READY=1");
            let mut buffer = [0u8; 64];
            let n = fake.recv(&mut buffer).unwrap();
            assert_eq!(&buffer[..n], b"READY=1");
        }
    }
}
