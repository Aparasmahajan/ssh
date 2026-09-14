//! Port forwarding.
//!
//! CLAUDE.md §8.1: forwards "multiplex as channels over" the existing transport,
//! so starting one costs a channel open, not a handshake.
//!
//! Implemented here:
//!
//! * **Local (`-L`)** — bind a local port; each accepted connection opens a
//!   `direct-tcpip` channel and the two are pumped together.
//! * **Dynamic (`-D`)** — a local SOCKS5 proxy; the destination comes from each
//!   client's handshake rather than from configuration.
//!
//! * **Remote (`-R`)** — ask the server to listen; each connection it accepts
//!   arrives as a channel, which is then joined to a local TCP connection. The
//!   direction is inverted from `-L`, which is why it needs a callback on the
//!   client handler rather than a listener here.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

use crate::transport::{SshError, Transport};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForwardKind {
    Local,
    Dynamic,
    Remote,
}

/// Live counters for one forward, sampled by the UI.
///
/// §13.1 rule 8: "Progress via `AtomicU64` sampled by the UI at 10 Hz. Never one
/// message per chunk." A busy tunnel moves thousands of chunks a second; these
/// are read on a timer, never pushed.
#[derive(Default, Debug)]
pub struct ForwardStats {
    pub connections: AtomicU64,
    pub active: AtomicU64,
    pub bytes_out: AtomicU64,
    pub bytes_in: AtomicU64,
    pub errors: AtomicU64,
}

pub struct Forward {
    pub id: String,
    pub kind: ForwardKind,
    /// The address actually bound, which may differ from what was requested
    /// when port 0 was used to mean "pick one".
    pub bound: SocketAddr,
    pub target: Option<(String, u16)>,
    pub stats: Arc<ForwardStats>,
    stop: Option<oneshot::Sender<()>>,
}

impl Forward {
    /// Stop listening and drop in-flight connections.
    pub fn stop(&mut self) {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
    }
}

impl Drop for Forward {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Start a local forward: `bind_addr:bind_port` → `target_host:target_port`
/// as seen from the server.
pub async fn start_local(
    transport: Arc<Transport>,
    id: String,
    bind_addr: &str,
    bind_port: u16,
    target_host: String,
    target_port: u16,
) -> Result<Forward, SshError> {
    let listener = TcpListener::bind((bind_addr, bind_port))
        .await
        .map_err(|e| SshError::Channel(format!("bind {bind_addr}:{bind_port}: {e}")))?;
    let bound = listener
        .local_addr()
        .map_err(|e| SshError::Channel(e.to_string()))?;

    let stats = Arc::new(ForwardStats::default());
    let (stop_tx, mut stop_rx) = oneshot::channel();

    {
        let stats = Arc::clone(&stats);
        let target = (target_host.clone(), target_port);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut stop_rx => break,
                    accepted = listener.accept() => {
                        let Ok((sock, peer)) = accepted else { break };
                        let transport = Arc::clone(&transport);
                        let stats = Arc::clone(&stats);
                        let target = target.clone();
                        stats.connections.fetch_add(1, Ordering::Relaxed);
                        stats.active.fetch_add(1, Ordering::Relaxed);
                        tokio::spawn(async move {
                            if let Err(_e) = pump_direct(&transport, sock, peer, &target.0, target.1, &stats).await {
                                stats.errors.fetch_add(1, Ordering::Relaxed);
                            }
                            stats.active.fetch_sub(1, Ordering::Relaxed);
                        });
                    }
                }
            }
        });
    }

    Ok(Forward {
        id,
        kind: ForwardKind::Local,
        bound,
        target: Some((target_host, target_port)),
        stats,
        stop: Some(stop_tx),
    })
}

