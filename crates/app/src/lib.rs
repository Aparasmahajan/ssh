//! Tauri commands and wiring.
//!
//! Everything the webview can invoke lives here; `src-tauri` is only the binary
//! that owns `tauri.conf.json` and registers these.

pub mod forwards;
pub mod localfs;
pub mod remote;
pub mod session;
pub mod shells;
pub mod vault;

use session::{registry, SessionId, Snapshot};

/// Attach every OpsPilot command to a Tauri builder.
///
/// The command list lives here, beside the commands, so adding one touches this
/// crate only. `src-tauri` keeps just `generate_context!`, which must be invoked
/// from the crate that owns `tauri.conf.json`.
///
/// Commands are deliberately NOT `pub`. `#[tauri::command]` expands to helper
/// macros named after the function plus a `pub use` of them; making the function
/// public re-imports those into the same module and fails to compile with E0255,
/// "the name `__cmd__app_version` is defined multiple times". Exposing a
/// registration function instead of the commands themselves is what Tauri's own
/// template does, and it avoids the problem by construction.
pub fn register(builder: tauri::Builder<tauri::Wry>) -> tauri::Builder<tauri::Wry> {
    builder.invoke_handler(tauri::generate_handler![
        app_version,
        session_open,
        session_write,
        session_snapshot,
        session_resize,
        session_scroll,
        session_close,
        list_shells,
        ssh_connect,
        ssh_disconnect,
        ssh_connections,
        ssh_open_shell,
        sftp_home,
        sftp_list,
        sftp_read,
        sftp_write,
        sftp_mkdir,
        sftp_rename,
        sftp_remove,
        sftp_download,
        sftp_upload,
        local_home,
        local_list,
        local_mkdir,
        local_remove,
        local_rename,
        workspace_list,
        workspace_load,
        config_generation,
        host_upsert,
        host_delete,
        keys_list,
        forward_start,
        forward_stop,
        forward_list,
        forward_auto_start,
        app_stats,
        session_stats,
        vault_status,
        vault_create,
        vault_unlock,
        vault_lock,
        vault_list,
        vault_set,
        vault_remove,
        vault_change_password,
        vault_audit,
    ])
}

/// Build identity, so the frontend can prove it is running inside the desktop
/// shell rather than a browser dev server.
#[tauri::command]
fn app_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Every shell this machine can actually start — cmd, PowerShell, Git Bash,
/// MSYS2, WSL, or the POSIX shells on a Unix host. Discovered by probing, so
/// nothing is offered that would fail to launch.
#[tauri::command]
fn list_shells() -> Vec<shells::ShellInfo> {
    shells::available()
}

/// Spawn a shell on a PTY. `shell` is a `ShellInfo::id`; omit for the default.
#[tauri::command]
fn session_open(rows: u16, cols: u16, shell: Option<String>) -> Result<SessionId, String> {
    registry().open(rows.max(1), cols.max(1), shell.as_deref())
}

/// Keyboard bytes to the PTY.
///
/// Takes `Vec<u8>` rather than a string: keystrokes are not always valid UTF-8
/// (Alt-modified keys, raw control sequences), and a lossy round-trip through
/// `String` would corrupt them.
#[tauri::command]
fn session_write(id: SessionId, bytes: Vec<u8>) -> Result<(), String> {
    registry().get(id)?.write(&bytes)
}

/// Rows changed since `since`. `None` means nothing changed — the common case on
/// an idle terminal, and the reason polling at 60 Hz is cheap.
#[tauri::command]
fn session_snapshot(id: SessionId, since: u64) -> Result<Option<Snapshot>, String> {
    registry().get(id)?.snapshot(since)
}

#[tauri::command]
fn session_resize(id: SessionId, rows: u16, cols: u16) -> Result<(), String> {
    registry().get(id)?.resize(rows.max(1), cols.max(1))
}

