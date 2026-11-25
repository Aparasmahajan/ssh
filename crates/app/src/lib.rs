//! Tauri commands and wiring.
//!
//! Everything the webview can invoke lives here; `src-tauri` is only the binary
//! that owns `tauri.conf.json` and registers these.

pub mod clipboard;
pub mod db;
pub mod docker;
pub mod forwards;
pub mod k8s;
pub mod localfs;
pub mod remote;
pub mod session;
pub mod shells;
pub mod transfers;
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
        app_reset,
        session_open,
        session_write,
        session_snapshot,
        session_resize,
        session_scroll,
        session_close,
        session_paste,
        session_paste_text,
        clipboard_write,
        docker_snapshot,
        docker_action,
        docker_exec,
        docker_logs,
        db_profiles,
        db_profile_save,
        db_profile_delete,
        db_open,
        db_close,
        db_run,
        db_cancel,
        db_window,
        db_set_view,
        k8s_contexts,
        k8s_snapshot,
        k8s_action,
        k8s_yaml,
        k8s_exec,
        k8s_logs,
        list_shells,
        ssh_connect,
        ssh_agent_status,
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
        transfer_start,
        transfer_list,
        transfer_cancel,
        transfer_clear,
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

/// Run blocking work — disk, the vault's Argon2 KDF, or network calls that
/// `block_on` the network runtime — on Tauri's blocking pool.
///
/// Tauri runs NON-async commands on the main (GTK) thread. Before this, an
/// SFTP download froze the whole window for the length of the transfer, an SSH
/// connect to a blackholed host froze it for the OS connect timeout (~2 min on
/// Linux), and unlocking the vault froze it for the KDF. `block_on` is allowed
/// on a blocking-pool thread, where it would panic inside an async task.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| format!("task failed: {e}"))?
}

