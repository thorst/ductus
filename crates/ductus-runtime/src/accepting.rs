//! What a listener accepts connections with (its TLS and its `allow:` list), swappable while it
//! listens: a deploy in place that changes an interface's `tls:` or `allow:` swaps them without
//! closing its port (0.7.5.2.2). Every MLLP and HTTP listener (and the REST API) accepts through
//! one.
//!
//! - **A new connection** uses what is current when it is accepted: the list is checked before
//!   anything is read, then the TLS handshake (on the connection's own task) with that TLS.
//! - **An open connection keeps its TLS session** (the certificate it was shown) until it
//!   reconnects, as with any server whose certificate is renewed.
//! - **An open connection the new `allow:` list refuses is closed after the message in hand**
//!   (Todd's answer to 0.7.5.2's Q3: a removed address is usually a security decision, and MLLP
//!   connections stay open for weeks). Each accepted connection is an `Admitted` stream that
//!   reads as ended once revoked, but only when what it last read has been answered (written and
//!   flushed): an MLLP sender gets the ACK of the message being stored, then the close; one in the
//!   middle of sending a frame has it read whole, stored and ACKed first. An HTTP connection
//!   between requests closes at once; a response written in several flushes (a large
//!   respond-mode body) can be cut short, and the caller sees a closed connection.

use crate::tls::{Admission, Conn};
use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::task::{Context, Poll, Waker};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio_util::sync::{CancellationToken, WaitForCancellationFutureOwned};

/// A listener's TLS (none: plain) and who may connect (none: anyone who can reach it).
pub struct Settings {
    pub tls: Option<tokio_rustls::TlsAcceptor>,
    pub admission: Admission,
}

/// One listener's settings now and its open connections.
pub struct Accepting {
    /// How the log names the listener: `interface `adt` (MLLP)`, `HTTPS 0.0.0.0:8443`.
    label: String,
    settings: RwLock<Arc<Settings>>,
    /// Each open connection's peer and its revocation.
    open: Mutex<HashMap<u64, (SocketAddr, CancellationToken)>>,
    next: AtomicU64,
}

