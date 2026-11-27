//! Walking a directory tree on either side.
//!
//! Remote: §6.6 — "readdir in batches. Never `stat` per entry." Each readdir
//! reply carries the attributes, so a tree costs one opendir/readdir/close
//! sequence per directory and nothing per file. Directories are read
//! concurrently on one channel, so a tree of depth d costs about d round trips,
//! not one per directory.
//!
//! Local: `std::fs` on the blocking pool (§3: no blocking fs in a tokio task).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures_util::stream::{FuturesUnordered, StreamExt};
use opspilot_core_ssh::russh_sftp::client::error::Error as SftpError;
use opspilot_core_ssh::russh_sftp::client::RawSftpSession;
use opspilot_core_ssh::russh_sftp::protocol::{FileAttributes, StatusCode};

use crate::pipe::{remote_join, Meta};
use crate::progress::Cancel;
use crate::Error;

/// Directories read at once during a remote walk.
const WALK_CONCURRENCY: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    /// Copied as a link by tar; skipped (and counted) by the SFTP path.
    Link,
    /// Devices, sockets, FIFOs, and names that are not safe to create.
    Other,
}

/// One entry, relative to the walk's root, `/`-separated on both sides.
#[derive(Clone, Debug)]
pub struct Item {
    pub rel: String,
    pub kind: Kind,
    pub meta: Meta,
}

#[derive(Debug, Default)]
pub struct Tree {
    /// Parents before children.
    pub dirs: Vec<Item>,
    pub files: Vec<Item>,
    pub links: Vec<Item>,
    pub other: usize,
}

impl Tree {
    pub fn bytes(&self) -> u64 {
        self.files.iter().map(|f| f.meta.size.unwrap_or(0)).sum()
    }
}

pub fn kind_of(attrs: &FileAttributes) -> Kind {
    if attrs.is_dir() {
        Kind::Dir
    } else if attrs.is_regular() {
        Kind::File
    } else if attrs.is_symlink() {
        Kind::Link
    } else {
        Kind::Other
    }
}

/// A name a server sent that is safe to create locally: one path component.
///
/// The server chooses these strings. `../../.bashrc`, `/etc/passwd`, `a/b`, or
/// on Windows `C:x` and `a\b`, would each write outside the download's target
/// directory if joined onto it. Refused, and counted as skipped.
pub fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains('\0')
        && !(cfg!(windows) && name.contains(':'))
}

pub fn meta_of(attrs: &FileAttributes) -> Meta {
    Meta {
        size: attrs.size,
        mode: attrs.permissions,
        mtime: attrs.mtime,
    }
}

/// Everything under `root` (not `root` itself).
pub async fn remote(raw: &Arc<RawSftpSession>, root: &str, cancel: &Cancel) -> Result<Tree, Error> {
    let mut tree = Tree::default();
    let mut pending: Vec<String> = vec![String::new()]; // relative dirs to read
    let mut inflight = FuturesUnordered::new();

    let read = |rel: String| {
        let raw = Arc::clone(raw);
        let path = if rel.is_empty() { root.to_string() } else { remote_join(root, &rel) };
        async move { (rel, read_dir(&raw, &path).await) }
    };

    loop {
        while inflight.len() < WALK_CONCURRENCY {
            match pending.pop() {
                Some(rel) => inflight.push(read(rel)),
                None => break,
            }
        }
        let (rel, entries) = tokio::select! {
            biased;
            _ = cancel.wait() => return Err(Error::Cancelled),
            r = inflight.next() => match r { Some(r) => r, None => break },
        };
        for (name, attrs) in entries? {
            if !safe_name(&name) {
                tree.other += 1;
                continue;
            }
            let child = if rel.is_empty() { name } else { format!("{rel}/{name}") };
            let item = Item {
                rel: child.clone(),
                kind: kind_of(&attrs),
                meta: meta_of(&attrs),
            };
            match item.kind {
                Kind::Dir => {
                    tree.dirs.push(item);
                    pending.push(child);
                }
                Kind::File => tree.files.push(item),
                Kind::Link => tree.links.push(item),
                Kind::Other => tree.other += 1,
            }
        }
    }
    // Reads completed in any order; creation needs parents first. A parent's
    // path is a strict prefix of its child's, so sorting by depth suffices.
    tree.dirs.sort_by_key(|d| d.rel.matches('/').count());
    Ok(tree)
}

async fn read_dir(raw: &RawSftpSession, path: &str) -> Result<Vec<(String, FileAttributes)>, Error> {
    let handle = raw.opendir(path).await.map_err(|e| Error::remote(path, e))?.handle;
    let mut out = Vec::new();
    let r = loop {
        match raw.readdir(handle.clone()).await {
            Ok(name) => {
                for f in name.files {
                    if f.filename != "." && f.filename != ".." {
                        out.push((f.filename, f.attrs));
                    }
                }
            }
            Err(SftpError::Status(s)) if s.status_code == StatusCode::Eof => break Ok(out),
            Err(e) => break Err(Error::remote(path, e)),
        }
    };
    let _ = raw.close(handle).await;
    r
}

/// Everything under `root`, without following symlinks.
pub async fn local(root: PathBuf) -> Result<Tree, Error> {
    tokio::task::spawn_blocking(move || {
        let mut tree = Tree::default();
        walk_local(&root, "", &mut tree)?;
        tree.dirs.sort_by_key(|d| d.rel.matches('/').count());
        Ok(tree)
    })
    .await
    .map_err(|e| Error::Io(e.to_string()))?
}

/// Recursion depth is the tree's depth. O(entries).
fn walk_local(dir: &Path, rel: &str, tree: &mut Tree) -> Result<(), Error> {
    let rd = std::fs::read_dir(dir).map_err(|e| Error::Io(format!("{}: {e}", dir.display())))?;
    for entry in rd {
        let entry = entry.map_err(|e| Error::Io(e.to_string()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let child = if rel.is_empty() { name } else { format!("{rel}/{name}") };
        // symlink_metadata: a link is copied as nothing, never followed, so a
        // link to `/` cannot turn an upload into the whole disk.
        let md = match std::fs::symlink_metadata(entry.path()) {
            Ok(m) => m,
            Err(_) => {
                tree.other += 1;
                continue;
            }
        };
        let meta = Meta {
            size: Some(md.len()),
            mode: local_mode(&md),
            mtime: md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as u32),
        };
        if md.is_dir() {
            tree.dirs.push(Item { rel: child.clone(), kind: Kind::Dir, meta });
            walk_local(&entry.path(), &child, tree)?;
        } else if md.is_file() {
            tree.files.push(Item { rel: child, kind: Kind::File, meta });
        } else if md.file_type().is_symlink() {
            tree.links.push(Item { rel: child, kind: Kind::Link, meta });
        } else {
            tree.other += 1;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn local_mode(md: &std::fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    Some(md.permissions().mode() & 0o777)
}

#[cfg(not(unix))]
fn local_mode(_: &std::fs::Metadata) -> Option<u32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_single_components_are_safe() {
        for ok in ["a", "a.txt", ".hidden", "..x", "x..", "with space", "ünïcode"] {
            assert!(safe_name(ok), "{ok}");
        }
        for bad in ["", ".", "..", "../x", "a/b", "/etc", "a\\b", "nul\0x"] {
            assert!(!safe_name(bad), "{bad:?}");
        }
    }
}
