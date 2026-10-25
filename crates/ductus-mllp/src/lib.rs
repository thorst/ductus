//! MLLP (Minimal Lower Layer Protocol) framing, listener and client.
//!
//! A frame is `<VT> message <FS><CR>`.

use bytes::{Buf, Bytes, BytesMut};
use std::future::Future;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
pub use tokio_util::sync::CancellationToken;

pub mod net;

pub const START: u8 = 0x0b;
pub const END: [u8; 2] = [0x1c, 0x0d];

pub fn frame(message: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(message.len() + 3);
    out.push(START);
    out.extend_from_slice(message);
    out.extend_from_slice(&END);
    out
}

/// Take one complete frame's payload out of `buf`, if present. Bytes before START are discarded.
fn take_frame(buf: &mut BytesMut) -> Option<Bytes> {
    let start = buf.iter().position(|&b| b == START)?;
    let end = buf[start..].windows(2).position(|w| w == END)? + start;
    let payload = buf[start + 1..end].to_vec();
    buf.advance(end + 2);
    Some(Bytes::from(payload))
}

/// Read the next frame. `Ok(None)` means the peer closed the connection.
pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R, buf: &mut BytesMut) -> io::Result<Option<Bytes>> {
    loop {
        if let Some(frame) = take_frame(buf) {
            return Ok(Some(frame));
        }
        if reader.read_buf(buf).await? == 0 {
            return Ok(None);
        }
    }
}

pub async fn write_frame<W: AsyncWrite + Unpin>(writer: &mut W, message: &[u8]) -> io::Result<()> {
    writer.write_all(&frame(message)).await?;
    writer.flush().await
}

/// Accept connections forever. Each message is passed to `handler`, whose return value is sent back as the ACK.
/// Messages on one connection are handled strictly in order: the next is read only after the ACK is written.
pub async fn serve<H, F>(listener: TcpListener, handler: H) -> io::Result<()>
where
    H: Fn(Bytes) -> F + Clone + Send + Sync + 'static,
    F: Future<Output = Bytes> + Send,
{
    let handler = move |message| {
        let answer = handler(message);
        async move { Some(answer.await) }
    };
    serve_until(listener, handler, CancellationToken::new()).await
}

/// `serve` until `stop` is cancelled, then close the listener and every connection, so senders
/// see the receiver as down (connection refused) and keep their messages queued. A message
/// being handled when `stop` comes is still answered first; a half-received one is dropped
/// unanswered (the sender sends it again later). A handler answering `None` closes the
/// connection without an ACK, for a message that arrived too late to be taken.
pub async fn serve_until<H, F>(listener: TcpListener, handler: H, stop: CancellationToken) -> io::Result<()>
where
    H: Fn(Bytes) -> F + Clone + Send + Sync + 'static,
    F: Future<Output = Option<Bytes>> + Send,
{
    serve_with(listener, |tcp, _| async move { Some(tcp) }, handler, stop).await
}

/// `serve_until` over connections that need setting up first (a TLS handshake): `open` turns
/// each accepted connection into the stream messages are read from, or `None` to drop it. It
/// runs on the connection's own task, so a slow handshake holds up no other sender, and the
/// listener closes at once on `stop`.
pub async fn serve_with<O, OF, S, H, F>(
    listener: TcpListener,
    open: O,
    handler: H,
    stop: CancellationToken,
) -> io::Result<()>
where
    O: Fn(TcpStream, SocketAddr) -> OF + Clone + Send + Sync + 'static,
    OF: Future<Output = Option<S>> + Send,
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    H: Fn(Bytes) -> F + Clone + Send + Sync + 'static,
    F: Future<Output = Option<Bytes>> + Send,
{
    loop {
        let (tcp, peer) = tokio::select! {
            accepted = listener.accept() => accepted?,
            _ = stop.cancelled() => return Ok(()),
        };
        let (open, handler, stop) = (open.clone(), handler.clone(), stop.clone());
        tokio::spawn(async move {
            let opened = tokio::select! {
                opened = open(tcp, peer) => opened,
                _ = stop.cancelled() => return,
            };
            let Some(mut stream) = opened else { return };
            let mut buf = BytesMut::with_capacity(16 * 1024);
            loop {
                let frame = tokio::select! {
                    frame = read_frame(&mut stream, &mut buf) => frame,
                    _ = stop.cancelled() => break,
                };
                match frame {
                    Ok(Some(message)) => {
                        let Some(ack) = handler(message).await else { break };
                        if let Err(e) = write_frame(&mut stream, &ack).await {
                            tracing::warn!(%peer, error = %e, "failed to write ACK");
                            break;
                        }
                    }
                    Ok(None) => break,
                    // A TLS sender that hangs up without a close_notify: an ordinary close for
                    // MLLP (frames carry their own end; a half-received one is dropped anyway),
                    // and many senders do it.
                    Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                        tracing::debug!(%peer, error = %e, "connection closed");
                        break;
                    }
                    Err(e) => {
                        tracing::warn!(%peer, error = %e, "connection error");
                        break;
                    }
                }
            }
        });
    }
}

