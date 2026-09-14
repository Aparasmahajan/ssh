//! The unlocked vault, and the commands the webview may call on it.
//!
//! CLAUDE.md §12.3 asks for the secrets broker to be **a separate process**, so
//! that "the UI receives a handle, not plaintext, so a webview XSS cannot read
//! the vault". This is not a separate process — that is a real gap and it is
//! recorded in COMPLETED.md.
//!
//! What *is* implemented is the property the broker was there to provide, at the
//! command boundary instead of the process boundary:
//!
//! > **There is no command that returns a secret.**
//!
//! Read the list below: `create`, `unlock`, `lock`, `list`, `set`, `remove`,
//! `change_password`, `audit`, `status`. Nothing returns plaintext, and
//! [`crate::vault::resolve`] — the one function that decrypts — is `pub(crate)`
//! and reachable only from the connect path in [`crate::remote`]. The frontend
//! authenticates by sending the *reference* `vault:prod/deploy`; the plaintext
//! is decrypted, used, and dropped without ever crossing the IPC boundary.
//!
//! So a webview compromise can enumerate record names, overwrite them, and
//! connect as the user — which is bad — but it cannot exfiltrate the passwords
//! themselves. A separate process would additionally stop a compromise of the
//! *native* side. That is the part still missing.

use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use opspilot_core_store::vault::{AuditAction, AuditLog, Secret, Vault, VaultError, REF_PREFIX};

/// Lock the vault after this long without use.
///
/// An unlocked vault is a decrypted root key in RAM. Leaving one unlocked all
/// day defeats most of the point, and every tool that gets this right has a
/// timeout.
const IDLE_TIMEOUT: Duration = Duration::from_secs(15 * 60);

struct State {
    vault: Option<Vault>,
    last_used: Instant,
}

fn state() -> MutexGuard<'static, State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(|| {
        Mutex::new(State {
            vault: None,
            last_used: Instant::now(),
        })
    })
    .lock()
    // A poisoned lock here means a panic happened while the vault was borrowed.
    // Recovering the guard is better than making the app unusable; the key is
    // still valid, and the alternative is a permanent lockout.
    .unwrap_or_else(|e| e.into_inner())
}

fn audit() -> AuditLog {
    AuditLog::new(AuditLog::default_path())
}

/// Borrow the unlocked vault, enforcing the idle timeout first.
fn with_vault<T>(f: impl FnOnce(&mut Vault) -> Result<T, VaultError>) -> Result<T, String> {
    let mut s = state();
    if s.vault.is_some() && s.last_used.elapsed() > IDLE_TIMEOUT {
        s.vault = None;
        let _ = audit().append(AuditAction::Locked, "idle-timeout");
    }
    let v = s.vault.as_mut().ok_or("the vault is locked")?;
    let out = f(v).map_err(|e| e.to_string())?;
    s.last_used = Instant::now();
    Ok(out)
}

/// What the UI needs to decide which dialog to show. No secret-derived data.
#[derive(serde::Serialize)]
pub struct VaultStatus {
    /// Does `vault.bin` exist? Drives "create" vs "unlock".
    pub exists: bool,
    pub unlocked: bool,
    pub records: usize,
    /// Portable display path, for the settings pane.
    pub path: String,
    pub idle_timeout_secs: u64,
}

pub fn status() -> VaultStatus {
    let mut s = state();
    if s.vault.is_some() && s.last_used.elapsed() > IDLE_TIMEOUT {
        s.vault = None;
        // The status poll is what usually notices the timeout first, since the
        // rail calls it every 5s while nothing else touches the vault.
        let _ = audit().append(AuditAction::Locked, "idle-timeout");
    }
    let path = Vault::default_path();
    VaultStatus {
        exists: Vault::exists(&path),
        unlocked: s.vault.is_some(),
        records: s.vault.as_ref().map(|v| v.len()).unwrap_or(0),
        path: path.display().to_string(),
        idle_timeout_secs: IDLE_TIMEOUT.as_secs(),
    }
}

pub fn create(password: String) -> Result<VaultStatus, String> {
    // A weak master password undoes Argon2 entirely. Refusing beats a warning
    // nobody reads, and this is the one password the user cannot rotate away
    // from without re-encrypting everything.
    if password.chars().count() < 8 {
        return Err("the master password must be at least 8 characters".into());
    }
    let path = Vault::default_path();
    let v = Vault::create(&path, &password).map_err(|e| e.to_string())?;
    let _ = audit().append(AuditAction::Created, "");
    let mut s = state();
    s.vault = Some(v);
    s.last_used = Instant::now();
    drop(s);
    Ok(status())
}