/// Start a SOCKS5 proxy that tunnels every connection over the transport.
pub async fn start_dynamic(
    transport: Arc<Transport>,
    id: String,
    bind_addr: &str,
    bind_port: u16,
) -> Result<Forward, SshError> {
    let listener = TcpListener::bind((bind_addr, bind_port))
        .await
        .map_err(|e| SshError::Channel(format!("bind {bind_addr}:{bind_port}: {e}")))?;
    let bound = listener
        .local_addr()
        .map_err(|e| SshError::Channel(e.to_string()))?;

    let stats = Arc::new(ForwardStats::default());
    let (stop_tx, mut stop_rx) = oneshot::channel();

    {
        let stats = Arc::clone(&stats);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut stop_rx => break,
                    accepted = listener.accept() => {
                        let Ok((mut sock, peer)) = accepted else { break };
                        let transport = Arc::clone(&transport);
                        let stats = Arc::clone(&stats);
                        stats.connections.fetch_add(1, Ordering::Relaxed);
                        stats.active.fetch_add(1, Ordering::Relaxed);
                        tokio::spawn(async move {
                            match socks5_handshake(&mut sock).await {
                                Ok((host, port)) => {
                                    if pump_direct(&transport, sock, peer, &host, port, &stats).await.is_err() {
                                        stats.errors.fetch_add(1, Ordering::Relaxed);
                                    }
                                }
                                Err(_) => {
                                    stats.errors.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                            stats.active.fetch_sub(1, Ordering::Relaxed);
                        });
                    }
                }
            }
        });
    }

    Ok(Forward {
        id,
        kind: ForwardKind::Dynamic,
        bound,
        target: None,
        stats,
        stop: Some(stop_tx),
    })
}

/// Start a remote forward: the SERVER listens on `bind_addr:bind_port`, and each
/// connection is delivered here and joined to `target_host:target_port` as seen
/// from this machine.
pub async fn start_remote(
    transport: Arc<Transport>,
    id: String,
    bind_addr: &str,
    bind_port: u16,
    target_host: String,
    target_port: u16,
) -> Result<Forward, SshError> {
    let (bound_port, mut incoming) = transport
        .request_remote_forward(bind_addr, bind_port)
        .await?;

    let stats = Arc::new(ForwardStats::default());
    let (stop_tx, mut stop_rx) = oneshot::channel();

    // `bound` describes the SERVER's listener, not a local socket. Reporting a
    // local address here would be a lie about where the port actually is.
    let bound: SocketAddr = format!("0.0.0.0:{bound_port}")
        .parse()
        .map_err(|e| SshError::Channel(format!("{e}")))?;

    {
        let stats = Arc::clone(&stats);
        let transport_for_cancel = Arc::clone(&transport);
        let bind_addr = bind_addr.to_string();
        let target = (target_host.clone(), target_port);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut stop_rx => {
                        transport_for_cancel.cancel_remote_forward(&bind_addr, bound_port).await;
                        break;
                    }
                    conn = incoming.recv() => {
                        let Some(conn) = conn else { break };
                        let stats = Arc::clone(&stats);
                        let target = target.clone();
                        stats.connections.fetch_add(1, Ordering::Relaxed);
                        stats.active.fetch_add(1, Ordering::Relaxed);
                        tokio::spawn(async move {
                            if pump_reverse(conn, &target.0, target.1, &stats).await.is_err() {
                                stats.errors.fetch_add(1, Ordering::Relaxed);
                            }
                            stats.active.fetch_sub(1, Ordering::Relaxed);
                        });
                    }
                }
            }
        });
    }

    Ok(Forward {
        id,
        kind: ForwardKind::Remote,
        bound,
        target: Some((target_host, target_port)),
        stats,
        stop: Some(stop_tx),
    })
}

/// Join a server-initiated channel to a local TCP connection.
async fn pump_reverse(
    conn: crate::transport::ForwardedConnection,
    host: &str,
    port: u16,
    stats: &ForwardStats,
) -> Result<(), SshError> {
    let sock = TcpStream::connect((host, port))
        .await
        .map_err(|e| SshError::Channel(format!("connect {host}:{port}: {e}")))?;
    let _ = sock.set_nodelay(true);

    let mut stream = conn.channel.into_stream();
    let (mut sock_read, mut sock_write) = tokio::io::split(sock);
    let (mut ch_read, mut ch_write) = tokio::io::split(&mut stream);

    let mut up = vec![0u8; 32 * 1024];
    let mut down = vec![0u8; 32 * 1024];

    loop {
        tokio::select! {
            n = ch_read.read(&mut down) => {
                let n = n.map_err(|e| SshError::Channel(e.to_string()))?;
                if n == 0 { break; }
                sock_write.write_all(&down[..n]).await.map_err(|e| SshError::Channel(e.to_string()))?;
                stats.bytes_in.fetch_add(n as u64, Ordering::Relaxed);
            }
            n = sock_read.read(&mut up) => {
                let n = n.map_err(|e| SshError::Channel(e.to_string()))?;
                if n == 0 { break; }
                ch_write.write_all(&up[..n]).await.map_err(|e| SshError::Channel(e.to_string()))?;
                stats.bytes_out.fetch_add(n as u64, Ordering::Relaxed);
            }
        }
    }
    let _ = sock_write.shutdown().await;
    Ok(())
}

