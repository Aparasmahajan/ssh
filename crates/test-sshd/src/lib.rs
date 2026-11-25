//! A throwaway OpenSSH server for tests, and a proxy that adds latency.
//!
//! Real `sshd`, run as the current user on a free loopback port, with its own
//! host key, `authorized_keys` and config: no root, no PAM, no password auth,
//! and nothing touched outside its temp dir. Tests that use it skip (loudly)
//! where OpenSSH is not installed.
//!
//! Why real OpenSSH and not an in-process server: the transfer engine's
//! behaviour depends on what OpenSSH actually does — the `limits@openssh.com`
//! values it reports, short reads, `posix-rename`, how `internal-sftp` paces
//! replies. A mock agrees with whatever the code assumed.
//!
//! The proxy delays each direction by a fixed amount, which is CLAUDE.md §16's
//! "tc netem injecting 40 ms RTT" without needing root. It does not drop
//! packets; loss is not simulated.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

pub struct Sshd {
    pub dir: PathBuf,
    pub port: u16,
    pub user: String,
    /// Private key accepted by the server.
    pub key: PathBuf,
    sshd: Child,
    agent: Option<Child>,
}

impl Drop for Sshd {
    fn drop(&mut self) {
        for c in std::iter::once(&mut self.sshd).chain(self.agent.as_mut()) {
            let _ = c.kill();
            let _ = c.wait();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

pub fn which(bin: &str) -> Option<PathBuf> {
    let out = Command::new("sh").args(["-c", &format!("command -v {bin}")]).output().ok()?;
    let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!p.is_empty()).then(|| PathBuf::from(p))
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// Poll `f` every 20 ms until it holds; panic after `secs`.
pub fn wait_for(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn keygen(path: &Path) {
    let ok = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-C", "", "-f"])
        .arg(path)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    assert!(ok, "ssh-keygen {}", path.display());
}

impl Sshd {
    /// Start one, or `None` (with a SKIPPED line) where OpenSSH is missing.
    /// `tag` keeps concurrent fixtures in one process apart.
    pub fn start(tag: &str) -> Option<Sshd> {
        let sshd = which("sshd").or_else(|| Some(PathBuf::from("/usr/sbin/sshd")).filter(|p| p.exists()));
        let (Some(sshd), Some(_)) = (sshd, which("ssh-keygen")) else {
            eprintln!("SKIPPED: needs sshd and ssh-keygen");
            return None;
        };
        // Short: it may hold a Unix socket, and those paths cap near 108 bytes.
        let dir = std::env::temp_dir().join(format!("ops-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        keygen(&dir.join("host_key"));
        keygen(&dir.join("user_key"));
        std::fs::copy(dir.join("user_key.pub"), dir.join("authorized_keys")).unwrap();

        let port = free_port();
        let d = dir.display();
        std::fs::write(
            dir.join("sshd_config"),
            format!(
                "Port {port}\nListenAddress 127.0.0.1\nHostKey {d}/host_key\nPidFile {d}/sshd.pid\n\
                 AuthorizedKeysFile {d}/authorized_keys\nStrictModes no\nUsePAM no\n\
                 PasswordAuthentication no\nKbdInteractiveAuthentication no\nPubkeyAuthentication yes\n\
                 PrintMotd no\nPrintLastLog no\nLogLevel ERROR\nSubsystem sftp internal-sftp\n"
            ),
        )
        .unwrap();
        // sshd re-execs itself and insists on an absolute path.
        let child = Command::new(&sshd)
            .args(["-D", "-e", "-f"])
            .arg(dir.join("sshd_config"))
            .stderr(std::fs::File::create(dir.join("sshd.log")).unwrap())
            .spawn()
            .unwrap();
        wait_for("sshd to listen", 5, || std::net::TcpStream::connect(("127.0.0.1", port)).is_ok());
        Some(Sshd {
            key: dir.join("user_key"),
            dir,
            port,
            user: std::env::var("USER").unwrap_or_else(|_| "root".into()),
            sshd: child,
            agent: None,
        })
    }

    /// Start a private `ssh-agent` holding the user key and return its socket.
    /// `SSH_AUTH_SOCK` is set explicitly on `ssh-add`, never inherited: if the
    /// private agent failed to start, an inherited one would be the user's own.
    pub fn agent(&mut self) -> Option<PathBuf> {
        which("ssh-agent")?;
        which("ssh-add")?;
        let sock = self.dir.join("a");
        let agent = Command::new("ssh-agent")
            .args(["-D", "-a"])
            .arg(&sock)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        self.agent = Some(agent);
        wait_for("the test agent's socket", 5, || sock.exists());
        let added = Command::new("ssh-add")
            .arg("-q")
            .arg(&self.key)
            .env("SSH_AUTH_SOCK", &sock)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(added, "ssh-add into the test agent");
        Some(sock)
    }

    pub fn log(&self) -> String {
        std::fs::read_to_string(self.dir.join("sshd.log")).unwrap_or_default()
    }
}

/* ---- The slow link ---------------------------------------------------------- */

/// Copy `from` → `to`, delivering each chunk `delay` after it was read, in
/// order. Bytes keep flowing while earlier ones wait: a delay, not a throttle.
async fn delayed_pipe(
    mut from: tokio::net::tcp::OwnedReadHalf,
    mut to: tokio::net::tcp::OwnedWriteHalf,
    delay: Duration,
) {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<(tokio::time::Instant, Vec<u8>)>(4096);
    let writer = tokio::spawn(async move {
        while let Some((at, bytes)) = rx.recv().await {
            tokio::time::sleep_until(at).await;
            if to.write_all(&bytes).await.is_err() {
                break;
            }
        }
        let _ = to.shutdown().await;
    });
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        match from.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let at = tokio::time::Instant::now() + delay;
                if tx.send((at, buf[..n].to_vec())).await.is_err() {
                    break;
                }
            }
        }
    }
    drop(tx);
    let _ = writer.await;
}

/// Listen on a free loopback port and forward to `upstream`, adding `one_way`
/// in each direction (RTT = 2 × `one_way`). Runs on `rt` until it is dropped.
pub fn delay_proxy(rt: &tokio::runtime::Runtime, upstream: u16, one_way: Duration) -> u16 {
    let port = free_port();
    let listen = rt.block_on(TcpListener::bind(("127.0.0.1", port))).unwrap();
    let up = SocketAddr::from(([127, 0, 0, 1], upstream));
    rt.spawn(async move {
        while let Ok((client, _)) = listen.accept().await {
            tokio::spawn(async move {
                let Ok(server) = TcpStream::connect(up).await else { return };
                let _ = client.set_nodelay(true);
                let _ = server.set_nodelay(true);
                let (cr, cw) = client.into_split();
                let (sr, sw) = server.into_split();
                tokio::join!(delayed_pipe(cr, sw, one_way), delayed_pipe(sr, cw, one_way));
            });
        }
    });
    port
}
