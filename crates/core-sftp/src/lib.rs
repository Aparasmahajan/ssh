//! The transfer engine. CLAUDE.md §6, "the flagship win".
//!
//! One entry point, [`run`], takes a transfer request and picks how to carry it:
//!
//! | Request | Method | Why |
//! |---|---|---|
//! | one file | [`Method::Pipelined`]: up to `w` requests in flight, AIMD (§6.1) | a large file at high RTT is bounded by requests in flight, not bandwidth |
//! | a directory of > 200 files averaging < 1 MB, server has `tar` | [`Method::TarPipe`] (§6.2) | per-file SFTP is ~3 round trips per file; tar is one stream |
//! | any other directory | [`Method::Parallel`]: 4–8 SFTP channels, largest file first | one big file must not hold up the rest |
//!
//! Progress is atomics in [`Progress`], sampled by the caller (§13.1 rule 8).
//! Cancellation is cooperative via [`Cancel`] and takes effect mid-request.
//!
//! Partial files never appear under the real name: each file is written to
//! `.name.opspilot-part` and renamed into place when complete.

pub mod aimd;
pub mod pipe;
pub mod progress;
pub mod session;
pub mod tarpipe;
pub mod tree;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use futures_util::future::try_join_all;
use opspilot_core_ssh::russh_sftp::client::error::Error as SftpError;
use opspilot_core_ssh::{SshError, Transport};

pub use aimd::Window;
pub use pipe::Meta;
pub use progress::{Cancel, Method, Progress};
use session::Session;
use tree::{Item, Kind, Tree};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The target exists and overwriting was not asked for.
    Exists(String),
    Cancelled,
    Ssh(String),
    Remote(String),
    Io(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Exists(p) => write!(f, "{p} already exists"),
            Error::Cancelled => f.write_str("cancelled"),
            Error::Ssh(e) | Error::Remote(e) | Error::Io(e) => f.write_str(e),
        }
    }
}

impl std::error::Error for Error {}

impl Error {
    pub(crate) fn ssh(e: SshError) -> Error {
        Error::Ssh(e.to_string())
    }
    pub(crate) fn sftp(e: SftpError) -> Error {
        Error::Remote(e.to_string())
    }
    pub(crate) fn remote(path: &str, e: SftpError) -> Error {
        Error::Remote(format!("{path}: {e}"))
    }
}

#[derive(Clone, Debug)]
pub enum Request {
    /// `remote` (a file or directory) into `local_dir/<its name>`.
    Download { remote: String, local_dir: PathBuf },
    /// `local` (a file or directory) into `remote_dir/<its name>`.
    Upload { local: PathBuf, remote_dir: String },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TarPolicy {
    /// §6.2's rule: > 200 files averaging < 1 MB.
    #[default]
    Auto,
    Never,
    Always,
}

#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Replace an existing target. For a directory, merge into it, replacing
    /// files that collide.
    pub overwrite: bool,
    pub tar: TarPolicy,
    /// `(start, min, max)` for every request window. `None` is §6.1's AIMD
    /// defaults. `Some((1, 1, 1))` is WinSCP: one request at a time.
    pub window: Option<(u32, u32, u32)>,
    /// SFTP channels for a directory. `None` picks 8 for small files, else 4.
    pub channels: Option<u32>,
    /// Bytes per read/write request, capped by the server's limit. `None` is
    /// the server's limit up to 256 KiB (§6.1). WinSCP uses 32 KiB.
    pub io_len: Option<u32>,
    /// Channels one large upload is striped over. `None` is
    /// [`UPLOAD_STRIPES`] for files of [`STRIPE_MIN`] or more.
    pub stripes: Option<u32>,
}

/// Channels for one large upload (see [`pipe::upload_striped`]). Measured on
/// a 128 MiB upload through 40 ms RTT to OpenSSH: 1 channel 41 MB/s,
/// 4 channels 96 MB/s, 8 channels 129 MB/s.
pub const UPLOAD_STRIPES: u32 = 8;
/// Below this, opening extra channels (~3 round trips, concurrently) costs
/// more than the server-window ceiling does.
pub const STRIPE_MIN: u64 = 16 * 1024 * 1024;

async fn open(t: &Transport, opts: &Options) -> Result<Session, Error> {
    let mut s = Session::open(t).await?;
    if let Some(n) = opts.io_len {
        s.read_len = s.read_len.min(n.max(512));
        s.write_len = s.write_len.min(n.max(512));
    }
    Ok(s)
}