pub fn unlock(password: String) -> Result<VaultStatus, String> {
    let path = Vault::default_path();
    match Vault::open(&path, &password) {
        Ok(v) => {
            let _ = audit().append(AuditAction::Unlocked, "");
            let mut s = state();
            s.vault = Some(v);
            s.last_used = Instant::now();
            drop(s);
            Ok(status())
        }
        Err(e) => {
            // Failed attempts are the ones worth logging.
            let _ = audit().append(AuditAction::FailedUnlock, "");
            Err(e.to_string())
        }
    }
}

pub fn lock() -> VaultStatus {
    let was_unlocked = state().vault.take().is_some();
    // Only log a lock that actually locked something; a UI that calls this on
    // close would otherwise fill the log with no-ops.
    if was_unlocked {
        let _ = audit().append(AuditAction::Locked, "");
    }
    status()
}

/// Record ids only. Never the secrets, and never their lengths.
pub fn list() -> Result<Vec<String>, String> {
    with_vault(|v| Ok(v.ids()))
}

/// Store a secret. This is the only direction plaintext travels over IPC, and
/// it is unavoidable: the user types the password into the UI.
pub fn set(id: String, secret: String) -> Result<Vec<String>, String> {
    with_vault(|v| {
        // `from_string` consumes the String's buffer, so the plaintext is not
        // duplicated on its way into the Zeroizing wrapper.
        v.set(&id, &Secret::from_string(secret))?;
        v.save()?;
        Ok(v.ids())
    })
    .inspect(|_| {
        let _ = audit().append(AuditAction::Written, &id);
    })
}

pub fn remove(id: String) -> Result<Vec<String>, String> {
    with_vault(|v| {
        if !v.remove(&id) {
            return Err(VaultError::NotFound(id.clone()));
        }
        v.save()?;
        Ok(v.ids())
    })
    .inspect(|_| {
        let _ = audit().append(AuditAction::Removed, &id);
    })
}

pub fn change_password(new_password: String) -> Result<VaultStatus, String> {
    if new_password.chars().count() < 8 {
        return Err("the master password must be at least 8 characters".into());
    }
    with_vault(|v| v.change_password(&new_password))?;
    let _ = audit().append(AuditAction::PasswordChanged, "");
    Ok(status())
}

/// The audit log, newest last, plus whether the hash chain still verifies.
#[derive(serde::Serialize)]
pub struct AuditView {
    pub entries: Vec<AuditRow>,
    /// `None` if the chain verifies; otherwise the sequence number where it
    /// broke. Shown in the UI, because a broken chain means the log was edited.
    pub broken_at: Option<u64>,
}

#[derive(serde::Serialize)]
pub struct AuditRow {
    pub seq: u64,
    pub action: String,
    pub id: String,
}

pub fn audit_log(limit: usize) -> Result<AuditView, String> {
    let log = audit();
    let all = log.read().map_err(|e| e.to_string())?;
    // `verify` returns Ok(count) when the chain holds and Err(seq) at the break,
    // so `.err()` is exactly "the sequence number where it broke, if it did".
    let broken_at = log.verify().map_err(|e| e.to_string())?.err();
    let start = all.len().saturating_sub(limit);
    Ok(AuditView {
        entries: all[start..]
            .iter()
            .map(|e| AuditRow {
                seq: e.seq,
                action: format!("{:?}", e.action).to_lowercase(),
                id: e.id.clone(),
            })
            .collect(),
        broken_at,
    })
}

/// Resolve a `vault:...` reference to plaintext for the connect path.
///
/// `pub(crate)`, and there is deliberately no `#[tauri::command]` wrapping it.
/// `Ok(None)` means the input was not a vault reference at all.
pub(crate) fn resolve(reference: &str) -> Result<Option<String>, String> {
    if !reference.starts_with(REF_PREFIX) {
        return Ok(None);
    }
    let id = reference.trim_start_matches(REF_PREFIX).to_string();
    let secret = with_vault(|v| v.resolve_ref(reference))?;
    let _ = audit().append(AuditAction::Read, &id);
    match secret {
        // One copy, handed straight to russh, which owns it until the connect
        // finishes. Still a `String` at this boundary — russh's auth API takes
        // one, so there is nothing to be gained by wrapping it here.
        Some(s) => Ok(Some(s.as_str().map_err(|e| e.to_string())?.to_string())),
        None => Ok(None),
    }
}

/// True when a reference needs an unlocked vault, so the UI can prompt *before*
/// starting a connect rather than failing halfway through one.
pub fn needs_unlock(reference: &str) -> bool {
    reference.starts_with(REF_PREFIX) && state().vault.is_none()
}

