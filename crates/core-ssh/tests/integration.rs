//! End-to-end tests against a real SSH server running in-process.
//!
//! There is no sshd and no Docker on the machine this was written on, and
//! "it compiles" is not evidence that an SSH client works. `russh` ships a
//! server, so the tests stand one up on a loopback port: real key exchange, real
//! authentication, real channels, real SFTP packets over a real subsystem.
//!
//! That is better than a localhost sshd would have been — no external state, no
//! machine-specific config, and the host key is chosen by the test, which is the
//! only way to exercise the "host key changed" path deliberately.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use russh::keys::ssh_key::private::Ed25519Keypair;
use russh::keys::PrivateKey;
use russh::server::{Auth, ChannelOpenHandle, Handler as ServerHandler, Msg, Server, Session};
use russh::{Channel, ChannelId};
use russh_sftp::protocol::{
    Attrs, File, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode, Version,
};
use tokio::net::TcpListener;

use opspilot_core_ssh::hostkey::HostKeyPolicy;
use opspilot_core_ssh::transport::{AuthMethod, Endpoint, Transport};

const USER: &str = "tester";
const PASSWORD: &str = "correct horse battery staple";

/// Deterministic server key from a fixed seed. No RNG feature needed, and no
/// PEM in the repo for a secret scanner to flag. Two seeds give two distinct
/// host identities, which is what the key-rotation test needs.
fn server_key(seed: u8) -> PrivateKey {
    PrivateKey::from(Ed25519Keypair::from_seed(&[seed; 32]))
}

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("opspilot-ssh-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/* ---- The test SSH server -------------------------------------------------- */

struct TestServer {
    root: PathBuf,
}

impl Server for TestServer {
    type Handler = TestHandler;
    fn new_client(&mut self, _peer: Option<SocketAddr>) -> TestHandler {
        TestHandler {
            root: self.root.clone(),
            channel: None,
            pty: None,
        }
    }
}

struct TestHandler {
    root: PathBuf,
    /// Stashed at open time; `subsystem_request` only receives a `ChannelId`,
    /// but running SFTP needs the `Channel` itself as a stream.
    channel: Option<Channel<Msg>>,
    pty: Option<(u32, u32)>,
}

impl ServerHandler for TestHandler {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        if user == USER && password == PASSWORD {
            Ok(Auth::Accept)
        } else {
            Ok(Auth::reject())
        }
    }

    async fn auth_publickey(
        &mut self,
        user: &str,
        _key: &russh::keys::ssh_key::PublicKey,
    ) -> Result<Auth, Self::Error> {
        // Any key for the right user. The client-side key handling is what is
        // under test, not this server's policy.
        if user == USER {
            Ok(Auth::Accept)
        } else {
            Ok(Auth::reject())
        }
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        self.channel = Some(channel);
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _term: &str,
        cols: u32,
        rows: u32,
        _pw: u32,
        _ph: u32,
        _modes: &[(russh::Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.pty = Some((cols, rows));
        session.channel_success(channel)?;
        Ok(())
    }

    async fn window_change_request(
        &mut self,
        channel: ChannelId,
        cols: u32,
        rows: u32,
        _pw: u32,
        _ph: u32,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.pty = Some((cols, rows));
        // Report the new size back so the test can assert the resize arrived.
        session.data(channel, format!("SIZE {cols}x{rows}\n").into_bytes())?;
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        let (cols, rows) = self.pty.unwrap_or((0, 0));
        session.data(channel, format!("READY {cols}x{rows}\n").into_bytes())?;
        Ok(())
    }

    /// An echo shell: whatever the client types comes back, which is what a real
    /// PTY does and is enough to prove the channel is bidirectional.
    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.data(channel, data.to_vec())?;
        Ok(())
    }

    /// Accept a remote-forward request and immediately open one connection
    /// back, so the client's `-R` path can be exercised without a real listener.
    async fn tcpip_forward(
        &mut self,
        address: &str,
        port: &mut u32,
        session: &mut Session,
    ) -> Result<bool, Self::Error> {
        // Port 0 means "you choose"; pick something deterministic.
        if *port == 0 {
            *port = 19999;
        }
        let handle = session.handle();
        let addr = address.to_string();
        let bound = *port;
        tokio::spawn(async move {
            // Give the client a moment to register its receiver, then deliver a
            // connection exactly as a real server would on an inbound TCP accept.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            if let Ok(ch) = handle
                .channel_open_forwarded_tcpip(addr, bound, "10.9.9.9", 4242)
                .await
            {
                let _ = ch.data(&b"reverse-hello"[..]).await;
            }
        });
        Ok(true)
    }

    async fn subsystem_request(
        &mut self,
        channel: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if name != "sftp" {
            session.channel_failure(channel)?;
            return Ok(());
        }
        session.channel_success(channel)?;
        if let Some(ch) = self.channel.take() {
            let root = self.root.clone();
            tokio::spawn(async move {
                russh_sftp::server::run(ch.into_stream(), SftpBackend::new(root)).await;
            });
        }
        Ok(())
    }
}

/* ---- A real, disk-backed SFTP server -------------------------------------- */

/// Backed by an actual temp directory rather than an in-memory fake, so the
/// client's listing, read and write paths are exercised against real metadata.
struct SftpBackend {
    root: PathBuf,
    /// handle string -> what it refers to
    dirs: HashMap<String, Vec<File>>,
    files: HashMap<String, (PathBuf, Vec<u8>, bool)>, // path, buffer, writing
    next: u64,
}

impl SftpBackend {
    fn new(root: PathBuf) -> Self {
        SftpBackend {
            root,
            dirs: HashMap::new(),
            files: HashMap::new(),
            next: 0,
        }
    }

    fn handle(&mut self, prefix: &str) -> String {
        self.next += 1;
        format!("{prefix}{}", self.next)
    }

    /// Map a client path into the sandbox. Rejects traversal — a test server
    /// that can be walked out of is a bad habit even in a test.
    fn resolve(&self, p: &str) -> PathBuf {
        let rel = p.trim_start_matches('/').replace('\\', "/");
        let mut out = self.root.clone();
        for part in rel.split('/') {
            if part.is_empty() || part == "." || part == ".." {
                continue;
            }
            out.push(part);
        }
        out
    }
}

fn attrs_for(meta: &std::fs::Metadata) -> FileAttributes {
    let mut a = FileAttributes {
        size: Some(meta.len()),
        ..Default::default()
    };
    a.set_dir(meta.is_dir());
    a.set_regular(meta.is_file());
    a.permissions = Some(if meta.is_dir() { 0o40755 } else { 0o100644 });
    a
}

impl russh_sftp::server::Handler for SftpBackend {
    type Error = StatusCode;

    fn unimplemented(&self) -> StatusCode {
        StatusCode::OpUnsupported
    }

    async fn init(
        &mut self,
        _version: u32,
        _ext: HashMap<String, String>,
    ) -> Result<Version, Self::Error> {
        Ok(Version::new())
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        let shown = if path == "." || path.is_empty() {
            "/".to_string()
        } else {
            format!("/{}", path.trim_start_matches('/'))
        };
        Ok(Name {
            id,
            files: vec![File::dummy(shown)],
        })
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, Self::Error> {
        let dir = self.resolve(&path);
        let rd = std::fs::read_dir(&dir).map_err(|_| StatusCode::NoSuchFile)?;
        let mut files = Vec::new();
        for e in rd.flatten() {
            let meta = match e.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            files.push(File::new(
                e.file_name().to_string_lossy().into_owned(),
                attrs_for(&meta),
            ));
        }
        let h = self.handle("d");
        self.dirs.insert(h.clone(), files);
        Ok(Handle { id, handle: h })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, Self::Error> {
        // Second call must report EOF or the client loops forever.
        match self.dirs.remove(&handle) {
            Some(files) if !files.is_empty() => Ok(Name { id, files }),
            _ => Err(StatusCode::Eof),
        }
    }

    async fn open(
        &mut self,
        id: u32,
        filename: String,
        pflags: OpenFlags,
        _attrs: FileAttributes,
    ) -> Result<Handle, Self::Error> {
        let path = self.resolve(&filename);
        let writing = pflags.contains(OpenFlags::WRITE) || pflags.contains(OpenFlags::CREATE);
        let buf = if writing && pflags.contains(OpenFlags::TRUNCATE) {
            Vec::new()
        } else {
            std::fs::read(&path).unwrap_or_default()
        };
        if !writing && !path.exists() {
            return Err(StatusCode::NoSuchFile);
        }
        let h = self.handle("f");
        self.files.insert(h.clone(), (path, buf, writing));
        Ok(Handle { id, handle: h })
    }

    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<russh_sftp::protocol::Data, Self::Error> {
        let (_, buf, _) = self.files.get(&handle).ok_or(StatusCode::Failure)?;
        let start = offset as usize;
        if start >= buf.len() {
            return Err(StatusCode::Eof);
        }
        let end = (start + len as usize).min(buf.len());
        Ok(russh_sftp::protocol::Data {
            id,
            data: buf[start..end].to_vec(),
        })
    }

    async fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<Status, Self::Error> {
        let entry = self.files.get_mut(&handle).ok_or(StatusCode::Failure)?;
        let start = offset as usize;
        if entry.1.len() < start + data.len() {
            entry.1.resize(start + data.len(), 0);
        }
        entry.1[start..start + data.len()].copy_from_slice(&data);
        Ok(Status {
            id,
            status_code: StatusCode::Ok,
            error_message: String::new(),
            language_tag: "en".into(),
        })
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
        if let Some((path, buf, writing)) = self.files.remove(&handle) {
            if writing {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                std::fs::write(&path, &buf).map_err(|_| StatusCode::Failure)?;
            }
        }
        self.dirs.remove(&handle);
        Ok(Status {
            id,
            status_code: StatusCode::Ok,
            error_message: String::new(),
            language_tag: "en".into(),
        })
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let meta = std::fs::metadata(self.resolve(&path)).map_err(|_| StatusCode::NoSuchFile)?;
        Ok(Attrs {
            id,
            attrs: attrs_for(&meta),
        })
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        self.stat(id, path).await
    }

    async fn mkdir(
        &mut self,
        id: u32,
        path: String,
        _attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        std::fs::create_dir_all(self.resolve(&path)).map_err(|_| StatusCode::Failure)?;
        Ok(Status {
            id,
            status_code: StatusCode::Ok,
            error_message: String::new(),
            language_tag: "en".into(),
        })
    }

    async fn remove(&mut self, id: u32, filename: String) -> Result<Status, Self::Error> {
        std::fs::remove_file(self.resolve(&filename)).map_err(|_| StatusCode::NoSuchFile)?;
        Ok(Status {
            id,
            status_code: StatusCode::Ok,
            error_message: String::new(),
            language_tag: "en".into(),
        })
    }

    async fn rename(&mut self, id: u32, old: String, new: String) -> Result<Status, Self::Error> {
        std::fs::rename(self.resolve(&old), self.resolve(&new)).map_err(|_| StatusCode::Failure)?;
        Ok(Status {
            id,
            status_code: StatusCode::Ok,
            error_message: String::new(),
            language_tag: "en".into(),
        })
    }
}

/* ---- Harness --------------------------------------------------------------- */

async fn start(seed: u8, root: PathBuf) -> SocketAddr {
    let config = Arc::new(russh::server::Config {
        keys: vec![server_key(seed)],
        auth_rejection_time: std::time::Duration::from_millis(1),
        ..Default::default()
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let mut server = TestServer { root };
        while let Ok((stream, peer)) = listener.accept().await {
            let handler = server.new_client(Some(peer));
            let cfg = Arc::clone(&config);
            tokio::spawn(async move {
                let _ = russh::server::run_stream(cfg, stream, handler).await;
                // Hold the connection open for the length of the test.
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            });
        }
    });
    addr
}

async fn connect(
    addr: SocketAddr,
    known_hosts: PathBuf,
    policy: HostKeyPolicy,
    auth: Vec<AuthMethod>,
) -> Result<Transport, opspilot_core_ssh::SshError> {
    Transport::connect(
        Endpoint {
            host: addr.ip().to_string(),
            port: addr.port(),
            user: USER.into(),
            identity: "test".into(),
        },
        &auth,
        policy,
        Some(known_hosts),
    )
    .await
}

/// `Transport` has no `Debug` (it holds a live russh handle), so `expect_err`
/// cannot be used directly.
fn expect_err(
    r: Result<Transport, opspilot_core_ssh::SshError>,
    msg: &str,
) -> opspilot_core_ssh::SshError {
    match r {
        Ok(_) => panic!("{msg}"),
        Err(e) => e,
    }
}

/* ---- Tests ----------------------------------------------------------------- */

#[tokio::test]
async fn password_auth_opens_a_shell_and_echoes() {
    let dir = tmp("shell");
    let addr = start(1, dir.clone()).await;
    let kh = dir.join("known_hosts");

    let t = connect(
        addr,
        kh,
        HostKeyPolicy::AcceptNew,
        vec![AuthMethod::Password(PASSWORD.into())],
    )
    .await
    .expect("connect + auth");

    let mut shell = t.open_shell(24, 80).await.expect("open shell");
    let mut out = shell.take_output().expect("output stream");

    let banner = tokio::time::timeout(std::time::Duration::from_secs(5), out.recv())
        .await
        .expect("banner timed out")
        .expect("channel closed");
    let banner = String::from_utf8_lossy(&banner).to_string();
    assert!(
        banner.contains("READY 80x24"),
        "PTY dimensions did not reach the server: {banner:?}"
    );

    shell.write(b"hello\r".to_vec()).unwrap();
    let echo = tokio::time::timeout(std::time::Duration::from_secs(5), out.recv())
        .await
        .expect("echo timed out")
        .expect("channel closed");
    assert_eq!(String::from_utf8_lossy(&echo), "hello\r");

    t.disconnect().await;
}

#[tokio::test]
async fn resize_reaches_the_server() {
    let dir = tmp("resize");
    let addr = start(1, dir.clone()).await;
    let t = connect(
        addr,
        dir.join("known_hosts"),
        HostKeyPolicy::AcceptNew,
        vec![AuthMethod::Password(PASSWORD.into())],
    )
    .await
    .unwrap();

    let mut shell = t.open_shell(24, 80).await.unwrap();
    let mut out = shell.take_output().unwrap();
    let _ = out.recv().await; // banner

    shell.resize(50, 120).unwrap();
    let msg = tokio::time::timeout(std::time::Duration::from_secs(5), out.recv())
        .await
        .expect("resize timed out")
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&msg).trim(), "SIZE 120x50");
}

#[tokio::test]
async fn a_wrong_password_is_rejected() {
    let dir = tmp("badpw");
    let addr = start(1, dir.clone()).await;
    let err = expect_err(
        connect(
            addr,
            dir.join("known_hosts"),
            HostKeyPolicy::AcceptNew,
            vec![AuthMethod::Password("not the password".into())],
        )
        .await,
        "must not authenticate",
    );
    assert!(
        matches!(err, opspilot_core_ssh::SshError::Auth(_)),
        "expected an auth error, got {err:?}"
    );
}

#[tokio::test]
async fn strict_policy_refuses_an_unknown_host() {
    let dir = tmp("strict");
    let addr = start(1, dir.clone()).await;
    let err = expect_err(
        connect(
            addr,
            dir.join("known_hosts"),
            HostKeyPolicy::Strict,
            vec![AuthMethod::Password(PASSWORD.into())],
        )
        .await,
        "unknown host must be refused under Strict",
    );
    assert!(
        matches!(err, opspilot_core_ssh::SshError::HostKey(_)),
        "expected a host key error, got {err:?}"
    );
}

/// The security property that matters most, end to end over a real handshake:
/// a server whose host key has changed is refused even under trust-on-first-use.
#[tokio::test]
async fn a_changed_host_key_is_refused_over_a_real_connection() {
    let dir = tmp("rotated");
    let kh = dir.join("known_hosts");

    // Same port cannot be reused, so pin the recorded identity to a fixed
    // hostname and point both servers at it by connecting to 127.0.0.1 on two
    // ports — known_hosts keys on host AND port, so instead run the second
    // server and rewrite the recorded entry's port. Simpler: connect twice to
    // two servers and assert the *first* is learned and the *second*, presenting
    // a different key for the same host:port, is refused.
    let first = start(1, dir.clone()).await;
    connect(
        first,
        kh.clone(),
        HostKeyPolicy::AcceptNew,
        vec![AuthMethod::Password(PASSWORD.into())],
    )
    .await
    .expect("first connection learns the key");

    // Rewrite the learned entry to claim the port the impostor will use.
    let second = start(2, dir.clone()).await;
    let recorded = std::fs::read_to_string(&kh).unwrap();
    let rewritten = recorded.replace(
        &format!("[127.0.0.1]:{}", first.port()),
        &format!("[127.0.0.1]:{}", second.port()),
    );
    std::fs::write(&kh, rewritten).unwrap();

    let err = expect_err(
        connect(
            second,
            kh,
            HostKeyPolicy::AcceptNew,
            vec![AuthMethod::Password(PASSWORD.into())],
        )
        .await,
        "a changed host key must be refused",
    );

    match err {
        opspilot_core_ssh::SshError::HostKey(msg) => {
            assert!(
                msg.contains("HAS CHANGED"),
                "the warning must be unmistakable, got: {msg}"
            );
        }
        other => panic!("expected HostKey, got {other:?}"),
    }
}

#[tokio::test]
async fn sftp_lists_reads_and_writes() {
    let dir = tmp("sftp");
    std::fs::write(dir.join("alpha.txt"), b"hello from alpha").unwrap();
    std::fs::create_dir_all(dir.join("subdir")).unwrap();

    let addr = start(1, dir.clone()).await;
    let t = connect(
        addr,
        dir.join("known_hosts"),
        HostKeyPolicy::AcceptNew,
        vec![AuthMethod::Password(PASSWORD.into())],
    )
    .await
    .unwrap();

    let sftp = t.open_sftp().await.expect("open sftp subsystem");

    // Listing: directories first, then names, with attributes from the single
    // readdir — no per-entry stat (§6.6).
    let entries = sftp.list("/").await.expect("list");
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"alpha.txt"), "missing file: {names:?}");
    assert!(names.contains(&"subdir"), "missing dir: {names:?}");
    assert_eq!(
        entries[0].name, "subdir",
        "directories must sort first, got {names:?}"
    );

    let alpha = entries.iter().find(|e| e.name == "alpha.txt").unwrap();
    assert_eq!(alpha.size, 16);
    assert_eq!(alpha.permission_string(), "rw-r--r--");

    // Read.
    let body = sftp.read("/alpha.txt").await.expect("read");
    assert_eq!(String::from_utf8_lossy(&body), "hello from alpha");

    // Write, then verify on disk — the bytes really crossed the wire.
    sftp.create("/beta.txt", b"written over sftp")
        .await
        .expect("create");
    let on_disk = std::fs::read_to_string(dir.join("beta.txt")).expect("file must exist");
    assert_eq!(on_disk, "written over sftp");

    // Rename and delete.
    sftp.rename("/beta.txt", "/gamma.txt")
        .await
        .expect("rename");
    assert!(dir.join("gamma.txt").exists());
    sftp.remove_file("/gamma.txt").await.expect("remove");
    assert!(!dir.join("gamma.txt").exists());
}