#[derive(Clone, Debug)]
pub struct Summary {
    pub method: Method,
    pub files: u64,
    pub bytes: u64,
    /// Not copied: symlinks on the SFTP path, special files, unsafe names.
    pub skipped: u64,
    /// The largest request window reached (§6.1), for the perf overlay.
    pub peak_window: u32,
}

/// §6.2: "Detect: queue has >200 files averaging <1 MB."
pub fn wants_tar(files: usize, bytes: u64) -> bool {
    files > 200 && bytes / (files.max(1) as u64) < 1024 * 1024
}

fn window(opts: &Options, chunk: u32) -> Window {
    match opts.window {
        Some((start, min, max)) => Window::bounded(chunk as u64, start, min, max),
        None => Window::new(chunk as u64),
    }
}

fn base_name(path: &str) -> Option<String> {
    let trimmed = path.trim_end_matches('/');
    let name = trimmed.rsplit('/').next().unwrap_or(trimmed);
    tree::safe_name(name).then(|| name.to_string())
}

fn local_join(root: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(root.to_path_buf(), |p, c| p.join(c))
}

/// Carry out one transfer. Updates `p` as it goes; returns what was done.
pub async fn run(
    t: Arc<Transport>,
    req: Request,
    opts: Options,
    p: Arc<Progress>,
    cancel: Arc<Cancel>,
) -> Result<Summary, Error> {
    let r = match req {
        Request::Download { remote, local_dir } => download(&t, &remote, &local_dir, &opts, &p, &cancel).await,
        Request::Upload { local, remote_dir } => upload(&t, &local, &remote_dir, &opts, &p, &cancel).await,
    };
    if cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    r.map(|mut s| {
        s.peak_window = p.window.load(Ordering::Relaxed);
        s
    })
}

async fn download(
    t: &Arc<Transport>,
    remote: &str,
    local_dir: &Path,
    opts: &Options,
    p: &Arc<Progress>,
    cancel: &Arc<Cancel>,
) -> Result<Summary, Error> {
    let name = base_name(remote).ok_or_else(|| Error::Remote(format!("{remote}: not a downloadable name")))?;
    let target = local_dir.join(&name);
    let ctl = open(t, opts).await?;
    // `stat`, not `lstat`: a symlink the user selected means its target.
    let attrs = ctl.raw.stat(remote).await.map_err(|e| Error::remote(remote, e))?.attrs;
    if !opts.overwrite && tokio::fs::try_exists(&target).await.unwrap_or(false) {
        return Err(Error::Exists(target.display().to_string()));
    }

    match tree::kind_of(&attrs) {
        Kind::File => {
            let meta = tree::meta_of(&attrs);
            p.files_total.store(1, Ordering::Relaxed);
            p.bytes_total.store(meta.size.unwrap_or(0), Ordering::Relaxed);
            p.set_method(Method::Pipelined);
            let mut w = window(opts, ctl.read_len);
            let n = pipe::download(&ctl, remote, meta, &target, &mut w, p, cancel).await?;
            p.file_done();
            Ok(Summary { method: Method::Pipelined, files: 1, bytes: n, skipped: 0, peak_window: 0 })
        }
        Kind::Dir => {
            let tree = tree::remote(&ctl.raw, remote, cancel).await?;
            p.files_total.store(tree.files.len() as u64, Ordering::Relaxed);
            let bytes = tree.bytes();

            if use_tar(opts, &tree) {
                if tarpipe::available(t).await {
                    p.set_method(Method::TarPipe);
                    p.bytes_total.store(tarpipe::archive_size(&tree), Ordering::Relaxed);
                    let parent = remote.trim_end_matches('/').rsplit_once('/').map_or("/", |(d, _)| if d.is_empty() { "/" } else { d });
                    tarpipe::download(t, parent, &name, local_dir.to_path_buf(), Arc::clone(p), cancel).await?;
                    return Ok(Summary {
                        method: Method::TarPipe,
                        files: p.files_done.load(Ordering::Relaxed),
                        bytes,
                        skipped: p.files_skipped.load(Ordering::Relaxed) + tree.other as u64,
                        peak_window: 0,
                    });
                }
                p.set_note("tar is not available on the server: using parallel SFTP");
            }

            p.bytes_total.store(bytes, Ordering::Relaxed);
            let skipped = (tree.links.len() + tree.other) as u64;
            p.files_skipped.store(skipped, Ordering::Relaxed);
            // Directories first, parents before children. O(dirs).
            let dirs: Vec<PathBuf> = std::iter::once(target.clone())
                .chain(tree.dirs.iter().map(|d| local_join(&target, &d.rel)))
                .collect();
            tokio::task::spawn_blocking(move || {
                for d in dirs {
                    std::fs::create_dir_all(&d).map_err(|e| Error::Io(format!("{}: {e}", d.display())))?;
                }
                Ok::<_, Error>(())
            })
            .await
            .map_err(|e| Error::Io(e.to_string()))??;

            let channels = parallel(t, opts, &tree, p, cancel, |s, item, w, p, cancel| {
                let remote_path = pipe::remote_join(remote, &item.rel);
                let local_path = local_join(&target, &item.rel);
                Box::pin(async move { pipe::download(s, &remote_path, item.meta, &local_path, w, p, cancel).await })
            })
            .await?;
            Ok(Summary {
                method: Method::Parallel { channels },
                files: tree.files.len() as u64,
                bytes,
                skipped,
                peak_window: 0,
            })
        }
        _ => Err(Error::Remote(format!("{remote}: not a file or directory"))),
    }
}

