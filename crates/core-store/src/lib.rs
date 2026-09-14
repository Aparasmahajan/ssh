//! Config as the source of truth — CLAUDE.md §12.
//!
//! > "An embedded DB is an opaque binary blob; a user cannot copy it to another
//! > PC with confidence, diff it, or put it in git. Config files can."
//!
//! What is here:
//!   · [`paths`] — the portable/state split, atomic writes, `~` round-tripping
//!   · [`model`] — hosts, identities, port forwards; TOML shapes
//!   · [`store`] — load/save/upsert with schema-version refusal
//!   · [`keys`]  — SSH key discovery, without ever reading private material
//!   · [`vault`] — AES-256-GCM secret storage whose only root of trust is the
//!                 master password, plus the hash-chained audit log
//!   · [`watch`] — hot reload: a generation counter the UI samples (§12.2)
//!
//! NOT here yet: the `rkyv` index cache (§12.4). Recorded in COMPLETED.md.

pub mod keys;
pub mod model;
pub mod paths;
pub mod store;
pub mod vault;
pub mod watch;

pub use keys::KeyInfo;
pub use model::{Environment, Forward, ForwardKind, Host, Identity, Workspace};
pub use vault::{AuditAction, AuditLog, Secret, Vault, VaultError};

#[cfg(test)]
pub(crate) mod testlock {
    use std::sync::{Mutex, MutexGuard, OnceLock};

    /// Serialises tests that set `OPSPILOT_CONFIG_DIR`.
    ///
    /// That variable is process-global and cargo runs tests in parallel threads,
    /// so without this the store tests silently point at each other's config
    /// root and fail in whatever order the scheduler picks. The symptom was a
    /// save-then-load returning zero hosts.
    pub fn lock() -> MutexGuard<'static, ()> {
        static L: OnceLock<Mutex<()>> = OnceLock::new();
        // A panicking test poisons the mutex; the next test still needs to run.
        L.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }
}
