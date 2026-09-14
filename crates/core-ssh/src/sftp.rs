//! SFTP over an SSH channel.
//!
//! CLAUDE.md §6. What is implemented here is the *correct* layer: listing,
//! stat, read, write, rename, delete, mkdir, canonicalize — the operations a
//! dual-pane browser needs.
//!
//! What is NOT here yet, and is called out rather than quietly skipped:
//!
//!   §6.1 AIMD pipelining — `russh-sftp`'s `File` does its own request
//!        management; a congestion window over raw `read`/`write` handles is a
//!        separate engine and belongs with the transfer queue, not the browser.
//!   §6.2 tar-pipe for small-file batches. Needs the transfer queue first.
//!   §6.5 Merkle compare / delta sync.
//!
//! §6.6 IS honoured, because it is the thing that makes a listing feel instant:
//! "readdir in batches. **Never `stat` per entry** — that is the WinSCP
//! 'loading…' spinner." `read_dir` returns attributes with the names, so this
//! code never issues a follow-up stat.

use russh::client::Msg;
use russh::Channel;
use russh_sftp::client::SftpSession;
use russh_sftp::protocol::OpenFlags;

use crate::transport::SshError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    Other,
}

/// One directory entry. Everything here comes from the single `readdir` that
/// produced it — no per-entry round trip.
#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub kind: EntryKind,
    pub size: u64,
    /// Unix mtime, seconds. `None` when the server did not send one.
    pub modified: Option<u64>,
    /// Unix permission bits, e.g. 0o755.
    pub permissions: Option<u32>,
}

impl Entry {
    /// `rwxr-xr-x`, the form §Step 10 wants shown next to the octal.
    pub fn permission_string(&self) -> String {
        let Some(mode) = self.permissions else {
            return "?????????".into();
        };
        let bit = |shift: u32, ch: char| {
            if mode >> shift & 1 == 1 {
                ch
            } else {
                '-'
            }
        };
        [
            bit(8, 'r'),
            bit(7, 'w'),
            bit(6, 'x'),
            bit(5, 'r'),
            bit(4, 'w'),
            bit(3, 'x'),
            bit(2, 'r'),
            bit(1, 'w'),
            bit(0, 'x'),
        ]
        .iter()
        .collect()
    }
}

pub struct Sftp {
    session: SftpSession,
}

impl Sftp {
    pub(crate) async fn new(channel: Channel<Msg>) -> Result<Sftp, SshError> {
        let session = SftpSession::new(channel.into_stream())
            .await
            .map_err(|e| SshError::Sftp(e.to_string()))?;
        Ok(Sftp { session })
    }

    /// Absolute path for `path`, resolving `.`, `~`-less relatives and symlinks.
    /// Used to turn the login directory into something displayable.
    pub async fn canonicalize(&self, path: &str) -> Result<String, SshError> {
        self.session
            .canonicalize(path)
            .await
            .map_err(|e| SshError::Sftp(e.to_string()))
    }

    /// List a directory.
    ///
    /// §6.6: one `readdir` sequence, attributes included, no per-entry `stat`.
    /// Sorted directories-first then by name — sorting server-side output once
    /// here is cheaper than making every caller do it, and the order is what a
    /// file browser wants anyway.
    pub async fn list(&self, path: &str) -> Result<Vec<Entry>, SshError> {
        let dir = self
            .session
            .read_dir(path)
            .await
            .map_err(|e| SshError::Sftp(e.to_string()))?;

        let mut out: Vec<Entry> = Vec::new();
        for item in dir {
            let name = item.file_name();
            if name == "." || name == ".." {
                continue;
            }
            let meta = item.metadata();
            let kind = if meta.is_dir() {
                EntryKind::Dir
            } else if meta.is_symlink() {
                EntryKind::Symlink
            } else if meta.is_regular() {
                EntryKind::File
            } else {
                EntryKind::Other
            };
            out.push(Entry {
                name,
                kind,
                size: meta.size.unwrap_or(0),
                modified: meta.mtime.map(|m| m as u64),
                permissions: meta.permissions,
            });
        }

        out.sort_by(|a, b| {
            let dir_first = (b.kind == EntryKind::Dir).cmp(&(a.kind == EntryKind::Dir));
            dir_first.then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        Ok(out)
    }

    pub async fn read(&self, path: &str) -> Result<Vec<u8>, SshError> {
        self.session
            .read(path)
            .await
            .map_err(|e| SshError::Sftp(e.to_string()))
    }

    pub async fn write(&self, path: &str, data: &[u8]) -> Result<(), SshError> {
        self.session
            .write(path, data)
            .await
            .map_err(|e| SshError::Sftp(e.to_string()))
    }

    /// Create-or-truncate, used by uploads.
    pub async fn create(&self, path: &str, data: &[u8]) -> Result<(), SshError> {
        use tokio::io::AsyncWriteExt;
        let mut file = self
            .session
            .open_with_flags(
                path,
                OpenFlags::CREATE | OpenFlags::WRITE | OpenFlags::TRUNCATE,
            )
            .await
            .map_err(|e| SshError::Sftp(e.to_string()))?;
        file.write_all(data)
            .await
            .map_err(|e| SshError::Sftp(e.to_string()))?;
        file.shutdown()
            .await
            .map_err(|e| SshError::Sftp(e.to_string()))?;
        Ok(())
    }

    pub async fn rename(&self, from: &str, to: &str) -> Result<(), SshError> {
        self.session
            .rename(from, to)
            .await
            .map_err(|e| SshError::Sftp(e.to_string()))
    }

    pub async fn remove_file(&self, path: &str) -> Result<(), SshError> {
        self.session
            .remove_file(path)
            .await
            .map_err(|e| SshError::Sftp(e.to_string()))
    }

    pub async fn remove_dir(&self, path: &str) -> Result<(), SshError> {
        self.session
            .remove_dir(path)
            .await
            .map_err(|e| SshError::Sftp(e.to_string()))
    }

    pub async fn create_dir(&self, path: &str) -> Result<(), SshError> {
        self.session
            .create_dir(path)
            .await
            .map_err(|e| SshError::Sftp(e.to_string()))
    }

    pub async fn exists(&self, path: &str) -> Result<bool, SshError> {
        self.session
            .try_exists(path)
            .await
            .map_err(|e| SshError::Sftp(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(mode: u32) -> Entry {
        Entry {
            name: "x".into(),
            kind: EntryKind::File,
            size: 0,
            modified: None,
            permissions: Some(mode),
        }
    }

    #[test]
    fn permission_strings_match_ls() {
        assert_eq!(entry(0o755).permission_string(), "rwxr-xr-x");
        assert_eq!(entry(0o644).permission_string(), "rw-r--r--");
        assert_eq!(entry(0o600).permission_string(), "rw-------");
        assert_eq!(entry(0o000).permission_string(), "---------");
        assert_eq!(entry(0o777).permission_string(), "rwxrwxrwx");
    }

    #[test]
    fn unknown_permissions_are_marked_not_guessed() {
        let e = Entry {
            name: "x".into(),
            kind: EntryKind::File,
            size: 0,
            modified: None,
            permissions: None,
        };
        assert_eq!(e.permission_string(), "?????????");
    }
}