/// Open a `direct-tcpip` channel and shuttle bytes both ways until either side
/// closes.
async fn pump_direct(
    transport: &Transport,
    sock: TcpStream,
    peer: SocketAddr,
    host: &str,
    port: u16,
    stats: &ForwardStats,
) -> Result<(), SshError> {
    // Nagle would add up to 40ms to every small write through the tunnel, which
    // is exactly what §14.6 warns about — and a tunnel is usually carrying an
    // interactive protocol.
    let _ = sock.set_nodelay(true);

    let channel = transport
        .open_direct_tcpip(host, port, &peer.ip().to_string(), peer.port())
        .await?;

    let mut stream = channel.into_stream();
    let (mut sock_read, mut sock_write) = tokio::io::split(sock);
    let (mut ch_read, mut ch_write) = tokio::io::split(&mut stream);

    let mut up = vec![0u8; 32 * 1024];
    let mut down = vec![0u8; 32 * 1024];

    loop {
        tokio::select! {
            n = sock_read.read(&mut up) => {
                let n = n.map_err(|e| SshError::Channel(e.to_string()))?;
                if n == 0 { break; }
                ch_write.write_all(&up[..n]).await.map_err(|e| SshError::Channel(e.to_string()))?;
                stats.bytes_out.fetch_add(n as u64, Ordering::Relaxed);
            }
            n = ch_read.read(&mut down) => {
                let n = n.map_err(|e| SshError::Channel(e.to_string()))?;
                if n == 0 { break; }
                sock_write.write_all(&down[..n]).await.map_err(|e| SshError::Channel(e.to_string()))?;
                stats.bytes_in.fetch_add(n as u64, Ordering::Relaxed);
            }
        }
    }
    let _ = sock_write.shutdown().await;
    Ok(())
}

/// A client that connects and then says nothing must not hold a task forever.
///
/// Found by a test: `[0x05, 0x01]` followed by silence parked the reader
/// indefinitely, keeping a tokio task and an `active` counter alive. On a proxy
/// that is a trivial way to exhaust the process. A real handshake completes in
/// well under a millisecond, so five seconds is enormously generous.
const HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

async fn socks5_handshake(sock: &mut TcpStream) -> Result<(String, u16), std::io::Error> {
    tokio::time::timeout(HANDSHAKE_TIMEOUT, socks5_handshake_inner(sock))
        .await
        .unwrap_or_else(|_| {
            Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "SOCKS5 handshake timed out",
            ))
        })
}