/// The `-R` path end to end: the client asks the server to listen, the server
/// opens a channel back, and the client joins it to a local TCP service.
#[tokio::test]
async fn a_remote_forward_delivers_a_server_initiated_connection() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let dir = tmp("rfwd");
    let addr = start(1, dir.clone()).await;
    let t = std::sync::Arc::new(
        connect(
            addr,
            dir.join("known_hosts"),
            HostKeyPolicy::AcceptNew,
            vec![AuthMethod::Password(PASSWORD.into())],
        )
        .await
        .unwrap(),
    );

    // A local "service" for the forward to deliver into.
    let service = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let service_addr = service.local_addr().unwrap();
    let served = tokio::spawn(async move {
        let (mut sock, _) = service.accept().await.unwrap();
        let mut buf = vec![0u8; 64];
        let n = sock.read(&mut buf).await.unwrap_or(0);
        let got = String::from_utf8_lossy(&buf[..n]).to_string();
        let _ = sock.write_all(b"ack").await;
        got
    });

    let _fwd = opspilot_core_ssh::forward::start_remote(
        std::sync::Arc::clone(&t),
        "r1".into(),
        "127.0.0.1",
        0,
        "127.0.0.1".into(),
        service_addr.port(),
    )
    .await
    .expect("remote forward should start");

    let got = tokio::time::timeout(std::time::Duration::from_secs(10), served)
        .await
        .expect("no connection was delivered to the local service")
        .unwrap();

    assert_eq!(
        got, "reverse-hello",
        "bytes from the server did not reach the local service"
    );
}

