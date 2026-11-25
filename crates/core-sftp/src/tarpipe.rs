//! Many small files: switch protocols. CLAUDE.md §6.2.
//!
//! > Per-file SFTP costs ~4 round trips (OPEN/WRITE/CLOSE/SETSTAT). 10,000
//! > files × 4 × 40 ms = 26 minutes of pure waiting. No window size fixes this.
//! > Tar-pipe: open an `exec` channel, stream `tar -cf - -C dir .` /
//! > `tar -xf - -C dir`. One round trip total.
//!
//! The local end is the `tar` crate on the blocking pool, bridged to the async
//! channel by a bounded queue in each direction (§13.1 rule 7).
//!
//! **Extraction is the security boundary.** On download the server writes the
//! archive, so every entry goes through `Entry::unpack_in`, which refuses
//! absolute paths, `..`, and writes through a symlink planted by an earlier
//! entry. Mode bits are kept without setuid/setgid (the crate's default).

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;

use opspilot_core_ssh::russh::ChannelMsg;
use opspilot_core_ssh::Transport;
use tokio::sync::mpsc;

use crate::progress::{Cancel, Progress};
use crate::tree::Tree;
use crate::Error;

/// Chunks queued between the network and the tar code, each up to 256 KiB.
const BRIDGE_DEPTH: usize = 32;
const BRIDGE_CHUNK: usize = 256 * 1024;
/// Bytes of the remote `tar`'s stderr kept for the error message.
const STDERR_KEEP: usize = 8 * 1024;

/// `'it'\''s'`: safe for any string under POSIX `sh`, including newlines.
/// Every path in a command line goes through this.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Bytes a tar of these files will be: a 512-byte header per entry, contents
/// padded to 512, two zero blocks at the end. Exact for plain names; long names
/// add a header each, so this is a progress estimate, not a promise.
pub fn archive_size(tree: &Tree) -> u64 {
    let pad = |n: u64| n.div_ceil(512) * 512;
    let files: u64 = tree.files.iter().map(|f| 512 + pad(f.meta.size.unwrap_or(0))).sum();
    files + 512 * (tree.dirs.len() + tree.links.len() + 1) as u64 + 1024
}

/// Is `tar` on the server's PATH? One exec round trip. A server without a
/// shell, or one that forbids exec, also answers no.
pub async fn available(t: &Transport) -> bool {
    let Ok(ch) = t.open_exec("command -v tar >/dev/null 2>&1").await else {
        return false;
    };
    let (mut read, _write) = ch.split();
    let wait = async {
        while let Some(msg) = read.wait().await {
            if let ChannelMsg::ExitStatus { exit_status } = msg {
                return exit_status == 0;
            }
        }
        false
    };
    tokio::time::timeout(std::time::Duration::from_secs(15), wait)
        .await
        .unwrap_or(false)
}

/// What the remote command left behind.
#[derive(Default)]
struct Outcome {
    status: Option<u32>,
    stderr: Vec<u8>,
}

impl Outcome {
    fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).trim().to_string()
    }

    /// GNU tar exits 1 for "file changed as we read it": a warning, and the
    /// archive is complete. 2 and above are failures.
    fn check(&self, what: &str, p: &Progress) -> Result<(), Error> {
        match self.status {
            Some(0) => Ok(()),
            Some(1) => {
                p.set_note(format!("tar warned: {}", self.stderr_text()));
                Ok(())
            }
            Some(n) => Err(Error::Remote(format!("{what}: tar exited {n}: {}", self.stderr_text()))),
            None => Err(Error::Remote(format!("{what}: the connection closed before tar finished"))),
        }
    }
}

/* ---- Download: remote tar -c → local unpack --------------------------------- */