async fn upload(
    t: &Arc<Transport>,
    local: &Path,
    remote_dir: &str,
    opts: &Options,
    p: &Arc<Progress>,
    cancel: &Arc<Cancel>,
) -> Result<Summary, Error> {
    let name = local
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| tree::safe_name(n))
        .ok_or_else(|| Error::Io(format!("{}: not an uploadable name", local.display())))?;
    let target = pipe::remote_join(remote_dir, &name);
    let md = tokio::fs::metadata(local)
        .await
        .map_err(|e| Error::Io(format!("{}: {e}", local.display())))?;
    let ctl = open(t, opts).await?;
    let exists = ctl.raw.lstat(target.clone()).await.ok();
    if exists.is_some() && !opts.overwrite {
        return Err(Error::Exists(target));
    }

    if md.is_file() {
        p.files_total.store(1, Ordering::Relaxed);
        p.bytes_total.store(md.len(), Ordering::Relaxed);
        p.set_method(Method::Pipelined);
        let stripes = opts
            .stripes
            .unwrap_or(if md.len() >= STRIPE_MIN { UPLOAD_STRIPES } else { 1 })
            .clamp(1, 16);
        let w = window(opts, ctl.write_len);
        let n = if stripes > 1 {
            let extra = try_join_all((1..stripes).map(|_| open(t, opts))).await?;
            let all: Vec<&Session> = std::iter::once(&ctl).chain(extra.iter()).collect();
            p.set_method(Method::Striped { channels: stripes });
            pipe::upload_striped(&all, local, &target, exists.is_some(), &w, p, cancel).await?
        } else {
            let mut w = w;
            pipe::upload(&ctl, local, &target, exists.is_some(), &mut w, p, cancel).await?
        };
        p.file_done();
        let method = if stripes > 1 { Method::Striped { channels: stripes } } else { Method::Pipelined };
        return Ok(Summary { method, files: 1, bytes: n, skipped: 0, peak_window: 0 });
    }
    if !md.is_dir() {
        return Err(Error::Io(format!("{}: not a file or directory", local.display())));
    }

    let tree = tree::local(local.to_path_buf()).await?;
    p.files_total.store(tree.files.len() as u64, Ordering::Relaxed);
    let bytes = tree.bytes();

    if use_tar(opts, &tree) {
        if tarpipe::available(t).await {
            p.set_method(Method::TarPipe);
            p.bytes_total.store(tarpipe::archive_size(&tree), Ordering::Relaxed);
            let (files, other) = (tree.files.len() as u64, tree.other as u64);
            tarpipe::upload(t, local.to_path_buf(), tree, remote_dir, &name, Arc::clone(p), cancel).await?;
            return Ok(Summary { method: Method::TarPipe, files, bytes, skipped: other, peak_window: 0 });
        }
        p.set_note("tar is not available on the server: using parallel SFTP");
    }

    p.bytes_total.store(bytes, Ordering::Relaxed);
    let skipped = (tree.links.len() + tree.other) as u64;
    p.files_skipped.store(skipped, Ordering::Relaxed);
    // Parents before children; each level's mkdirs in flight together, so a
    // tree costs one round trip per level, not one per directory.
    mkdir(&ctl, &target).await?;
    let mut level = 1;
    let mut i = 0;
    while i < tree.dirs.len() {
        let start = i;
        while i < tree.dirs.len() && tree.dirs[i].rel.matches('/').count() + 1 == level {
            i += 1;
        }
        let batch = tree.dirs[start..i].iter().map(|d| {
            let path = pipe::remote_join(&target, &d.rel);
            let ctl = &ctl;
            async move { mkdir(ctl, &path).await }
        });
        try_join_all(batch).await?;
        level += 1;
    }

    let replace = opts.overwrite;
    let channels = parallel(t, opts, &tree, p, cancel, |s, item, w, p, cancel| {
        let local_path = local_join(local, &item.rel);
        let remote_path = pipe::remote_join(&target, &item.rel);
        Box::pin(async move { pipe::upload(s, &local_path, &remote_path, replace, w, p, cancel).await })
    })
    .await?;
    Ok(Summary { method: Method::Parallel { channels }, files: tree.files.len() as u64, bytes, skipped, peak_window: 0 })
}

