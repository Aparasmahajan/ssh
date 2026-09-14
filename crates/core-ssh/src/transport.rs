//! The SSH transport, and shell channels over it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use russh::client::{self, Handle, Msg};
use russh::keys::{load_secret_key, PrivateKeyWithHashAlg};
use russh::{Channel, ChannelMsg, Disconnect};
use tokio::sync::mpsc;

use crate::agent;
use crate::hostkey::{self, HostKeyPolicy, Verdict};

/// A connection the server opened back to us for a remote (`-R`) forward.
pub struct ForwardedConnection {
    pub channel: Channel<Msg>,
    /// The `bind_address:port` on the server that accepted it.
    pub connected_port: u16,
}

/// §6.1: "Widen the SSH channel window too. `russh` defaults are small (~2 MB or
/// less); a narrow channel window stalls you no matter how deep the SFTP pipeline
/// is. Set it to ≥ 8 MB and adjust proactively, not lazily."
const WINDOW_SIZE: u32 = 8 * 1024 * 1024;
const MAX_PACKET: u32 = 64 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum SshError {
    #[error("connect failed: {0}")]
    Connect(String),
    #[error("host key rejected — {0}")]
    HostKey(String),
    #[error("authentication failed: {0}")]
    Auth(String),
    #[error("channel error: {0}")]
    Channel(String),
    #[error("sftp error: {0}")]
    Sftp(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Identifies a transport. §8.1 keys transports by the whole tuple, not by host:
/// two identities on one host are two transports, because they authenticate
/// differently and must not share credentials.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
    pub user: String,
    /// Names the identity (e.g. "deploy", "backup"). Part of the key so
    /// `SFTP·deploy` and `SFTP·backup` never collapse onto one connection.
    pub identity: String,
}

#[derive(Clone, Debug)]
pub enum AuthMethod {
    Password(String),
    /// An OpenSSH private key file, optionally encrypted.
    PrivateKey {
        path: PathBuf,
        passphrase: Option<String>,
    },
    /// Whatever the running SSH agent holds.
    ///
    /// This is what most people actually use, and it is the only method that can
    /// succeed with **no prompt of any kind** — which is also what makes it the
    /// only one §8.2's speculative connect could ever be allowed to use.
    ///
    /// Carries no secret: the private key never leaves the agent. OpsPilot sends
    /// the signature request and the agent signs it.
    Agent,
}

/// Client-side handler. Its only job is the host key check, and getting that
/// wrong is the difference between SSH and a plaintext socket with extra steps.
struct ClientHandler {
    host: String,
    port: u16,
    policy: HostKeyPolicy,
    known_hosts: Option<PathBuf>,
    verdict: Arc<Mutex<Option<Verdict>>>,
    /// Never written, and the compiler is right to say so.
    ///
    /// Populating it needs russh's `kex_done` hook, whose `negotiation::Names`
    /// argument type is private in 0.62 — the override does not compile. So the
    /// status rail reports the cipher as `n/a` rather than guessing. The field
    /// stays because the `Transport` side of the pair is real and the plumbing
    /// should not have to be rebuilt when russh exports the type.
    #[allow(dead_code)]
    negotiated: Arc<Mutex<Option<Negotiated>>>,
    /// Where to deliver channels the SERVER opens at us.
    ///
    /// A remote forward inverts the usual direction: the listener lives on the
    /// server, and each accepted connection arrives here as a new channel. The
    /// handler cannot do the work itself — it has no access to the forward's
    /// target — so it hands the channel to whoever registered for that port.
    forwarded: Arc<Mutex<HashMap<u16, mpsc::UnboundedSender<ForwardedConnection>>>>,
}

/// What the two ends agreed on during key exchange.
///
/// **Never populated on russh 0.62.** The only hook that carries this is
/// `Handler::kex_done`, whose `names: &negotiation::Names` parameter cannot be
/// named from outside the crate — `russh::negotiation` is private and there is
/// no public accessor on `Handle` or `Session`. So the type exists and the
/// plumbing is in place, and the status rail reports the cipher as unknown
/// rather than displaying a plausible guess.
///
/// Fixing this needs an upstream change (export `negotiation::Names`, or add an
/// accessor). Recorded in COMPLETED.md.
#[derive(Clone, Debug, Default)]
pub struct Negotiated {
    pub kex: String,
    pub host_key: String,
    pub cipher: String,
    pub mac: String,
}

impl client::Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::ssh_key::PublicKey,
    ) -> Result<bool, Self::Error> {
        let verdict = match &self.known_hosts {
            Some(path) => {
                hostkey::verify(&self.host, self.port, server_public_key, path, self.policy)
            }
            None => Verdict::Error("no known_hosts path configured".into()),
        };
        let accepted = verdict.accepted();
        *self.verdict.lock().unwrap() = Some(verdict);
        Ok(accepted)
    }

    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: Channel<Msg>,
        connected_address: &str,
        connected_port: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: client::ChannelOpenHandle,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        let port = connected_port as u16;
        let sink = self
            .forwarded
            .lock()
            .ok()
            .and_then(|m| m.get(&port).cloned());
        match sink {
            Some(tx) => {
                let _ = tx.send(ForwardedConnection {
                    channel,
                    connected_port: port,
                });
            }
            None => {
                // Nobody is listening for this port. Closing beats leaking a
                // channel per connection for a forward that was stopped.
                let _ = channel.close().await;
                let _ = connected_address;
            }
        }
        Ok(())
    }
}

