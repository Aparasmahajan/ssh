//! Hot reload — CLAUDE.md §12.2.
//!
//! > "**Hot reload**: watch the config dir with `notify`. Editing
//! > `production.toml` in Vim makes hosts appear in the tree. Termius makes you
//! > click through dialogs; this is a real differentiator and it costs almost
//! > nothing."
//!
//! ## Why a counter and not an event
//!
//! The watcher does not push. It bumps an `AtomicU64` and the UI samples it,
//! exactly as the PTY reader does (§13.1 rule 8, and §18.9 on why that rule
//! matters more than it first appears). An editor writing one file produces
//! several filesystem events — Vim writes a temp file, renames it, and removes a
//! backup — and pushing one reload per event would reparse the workspace three
//! times for one save. A counter collapses them for free: however many events
//! arrive between two polls, the UI reloads once.
//!
//! ## Cost
//!
//! `notify` is started **lazily**, on the first call to [`generation`], and the
//! OS watcher thread does not exist until then. Nothing is spent at startup, so
//! the §4 cold-start budget is untouched. On Windows this is
//! `ReadDirectoryChangesW`; on Linux, inotify; on macOS, FSEvents.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

static GENERATION: AtomicU64 = AtomicU64::new(0);

/// Bumps whenever a config file on disk changes. Poll it; when it differs from
/// the value you last saw, reload.
///
/// Starts the watcher on first call. Returns 0 forever if a watcher cannot be
/// created — a missing inotify instance must degrade to "no hot reload", never
/// to a failure to open the app.
pub fn generation() -> u64 {
    static WATCHER: OnceLock<Option<notify::RecommendedWatcher>> = OnceLock::new();
    WATCHER.get_or_init(start);
    GENERATION.load(Ordering::Relaxed)
}

/// Bump the counter without the filesystem. For tests, and for any in-process
/// write that wants the UI to notice immediately rather than at watcher latency.
pub fn bump() {
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

fn start() -> Option<notify::RecommendedWatcher> {
    // The directory must exist before it can be watched, and on first run it
    // does not. Creating it here is the same thing `ensure_config_dir` does at
    // the first save.
    let root = crate::paths::config_dir();
    let _ = std::fs::create_dir_all(root.join("workspaces"));
    spawn(&root)
}

/// Watch one directory tree, bumping the generation on config-file changes.
///
/// Separate from [`start`] so a test can watch a scratch directory: the live
/// watcher is behind a `OnceLock` keyed to whatever `OPSPILOT_CONFIG_DIR` said
/// on first use, which is not something a test can meaningfully re-point.
fn spawn(root: &Path) -> Option<notify::RecommendedWatcher> {
    use notify::{RecursiveMode, Watcher};

    let mut watcher = notify::recommended_watcher(|res: notify::Result<notify::Event>| {
        let Ok(event) = res else {
            return;
        };
        if event.paths.iter().any(|p| is_config_file(p)) {
            bump();
        }
    })
    .ok()?;

    // Recursive: `workspaces/` sits under the root, and so will `api/` and
    // `themes/` (§12.1).
    watcher.watch(root, RecursiveMode::Recursive).ok()?;
    Some(watcher)
}

/// Only real config files count.
///
/// An editor leaves a trail of `.swp`, `4913`, and `~` files in the directory it
/// is saving into, and `write_atomic` leaves a `.tmpNNNN` mid-write. Counting
/// those would reload the tree several times per save — and, worse, would fire a
/// reload for a file whose content is not config at all.
fn is_config_file(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()) == Some("toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_toml_files_count_as_config() {
        assert!(is_config_file(Path::new("/c/opspilot/workspaces/prod.toml")));
        // Vim's swap file, its numbered write probe, and our own atomic-write
        // temp file all live in the same directory as the thing being saved.
        assert!(!is_config_file(Path::new("/c/.prod.toml.swp")));
        assert!(!is_config_file(Path::new("/c/4913")));
        assert!(!is_config_file(Path::new("/c/prod.tmp12345")));
        assert!(!is_config_file(Path::new("/c/vault.bin")));
    }

    #[test]
    fn the_generation_only_ever_moves_forward() {
        let a = generation();
        bump();
        let b = generation();
        assert!(b > a, "{a} -> {b}");
    }

    /// The whole point of the feature, against a real filesystem.
    ///
    /// Everything else here is a predicate test; this is the one that fails if
    /// `notify` is wired up wrongly — wrong path, non-recursive, watcher dropped
    /// immediately. It watches a scratch directory rather than the live config
    /// root, because the live watcher is a process-global `OnceLock`.
    ///
    /// Waits in short steps up to a generous ceiling instead of sleeping a fixed
    /// interval: filesystem notification latency varies by platform and by how
    /// loaded the machine is, and a fixed sleep is how a test like this becomes
    /// flaky.
    #[test]
    fn a_real_edit_on_disk_moves_the_generation() {
        let dir =
            std::env::temp_dir().join(format!("opspilot-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("workspaces")).unwrap();

        let Some(_watcher) = spawn(&dir) else {
            // No watcher available (a sandbox without inotify). The feature
            // degrades to "no hot reload" by design, so this is not a failure.
            return;
        };

        let before = GENERATION.load(Ordering::Relaxed);
        crate::paths::write_atomic(&dir.join("workspaces").join("prod.toml"), "name = \"prod\"")
            .unwrap();

        let mut moved = false;
        for _ in 0..100 {
            if GENERATION.load(Ordering::Relaxed) > before {
                moved = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(moved, "editing a workspace file did not bump the generation");
    }
}