/// Pull `remote_parent/name` into `local_parent/name`.
pub async fn download(
    t: &Transport,
    remote_parent: &str,
    name: &str,
    local_parent: PathBuf,
    p: Arc<Progress>,
    cancel: &Cancel,
) -> Result<(), Error> {
    // `./name`, not `name`: a name starting with `-` must not read as an option.
    let cmd = format!(
        "tar -cf - -C {} {}",
        shell_quote(remote_parent),
        shell_quote(&format!("./{name}"))
    );
    let ch = t.open_exec(&cmd).await.map_err(Error::ssh)?;
    let (mut read, write) = ch.split();
    let _ = write.eof().await; // tar -c reads no stdin

    let (tx, rx) = mpsc::channel::<Vec<u8>>(BRIDGE_DEPTH);
    let progress = Arc::clone(&p);
    let unpack = tokio::task::spawn_blocking(move || unpack_all(rx, &local_parent, &progress));

    let mut out = Outcome::default();
    let mut tx = Some(tx);
    let pumped = async {
        while let Some(msg) = read.wait().await {
            match msg {
                ChannelMsg::Data { data } => {
                    p.add_bytes(data.len() as u64);
                    // The unpacker has stopped (an error): stop feeding it,
                    // but keep reading for the exit status.
                    if let Some(t) = &tx {
                        if t.send(data.to_vec()).await.is_err() {
                            tx = None;
                        }
                    }
                }
                ChannelMsg::ExtendedData { data, .. } => keep_stderr(&mut out.stderr, &data),
                ChannelMsg::ExitStatus { exit_status } => out.status = Some(exit_status),
                ChannelMsg::Eof => tx = None, // end of the archive
                ChannelMsg::Close => break,
                _ => {}
            }
        }
        tx = None;
    };
    tokio::select! {
        biased;
        _ = cancel.wait() => {
            let _ = write.close().await;
            return Err(Error::Cancelled);
        }
        _ = pumped => {}
    }
    let unpacked = unpack.await.map_err(|e| Error::Io(e.to_string()))?;
    out.check(name, &p)?;
    unpacked
}

/// Runs on the blocking pool. O(archive bytes).
fn unpack_all(rx: mpsc::Receiver<Vec<u8>>, dst: &std::path::Path, p: &Progress) -> Result<(), Error> {
    std::fs::create_dir_all(dst).map_err(|e| Error::Io(format!("{}: {e}", dst.display())))?;
    let mut archive = tar::Archive::new(BridgeReader { rx, cur: Vec::new(), pos: 0 });
    archive.set_preserve_mtime(true);
    archive.set_overwrite(true);
    let entries = archive.entries().map_err(|e| Error::Io(format!("tar: {e}")))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| Error::Io(format!("tar: {e}")))?;
        let regular = entry.header().entry_type().is_file();
        // `unpack_in` is the guard: false means the entry tried to leave
        // `dst` and was refused.
        match entry.unpack_in(dst) {
            Ok(true) if regular => p.file_done(),
            Ok(true) => {}
            Ok(false) => {
                p.files_skipped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            Err(e) => return Err(Error::Io(format!("tar: {e}"))),
        }
    }
    Ok(())
}

struct BridgeReader {
    rx: mpsc::Receiver<Vec<u8>>,
    cur: Vec<u8>,
    pos: usize,
}

