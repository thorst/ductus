//! MLLP (Minimal Lower Layer Protocol) framing, listener and client.
//!
//! A frame is `<VT> message <FS><CR>`.

use bytes::{Buf, Bytes, BytesMut};
use std::future::Future;
use std::io;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

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
    loop {
        let (mut stream, peer) = listener.accept().await?;
        let handler = handler.clone();
        tokio::spawn(async move {
            let mut buf = BytesMut::with_capacity(16 * 1024);
            loop {
                match read_frame(&mut stream, &mut buf).await {
                    Ok(Some(message)) => {
                        let ack = handler(message).await;
                        if let Err(e) = write_frame(&mut stream, &ack).await {
                            tracing::warn!(%peer, error = %e, "failed to write ACK");
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        tracing::warn!(%peer, error = %e, "connection error");
                        break;
                    }
                }
            }
        });
    }
}

/// A persistent MLLP connection that reconnects on demand.
pub struct Client {
    addr: String,
    timeout: Duration,
    stream: Option<TcpStream>,
    buf: BytesMut,
}

impl Client {
    pub fn new(addr: impl Into<String>, timeout: Duration) -> Self {
        Self { addr: addr.into(), timeout, stream: None, buf: BytesMut::new() }
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

    async fn exchange(&mut self, message: &[u8]) -> io::Result<Bytes> {
        if self.stream.is_none() {
            let stream = TcpStream::connect(&self.addr).await?;
            stream.set_nodelay(true)?;
            self.stream = Some(stream);
            self.buf.clear();
        }
        let stream = self.stream.as_mut().expect("connected above");
        write_frame(stream, message).await?;
        read_frame(stream, &mut self.buf)
            .await?
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "connection closed before ACK"))
    }

    fn reset(&mut self) {
        self.stream = None;
        self.buf.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