impl Accepting {
    pub fn new(label: &str, tls: Option<Arc<rustls::ServerConfig>>, admission: Admission) -> Arc<Self> {
        Arc::new(Self {
            label: label.to_string(),
            settings: RwLock::new(Arc::new(Settings { tls: tls.map(tokio_rustls::TlsAcceptor::from), admission })),
            open: Mutex::default(),
            next: AtomicU64::new(0),
        })
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    /// The settings now.
    pub fn settings(&self) -> Arc<Settings> {
        self.settings.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// A connection just accepted: keepalive on, and the settings to open it with when its
    /// address may connect (None: drop it unread, the refusal counted and logged).
    pub fn admits(&self, tcp: &TcpStream, peer: SocketAddr) -> Option<Arc<Settings>> {
        crate::allow::keepalive(tcp);
        let settings = self.settings();
        settings.admission.as_ref().is_none_or(|a| a.admits(peer)).then_some(settings)
    }

    /// Open an admitted connection with `settings` (its TLS handshake, within the handshake's
    /// timeout; a refused one is logged) and keep it among the open ones. None: the handshake
    /// failed.
    pub async fn open(self: &Arc<Self>, settings: &Settings, tcp: TcpStream, peer: SocketAddr) -> Option<Admitted> {
        let conn = match &settings.tls {
            None => Conn::Plain(tcp),
            Some(acceptor) => Conn::Tls(Box::new(crate::tls::handshake(acceptor, &self.label, tcp, peer).await?)),
        };
        Some(self.register(conn, peer))
    }

    /// `admits` then `open`: what an MLLP listener does on each connection's own task.
    pub async fn accept(self: &Arc<Self>, tcp: TcpStream, peer: SocketAddr) -> Option<Admitted> {
        let settings = self.admits(&tcp, peer)?;
        self.open(&settings, tcp, peer).await
    }

    fn register(self: &Arc<Self>, conn: Conn, peer: SocketAddr) -> Admitted {
        let token = CancellationToken::new();
        let id = self.next.fetch_add(1, Relaxed);
        self.open.lock().unwrap_or_else(|e| e.into_inner()).insert(id, (peer, token.clone()));
        // The list may have changed during the handshake (after `swap` judged the open ones).
        if self.settings().admission.as_ref().is_some_and(|a| !a.permits(peer)) {
            token.cancel();
        }
        Admitted {
            conn,
            revoked: Box::pin(token.clone().cancelled_owned()),
            token,
            answered: true,
            wrote: false,
            reader: None,
            _open: Registration { accepting: Arc::downgrade(self), id },
        }
    }

    /// Accept with `tls` and `admission` from now on. Open connections keep their TLS session;
    /// those from addresses `admission` refuses are closed after the message in hand (each
    /// logged). Returns how many.
    pub fn swap(&self, tls: Option<Arc<rustls::ServerConfig>>, admission: Admission) -> usize {
        let settings = Arc::new(Settings { tls: tls.map(tokio_rustls::TlsAcceptor::from), admission });
        *self.settings.write().unwrap_or_else(|e| e.into_inner()) = settings.clone();
        let Some(admission) = &settings.admission else { return 0 };
        let open = self.open.lock().unwrap_or_else(|e| e.into_inner());
        let mut closed = 0;
        for (peer, token) in open.values() {
            if !token.is_cancelled() && !admission.permits(*peer) {
                tracing::info!(listener = %self.label, %peer, "allow: closing a connection its new list refuses, after the message in hand");
                token.cancel();
                closed += 1;
            }
        }
        closed
    }

    /// How many connections are open.
    pub fn open_connections(&self) -> usize {
        self.open.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

/// Takes an `Admitted` connection off its listener's open ones when it closes.
struct Registration {
    accepting: Weak<Accepting>,
    id: u64,
}

impl Drop for Registration {
    fn drop(&mut self) {
        if let Some(accepting) = self.accepting.upgrade() {
            accepting.open.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.id);
        }
    }
}

/// A connection a listener accepted (plain, or TLS after its handshake), which reads as ended
/// once its listener's new `allow:` list refuses it and what it last read has been answered.
pub struct Admitted {
    conn: Conn,
    token: CancellationToken,
    revoked: Pin<Box<WaitForCancellationFutureOwned>>,
    /// Everything read so far has been answered (written, then flushed).
    answered: bool,
    /// Something was written since the last read.
    wrote: bool,
    /// A read waiting while what it read before is unanswered: woken once answered.
    reader: Option<Waker>,
    _open: Registration,
}

impl Admitted {
    pub fn get_ref(&self) -> &Conn {
        &self.conn
    }
}

impl AsyncRead for Admitted {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        let me = self.get_mut();
        // Revoked with nothing left to answer: the end, as if the peer closed.
        if me.answered && (me.token.is_cancelled() || me.revoked.as_mut().poll(cx).is_ready()) {
            return Poll::Ready(Ok(()));
        }
        let before = buf.filled().len();
        let polled = Pin::new(&mut me.conn).poll_read(cx, buf);
        match &polled {
            Poll::Ready(Ok(())) if buf.filled().len() > before => {
                me.answered = false;
                me.wrote = false;
            }
            Poll::Pending if !me.answered => me.reader = Some(cx.waker().clone()),
            _ => {}
        }
        polled
    }
}

impl AsyncWrite for Admitted {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
        let me = self.get_mut();
        let polled = Pin::new(&mut me.conn).poll_write(cx, buf);
        if matches!(polled, Poll::Ready(Ok(n)) if n > 0) {
            me.wrote = true;
        }
        polled
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<std::io::Result<usize>> {
        let me = self.get_mut();
        let polled = Pin::new(&mut me.conn).poll_write_vectored(cx, bufs);
        if matches!(polled, Poll::Ready(Ok(n)) if n > 0) {
            me.wrote = true;
        }
        polled
    }

    fn is_write_vectored(&self) -> bool {
        self.conn.is_write_vectored()
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let me = self.get_mut();
        let polled = Pin::new(&mut me.conn).poll_flush(cx);
        if matches!(polled, Poll::Ready(Ok(()))) && me.wrote && !me.answered {
            me.answered = true;
            // A read waiting on the peer: let it see the end now, if revoked meanwhile.
            if me.token.is_cancelled()
                && let Some(reader) = me.reader.take()
            {
                reader.wake();
            }
        }
        polled
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().conn).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Who may connect: `allow` (addresses only, no names).
    fn admission(allow: &[&str]) -> Admission {
        let allow = ductus_core::allow::Allow(allow.iter().map(|a| a.to_string()).collect());
        let names = crate::allow::Names::new([], crate::allow::system_resolver());
        Some(crate::allow::Admission::new("test-accepting", "127.0.0.1", 0, &allow, names).unwrap())
    }

    /// A line echo server (one answer per line read) accepting through `accepting`.
    async fn echo(accepting: Arc<Accepting>) -> SocketAddr {
        let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = tcp.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (stream, peer) = tcp.accept().await.unwrap();
                let accepting = accepting.clone();
                tokio::spawn(async move {
                    let Some(mut conn) = accepting.accept(stream, peer).await else { return };
                    let mut line = Vec::new();
                    let mut byte = [0u8; 1];
                    while let Ok(1) = conn.read(&mut byte).await {
                        line.push(byte[0]);
                        if byte[0] == b'\n' {
                            // Slow enough that a swap lands while a line is being answered.
                            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                            if conn.write_all(&line).await.is_err() || conn.flush().await.is_err() {
                                return;
                            }
                            line.clear();
                        }
                    }
                });
            }
        });
        addr
    }

    async fn read_line(tcp: &mut TcpStream) -> Option<String> {
        let mut line = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            match tokio::time::timeout(std::time::Duration::from_secs(5), tcp.read(&mut byte)).await.expect("in time") {
                Ok(1) => {
                    line.push(byte[0]);
                    if byte[0] == b'\n' {
                        return Some(String::from_utf8(line).unwrap());
                    }
                }
                _ => return None,
            }
        }
    }

    /// Connect to `addr` from `from` (Linux: every 127.x address is this machine).
    async fn connect_from(from: &str, addr: SocketAddr) -> TcpStream {
        let socket = tokio::net::TcpSocket::new_v4().unwrap();
        socket.bind(format!("{from}:0").parse().unwrap()).unwrap();
        socket.connect(addr).await.unwrap()
    }

    /// A narrower list: the open connection it refuses gets the answer to what it sent, then is
    /// closed; one it still lets in stays open; new connections are judged by it.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_narrower_list_closes_refused_connections_after_their_answer() {
        let accepting = Accepting::new("test", None, admission(&["127.0.0.0/8"]));
        let addr = echo(accepting.clone()).await;
        let mut kept = connect_from("127.0.0.2", addr).await;
        let mut refused = connect_from("127.0.0.3", addr).await;
        for tcp in [&mut kept, &mut refused] {
            tcp.write_all(b"one\n").await.unwrap();
            assert_eq!(read_line(tcp).await.as_deref(), Some("one\n"));
        }
        assert_eq!(accepting.open_connections(), 2);

        // `refused` sends; the list narrows while its line is being answered: the answer comes,
        // then the end.
        refused.write_all(b"two\n").await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert_eq!(accepting.swap(None, admission(&["127.0.0.2"])), 1, "only the refused one");
        assert_eq!(read_line(&mut refused).await.as_deref(), Some("two\n"), "answered first");
        assert_eq!(read_line(&mut refused).await, None, "then closed");

        kept.write_all(b"three\n").await.unwrap();
        assert_eq!(read_line(&mut kept).await.as_deref(), Some("three\n"), "still open");

        // A new connection from the refused address is refused unread.
        let mut new = connect_from("127.0.0.3", addr).await;
        let _ = new.write_all(b"four\n").await;
        assert_eq!(read_line(&mut new).await, None);
    }

    /// Revoked while idle (nothing unanswered): closed at once.
    #[tokio::test]
    async fn an_idle_refused_connection_closes_at_once() {
        let accepting = Accepting::new("test", None, None);
        let addr = echo(accepting.clone()).await;
        let mut tcp = TcpStream::connect(addr).await.unwrap();
        tcp.write_all(b"one\n").await.unwrap();
        assert_eq!(read_line(&mut tcp).await.as_deref(), Some("one\n"));
        assert_eq!(accepting.swap(None, admission(&["10.9.9.9"])), 1);
        assert_eq!(read_line(&mut tcp).await, None);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(accepting.open_connections(), 0, "taken off when it closed");
    }
}