impl Read for BridgeReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        while self.pos >= self.cur.len() {
            match self.rx.blocking_recv() {
                Some(next) => {
                    self.cur = next;
                    self.pos = 0;
                }
                None => return Ok(0),
            }
        }
        let n = buf.len().min(self.cur.len() - self.pos);
        buf[..n].copy_from_slice(&self.cur[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/* ---- Upload: local pack → remote tar -x ------------------------------------- */

/// Push `local_root` (a directory, already walked into `tree`) to
/// `remote_parent/name`.
pub async fn upload(
    t: &Transport,
    local_root: PathBuf,
    tree: Tree,
    remote_parent: &str,
    name: &str,
    p: Arc<Progress>,
    cancel: &Cancel,
) -> Result<(), Error> {
    let cmd = format!("tar -xf - -C {}", shell_quote(remote_parent));
    let ch = t.open_exec(&cmd).await.map_err(Error::ssh)?;
    let (mut read, write) = ch.split();

    // The remote's stderr and exit status, read concurrently with the upload:
    // a tar that fails early (permission denied) must be heard while we are
    // still writing, not after.
    let status = tokio::spawn(async move {
        let mut out = Outcome::default();
        while let Some(msg) = read.wait().await {
            match msg {
                ChannelMsg::ExtendedData { data, .. } | ChannelMsg::Data { data } => {
                    keep_stderr(&mut out.stderr, &data)
                }
                ChannelMsg::ExitStatus { exit_status } => out.status = Some(exit_status),
                ChannelMsg::Close => break,
                _ => {}
            }
        }
        out
    });

    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(BRIDGE_DEPTH);
    let progress = Arc::clone(&p);
    let name_owned = name.to_string();
    let pack = tokio::task::spawn_blocking(move || pack_all(tx, &local_root, &tree, &name_owned, &progress));

    let send = async {
        while let Some(chunk) = rx.recv().await {
            let n = chunk.len() as u64;
            // Waits on the channel window: this is the backpressure.
            if write.data_bytes(chunk).await.is_err() {
                break; // the remote end went away; its status says why
            }
            p.add_bytes(n);
        }
        let _ = write.eof().await;
    };
    tokio::select! {
        biased;
        _ = cancel.wait() => {
            let _ = write.close().await;
            status.abort();
            return Err(Error::Cancelled);
        }
        _ = send => {}
    }
    let packed = pack.await.map_err(|e| Error::Io(e.to_string()))?;
    let out = tokio::select! {
        biased;
        _ = cancel.wait() => return Err(Error::Cancelled),
        o = status => o.map_err(|e| Error::Io(e.to_string()))?,
    };
    // The remote verdict first: a broken pipe locally is usually its symptom.
    out.check(name, &p)?;
    packed
}

/// Runs on the blocking pool. Entries come from the walk, so symlinks are
/// stored as links and never followed. O(tree bytes).
fn pack_all(
    tx: mpsc::Sender<Vec<u8>>,
    root: &std::path::Path,
    tree: &Tree,
    name: &str,
    p: &Progress,
) -> Result<(), Error> {
    let mut b = tar::Builder::new(BridgeWriter { tx, buf: Vec::with_capacity(BRIDGE_CHUNK) });
    b.follow_symlinks(false);
    let io = |e: std::io::Error| Error::Io(format!("tar: {e}"));
    let local = |rel: &str| rel.split('/').fold(root.to_path_buf(), |p, c| p.join(c));

    b.append_dir(name, root).map_err(io)?;
    for d in &tree.dirs {
        b.append_dir(format!("{name}/{}", d.rel), local(&d.rel)).map_err(io)?;
    }
    for l in &tree.links {
        b.append_path_with_name(local(&l.rel), format!("{name}/{}", l.rel)).map_err(io)?;
    }
    for f in &tree.files {
        b.append_path_with_name(local(&f.rel), format!("{name}/{}", f.rel)).map_err(io)?;
        p.file_done();
    }
    let mut w = b.into_inner().map_err(io)?;
    w.flush().map_err(io)?;
    Ok(())
}

struct BridgeWriter {
    tx: mpsc::Sender<Vec<u8>>,
    buf: Vec<u8>,
}

impl BridgeWriter {
    fn send(&mut self) -> std::io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let chunk = std::mem::replace(&mut self.buf, Vec::with_capacity(BRIDGE_CHUNK));
        self.tx
            .blocking_send(chunk)
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "upload stopped"))
    }
}

impl Write for BridgeWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let n = data.len().min(BRIDGE_CHUNK - self.buf.len());
        self.buf.extend_from_slice(&data[..n]);
        if self.buf.len() >= BRIDGE_CHUNK {
            self.send()?;
        }
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.send()
    }
}