/// Release everything a previous page load left behind: terminal sessions,
/// database connections, the Docker watch, Kubernetes informers. The frontend
/// calls this once when it starts. Its state lives in memory only, so after a
/// webview reload every one of these is unreachable — and without this, each
/// reload leaked a full set. Returns how many terminal sessions were closed.
#[tauri::command]
async fn app_reset() -> Result<usize, String> {
    let n = blocking(|| Ok(registry().close_all())).await?;
    db::close_all().await?;
    docker::stop();
    k8s::stop();
    Ok(n)
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
async fn list_shells() -> Result<Vec<shells::ShellInfo>, String> {
    blocking(move || {
        Ok({
        shells::available()
        })
    })
    .await
}

/// Spawn a shell on a PTY. `shell` is a `ShellInfo::id`; omit for the default.
#[tauri::command]
async fn session_open(rows: u16, cols: u16, shell: Option<String>) -> Result<SessionId, String> {
    blocking(move || {
        registry().open(rows.max(1), cols.max(1), shell.as_deref())
    })
    .await
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
async fn session_close(id: SessionId) -> Result<(), String> {
    blocking(move || {
        registry().close(id)
    })
    .await
}

/// Paste the system clipboard (or PRIMARY) into these sessions.
///
/// The text is read and written here, natively — it is never returned to the
/// webview. See `clipboard.rs` for why that matters. Errors with
/// `clipboard::UNSUPPORTED` where there is no native clipboard; the frontend
/// then reads `navigator.clipboard` and calls `session_paste_text`.
#[tauri::command]
async fn session_paste(
    app: tauri::AppHandle,
    ids: Vec<SessionId>,
    selection: clipboard::Selection,
) -> Result<(), String> {
    match clipboard::read(&app, selection).await? {
        Some(text) => clipboard::paste_into(&ids, &text),
        None => Ok(()),
    }
}

/// Paste text the webview already has (the non-Linux fallback). Same encoding
/// as `session_paste`: line endings and bracketed paste are decided here.
#[tauri::command]
fn session_paste_text(ids: Vec<SessionId>, text: String) -> Result<(), String> {
    clipboard::paste_into(&ids, &text)
}

/// Put a terminal selection on the clipboard or PRIMARY.
#[tauri::command]
async fn clipboard_write(
    app: tauri::AppHandle,
    selection: clipboard::Selection,
    text: String,
) -> Result<(), String> {
    clipboard::write(&app, selection, text).await
}

/* ---- Docker ----------------------------------------------------------------- */

/// Live container list, `None` when unchanged since `since` (§13.1 rule 8).
#[tauri::command]
fn docker_snapshot(since: u64) -> Result<Option<docker::DockerView>, String> {
    docker::snapshot(since)
}

/// start | stop | restart | pause | unpause | remove | remove-force
#[tauri::command]
async fn docker_action(id: String, action: String) -> Result<(), String> {
    docker::action(id, action).await
}

/// A shell inside a running container, as a terminal session id.
#[tauri::command]
async fn docker_exec(id: String, rows: u16, cols: u16) -> Result<SessionId, String> {
    docker::exec(id, rows.max(1), cols.max(1)).await
}

/// A container's log, followed, as a read-only terminal session id.
#[tauri::command]
fn docker_logs(id: String, rows: u16, cols: u16) -> Result<SessionId, String> {
    docker::logs(&id, rows.max(1), cols.max(1))
}

/* ---- Databases -------------------------------------------------------------- */

#[tauri::command]
async fn db_profiles() -> Result<Vec<opspilot_core_store::databases::DbProfile>, String> {
    db::profiles().await
}

#[tauri::command]
async fn db_profile_save(
    profile: opspilot_core_store::databases::DbProfile,
) -> Result<Vec<opspilot_core_store::databases::DbProfile>, String> {
    db::save_profile(profile).await
}

#[tauri::command]
async fn db_profile_delete(id: String) -> Result<Vec<opspilot_core_store::databases::DbProfile>, String> {
    db::delete_profile(id).await
}

/// Connect with a saved profile. `password` is used only when the profile has
/// no vault reference, and only for this connect.
#[tauri::command]
async fn db_open(profile: String, password: Option<String>) -> Result<db::Opened, String> {
    db::open(profile, password).await
}

#[tauri::command]
async fn db_close(session: u32) -> Result<(), String> {
    db::close(session).await
}

#[tauri::command]
async fn db_run(session: u32, sql: String) -> Result<u64, String> {
    db::run(session, sql).await
}

#[tauri::command]
async fn db_cancel(session: u32) -> Result<(), String> {
    db::cancel(session).await
}

#[tauri::command]
async fn db_window(session: u32, offset: usize, limit: usize) -> Result<Option<db::WindowView>, String> {
    db::window(session, offset, limit).await
}

/// `sort_column` + `descending`, and a case-insensitive substring filter.
#[tauri::command]
async fn db_set_view(
    session: u32,
    sort_column: Option<usize>,
    descending: bool,
    filter: String,
) -> Result<(), String> {
    db::set_view(session, sort_column.map(|c| (c, descending)), filter).await
}

/* ---- Kubernetes -------------------------------------------------------------- */

#[tauri::command]
async fn k8s_contexts() -> Result<opspilot_core_kube::Contexts, String> {
    blocking(move || {
        k8s::contexts()
    })
    .await
}

/// Informer-backed list; `None` when unchanged since `since` (§10, §13.1).
#[tauri::command]
async fn k8s_snapshot(
    context: String,
    kind: String,
    namespace: String,
    since: u64,
) -> Result<Option<opspilot_core_kube::Snapshot>, String> {
    k8s::snapshot(context, kind, namespace, since).await
}

/// delete-pod | scale | rollout-restart
#[tauri::command]
async fn k8s_action(
    context: String,
    action: String,
    namespace: String,
    name: String,
    replicas: Option<i32>,
) -> Result<(), String> {
    k8s::action(context, action, namespace, name, replicas).await
}

#[tauri::command]
async fn k8s_yaml(context: String, kind: String, namespace: String, name: String) -> Result<String, String> {
    k8s::yaml(context, kind, namespace, name).await
}

#[tauri::command]
async fn k8s_exec(
    context: String,
    namespace: String,
    pod: String,
    container: Option<String>,
    rows: u16,
    cols: u16,
) -> Result<SessionId, String> {
    k8s::exec(context, namespace, pod, container, rows.max(1), cols.max(1)).await
}

#[tauri::command]
async fn k8s_logs(
    context: String,
    namespace: String,
    pod: String,
    container: Option<String>,
    rows: u16,
    cols: u16,
) -> Result<SessionId, String> {
    k8s::logs(context, namespace, pod, container, rows.max(1), cols.max(1)).await
}

/* ---- Remote: SSH and SFTP ------------------------------------------------- */

/// Connect, or reuse the existing transport for this exact identity (§8.1).
#[tauri::command]
async fn ssh_connect(request: remote::ConnectRequest) -> Result<remote::ConnectionInfo, String> {
    blocking(move || {
        remote::connect(request)
    })
    .await
}

/// Whether an SSH agent is reachable and which public keys it holds. Public
/// keys and comments only; an agent never hands out anything else.
#[tauri::command]
async fn ssh_agent_status() -> Result<remote::AgentInfo, String> {
    blocking(|| Ok(remote::agent_status())).await
}

#[tauri::command]
async fn ssh_disconnect(key: String) -> Result<(), String> {
    blocking(move || {
        remote::disconnect(&key)
    })
    .await
}

#[tauri::command]
fn ssh_connections() -> Vec<remote::ConnectionInfo> {
    remote::list()
}

/// A remote shell, as a session indistinguishable from a local one to the UI.
#[tauri::command]
async fn ssh_open_shell(key: String, rows: u16, cols: u16) -> Result<SessionId, String> {
    blocking(move || {
        let transport = remote::get(&key)?;
        registry().open_ssh(rows, cols, transport)
    })
    .await
}

#[tauri::command]
async fn sftp_home(key: String) -> Result<String, String> {
    blocking(move || {
        remote::sftp_home(&key)
    })
    .await
}

#[tauri::command]
async fn sftp_list(key: String, path: String) -> Result<remote::Listing, String> {
    blocking(move || {
        remote::sftp_list(&key, &path)
    })
    .await
}

#[tauri::command]
async fn sftp_read(key: String, path: String) -> Result<Vec<u8>, String> {
    blocking(move || {
        remote::sftp_read(&key, &path)
    })
    .await
}

#[tauri::command]
async fn sftp_write(key: String, path: String, data: Vec<u8>) -> Result<(), String> {
    blocking(move || {
        remote::sftp_write(&key, &path, data)
    })
    .await
}

#[tauri::command]
async fn sftp_mkdir(key: String, path: String) -> Result<(), String> {
    blocking(move || {
        remote::sftp_mkdir(&key, &path)
    })
    .await
}

#[tauri::command]
async fn sftp_rename(key: String, from: String, to: String) -> Result<(), String> {
    blocking(move || {
        remote::sftp_rename(&key, &from, &to)
    })
    .await
}

#[tauri::command]
async fn sftp_remove(key: String, path: String, is_dir: bool) -> Result<(), String> {
    blocking(move || {
        remote::sftp_remove(&key, &path, is_dir)
    })
    .await
}

/* ---- Transfers ------------------------------------------------------------ */

/// Queue a transfer (§6). In-memory and a spawn, so synchronous is fine
/// (§18.19). `source` is remote for a download, local for an upload;
/// `target_dir` is the directory it lands in on the other side.
#[tauri::command]
fn transfer_start(
    key: String,
    direction: transfers::Direction,
    source: String,
    target_dir: String,
    overwrite: bool,
) -> Result<u64, String> {
    transfers::start(&key, direction, &source, &target_dir, overwrite)
}

/// Every transfer, sampled by the UI at 10 Hz (§13.1 rule 8). Atomics only.
#[tauri::command]
fn transfer_list() -> Vec<transfers::TransferView> {
    transfers::list()
}

#[tauri::command]
fn transfer_cancel(id: u64) -> Result<(), String> {
    transfers::cancel(id)
}

#[tauri::command]
fn transfer_clear() {
    transfers::clear()
}

/* ---- Local filesystem ------------------------------------------------------ */

#[tauri::command]
fn local_home() -> String {
    localfs::home()
}

#[tauri::command]
async fn local_list(path: String) -> Result<localfs::LocalListing, String> {
    blocking(move || {
        localfs::list(&path)
    })
    .await
}

#[tauri::command]
async fn local_mkdir(path: String) -> Result<(), String> {
    blocking(move || {
        localfs::mkdir(&path)
    })
    .await
}

#[tauri::command]
async fn local_remove(path: String, is_dir: bool) -> Result<(), String> {
    blocking(move || {
        localfs::remove(&path, is_dir)
    })
    .await
}

#[tauri::command]
async fn local_rename(from: String, to: String) -> Result<(), String> {
    blocking(move || {
        localfs::rename(&from, &to)
    })
    .await
}

/* ---- Workspace, keys, forwards --------------------------------------------- */

use opspilot_core_store::{keys as keystore, model::Host as StoredHost, store, Workspace};

#[tauri::command]
async fn workspace_list() -> Result<Vec<String>, String> {
    blocking(move || {
        Ok({
        let names = store::list_workspaces();
        if names.is_empty() {
            // First run: create one rather than showing an empty app with no way
            // forward.
            let _ = store::ensure_default();
            return Ok(store::list_workspaces());
        }
        names
        })
    })
    .await
}

#[tauri::command]
async fn workspace_load(name: String) -> Result<Workspace, String> {
    blocking(move || {
        store::load(&name)
    })
    .await
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
async fn host_upsert(workspace: String, host: StoredHost) -> Result<Workspace, String> {
    blocking(move || {
        store::upsert_host(&workspace, host)
    })
    .await
}

#[tauri::command]
async fn host_delete(workspace: String, host_id: String) -> Result<Workspace, String> {
    blocking(move || {
        store::delete_host(&workspace, &host_id)
    })
    .await
}

/// Private keys found on this machine. Never reads private key material.
#[tauri::command]
async fn keys_list() -> Result<Vec<keystore::KeyInfo>, String> {
    blocking(move || {
        Ok({
        keystore::discover()
        })
    })
    .await
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn forward_start(
    connection_key: String,
    id: String,
    kind: String,
    bind_address: String,
    bind_port: u16,
    target_host: Option<String>,
    target_port: Option<u16>,
) -> Result<forwards::ForwardStatus, String> {
    blocking(move || {
        forwards::start(
            &connection_key,
            &id,
            &kind,
            &bind_address,
            bind_port,
            target_host,
            target_port,
        )
    })
    .await
}

#[tauri::command]
async fn forward_stop(id: String) -> Result<(), String> {
    blocking(move || {
        forwards::stop(&id)
    })
    .await
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
async fn forward_auto_start(
    connection_key: String,
    workspace: String,
    host_id: String,
) -> Result<Vec<forwards::AutoStarted>, String> {
    blocking(move || {
        forwards::auto_start(&connection_key, &workspace, &host_id)
    })
    .await
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
async fn vault_create(password: String) -> Result<vault::VaultStatus, String> {
    blocking(move || {
        vault::create(password)
    })
    .await
}

#[tauri::command]
async fn vault_unlock(password: String) -> Result<vault::VaultStatus, String> {
    blocking(move || {
        vault::unlock(password)
    })
    .await
}

#[tauri::command]
fn vault_lock() -> vault::VaultStatus {
    vault::lock()
}

/// Record ids. Never secrets.
#[tauri::command]
async fn vault_list() -> Result<Vec<String>, String> {
    blocking(move || {
        vault::list()
    })
    .await
}

#[tauri::command]
async fn vault_set(id: String, secret: String) -> Result<Vec<String>, String> {
    blocking(move || {
        vault::set(id, secret)
    })
    .await
}

#[tauri::command]
async fn vault_remove(id: String) -> Result<Vec<String>, String> {
    blocking(move || {
        vault::remove(id)
    })
    .await
}

#[tauri::command]
async fn vault_change_password(password: String) -> Result<vault::VaultStatus, String> {
    blocking(move || {
        vault::change_password(password)
    })
    .await
}

#[tauri::command]
async fn vault_audit(limit: Option<usize>) -> Result<vault::AuditView, String> {
    blocking(move || {
        vault::audit_log(limit.unwrap_or(200))
    })
    .await
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
        for banned in [
            "vault_get",
            "vault_read",
            "vault_reveal",
            "vault_export",
            // Same class: the clipboard routinely holds a copied password.
            // Paste is `session_paste`, which reads and writes natively.
            "clipboard_read",
            "clipboard_get",
        ] {
            assert!(
                !handler.contains(banned),
                "`{banned}` is registered as a command — the webview must never be                  able to read a secret back out of the vault (CLAUDE.md §12.3)"
            );
        }
    }
}