/// Test-only: the KDF the app uses. Exposed so a test can assert the shipped
/// parameters without reaching into core-store's privates.
#[cfg(test)]
pub(crate) fn default_kdf() -> opspilot_core_store::vault::Kdf {
    opspilot_core_store::vault::Kdf::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialises the tests that touch real state.
    ///
    /// Two things here are process-global: `OPSPILOT_CONFIG_DIR`, which decides
    /// where `vault.bin` lives, and the unlocked-vault singleton itself. Cargo
    /// runs tests in parallel threads, so without this they clobber each other
    /// — the same trap that produced silent save-then-load-nothing failures in
    /// `core-store` (see `testlock` there).
    fn lock_tests() -> std::sync::MutexGuard<'static, ()> {
        // Crate-wide, not module-wide: `forwards` sets the same variable.
        crate::testenv::lock()
    }

    /// Point the config root at a scratch directory and start from locked.
    fn isolate(tag: &str) -> std::path::PathBuf {
        let d = crate::testenv::isolate(&format!("vault-{tag}"));
        state().vault = None;
        d
    }

    /// The whole app-layer path, which the core-store tests do not cover: the
    /// command functions, the state singleton, and reference resolution.
    ///
    /// Uses the shipped Argon2 parameters (m=64 MiB), so it is the slowest test
    /// in the crate by some margin — three derivations at roughly 100 ms each.
    /// Worth it: this is the only place the wiring a user actually touches is
    /// exercised end to end.
    #[test]
    fn create_store_lock_unlock_resolve() {
        let _g = lock_tests();
        isolate("happy");

        let st = create("a good master password".into()).expect("create");
        assert!(st.exists && st.unlocked && st.records == 0);

        set("prod/root".into(), "s3cr3t".into()).expect("set");
        assert_eq!(list().unwrap(), vec!["prod/root"]);

        // Resolution works while unlocked...
        assert_eq!(
            resolve("vault:prod/root").unwrap().as_deref(),
            Some("s3cr3t")
        );
        // ...and a non-reference still passes through untouched.
        assert_eq!(resolve("~/.ssh/id_ed25519").unwrap(), None);

        let st = lock();
        assert!(st.exists && !st.unlocked);
        // Locking a locked vault must not add a second entry.
        let before = audit_log(1000).unwrap().entries.len();
        lock();
        assert_eq!(audit_log(1000).unwrap().entries.len(), before);
        assert!(
            resolve("vault:prod/root").is_err(),
            "a locked vault resolved"
        );
        assert!(needs_unlock("vault:prod/root"));

        // Wrong password must not unlock, and must not corrupt anything.
        assert!(unlock("a bad master password".into()).is_err());
        let st = unlock("a good master password".into()).expect("unlock");
        assert!(st.unlocked);
        assert_eq!(st.records, 1);
        assert_eq!(
            resolve("vault:prod/root").unwrap().as_deref(),
            Some("s3cr3t")
        );

        // A reference naming nothing is an error, not a silent no-password
        // connect.
        assert!(resolve("vault:does/not/exist").is_err());

        remove("prod/root".into()).expect("remove");
        assert!(list().unwrap().is_empty());

        // Every one of those operations should be in the audit log, and the
        // chain must still verify.
        let view = audit_log(100).expect("audit");
        assert!(
            view.broken_at.is_none(),
            "chain broke at {:?}",
            view.broken_at
        );
        let actions: Vec<&str> = view.entries.iter().map(|e| e.action.as_str()).collect();
        for want in [
            "created",
            "written",
            "read",
            "unlocked",
            "failedunlock",
            "removed",
        ] {
            assert!(actions.contains(&want), "missing {want} in {actions:?}");
        }

        state().vault = None;
        std::env::remove_var("OPSPILOT_CONFIG_DIR");
    }

    #[test]
    fn a_locked_vault_refuses_every_operation() {
        let _g = lock_tests();
        state().vault = None;
        assert!(list().is_err());
        assert!(set("a".into(), "b".into()).is_err());
        assert!(remove("a".into()).is_err());
        assert!(change_password("longenough".into()).is_err());
        // ...and resolving fails rather than connecting without a password.
        assert!(resolve("vault:prod/root").is_err());
    }

    #[test]
    fn a_non_vault_reference_resolves_to_none_even_when_locked() {
        let _g = lock_tests();
        state().vault = None;
        // A host with only a key path must not be blocked on the vault.
        assert_eq!(resolve("~/.ssh/id_ed25519").unwrap(), None);
        assert_eq!(resolve("").unwrap(), None);
        assert!(!needs_unlock("~/.ssh/id_ed25519"));
    }

    #[test]
    fn short_master_passwords_are_refused() {
        let _g = lock_tests();
        // Checked before touching the filesystem, so this is safe to call.
        assert!(create("short".into()).is_err());
        assert!(change_password("1234567".into()).is_err());
    }

    #[test]
    fn the_shipped_kdf_matches_the_spec() {
        // §12.3: "Argon2id (m=64 MiB, t=3, p=4)".
        let k = default_kdf();
        assert_eq!(k.m_cost, 64 * 1024);
        assert_eq!(k.t_cost, 3);
        assert_eq!(k.p_cost, 4);
    }
}