/// Scroll the view through scrollback. Positive `delta` moves back in history.
#[tauri::command]
fn session_scroll(id: SessionId, delta: i64) -> Result<usize, String> {
    registry().get(id)?.scroll(delta)
}

#[tauri::command]
fn session_close(id: SessionId) -> Result<(), String> {
    registry().close(id)
}

/* ---- Remote: SSH and SFTP ------------------------------------------------- */

/// Connect, or reuse the existing transport for this exact identity (§8.1).
#[tauri::command]
fn ssh_connect(request: remote::ConnectRequest) -> Result<remote::ConnectionInfo, String> {
    remote::connect(request)
}

#[tauri::command]
fn ssh_disconnect(key: String) -> Result<(), String> {
    remote::disconnect(&key)
}

#[tauri::command]
fn ssh_connections() -> Vec<remote::ConnectionInfo> {
    remote::list()
}

/// A remote shell, as a session indistinguishable from a local one to the UI.
#[tauri::command]
fn ssh_open_shell(key: String, rows: u16, cols: u16) -> Result<SessionId, String> {
    let transport = remote::get(&key)?;
    registry().open_ssh(rows, cols, transport)
}

#[tauri::command]
fn sftp_home(key: String) -> Result<String, String> {
    remote::sftp_home(&key)
}

#[tauri::command]
fn sftp_list(key: String, path: String) -> Result<remote::Listing, String> {
    remote::sftp_list(&key, &path)
}

#[tauri::command]
fn sftp_read(key: String, path: String) -> Result<Vec<u8>, String> {
    remote::sftp_read(&key, &path)
}

#[tauri::command]
fn sftp_write(key: String, path: String, data: Vec<u8>) -> Result<(), String> {
    remote::sftp_write(&key, &path, data)
}

#[tauri::command]
fn sftp_mkdir(key: String, path: String) -> Result<(), String> {
    remote::sftp_mkdir(&key, &path)
}

#[tauri::command]
fn sftp_rename(key: String, from: String, to: String) -> Result<(), String> {
    remote::sftp_rename(&key, &from, &to)
}

#[tauri::command]
fn sftp_remove(key: String, path: String, is_dir: bool) -> Result<(), String> {
    remote::sftp_remove(&key, &path, is_dir)
}

/* ---- Transfers ------------------------------------------------------------ */

/// Remote -> local.
///
/// Whole-file for now. CLAUDE.md §6.1's AIMD pipelining and §6.2's tar-pipe both
/// belong to the transfer *queue*, which does not exist yet; this is the browser
/// action, and it is honest about being one file at a time.
#[tauri::command]
fn sftp_download(key: String, remote: String, local: String) -> Result<u64, String> {
    let bytes = remote::sftp_read(&key, &remote)?;
    let n = bytes.len() as u64;
    localfs::write(&local, &bytes)?;
    Ok(n)
}

/// Local -> remote.
#[tauri::command]
fn sftp_upload(key: String, local: String, remote: String) -> Result<u64, String> {
    let bytes = localfs::read(&local)?;
    let n = bytes.len() as u64;
    remote::sftp_write(&key, &remote, bytes)?;
    Ok(n)
}

/* ---- Local filesystem ------------------------------------------------------ */

#[tauri::command]
fn local_home() -> String {
    localfs::home()
}

#[tauri::command]
fn local_list(path: String) -> Result<localfs::LocalListing, String> {
    localfs::list(&path)
}

#[tauri::command]
fn local_mkdir(path: String) -> Result<(), String> {
    localfs::mkdir(&path)
}

#[tauri::command]
fn local_remove(path: String, is_dir: bool) -> Result<(), String> {
    localfs::remove(&path, is_dir)
}

#[tauri::command]
fn local_rename(from: String, to: String) -> Result<(), String> {
    localfs::rename(&from, &to)
}

/* ---- Workspace, keys, forwards --------------------------------------------- */

use opspilot_core_store::{keys as keystore, model::Host as StoredHost, store, Workspace};