fn keep_stderr(buf: &mut Vec<u8>, data: &[u8]) {
    let room = STDERR_KEEP.saturating_sub(buf.len());
    buf.extend_from_slice(&data[..data.len().min(room)]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_survives_every_shell_metacharacter() {
        assert_eq!(shell_quote("plain"), "'plain'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        // Run the quoted string through a real sh and get it back unchanged.
        let nasty = "a b;$(rm -rf ~)`x`\"q\"'s'\n\\*?[x]&|<>#~";
        let out = std::process::Command::new("sh")
            .args(["-c", &format!("printf %s {}", shell_quote(nasty))])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), nasty);
    }

    #[test]
    fn archive_size_counts_headers_and_padding() {
        use crate::pipe::Meta;
        use crate::tree::{Item, Kind};
        let f = |n| Item { rel: "x".into(), kind: Kind::File, meta: Meta { size: Some(n), ..Default::default() } };
        let tree = Tree { files: vec![f(1), f(512), f(513)], ..Default::default() };
        // 3 headers + (512 + 512 + 1024) data + root dir header + 1024 trailer.
        assert_eq!(archive_size(&tree), 3 * 512 + 2048 + 512 + 1024);
    }

    /// Pack a real tree and unpack it through the bridges, with no network:
    /// the bytes that would cross the channel go straight from one bridge to
    /// the other.
    #[test]
    fn a_tree_round_trips_through_the_bridges() {
        let base = std::env::temp_dir().join(format!("opspilot-tar-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let src = base.join("src");
        std::fs::create_dir_all(src.join("sub/deeper")).unwrap();
        std::fs::create_dir_all(src.join("empty")).unwrap();
        std::fs::write(src.join("a.txt"), b"alpha").unwrap();
        std::fs::write(src.join("sub/deeper/b.bin"), vec![7u8; 70_000]).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("a.txt", src.join("link")).unwrap();

        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let tree = rt.block_on(crate::tree::local(src.clone())).unwrap();
        let p = Progress::default();
        let (tx, rx) = mpsc::channel(BRIDGE_DEPTH);
        let dst = base.join("dst");
        let unpack = {
            let dst = dst.clone();
            std::thread::spawn(move || unpack_all(rx, &dst, &Progress::default()))
        };
        pack_all(tx, &src, &tree, "copy", &p).unwrap();
        unpack.join().unwrap().unwrap();

        assert_eq!(std::fs::read(dst.join("copy/a.txt")).unwrap(), b"alpha");
        assert_eq!(std::fs::read(dst.join("copy/sub/deeper/b.bin")).unwrap().len(), 70_000);
        assert!(dst.join("copy/empty").is_dir());
        #[cfg(unix)]
        assert_eq!(std::fs::read_link(dst.join("copy/link")).unwrap(), PathBuf::from("a.txt"));
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A hostile archive: entries that climb out of the destination are
    /// refused, the rest still land.
    #[test]
    fn a_hostile_archive_cannot_write_outside_the_destination() {
        let base = std::env::temp_dir().join(format!("opspilot-tarx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dst = base.join("dst");

        let mut raw = Vec::new();
        {
            let mut b = tar::Builder::new(&mut raw);
            let mut add = |path: &[u8], body: &[u8]| {
                let mut h = tar::Header::new_gnu();
                // Write the name bytes directly: the builder's own setters
                // refuse `..`, which is the point, but a server is not bound
                // by them.
                h.as_old_mut().name[..path.len()].copy_from_slice(path);
                h.set_size(body.len() as u64);
                h.set_entry_type(tar::EntryType::Regular);
                h.set_mode(0o644);
                h.set_cksum();
                b.append(&h, body).unwrap();
            };
            add(b"../escaped.txt", b"bad");
            add(b"ok/inside.txt", b"good");
        }
        let (tx, rx) = mpsc::channel(4);
        tx.blocking_send(raw).unwrap();
        drop(tx);
        let p = Progress::default();
        unpack_all(rx, &dst, &p).unwrap();

        assert!(!base.join("escaped.txt").exists(), "an entry escaped the destination");
        assert_eq!(std::fs::read(dst.join("ok/inside.txt")).unwrap(), b"good");
        assert_eq!(p.files_skipped.load(std::sync::atomic::Ordering::Relaxed), 1);
        let _ = std::fs::remove_dir_all(&base);
    }
}
