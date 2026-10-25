//! Outgoing TCP connections that are refused as promptly on Windows as elsewhere.
//!
//! Windows answers a refused connection (the host's reset) by sending the SYN again, 0.5 s
//! apart: by default it takes ~2 s to say "refused" (Linux: at once). Doctor's 2 s wait then
//! read a closed port as dropped traffic (a firewall), and a name whose first address has
//! nothing listening (`localhost` is `::1` first) cost 2 s before the next address was tried.
//! Our connections ask for two SYN retransmissions instead of the default: refused in ~1 s, and
//! a SYN lost on the way is still sent twice more (given up after ~7 s instead of ~21 s, longer
//! than any timeout of ours anyway). Measured on Windows 11: refused 2.03 s → ~1 s.

use std::io;
use std::net::SocketAddr;
use tokio::net::{TcpSocket, TcpStream, ToSocketAddrs};

/// SYN retransmissions on Windows (its default makes a refusal take ~2 s; none would give up on
/// the first lost SYN).
#[cfg(windows)]
const SYN_RETRANSMISSIONS: u8 = 2;

/// A socket for connecting to `address`, set up as every outgoing connection of ours is.
pub fn socket(address: SocketAddr) -> io::Result<TcpSocket> {
    let socket = if address.is_ipv4() { TcpSocket::new_v4()? } else { TcpSocket::new_v6()? };
    #[cfg(windows)]
    refuse_promptly(&socket);
    Ok(socket)
}

/// Connect to the first of `target`'s addresses that accepts, in the resolver's order (as
/// `TcpStream::connect`), the error being the last address's.
pub async fn connect(target: impl ToSocketAddrs) -> io::Result<TcpStream> {
    let mut last = None;
    for address in tokio::net::lookup_host(target).await? {
        match socket(address)?.connect(address).await {
            Ok(tcp) => return Ok(tcp),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "the name has no address")))
}

/// Best effort: a Windows without the option (before Windows 8) keeps its default.
#[cfg(windows)]
fn refuse_promptly(socket: &TcpSocket) {
    use std::os::windows::io::AsRawSocket;
    use windows_sys::Win32::Networking::WinSock::{SIO_TCP_INITIAL_RTO, TCP_INITIAL_RTO_PARAMETERS, WSAIoctl};
    // Rtt 0: the system's initial retransmission timeout.
    let parameters = TCP_INITIAL_RTO_PARAMETERS { Rtt: 0, MaxSynRetransmissions: SYN_RETRANSMISSIONS };
    let mut returned = 0u32;
    // SAFETY: the socket is open for the call's duration; the input is `parameters` with its own
    // size, there's no output buffer, and no overlapped I/O (the call completes at once).
    let failed = unsafe {
        WSAIoctl(
            socket.as_raw_socket() as _,
            SIO_TCP_INITIAL_RTO,
            std::ptr::from_ref(&parameters).cast(),
            size_of::<TCP_INITIAL_RTO_PARAMETERS>() as u32,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
            None,
        )
    } != 0;
    if failed {
        tracing::debug!(error = %io::Error::last_os_error(), "SIO_TCP_INITIAL_RTO not set");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[tokio::test]
    async fn a_closed_port_is_refused_within_a_second_and_a_half() {
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let started = Instant::now();
        let refused = connect(("127.0.0.1", free)).await.unwrap_err();
        assert_eq!(refused.kind(), io::ErrorKind::ConnectionRefused, "{refused}");
        // Windows's default is ~2 s (doctor's whole wait).
        assert!(started.elapsed() < Duration::from_millis(1500), "refused after {:?}", started.elapsed());
    }

    #[tokio::test]
    async fn a_name_goes_on_to_its_next_address() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let tcp = connect(format!("localhost:{port}")).await.unwrap();
        assert_eq!(tcp.peer_addr().unwrap(), SocketAddr::from(([127, 0, 0, 1], port)));
    }
}
