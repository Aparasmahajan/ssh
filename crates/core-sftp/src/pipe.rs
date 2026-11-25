//! One file, many requests in flight. CLAUDE.md §6.1.
//!
//! > WinSCP sends one `SSH_FXP_READ`, waits a full RTT, sends the next. On a
//! > 40 ms link that caps at ~800 KB/s regardless of bandwidth.
//!
//! Here up to `w` reads (or writes) are outstanding at once, `w` set by the AIMD
//! window. Responses can arrive out of order; they are reassembled in a
//! `BTreeMap<offset, bytes>` and only the contiguous prefix is written (§6.1).
//!
//! Both directions write to a part file next to the target and rename it into
//! place at the end, so a failed or cancelled transfer never leaves a truncated
//! file under the real name, or clobbers a good one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use futures_util::stream::{FuturesUnordered, StreamExt};
use opspilot_core_ssh::russh_sftp::client::error::Error as SftpError;
use opspilot_core_ssh::russh_sftp::protocol::{FileAttributes, OpenFlags, StatusCode};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::aimd::Window;
use crate::progress::{Cancel, Progress};
use crate::session::Session;
use crate::Error;

/// Out-of-order bytes held waiting for a gap to fill. Past this the window is
/// too deep for the path: halve it (§6.1's "buffer-full").
const REORDER_LIMIT: usize = 64 * 1024 * 1024;

/// What a listing already told us about a remote file, so a download needs no
/// `stat` of its own (§6.6).
#[derive(Clone, Copy, Debug, Default)]
pub struct Meta {
    pub size: Option<u64>,
    pub mode: Option<u32>,
    pub mtime: Option<u32>,
}

/// `dir/.name.opspilot-part`. Hidden, and obviously not the real file.
pub fn part_name(name: &str) -> String {
    format!(".{name}.opspilot-part")
}

fn local_part(target: &Path) -> PathBuf {
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    target.with_file_name(part_name(&name))
}

pub fn remote_join(dir: &str, name: &str) -> String {
    if dir.is_empty() || dir == "/" {
        format!("/{name}")
    } else {
        format!("{}/{name}", dir.trim_end_matches('/'))
    }
}

fn remote_part(target: &str) -> String {
    match target.rsplit_once('/') {
        Some((dir, name)) => remote_join(if dir.is_empty() { "/" } else { dir }, &part_name(name)),
        None => part_name(target),
    }
}

/// Remote → local. Returns bytes written.
pub async fn download(
    s: &Session,
    remote: &str,
    meta: Meta,
    local: &Path,
    win: &mut Window,
    p: &Progress,
    cancel: &Cancel,
) -> Result<u64, Error> {
    let part = local_part(local);
    let r = download_to(s, remote, meta, &part, win, p, cancel).await;
    match r {
        Ok(n) => {
            tokio::fs::rename(&part, local)
                .await
                .map_err(|e| Error::Io(format!("{}: {e}", local.display())))?;
            apply_local_meta(local, meta).await;
            Ok(n)
        }
        Err(e) => {
            let _ = tokio::fs::remove_file(&part).await;
            Err(e)
        }
    }
}

type ReadResult = (u64, u32, Instant, Result<Vec<u8>, SftpError>);