/// What a client's messages travel over: a TCP connection, or one set up on top of it (TLS).
pub trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}

impl<S: AsyncRead + AsyncWrite + Unpin + Send> Stream for S {}

type Opened = Pin<Box<dyn Future<Output = io::Result<Box<dyn Stream>>> + Send>>;
type Open = Arc<dyn Fn(TcpStream) -> Opened + Send + Sync>;

/// A persistent MLLP connection that reconnects on demand.
pub struct Client {
    addr: String,
    timeout: Duration,
    /// Turns a new TCP connection into the stream messages are sent on (a TLS handshake), within
    /// the send's timeout. None: plain TCP.
    open: Option<Open>,
    /// Connect from this local address (None: the OS picks).
    from: Option<IpAddr>,
    stream: Option<Box<dyn Stream>>,
    buf: BytesMut,
}

impl Client {
    pub fn new(addr: impl Into<String>, timeout: Duration) -> Self {
        Self { addr: addr.into(), timeout, open: None, from: None, stream: None, buf: BytesMut::new() }
    }

    /// Connect from `local` (one of this machine's addresses), as a sender elsewhere would:
    /// `ductus gen --from` tries a receiver's `allow:` list.
    pub fn from(self, local: IpAddr) -> Self {
        Self { from: Some(local), ..self }
    }

    /// A client whose connections need setting up first (a TLS handshake): `open` turns each new
    /// TCP connection into the stream messages are sent on. Its errors are the send's errors.
    pub fn with_connect<O, OF, S>(addr: impl Into<String>, timeout: Duration, open: O) -> Self
    where
        O: Fn(TcpStream) -> OF + Send + Sync + 'static,
        OF: Future<Output = io::Result<S>> + Send + 'static,
        S: Stream + 'static,
    {
        let open: Open = Arc::new(move |tcp| {
            let opened = open(tcp);
            Box::pin(async move { Ok(Box::new(opened.await?) as Box<dyn Stream>) })
        });
        Self { open: Some(open), ..Self::new(addr, timeout) }
    }

    /// Send one message and wait for its ACK payload. On any error the connection is dropped,
    /// so the next call reconnects.
    pub async fn send(&mut self, message: &[u8]) -> io::Result<Bytes> {
        let result = tokio::time::timeout(self.timeout, self.exchange(message)).await;
        match result {
            Ok(Ok(ack)) => Ok(ack),
            Ok(Err(e)) => {
                self.reset();
                Err(e)
            }
            Err(_) => {
                self.reset();
                Err(io::Error::new(io::ErrorKind::TimedOut, "timed out waiting for ACK"))
            }
        }
    }