pub struct Transport {
    handle: Handle<ClientHandler>,
    endpoint: Endpoint,
    forwarded: Arc<Mutex<HashMap<u16, mpsc::UnboundedSender<ForwardedConnection>>>>,
    negotiated: Arc<Mutex<Option<Negotiated>>>,
    /// Milliseconds for the TCP connect. Not a live RTT — see `connect_rtt_ms`.
    connect_rtt_ms: u32,
}

impl Transport {
    /// Connect and authenticate.
    pub async fn connect(
        endpoint: Endpoint,
        auth: &[AuthMethod],
        policy: HostKeyPolicy,
        known_hosts: Option<PathBuf>,
    ) -> Result<Transport, SshError> {
        let config = client::Config {
            // §14.6: "TCP_NODELAY on every socket. Nagle's algorithm adds up to
            // 40 ms to interactive traffic. This is one line and it is not
            // optional." russh's default is false, so this line IS the fix.
            nodelay: true,
            window_size: WINDOW_SIZE,
            maximum_packet_size: MAX_PACKET,
            // §8.3: "Adaptive keepalive: start 30 s".
            keepalive_interval: Some(std::time::Duration::from_secs(30)),
            keepalive_max: 3,
            ..Default::default()
        };

        let verdict = Arc::new(Mutex::new(None));
        let negotiated = Arc::new(Mutex::new(None));
        let forwarded: Arc<Mutex<HashMap<u16, mpsc::UnboundedSender<ForwardedConnection>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let handler = ClientHandler {
            host: endpoint.host.clone(),
            port: endpoint.port,
            policy,
            known_hosts: known_hosts.or_else(hostkey::default_known_hosts),
            verdict: Arc::clone(&verdict),
            negotiated: Arc::clone(&negotiated),
            forwarded: Arc::clone(&forwarded),
        };

        // Timed around the TCP connect and handshake. This is a real
        // measurement of the path, and it is reported as such — §4's live
        // per-keystroke RTT needs a probe that does not exist yet, and showing a
        // made-up number would be worse than showing this one honestly.
        let started = std::time::Instant::now();
        let mut handle = client::connect(
            Arc::new(config),
            (endpoint.host.as_str(), endpoint.port),
            handler,
        )
        .await
        .map_err(|e| {
            // A rejected host key surfaces from russh as a generic failure, so
            // report the verdict instead — "connection closed" would send the
            // user looking at their network when the answer is on screen.
            if let Some(v) = verdict.lock().unwrap().as_ref() {
                if !v.accepted() {
                    return SshError::HostKey(v.message(&endpoint.host));
                }
            }
            SshError::Connect(e.to_string())
        })?;

        let connect_rtt_ms = started.elapsed().as_millis().min(u32::MAX as u128) as u32;

        Self::authenticate(&mut handle, &endpoint, auth).await?;
        Ok(Transport {
            handle,
            endpoint,
            negotiated,
            connect_rtt_ms,
            forwarded,
        })
    }