/// Minimal SOCKS5: no auth, CONNECT only.
///
/// Enough for a browser or `curl --socks5`. BIND and UDP ASSOCIATE are refused
/// explicitly rather than left to time out.
async fn socks5_handshake_inner(sock: &mut TcpStream) -> Result<(String, u16), std::io::Error> {
    use std::io::{Error, ErrorKind};

    let mut head = [0u8; 2];
    sock.read_exact(&mut head).await?;
    if head[0] != 0x05 {
        return Err(Error::new(ErrorKind::InvalidData, "not SOCKS5"));
    }
    let mut methods = vec![0u8; head[1] as usize];
    sock.read_exact(&mut methods).await?;
    // 0x00 = no authentication. This proxy is bound to loopback by default and
    // its authentication is the SSH connection underneath it.
    sock.write_all(&[0x05, 0x00]).await?;

    let mut req = [0u8; 4];
    sock.read_exact(&mut req).await?;
    if req[1] != 0x01 {
        // Not CONNECT.
        sock.write_all(&[0x05, 0x07, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
            .await?;
        return Err(Error::new(ErrorKind::Unsupported, "only CONNECT"));
    }

    let host = match req[3] {
        0x01 => {
            let mut v4 = [0u8; 4];
            sock.read_exact(&mut v4).await?;
            format!("{}.{}.{}.{}", v4[0], v4[1], v4[2], v4[3])
        }
        0x03 => {
            let mut len = [0u8; 1];
            sock.read_exact(&mut len).await?;
            let mut name = vec![0u8; len[0] as usize];
            sock.read_exact(&mut name).await?;
            String::from_utf8_lossy(&name).into_owned()
        }
        0x04 => {
            let mut v6 = [0u8; 16];
            sock.read_exact(&mut v6).await?;
            std::net::Ipv6Addr::from(v6).to_string()
        }
        _ => return Err(Error::new(ErrorKind::InvalidData, "bad address type")),
    };

    let mut port = [0u8; 2];
    sock.read_exact(&mut port).await?;
    let port = u16::from_be_bytes(port);

    // Success. The bound address is reported as 0.0.0.0:0 — clients ignore it
    // for CONNECT, and inventing one would be a lie about the tunnel's shape.
    sock.write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
        .await?;
    Ok((host, port))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive the SOCKS5 handshake over an in-memory pipe.
    async fn socks_roundtrip(request: Vec<u8>) -> Option<(String, u16)> {
        // A real TcpStream is needed by the function signature, so use a
        // loopback pair rather than a mock — it also proves the byte order is
        // right on the wire, not just in a buffer.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let client = tokio::spawn(async move {
            let mut c = TcpStream::connect(addr).await.unwrap();
            c.write_all(&request).await.unwrap();
            let mut greet = [0u8; 2];
            c.read_exact(&mut greet).await.ok();
            let mut reply = [0u8; 10];
            c.read_exact(&mut reply).await.ok();
            reply
        });
        let (mut server, _) = listener.accept().await.unwrap();
        let got = socks5_handshake(&mut server).await.ok();

        // Drop the server socket BEFORE joining the client.
        //
        // On a rejected handshake the server answers nothing at all, so the
        // client sits in `read_exact` waiting for bytes that will never come —
        // and awaiting it first deadlocks the test. Closing the socket gives the
        // client EOF instead. This hung the whole `core-ssh` test binary, which
        // in turn wedged every concurrent cargo invocation on the build lock.
        drop(server);
        let _ = client.await;
        got
    }

    #[tokio::test]
    async fn socks5_parses_a_domain_connect() {
        // VER NMETHODS METHODS | VER CMD RSV ATYP LEN "example.com" PORT
        let mut req = vec![0x05, 0x01, 0x00, 0x05, 0x01, 0x00, 0x03, 11];
        req.extend_from_slice(b"example.com");
        req.extend_from_slice(&443u16.to_be_bytes());
        assert_eq!(
            socks_roundtrip(req).await,
            Some(("example.com".into(), 443))
        );
    }

    #[tokio::test]
    async fn socks5_parses_an_ipv4_connect() {
        let mut req = vec![0x05, 0x01, 0x00, 0x05, 0x01, 0x00, 0x01, 10, 20, 4, 11];
        req.extend_from_slice(&5432u16.to_be_bytes());
        assert_eq!(
            socks_roundtrip(req).await,
            Some(("10.20.4.11".into(), 5432))
        );
    }

    #[tokio::test]
    async fn socks5_refuses_a_non_connect_command() {
        // CMD 0x02 is BIND, which this proxy does not implement. Refusing is
        // better than accepting and hanging.
        let req = vec![
            0x05, 0x01, 0x00, 0x05, 0x02, 0x00, 0x01, 127, 0, 0, 1, 0, 80,
        ];
        assert_eq!(socks_roundtrip(req).await, None);
    }

    #[tokio::test]
    async fn socks4_is_rejected() {
        let req = vec![0x04, 0x01, 0x00, 0x50];
        assert_eq!(socks_roundtrip(req).await, None);
    }

    #[test]
    fn stats_start_at_zero() {
        let s = ForwardStats::default();
        assert_eq!(s.connections.load(Ordering::Relaxed), 0);
        assert_eq!(s.bytes_in.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn a_truncated_handshake_does_not_hang() {
        // A client that connects and says almost nothing must not park a task
        // forever. Two bytes of a SOCKS5 greeting that promises one method,
        // then silence.
        let req = vec![0x05, 0x01];
        // Comfortably longer than HANDSHAKE_TIMEOUT, so a pass means the
        // handshake gave up on its own rather than the test masking a hang.
        let got = tokio::time::timeout(
            HANDSHAKE_TIMEOUT + std::time::Duration::from_secs(5),
            socks_roundtrip(req),
        )
        .await;
        assert!(got.is_ok(), "handshake blocked instead of failing");
        assert_eq!(got.unwrap(), None);
    }
}