    /// Send one message without waiting for an ACK (for receivers that don't send one).
    /// Anything the receiver does send is read and discarded so its buffer never fills.
    pub async fn send_no_ack(&mut self, message: &[u8]) -> io::Result<()> {
        let result = tokio::time::timeout(self.timeout, async {
            let stream = self.connect().await?;
            write_frame(stream, message).await?;
            let mut scratch = [0u8; 4096];
            loop {
                match try_read(stream, &mut scratch) {
                    Ok(Some(0)) => {
                        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "receiver closed the connection"));
                    }
                    Ok(Some(_)) => continue,
                    Ok(None) => return Ok(()),
                    Err(e) => return Err(e),
                }
            }
        })
        .await;
        match result {
            Ok(Ok(())) => Ok(()),
            // The message was written before the close: with no ACK, delivery can't be confirmed either way.
            Ok(Err(e)) if e.kind() == io::ErrorKind::UnexpectedEof => {
                self.reset();
                Ok(())
            }
            Ok(Err(e)) => {
                self.reset();
                Err(e)
            }
            Err(_) => {
                self.reset();
                Err(io::Error::new(io::ErrorKind::TimedOut, "timed out sending"))
            }
        }
    }

    async fn connect(&mut self) -> io::Result<&mut Box<dyn Stream>> {
        if self.stream.is_none() {
            let tcp = match self.from {
                None => net::connect(&self.addr).await?,
                Some(local) => {
                    let target = tokio::net::lookup_host(&self.addr)
                        .await?
                        .find(|a| a.is_ipv4() == local.is_ipv4())
                        .ok_or_else(|| io::Error::other(format!("{} has no address of {local}'s kind", self.addr)))?;
                    let socket = net::socket(target)?;
                    socket.bind(SocketAddr::new(local, 0))?;
                    socket.connect(target).await?
                }
            };
            tcp.set_nodelay(true)?;
            let stream = match &self.open {
                Some(open) => open(tcp).await?,
                None => Box::new(tcp),
            };
            self.stream = Some(stream);
            self.buf.clear();
        }
        Ok(self.stream.as_mut().expect("connected above"))
    }

    async fn exchange(&mut self, message: &[u8]) -> io::Result<Bytes> {
        let stream = self.connect().await?;
        write_frame(stream, message).await?;
        let stream = self.stream.as_mut().expect("connected");
        read_frame(stream, &mut self.buf)
            .await?
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "connection closed before ACK"))
    }

    fn reset(&mut self) {
        self.stream = None;
        self.buf.clear();
    }
}