#[tauri::command]
fn workspace_list() -> Vec<String> {
    let names = store::list_workspaces();
    if names.is_empty() {
        // First run: create one rather than showing an empty app with no way
        // forward.
        let _ = store::ensure_default();
        return store::list_workspaces();
    }
    names
}

#[tauri::command]
fn workspace_load(name: String) -> Result<Workspace, String> {
    store::load(&name)
}

/// Bumps whenever a config file changes on disk (§12.2 hot reload).
///
/// Sampled by the UI rather than pushed, which is §13.1 rule 8 and also what
/// collapses the three filesystem events Vim produces per save into one reload.
/// The first call starts the watcher; nothing runs until then.
#[tauri::command]
fn config_generation() -> u64 {
    opspilot_core_store::watch::generation()
}

#[tauri::command]
fn host_upsert(workspace: String, host: StoredHost) -> Result<Workspace, String> {
    store::upsert_host(&workspace, host)
}

#[tauri::command]
fn host_delete(workspace: String, host_id: String) -> Result<Workspace, String> {
    store::delete_host(&workspace, &host_id)
}

/// Private keys found on this machine. Never reads private key material.
#[tauri::command]
fn keys_list() -> Vec<keystore::KeyInfo> {
    keystore::discover()
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
fn forward_start(
    connection_key: String,
    id: String,
    kind: String,
    bind_address: String,
    bind_port: u16,
    target_host: Option<String>,
    target_port: Option<u16>,
) -> Result<forwards::ForwardStatus, String> {
    forwards::start(
        &connection_key,
        &id,
        &kind,
        &bind_address,
        bind_port,
        target_host,
        target_port,
    )
}

#[tauri::command]
fn forward_stop(id: String) -> Result<(), String> {
    forwards::stop(&id)
}

#[tauri::command]
fn forward_list() -> Vec<forwards::ForwardStatus> {
    forwards::list()
}

/// Start the saved forwards flagged `auto_start` on a host that just connected.
///
/// Returns one outcome per forward. Failures are inside the list, not in the
/// `Result`: a tunnel that cannot bind must not cost the user the session they
/// actually asked for.
#[tauri::command]
fn forward_auto_start(
    connection_key: String,
    workspace: String,
    host_id: String,
) -> Result<Vec<forwards::AutoStarted>, String> {
    forwards::auto_start(&connection_key, &workspace, &host_id)
}

/* ---- Status rail telemetry -------------------------------------------------- */

/// Real numbers for the status rail.
///
/// Every field here is measured. Where something cannot be measured yet it is
/// `None` and the rail shows an em dash — a fabricated throughput figure in a
/// tool used to manage production is worse than an honest blank.
#[derive(serde::Serialize)]
pub struct AppStats {
    pub sessions: usize,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub connections: usize,
    /// Negotiated cipher of the most recently opened connection.
    pub cipher: Option<String>,
    pub kex: Option<String>,
    pub host_key: Option<String>,
    /// TCP connect + handshake time, ms. NOT a live per-packet RTT.
    pub connect_rtt_ms: Option<u32>,
    pub forwards: usize,
    pub forward_bytes_in: u64,
    pub forward_bytes_out: u64,
}

/// Per-session detail for the inspector panel.
#[tauri::command]
fn session_stats() -> Vec<session::SessionStat> {
    registry().stats()
}

#[tauri::command]
fn app_stats() -> AppStats {
    let (sessions, bytes_in, bytes_out) = registry().totals();
    let conns = remote::list();

    let mut cipher = None;
    let mut kex = None;
    let mut host_key = None;
    let mut connect_rtt_ms = None;
    if let Some(first) = conns.first() {
        if let Ok(t) = remote::get(&first.key) {
            if let Some(n) = t.negotiated() {
                cipher = Some(n.cipher);
                kex = Some(n.kex);
                host_key = Some(n.host_key);
            }
            connect_rtt_ms = Some(t.connect_rtt_ms());
        }
    }

    let fwd = forwards::list();
    AppStats {
        sessions,
        bytes_in,
        bytes_out,
        connections: conns.len(),
        cipher,
        kex,
        host_key,
        connect_rtt_ms,
        forwards: fwd.len(),
        forward_bytes_in: fwd.iter().map(|f| f.bytes_in).sum(),
        forward_bytes_out: fwd.iter().map(|f| f.bytes_out).sum(),
    }
}

/* ---- Vault (§12.3) ---------------------------------------------------------- */

/// Note what is absent from this list: there is no `vault_get`.
///
/// §12.3 wanted a separate broker process so "a webview XSS cannot read the
/// vault". This is not that, but the same property is enforced at the command
/// boundary: no command returns a secret. Authentication is done by sending the
/// reference `vault:prod/deploy`, which `remote::connect` resolves natively.
#[tauri::command]
fn vault_status() -> vault::VaultStatus {
    vault::status()
}

#[tauri::command]
fn vault_create(password: String) -> Result<vault::VaultStatus, String> {
    vault::create(password)
}

#[tauri::command]
fn vault_unlock(password: String) -> Result<vault::VaultStatus, String> {
    vault::unlock(password)
}

#[tauri::command]
fn vault_lock() -> vault::VaultStatus {
    vault::lock()
}

/// Record ids. Never secrets.
#[tauri::command]
fn vault_list() -> Result<Vec<String>, String> {
    vault::list()
}

#[tauri::command]
fn vault_set(id: String, secret: String) -> Result<Vec<String>, String> {
    vault::set(id, secret)
}

#[tauri::command]
fn vault_remove(id: String) -> Result<Vec<String>, String> {
    vault::remove(id)
}

#[tauri::command]
fn vault_change_password(password: String) -> Result<vault::VaultStatus, String> {
    vault::change_password(password)
}

#[tauri::command]
fn vault_audit(limit: Option<usize>) -> Result<vault::AuditView, String> {
    vault::audit_log(limit.unwrap_or(200))
}

/// Test-only helpers for the process-global config root.
///
/// `OPSPILOT_CONFIG_DIR` decides where every config file and the vault live, and
/// it is an environment variable — process-global, while cargo runs tests in
/// parallel threads. Two modules setting it independently is the trap recorded
/// as §18.10: the symptom is a save-then-load returning nothing, which looks
/// like a storage bug and is a concurrency bug. One lock for the whole crate.
#[cfg(test)]
pub(crate) mod testenv {
    use std::path::PathBuf;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    pub fn lock() -> MutexGuard<'static, ()> {
        static L: OnceLock<Mutex<()>> = OnceLock::new();
        // A panicking test poisons the mutex; the next test still needs to run.
        L.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Point the config root at an empty scratch directory. Call under [`lock`].
    pub fn isolate(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("opspilot-app-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("scratch config dir");
        std::env::set_var("OPSPILOT_CONFIG_DIR", &d);
        d
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_is_not_empty() {
        assert!(!super::app_version().is_empty());
    }

    /// The security property that stands in for §12.3's broker process: the
    /// generated command list must never contain a secret-returning command.
    ///
    /// Asserted against this file's own text, so adding `vault_get` to the
    /// handler list fails the build's test run rather than quietly shipping.
    #[test]
    fn no_command_returns_a_secret() {
        let src = include_str!("lib.rs");
        let handler = src
            .split("invoke_handler(tauri::generate_handler![")
            .nth(1)
            .and_then(|s| s.split("])").next())
            .expect("command list not found");
        for banned in ["vault_get", "vault_read", "vault_reveal", "vault_export"] {
            assert!(
                !handler.contains(banned),
                "`{banned}` is registered as a command — the webview must never be                  able to read a secret back out of the vault (CLAUDE.md §12.3)"
            );
        }
    }
}