    async fn authenticate(
        handle: &mut Handle<ClientHandler>,
        endpoint: &Endpoint,
        auth: &[AuthMethod],
    ) -> Result<(), SshError> {
        let mut last = String::from("no authentication methods were offered");

        for method in auth {
            let result = match method {
                AuthMethod::Password(p) => handle
                    .authenticate_password(&endpoint.user, p.clone())
                    .await
                    .map_err(|e| SshError::Auth(e.to_string()))?,
                AuthMethod::PrivateKey { path, passphrase } => {
                    let key = load_secret_key(path, passphrase.as_deref())
                        .map_err(|e| SshError::Auth(format!("{}: {e}", path.display())))?;
                    // Let the server pick the RSA hash; ed25519 ignores it.
                    let best = handle
                        .best_supported_rsa_hash()
                        .await
                        .map_err(|e| SshError::Auth(e.to_string()))?
                        .flatten();
                    handle
                        .authenticate_publickey(
                            &endpoint.user,
                            PrivateKeyWithHashAlg::new(Arc::new(key), best),
                        )
                        .await
                        .map_err(|e| SshError::Auth(e.to_string()))?
                }
                AuthMethod::Agent => match agent::authenticate(handle, &endpoint.user).await {
                    Ok(true) => return Ok(()),
                    Ok(false) => {
                        last = "the agent's keys were all rejected".into();
                        continue;
                    }
                    Err(e) => {
                        // A missing agent is not a failure of the connection —
                        // it is one method that cannot be tried. Say which, and
                        // let the next method run.
                        last = e;
                        continue;
                    }
                },
            };

            if result.success() {
                return Ok(());
            }
            last = match method {
                AuthMethod::Password(_) => "password rejected".into(),
                AuthMethod::PrivateKey { path, .. } => {
                    format!("key rejected: {}", path.display())
                }
                AuthMethod::Agent => last,
            };
        }
        Err(SshError::Auth(last))
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    pub fn is_closed(&self) -> bool {
        self.handle.is_closed()
    }

    /// The negotiated algorithms — always `None` today, see [`Negotiated`].
    pub fn negotiated(&self) -> Option<Negotiated> {
        self.negotiated.lock().ok().and_then(|g| g.clone())
    }

    /// Time for the TCP connect plus handshake, in ms.
    pub fn connect_rtt_ms(&self) -> u32 {
        self.connect_rtt_ms
    }

    /// Open an interactive shell. §8.1: this is a *channel*, not a new
    /// connection, which is the entire reason a second tab is cheap.
    pub async fn open_shell(&self, rows: u16, cols: u16) -> Result<Shell, SshError> {
        let channel = self
            .handle
            .channel_open_session()
            .await
            .map_err(|e| SshError::Channel(e.to_string()))?;

        channel
            .request_pty(true, "xterm-256color", cols as u32, rows as u32, 0, 0, &[])
            .await
            .map_err(|e| SshError::Channel(e.to_string()))?;
        channel
            .request_shell(true)
            .await
            .map_err(|e| SshError::Channel(e.to_string()))?;

        Ok(Shell::new(channel))
    }

    /// Ask the server to listen on `address:port` and send us each connection.
    ///
    /// Returns the port actually bound — asking for 0 lets the server choose,
    /// and the caller needs to know which one it got.
    pub async fn request_remote_forward(
        &self,
        address: &str,
        port: u16,
    ) -> Result<(u16, mpsc::UnboundedReceiver<ForwardedConnection>), SshError> {
        let granted = self
            .handle
            .tcpip_forward(address, port as u32)
            .await
            .map_err(|e| SshError::Channel(format!("remote forward {address}:{port}: {e}")))?;
        // The server echoes 0 when it was given a concrete port.
        let bound = if granted == 0 { port } else { granted as u16 };

        let (tx, rx) = mpsc::unbounded_channel();
        self.forwarded
            .lock()
            .map_err(|_| SshError::Channel("forward table poisoned".into()))?
            .insert(bound, tx);
        Ok((bound, rx))
    }

    /// Stop a remote forward and stop accepting its channels.
    pub async fn cancel_remote_forward(&self, address: &str, port: u16) {
        let _ = self.handle.cancel_tcpip_forward(address, port as u32).await;
        if let Ok(mut m) = self.forwarded.lock() {
            m.remove(&port);
        }
    }

    /// Open a `direct-tcpip` channel — the client side of a `-L` forward.
    ///
    /// §8.1: a forward is a channel on the existing transport, so starting one
    /// costs a channel open rather than a handshake.
    pub async fn open_direct_tcpip(
        &self,
        target_host: &str,
        target_port: u16,
        originator_ip: &str,
        originator_port: u16,
    ) -> Result<Channel<Msg>, SshError> {
        self.handle
            .channel_open_direct_tcpip(
                target_host,
                target_port as u32,
                originator_ip,
                originator_port as u32,
            )
            .await
            .map_err(|e| SshError::Channel(e.to_string()))
    }

    /// Open the SFTP subsystem on its own channel over the same transport.
    pub async fn open_sftp(&self) -> Result<crate::sftp::Sftp, SshError> {
        let channel = self
            .handle
            .channel_open_session()
            .await
            .map_err(|e| SshError::Channel(e.to_string()))?;
        channel
            .request_subsystem(true, "sftp")
            .await
            .map_err(|e| SshError::Channel(e.to_string()))?;
        crate::sftp::Sftp::new(channel).await
    }

    pub async fn disconnect(&self) {
        let _ = self
            .handle
            .disconnect(Disconnect::ByApplication, "", "en")
            .await;
    }
}

/// An interactive shell channel.
///
/// Output is pushed onto an mpsc channel by a task that owns the russh channel;
/// input goes back the other way. The consumer never touches russh directly,
/// which keeps the terminal side free of async plumbing.
pub struct Shell {
    input: mpsc::UnboundedSender<ShellCmd>,
    output: Option<mpsc::UnboundedReceiver<Vec<u8>>>,
}

enum ShellCmd {
    Data(Vec<u8>),
    Resize { rows: u16, cols: u16 },
    Close,
}

impl Shell {
    fn new(channel: Channel<Msg>) -> Shell {
        let (in_tx, mut in_rx) = mpsc::unbounded_channel::<ShellCmd>();
        let (out_tx, out_rx) = mpsc::unbounded_channel::<Vec<u8>>();

        tokio::spawn(async move {
            let mut channel = channel;
            loop {
                tokio::select! {
                    cmd = in_rx.recv() => match cmd {
                        Some(ShellCmd::Data(bytes)) => {
                            if channel.data_bytes(bytes).await.is_err() { break; }
                        }
                        Some(ShellCmd::Resize { rows, cols }) => {
                            let _ = channel
                                .window_change(cols as u32, rows as u32, 0, 0)
                                .await;
                        }
                        Some(ShellCmd::Close) | None => {
                            let _ = channel.eof().await;
                            let _ = channel.close().await;
                            break;
                        }
                    },
                    msg = channel.wait() => match msg {
                        Some(ChannelMsg::Data { data }) => {
                            if out_tx.send(data.to_vec()).is_err() { break; }
                        }
                        // stderr from a shell belongs on screen with everything
                        // else; a terminal has one output stream.
                        Some(ChannelMsg::ExtendedData { data, .. }) => {
                            if out_tx.send(data.to_vec()).is_err() { break; }
                        }
                        Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => break,
                        Some(_) => {}
                    },
                }
            }
        });

        Shell {
            input: in_tx,
            output: Some(out_rx),
        }
    }

    /// Take the output stream. Callable once — the receiver is moved to whoever
    /// drives the terminal.
    pub fn take_output(&mut self) -> Option<mpsc::UnboundedReceiver<Vec<u8>>> {
        self.output.take()
    }

    pub fn write(&self, bytes: Vec<u8>) -> Result<(), SshError> {
        self.input
            .send(ShellCmd::Data(bytes))
            .map_err(|_| SshError::Channel("shell closed".into()))
    }

    pub fn resize(&self, rows: u16, cols: u16) -> Result<(), SshError> {
        self.input
            .send(ShellCmd::Resize { rows, cols })
            .map_err(|_| SshError::Channel("shell closed".into()))
    }

    pub fn close(&self) {
        let _ = self.input.send(ShellCmd::Close);
    }
}