#[allow(clippy::too_many_arguments)]
async fn download_to(
    s: &Session,
    remote: &str,
    meta: Meta,
    part: &Path,
    win: &mut Window,
    p: &Progress,
    cancel: &Cancel,
) -> Result<u64, Error> {
    let handle = s
        .raw
        .open(remote, OpenFlags::READ, FileAttributes::empty())
        .await
        .map_err(|e| Error::remote(remote, e))?
        .handle;
    let r = async {
        // §6.6: the listing already sent the size. Only a bare path needs a
        // round trip for it.
        let size = match meta.size {
            Some(n) => Some(n),
            None => s.raw.fstat(handle.clone()).await.ok().and_then(|a| a.attrs.size),
        };
        let limit = size.unwrap_or(u64::MAX);
        let mut file = tokio::fs::File::create(part)
            .await
            .map_err(|e| Error::Io(format!("{}: {e}", part.display())))?;

        let chunk = s.read_len;
        let mut next = 0u64; // next offset to request
        let mut written = 0u64; // contiguous prefix on disk
        let mut eof: Option<u64> = None;
        let mut held: BTreeMap<u64, Vec<u8>> = BTreeMap::new();
        let mut held_bytes = 0usize;
        // Remainders of short reads, requested before new offsets.
        let mut retry: Vec<(u64, u32)> = Vec::new();
        let mut inflight: FuturesUnordered<_> = FuturesUnordered::new();

        let issue = |off: u64, len: u32| {
            let raw = Arc::clone(&s.raw);
            let h = handle.clone();
            async move {
                let t = Instant::now();
                let r = raw.read(h, off, len).await.map(|d| d.data);
                (off, len, t, r) as ReadResult
            }
        };

        loop {
            // Fill the window. O(w) per completion.
            while (inflight.len() as u32) < win.get() {
                if let Some((off, len)) = retry.pop() {
                    inflight.push(issue(off, len));
                    continue;
                }
                let end = eof.unwrap_or(limit);
                if next >= end {
                    break;
                }
                let len = (end - next).min(chunk as u64) as u32;
                inflight.push(issue(next, len));
                next += len as u64;
            }
            p.show_window(win.get());
            if inflight.is_empty() {
                break;
            }

            let (off, len, started, res) = tokio::select! {
                biased;
                _ = cancel.wait() => return Err(Error::Cancelled),
                r = inflight.next() => match r { Some(r) => r, None => break },
            };
            match res {
                Ok(data) if !data.is_empty() => {
                    let n = data.len() as u64;
                    win.on_ack(started.elapsed(), n, Instant::now());
                    p.add_bytes(n);
                    // A short read is legal: ask for the rest.
                    if (n as u32) < len {
                        retry.push((off + n, len - n as u32));
                    }
                    held_bytes += data.len();
                    held.insert(off, data);
                }
                // Empty data, or EOF status: the file ends at or before `off`.
                Ok(_) => eof = Some(eof.map_or(off, |e| e.min(off))),
                Err(SftpError::Status(st)) if st.status_code == StatusCode::Eof => {
                    eof = Some(eof.map_or(off, |e| e.min(off)));
                }
                Err(SftpError::Timeout) => {
                    win.on_stall();
                    retry.push((off, len));
                }
                Err(e) => return Err(Error::remote(remote, e)),
            }

            // Flush the contiguous prefix. Each entry is written once: O(n).
            while let Some(data) = held.remove(&written) {
                held_bytes -= data.len();
                file.write_all(&data)
                    .await
                    .map_err(|e| Error::Io(format!("{}: {e}", part.display())))?;
                written += data.len() as u64;
            }
            if held_bytes > REORDER_LIMIT {
                win.on_stall();
            }
        }

        // A file that shrank ends at its EOF; one that did not is complete at
        // its listed size. Anything still held is a gap: fail, never truncate
        // silently.
        let expected = eof.map_or(limit, |e| e.min(limit));
        if !held.is_empty() || (size.is_some() && written != expected) {
            return Err(Error::Remote(format!(
                "{remote}: incomplete read ({written} of {expected} bytes)"
            )));
        }
        file.flush().await.map_err(|e| Error::Io(e.to_string()))?;
        Ok(written)
    }
    .await;
    // Close without waiting: a read handle has nothing to flush, and the
    // reply would cost a round trip per file.
    let raw = Arc::clone(&s.raw);
    tokio::spawn(async move {
        let _ = raw.close(handle).await;
    });
    r
}

async fn apply_local_meta(path: &Path, meta: Meta) {
    let path = path.to_path_buf();
    let _ = tokio::task::spawn_blocking(move || {
        #[cfg(unix)]
        if let Some(mode) = meta.mode {
            use std::os::unix::fs::PermissionsExt;
            // Permission bits only: never setuid/setgid/sticky from a server.
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode & 0o777));
        }
        if let Some(m) = meta.mtime {
            let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(m as u64);
            if let Ok(f) = std::fs::OpenOptions::new().write(true).open(&path) {
                let _ = f.set_times(std::fs::FileTimes::new().set_modified(t));
            }
        }
    })
    .await;
}