/// Read what has already arrived, without waiting: `None` when nothing has.
fn try_read(stream: &mut Box<dyn Stream>, buf: &mut [u8]) -> io::Result<Option<usize>> {
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    let mut read = tokio::io::ReadBuf::new(buf);
    match Pin::new(stream).poll_read(&mut cx, &mut read) {
        std::task::Poll::Ready(Ok(())) => Ok(Some(read.filled().len())),
        std::task::Poll::Ready(Err(e)) => Err(e),
        std::task::Poll::Pending => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A listener that ACKs with `code`, counting messages.
    async fn receiver(code: &'static str) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c = count.clone();
        tokio::spawn(serve(listener, move |_m: Bytes| {
            c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async move { Bytes::from(format!("MSH|^~\\&|R||||||ACK|1|P|2.5\rMSA|{code}|1\r")) }
        }));
        (addr, count)
    }

    #[tokio::test]
    async fn client_round_trips_and_reconnects() {
        let (addr, count) = receiver("AA").await;
        let mut client = Client::new(addr, Duration::from_secs(2));
        for _ in 0..3 {
            let ack = client.send(b"MSH|^~\\&|A").await.unwrap();
            assert!(String::from_utf8_lossy(&ack).contains("MSA|AA|1"));
        }
        client.reset();
        client.send(b"MSH|^~\\&|A").await.unwrap();
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn send_without_ack_discards_replies() {
        let (addr, count) = receiver("AA").await;
        let mut client = Client::new(addr, Duration::from_secs(2));
        for _ in 0..50 {
            client.send_no_ack(b"MSH|^~\\&|A").await.unwrap();
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 50);
    }

    #[tokio::test]
    async fn stopping_closes_the_listener_and_open_connections() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let stop = CancellationToken::new();
        // "late" stands for a message that arrives after the gate closed: no ACK, just closed.
        let handler = |m: Bytes| async move { (!m.starts_with(b"late")).then(|| Bytes::from_static(b"ACK")) };
        let server = tokio::spawn(serve_until(listener, handler, stop.clone()));
        let mut client = Client::new(addr.clone(), Duration::from_secs(2));
        assert_eq!(&client.send(b"MSH|1").await.unwrap()[..], b"ACK");
        let err = client.send(b"late").await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof, "closed without an ACK");

        let mut open = Client::new(addr.clone(), Duration::from_secs(2));
        open.send(b"MSH|2").await.unwrap();
        stop.cancel();
        server.await.unwrap().unwrap();
        // The open connection is closed and new ones are refused.
        assert!(open.send(b"MSH|3").await.is_err());
        let refused = TcpStream::connect(&addr).await.unwrap_err();
        assert_eq!(refused.kind(), io::ErrorKind::ConnectionRefused);
    }

    #[tokio::test]
    async fn connections_are_opened_on_their_own_task_and_can_be_refused() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let stop = CancellationToken::new();
        // The first connection's setup never finishes; the second is refused; the third is served.
        let opened = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let open = move |tcp: TcpStream, _peer: SocketAddr| {
            let n = opened.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async move {
                match n {
                    0 => std::future::pending().await,
                    1 => None,
                    _ => Some(tcp),
                }
            }
        };
        let handler = |_m: Bytes| async move { Some(Bytes::from_static(b"ACK")) };
        let server = tokio::spawn(serve_with(listener, open, handler, stop.clone()));
        let _stuck = TcpStream::connect(&addr).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let mut refused = Client::new(addr.clone(), Duration::from_secs(2));
        assert!(refused.send(b"MSH|1").await.is_err(), "dropped without an answer");
        let mut served = Client::new(addr.clone(), Duration::from_secs(2));
        assert_eq!(&served.send(b"MSH|2").await.unwrap()[..], b"ACK", "not held up by the stuck one");
        stop.cancel();
        server.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn connections_are_set_up_by_the_connect_step_once_each() {
        let (addr, count) = receiver("AA").await;
        let opened = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let o = opened.clone();
        let mut client = Client::with_connect(addr.clone(), Duration::from_secs(2), move |tcp: TcpStream| {
            o.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async move { Ok(tcp) }
        });
        for _ in 0..3 {
            client.send(b"MSH|^~\\&|A").await.unwrap();
        }
        client.send_no_ack(b"MSH|^~\\&|A").await.unwrap();
        assert_eq!(opened.load(std::sync::atomic::Ordering::SeqCst), 1, "one connection, set up once");

        // A connect step that fails (a refused handshake) is the send's error; the next send
        // tries a new connection.
        let mut refused = Client::with_connect(addr, Duration::from_secs(2), |_tcp: TcpStream| async {
            Err::<TcpStream, _>(io::Error::new(io::ErrorKind::InvalidData, "handshake refused"))
        });
        let err = refused.send(b"MSH|^~\\&|A").await.unwrap_err();
        assert_eq!(err.to_string(), "handshake refused");
        assert!(refused.send_no_ack(b"MSH|^~\\&|A").await.is_err());
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 4, "nothing sent over a refused connection");
    }

    #[tokio::test]
    async fn unreachable_receiver_is_an_error() {
        let mut client = Client::new("127.0.0.1:1", Duration::from_millis(500));
        assert!(client.send(b"MSH|^~\\&|A").await.is_err());
        assert!(client.send_no_ack(b"MSH|^~\\&|A").await.is_err());
    }

    #[test]
    fn frames_round_trip() {
        let mut buf = BytesMut::new();
        buf.extend_from_slice(b"junk");
        buf.extend_from_slice(&frame(b"MSH|one"));
        buf.extend_from_slice(&frame(b"MSH|two"));
        buf.extend_from_slice(&[START, b'M']);
        assert_eq!(take_frame(&mut buf).as_deref(), Some(&b"MSH|one"[..]));
        assert_eq!(take_frame(&mut buf).as_deref(), Some(&b"MSH|two"[..]));
        assert_eq!(take_frame(&mut buf), None);
        assert_eq!(&buf[..], &[START, b'M']);
    }
}
