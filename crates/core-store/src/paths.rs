//! Where config lives, and the rules for writing it.
//!
//! CLAUDE.md §12.1 splits storage in two:
//!
//! * `~/.config/opspilot/` — **PORTABLE**. "Copy this and it works." Hosts,
//!   identities, keybindings, known_hosts, the vault.
//! * OS state dir — **DO NOT COPY**. Window geometry, caches, logs.
//!
//! `OPSPILOT_CONFIG_DIR` overrides the portable root, "so a fully portable USB
//! install is just an env var".

use std::path::{Path, PathBuf};

/// The portable config root. Created on demand.
pub fn config_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("OPSPILOT_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("opspilot")
}

/// The machine-local state root — rebuildable, never exported.
///
/// On Linux this is `$XDG_STATE_HOME` (`~/.local/state`), which the XDG spec
/// defines for exactly this: logs, history, window geometry — "data that should
/// persist between restarts but is not important or portable enough" for
/// `~/.local/share`. `dirs::state_dir()` is `None` on Windows and macOS, which
/// fall through to `%LOCALAPPDATA%` and `~/Library/Application Support`.
pub fn state_dir() -> PathBuf {
    dirs::state_dir()
        .or_else(dirs::data_local_dir)
        .unwrap_or_else(config_dir)
        .join("opspilot")
}

pub fn ensure_config_dir() -> std::io::Result<PathBuf> {
    let d = config_dir();
    std::fs::create_dir_all(&d)?;
    Ok(d)
}

/// Write a file atomically: temp → fsync → rename.
///
/// §12.2: "Atomic writes always... Never rewrite a workspace file in place. A
/// crash mid-write must not cost the user their host list."
pub fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Same directory, so the rename is on one filesystem and therefore atomic.
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));

    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(contents.as_bytes())?;
        // fsync before rename, or a crash can leave a correctly-named file full
        // of zeroes — which is worse than the old file, not better.
        f.sync_all()?;
    }

    // Windows will not rename onto an existing file; remove first. This is the
    // one window where the config is briefly absent, which is why the temp file
    // is already complete and fsynced before we get here.
    #[cfg(windows)]
    let _ = std::fs::remove_file(path);

    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;

    // On Linux the rename itself lives in the *directory*, and until the
    // directory is fsynced a power cut can roll it back — ext4 and XFS both
    // document this. The file's data was synced above; this syncs its name.
    // Best effort: some filesystems (and FUSE mounts) refuse to open a
    // directory for sync, and the write has already succeeded.
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        let dir = if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        };
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
    Ok(())
}

/// Collapse `$HOME` to `~` for storage.
///
/// §12.2: "**No absolute paths in portable config — ever.** This is the single
/// most common thing that breaks an exported config." A key at
/// `C:\Users\alice\.ssh\id_ed25519` must be stored as `~/.ssh/id_ed25519` or the
/// config is worthless on any other machine.
pub fn portable(path: &str) -> String {
    // Only Windows uses `\` as a separator. On Linux and macOS it is a legal
    // filename character, and rewriting it would point at a different file.
    let normalise = |s: &str| {
        if cfg!(windows) {
            s.replace('\\', "/")
        } else {
            s.to_string()
        }
    };
    let p = normalise(path);
    if let Some(home) = dirs::home_dir() {
        let h = normalise(&home.to_string_lossy());
        let h = h.trim_end_matches('/');
        if let Some(rest) = p.strip_prefix(h) {
            // Component boundary only: with $HOME=/home/al, the path
            // /home/alice/key must stay absolute, not become `~ice/key`.
            if rest.is_empty() || rest.starts_with('/') {
                return format!("~{}", rest);
            }
        }
    }
    p
}

/// Expand `~` and `${env:VAR}` back to a real path at load time.
pub fn resolve(stored: &str) -> String {
    let mut s = stored.to_string();

    while let (Some(a), Some(b)) = (s.find("${env:"), s.find('}')) {
        if b < a {
            break;
        }
        let name = &s[a + 6..b];
        let value = std::env::var(name).unwrap_or_default();
        s = format!("{}{}{}", &s[..a], value, &s[b + 1..]);
    }

    if let Some(rest) = s.strip_prefix('~') {
        if let Some(home) = dirs::home_dir() {
            let joined = format!("{}{}", home.to_string_lossy(), rest);
            return if cfg!(windows) {
                joined.replace('/', "\\")
            } else {
                joined
            };
        }
    }
    if cfg!(windows) {
        s.replace('/', "\\")
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_paths_round_trip_through_portable_form() {
        let home = dirs::home_dir().unwrap();
        let key = home.join(".ssh").join("id_ed25519");
        let stored = portable(&key.to_string_lossy());

        assert!(
            stored.starts_with("~/"),
            "must not store an absolute path, got {stored}"
        );
        assert!(
            !stored.contains(':'),
            "a drive letter in portable config breaks export: {stored}"
        );

        let back = resolve(&stored);
        assert_eq!(
            std::path::Path::new(&back),
            key.as_path(),
            "round trip must land on the original path"
        );
    }

    #[test]
    fn a_sibling_of_home_is_not_collapsed_to_tilde() {
        let home = dirs::home_dir().unwrap();
        let h = home.to_string_lossy().replace('\\', "/");
        let sibling = format!("{}x/.ssh/id_ed25519", h.trim_end_matches('/'));
        assert!(
            !portable(&sibling).starts_with('~'),
            "{sibling} is not under $HOME, got {}",
            portable(&sibling)
        );
        assert_eq!(portable(&h), "~");
    }

    #[cfg(unix)]
    #[test]
    fn backslash_is_a_filename_character_on_unix() {
        assert_eq!(portable("/srv/odd\\name"), "/srv/odd\\name");
    }

    #[test]
    fn env_references_expand() {
        std::env::set_var("OPSPILOT_TEST_KEYDIR", "/keys");
        assert_eq!(
            resolve("${env:OPSPILOT_TEST_KEYDIR}/id").replace('\\', "/"),
            "/keys/id"
        );
    }

    #[test]
    fn atomic_write_replaces_content_and_leaves_no_temp_files() {
        let dir = std::env::temp_dir().join(format!("opspilot-atomic-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("hosts.toml");

        write_atomic(&f, "first = 1").unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "first = 1");

        write_atomic(&f, "second = 2").unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "second = 2");

        let strays: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("tmp"))
            .collect();
        assert!(strays.is_empty(), "left temp files behind: {strays:?}");
    }

    #[test]
    fn config_dir_is_overridable_for_portable_installs() {
        let _guard = crate::testlock::lock();
        std::env::set_var("OPSPILOT_CONFIG_DIR", "/tmp/portable-opspilot");
        assert_eq!(config_dir(), PathBuf::from("/tmp/portable-opspilot"));
        std::env::remove_var("OPSPILOT_CONFIG_DIR");
    }
}