/// Local → remote. `replace` allows an existing `remote` to be replaced.
pub async fn upload(
    s: &Session,
    local: &Path,
    remote: &str,
    replace: bool,
    win: &mut Window,
    p: &Progress,
    cancel: &Cancel,
) -> Result<u64, Error> {
    let start = win.clone();
    let r = upload_striped(&[s], local, remote, replace, &start, p, cancel).await;
    p.show_window(win.get());
    r
}

/// One upload over several channels, each writing a disjoint range of the
/// same part file.
///
/// **Why.** The server's receive window caps what one channel can carry per
/// round trip, and the client cannot change it. OpenSSH advertises 2 MiB, so a
/// single channel uploads at most ~50 MB/s through a 40 ms RTT, whatever the
/// request window (measured: 41 MB/s). Every channel has its own window, so N
/// channels lift the cap N-fold. OpenSSH's `sftp` and WinSCP both stop at the
/// single-channel ceiling.
///
/// Workers claim the next chunk from one shared offset, so a slower channel
/// simply does less of the file. Session 0 creates (and truncates) the part
/// file; the others open it for writing only. Each worker has its own window,
/// started from `start`.
pub async fn upload_striped(
    sessions: &[&Session],
    local: &Path,
    remote: &str,
    replace: bool,
    start: &Window,
    p: &Progress,
    cancel: &Cancel,
) -> Result<u64, Error> {
    let lead = sessions[0];
    let part = remote_part(remote);
    let r = upload_parts(sessions, local, &part, start, p, cancel).await;
    match r {
        Ok(n) => {
            lead.rename(&part, remote, replace).await?;
            Ok(n)
        }
        Err(e) => {
            // Background: never wait on a connection that may be why this
            // failed.
            let raw = Arc::clone(&lead.raw);
            tokio::spawn(async move {
                let _ = raw.remove(part).await;
            });
            Err(e)
        }
    }
}

type WriteResult = (u32, Instant, Result<(), SftpError>);

async fn upload_parts(
    sessions: &[&Session],
    local: &Path,
    part: &str,
    start: &Window,
    p: &Progress,
    cancel: &Cancel,
) -> Result<u64, Error> {
    let md = tokio::fs::metadata(local)
        .await
        .map_err(|e| Error::Io(format!("{}: {e}", local.display())))?;
    let size = md.len();
    let mode = local_mode(&md);
    let mtime = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as u32);

    let open = |s: &Session, flags: OpenFlags| {
        let raw = Arc::clone(&s.raw);
        async move {
            raw.open(
                part,
                flags,
                // `empty()`, NEVER `default()`: russh-sftp's Default is size 0,
                // uid/gid 0, times 0 and mode 0o777|DIR. As setstat it
                // truncated every upload to nothing (the round-trip test caught
                // it); as create/mkdir attributes it asks for mode 0o777, which
                // only the server's umask stands between and a world-writable
                // result. See CLAUDE.md §18.22.
                FileAttributes::empty(),
            )
            .await
            .map(|h| h.handle)
            .map_err(|e| Error::remote(part, e))
        }
    };
    // The lead creates and truncates; the rest open what now exists.
    let lead = open(sessions[0], OpenFlags::CREATE | OpenFlags::TRUNCATE | OpenFlags::WRITE).await?;
    let mut handles = vec![lead.clone()];
    let rest = futures_util::future::join_all(sessions[1..].iter().map(|s| open(s, OpenFlags::WRITE))).await;
    let mut opened = Ok(());
    for h in rest {
        match h {
            Ok(h) => handles.push(h),
            Err(e) => opened = Err(e),
        }
    }

    let next = std::sync::atomic::AtomicU64::new(0);
    let r = match opened {
        Err(e) => Err(e),
        Ok(()) => {
            let workers = sessions.iter().zip(&handles).map(|(s, h)| {
                stripe(s, h, local, size, &next, start.clone(), p, cancel)
            });
            futures_util::future::try_join_all(workers).await.map(|_| size)
        }
    };

    // Close every handle but the lead's in the background: nothing to report.
    for (s, h) in sessions.iter().zip(&handles).skip(1) {
        let (raw, h) = (Arc::clone(&s.raw), h.clone());
        tokio::spawn(async move {
            let _ = raw.close(h).await;
        });
    }
    if r.is_err() {
        let raw = Arc::clone(&sessions[0].raw);
        tokio::spawn(async move {
            let _ = raw.close(lead).await;
        });
        return r;
    }

    // Mode and mtime, and the close, in one round trip rather than two. The
    // close is awaited: on a write handle it is where the server reports a
    // failed flush. Only these three fields; everything else is not sent.
    let attrs = FileAttributes {
        permissions: mode,
        atime: mtime,
        mtime,
        ..FileAttributes::empty()
    };
    let raw = &sessions[0].raw;
    let (set, close) = tokio::join!(raw.fsetstat(lead.clone(), attrs), raw.close(lead));
    close.map_err(|e| Error::remote(part, e))?;
    let _ = set; // best effort: some servers refuse setstat
    r
}