/// Create `path`, or accept that it is already a directory.
async fn mkdir(s: &Session, path: &str) -> Result<(), Error> {
    use opspilot_core_ssh::russh_sftp::protocol::FileAttributes;
    // `empty()`: `default()` asks for mode 0o777, leaving only the server's
    // umask between this and a world-writable directory (§18.22).
    match s.raw.mkdir(path, FileAttributes::empty()).await {
        Ok(_) => Ok(()),
        Err(e) => match s.raw.lstat(path).await {
            Ok(a) if a.attrs.is_dir() => Ok(()),
            _ => Err(Error::remote(path, e)),
        },
    }
}

fn use_tar(opts: &Options, tree: &Tree) -> bool {
    match opts.tar {
        TarPolicy::Never => false,
        TarPolicy::Always => true,
        TarPolicy::Auto => wants_tar(tree.files.len(), tree.bytes()),
    }
}

type FileFuture<'a> = std::pin::Pin<Box<dyn std::future::Future<Output = Result<u64, Error>> + Send + 'a>>;

/// Spread `tree.files` over several SFTP channels. Returns the channel count.
///
/// One shared queue, largest file first: every worker pulls the next file
/// when it finishes one, so a 500 MB file occupies one worker while the others
/// drain everything else (§6.2's "one 500 MB file doesn't starve a worker").
/// §6.2 names a work-stealing deque; with workers that pull rather than own a
/// share, a single queue already has that property and needs no dependency.
async fn parallel<F>(
    t: &Arc<Transport>,
    opts: &Options,
    tree: &Tree,
    p: &Arc<Progress>,
    cancel: &Arc<Cancel>,
    each: F,
) -> Result<u32, Error>
where
    F: for<'a> Fn(&'a Session, Item, &'a mut Window, &'a Progress, &'a Cancel) -> FileFuture<'a> + Sync,
{
    let small = tree.bytes() / (tree.files.len().max(1) as u64) < 1024 * 1024;
    let channels = opts
        .channels
        .unwrap_or(if small { 8 } else { 4 })
        .clamp(1, 16)
        .min(tree.files.len().max(1) as u32);
    p.set_method(Method::Parallel { channels });

    let mut files = tree.files.clone();
    // Largest first (LPT): minimises the time the last worker runs alone.
    // O(n log n).
    files.sort_by_key(|f| std::cmp::Reverse(f.meta.size));
    let queue = Mutex::new(VecDeque::from(files));

    let worker = || async {
        let s = open(t, opts).await?;
        let mut w = window(opts, s.read_len.min(s.write_len));
        loop {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let Some(item) = queue.lock().ok().and_then(|mut q| q.pop_front()) else {
                return Ok(());
            };
            each(&s, item, &mut w, p, cancel).await?;
            p.file_done();
        }
    };
    try_join_all((0..channels).map(|_| worker())).await?;
    Ok(channels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tar_rule_is_section_6_2s() {
        assert!(!wants_tar(200, 200 * 4096), "200 is not > 200");
        assert!(wants_tar(201, 201 * 4096));
        assert!(!wants_tar(1000, 1000 * 2 * 1024 * 1024), "average over 1 MB");
        assert!(!wants_tar(0, 0));
    }

    #[test]
    fn names_come_from_the_last_component_and_must_be_safe() {
        assert_eq!(base_name("/srv/app/").as_deref(), Some("app"));
        assert_eq!(base_name("/srv/app/x.log").as_deref(), Some("x.log"));
        assert_eq!(base_name("/"), None);
        assert_eq!(base_name("/srv/.."), None);
    }
}
