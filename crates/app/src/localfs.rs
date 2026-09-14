//! The local half of the SFTP dual pane.
//!
//! UI_BUILD_GUIDE Step 10 is "local pane | transfer controls | remote pane", so
//! the local side needs listing too. It is done here with `std::fs` rather than
//! by adding `tauri-plugin-fs`: the plugin would grant the *webview* filesystem
//! access, which §12.3's "a webview XSS cannot read the vault" reasoning argues
//! against, and it would be a dependency for something that is thirty lines.
//!
//! The wire type is deliberately the same `SftpEntry` the remote side uses, so
//! one pane component renders both.

use crate::remote::SftpEntry;
use serde::Serialize;

#[derive(Serialize, Clone, Debug)]
pub struct LocalListing {
    pub path: String,
    pub parent: Option<String>,
    pub entries: Vec<SftpEntry>,
}

pub fn home() -> String {
    #[allow(deprecated)]
    std::env::home_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| ".".into())
}

/// Unix-style permission bits from what this platform actually knows.
///
/// Windows has no mode bits, so reporting `0644`/`0755` there would be a
/// fabrication. It reports read-only vs writable, which is the only permission
/// fact Windows has, and the UI shows `?????????` for the rest rather than
/// inventing plausible-looking values.
fn permissions(meta: &std::fs::Metadata) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Some(meta.permissions().mode() & 0o7777)
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        None
    }
}

fn modified(meta: &std::fs::Metadata) -> Option<u64> {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
}

pub fn list(path: &str) -> Result<LocalListing, String> {
    let dir = std::path::Path::new(path);
    let rd = std::fs::read_dir(dir).map_err(|e| format!("{path}: {e}"))?;

    let mut entries: Vec<SftpEntry> = Vec::new();
    for e in rd.flatten() {
        // A permission error on one entry must not fail the whole listing —
        // Windows user profiles are full of junctions that cannot be stat'd.
        let Ok(meta) = e.metadata() else { continue };
        let kind = if meta.is_dir() {
            "dir"
        } else if meta.file_type().is_symlink() {
            "symlink"
        } else if meta.is_file() {
            "file"
        } else {
            "other"
        };
        let perms = permissions(&meta);
        entries.push(SftpEntry {
            name: e.file_name().to_string_lossy().into_owned(),
            kind,
            size: meta.len(),
            modified: modified(&meta),
            permissions: perms.map(perm_string).unwrap_or_else(|| "?????????".into()),
            octal: perms.map(|m| format!("{:04o}", m & 0o7777)),
        });
    }

    // Same order as the remote pane: directories first, then case-insensitive
    // name. Two panes that sort differently are hard to compare at a glance.
    entries.sort_by(|a, b| {
        (b.kind == "dir")
            .cmp(&(a.kind == "dir"))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    Ok(LocalListing {
        path: dir.to_string_lossy().into_owned(),
        parent: dir
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .filter(|p| !p.is_empty()),
        entries,
    })
}

fn perm_string(mode: u32) -> String {
    let bit = |shift: u32, ch: char| if mode >> shift & 1 == 1 { ch } else { '-' };
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

pub fn read(path: &str) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("{path}: {e}"))
}

pub fn write(path: &str, data: &[u8]) -> Result<(), String> {
    if let Some(parent) = std::path::Path::new(path).parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    std::fs::write(path, data).map_err(|e| format!("{path}: {e}"))
}

pub fn mkdir(path: &str) -> Result<(), String> {
    std::fs::create_dir_all(path).map_err(|e| format!("{path}: {e}"))
}

pub fn remove(path: &str, is_dir: bool) -> Result<(), String> {
    if is_dir {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
    .map_err(|e| format!("{path}: {e}"))
}

pub fn rename(from: &str, to: &str) -> Result<(), String> {
    std::fs::rename(from, to).map_err(|e| format!("{from} -> {to}: {e}"))
}

/// Join a directory and a child name using the local separator.
pub fn join(dir: &str, name: &str) -> String {
    std::path::Path::new(dir)
        .join(name)
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("opspilot-lfs-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn lists_directories_first_then_name() {
        let d = tmp("sort");
        std::fs::write(d.join("b.txt"), b"x").unwrap();
        std::fs::write(d.join("A.txt"), b"xx").unwrap();
        std::fs::create_dir(d.join("zdir")).unwrap();

        let l = list(d.to_str().unwrap()).unwrap();
        let names: Vec<&str> = l.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["zdir", "A.txt", "b.txt"]);
        assert_eq!(l.entries[1].size, 2, "A.txt is 2 bytes");
    }

    #[test]
    fn round_trips_a_file() {
        let d = tmp("rt");
        let f = join(d.to_str().unwrap(), "hello.bin");
        write(&f, b"bytes").unwrap();
        assert_eq!(read(&f).unwrap(), b"bytes");
        remove(&f, false).unwrap();
        assert!(read(&f).is_err());
    }

    #[test]
    fn parent_is_reported_for_navigation() {
        let d = tmp("parent");
        let l = list(d.to_str().unwrap()).unwrap();
        assert!(l.parent.is_some(), "a temp dir always has a parent");
    }

    #[test]
    fn a_missing_directory_is_an_error_not_an_empty_listing() {
        // Silently showing an empty pane for a bad path is how users lose files.
        assert!(list("/definitely/not/here/at/all").is_err());
    }
}