/// One channel's share of an upload: claim a chunk, read it, write it, with up
/// to `win` writes in flight. O(chunks claimed).
#[allow(clippy::too_many_arguments)]
async fn stripe(
    s: &Session,
    handle: &str,
    local: &Path,
    size: u64,
    next: &std::sync::atomic::AtomicU64,
    mut win: Window,
    p: &Progress,
    cancel: &Cancel,
) -> Result<(), Error> {
    use std::sync::atomic::Ordering;
    use tokio::io::AsyncSeekExt;
    let mut file = tokio::fs::File::open(local)
        .await
        .map_err(|e| Error::Io(format!("{}: {e}", local.display())))?;
    let chunk = s.write_len as u64;
    let mut pos = 0u64; // where `file`'s cursor is, to skip needless seeks
    let mut done = false;
    let mut inflight: FuturesUnordered<_> = FuturesUnordered::new();
    loop {
        while !done && (inflight.len() as u32) < win.get() {
            let off = next.fetch_add(chunk, Ordering::Relaxed);
            if off >= size {
                done = true;
                break;
            }
            let len = chunk.min(size - off) as usize;
            if pos != off {
                file.seek(std::io::SeekFrom::Start(off))
                    .await
                    .map_err(|e| Error::Io(e.to_string()))?;
            }
            let mut buf = vec![0u8; len];
            file.read_exact(&mut buf)
                .await
                .map_err(|e| Error::Io(format!("{}: {e}", local.display())))?;
            pos = off + len as u64;
            let raw = Arc::clone(&s.raw);
            let h = handle.to_string();
            inflight.push(async move {
                let t = Instant::now();
                let r = raw.write(h, off, buf).await.map(|_| ());
                (len as u32, t, r) as WriteResult
            });
        }
        p.show_window(win.get());
        if inflight.is_empty() {
            return Ok(());
        }
        let (len, started, res) = tokio::select! {
            biased;
            _ = cancel.wait() => return Err(Error::Cancelled),
            r = inflight.next() => match r { Some(r) => r, None => return Ok(()) },
        };
        res.map_err(|e| Error::remote(handle, e))?;
        win.on_ack(started.elapsed(), len as u64, Instant::now());
        p.add_bytes(len as u64);
    }
}

#[cfg(unix)]
fn local_mode(md: &std::fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    Some(md.permissions().mode() & 0o777)
}

#[cfg(not(unix))]
fn local_mode(_: &std::fs::Metadata) -> Option<u32> {
    // Windows has no mode bits (CLAUDE.md §18.11): send none rather than
    // inventing 0644. The server's umask decides.
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn part_files_sit_next_to_the_target() {
        assert_eq!(remote_part("/srv/app/data.bin"), "/srv/app/.data.bin.opspilot-part");
        assert_eq!(remote_part("/data.bin"), "/.data.bin.opspilot-part");
        assert_eq!(
            local_part(Path::new("/home/u/x.tar")),
            PathBuf::from("/home/u/.x.tar.opspilot-part")
        );
    }

    #[test]
    fn remote_paths_join_with_one_slash() {
        assert_eq!(remote_join("/srv/", "a"), "/srv/a");
        assert_eq!(remote_join("/", "a"), "/a");
        assert_eq!(remote_join("", "a"), "/a");
        assert_eq!(remote_join("/srv", "a"), "/srv/a");
    }
}