/// The vault's whole reason for existing, proved end to end.
///
/// Every other vault test asserts a property of the *file format*. This one
/// asserts the thing a user cares about: a password typed once, stored, and
/// then never typed again still logs in. It runs against the same in-process
/// SSH server as the rest of this file, so the handshake is real — if the
/// secret round-tripped through Argon2 and AES-GCM with so much as a byte out
/// of place, authentication fails here rather than passing a unit test.
#[tokio::test]
async fn a_secret_stored_in_the_vault_authenticates_a_real_connection() {
    use opspilot_core_store::vault::{Secret, Vault};

    let dir = tmp("vault-e2e");
    let addr = start(1, dir.clone()).await;

    // Store the server password under a reference, then drop the vault so
    // nothing but the file survives.
    let vault_path = dir.join("vault.bin");
    {
        let mut v = Vault::create(&vault_path, "the master password").unwrap();
        v.set("test/server", &Secret::from_string(PASSWORD.to_string()))
            .unwrap();
        v.save().unwrap();
    }

    // Reopen and resolve the reference exactly as the connect path does.
    let reopened = Vault::open(&vault_path, "the master password").unwrap();
    let resolved = reopened
        .resolve_ref("vault:test/server")
        .expect("resolving the reference failed")
        .expect("the reference named no record");

    let t = connect(
        addr,
        dir.join("known_hosts"),
        HostKeyPolicy::AcceptNew,
        vec![AuthMethod::Password(resolved.as_str().unwrap().to_string())],
    )
    .await
    .expect("a password recovered from the vault must authenticate");

    // And the connection is genuinely usable, not merely authenticated.
    let sftp = t
        .open_sftp()
        .await
        .expect("sftp over the vault-authenticated session");
    let entries = sftp.list("/").await.expect("list over the vault session");
    assert!(
        !entries.is_empty(),
        "the session authenticated but is unusable"
    );

    // A wrong master password must NOT yield a working credential.
    assert!(
        Vault::open(&vault_path, "the wrong master password").is_err(),
        "the vault opened with the wrong password"
    );
}
