//! Remote connections: the transport pool, and SFTP commands over it.
//!
//! CLAUDE.md §8.1 is the design:
//!
//! > "One SSH transport per host; SSH sessions, SFTP profiles, and port forwards
//! > multiplex as channels over it. This is what makes a new tab cost 20 ms while
//! > Termius re-handshakes for 900 ms."
//! >
//! > "But: the PRD requires multiple SFTP identities per host, and different
//! > usernames/keys need *separate* transports."
//!
//! So the pool is keyed by the whole `(host, port, user, identity)` tuple, not by
//! host. Opening a shell and browsing files on the same identity share one
//! handshake; `SFTP·deploy` and `SFTP·backup` deliberately do not.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use opspilot_core_ssh::hostkey::HostKeyPolicy;
use opspilot_core_ssh::transport::{AuthMethod, Endpoint, SshError, Transport};
use opspilot_core_ssh::{Entry, EntryKind};
use serde::{Deserialize, Serialize};

/// The async runtime for network I/O.
///
/// §3: "tokio (many-connection I/O)". Deliberately separate from the terminal
/// hot path, which §13.2 keeps on a dedicated OS thread — this runtime exists
/// for sockets, not for keystrokes. Created on first use, so a session that
/// never connects to anything pays nothing at startup (§14.5).
pub fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("opspilot-net")
            .build()
            .expect("failed to start the network runtime")
    })
}

/* ---- Wire types ----------------------------------------------------------- */

#[derive(Deserialize, Clone, Debug)]
pub struct AuthSpec {
    /// Plaintext typed for this one connect. Stays in memory for its duration,
    /// is never logged, never stored, and never returned to the webview.
    ///
    /// Prefer [`AuthSpec::password_ref`]: a saved host should carry a reference,
    /// so the password never crosses IPC at all.
    pub password: Option<String>,
    pub key_path: Option<String>,
    pub passphrase: Option<String>,
    /// A vault reference such as `vault:prod/deploy` (§12.3). Resolved here, in
    /// the native process; the webview only ever holds the reference.
    #[serde(default)]
    pub password_ref: Option<String>,
    /// Same, for an encrypted private key's passphrase.
    #[serde(default)]
    pub passphrase_ref: Option<String>,
    /// Offer the SSH agent's keys. `None` means yes, as with `ssh`.
    #[serde(default)]
    pub use_agent: Option<bool>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct ConnectRequest {
    pub host: String,
    pub port: u16,
    pub user: String,
    /// Names the identity ("deploy", "backup"). Part of the pool key.
    pub identity: String,
    pub auth: AuthSpec,
    /// "strict" | "accept-new" | "verify-only". Defaults to accept-new.
    pub host_key_policy: Option<String>,
}

impl ConnectRequest {
    fn endpoint(&self) -> Endpoint {
        Endpoint {
            host: self.host.clone(),
            port: self.port,
            user: self.user.clone(),
            identity: self.identity.clone(),
        }
    }

    fn policy(&self) -> HostKeyPolicy {
        match self.host_key_policy.as_deref() {
            Some("strict") => HostKeyPolicy::Strict,
            Some("verify-only") => HostKeyPolicy::VerifyOnly,
            _ => HostKeyPolicy::AcceptNew,
        }
    }

    /// Resolve the auth spec into concrete methods, decrypting any vault
    /// references on the way.
    ///
    /// Fallible now, because a reference to a locked vault must stop the connect
    /// rather than silently fall through to "no password" — which on a
    /// password-only server produces a confusing auth failure, and on a
    /// permissive one could connect with the wrong credential entirely.
    fn methods(&self) -> Result<Vec<AuthMethod>, String> {
        let mut v = Vec::new();

        // An explicit reference wins over a typed value: if a host is configured
        // to use the vault, a stale password left in the dialog must not shadow
        // it.
        let passphrase = match &self.auth.passphrase_ref {
            Some(r) if !r.is_empty() => crate::vault::resolve(r)?,
            _ => self.auth.passphrase.clone(),
        }
        .filter(|s| !s.is_empty());

        let password = match &self.auth.password_ref {
            Some(r) if !r.is_empty() => crate::vault::resolve(r)?,
            _ => self.auth.password.clone(),
        }
        .filter(|s| !s.is_empty());

        // Order: the configured key, then the agent, then the password.
        //
        // A configured key is the user saying which key this host takes, so it
        // goes before the agent's whole key list: each rejected key counts
        // against the server's MaxAuthTries (6 by default), and an agent with
        // many keys can exhaust it before the right one is offered. The agent
        // goes before the password because it needs no prompt, and a password
        // tried against a key-only server just burns an attempt.
        if let Some(p) = &self.auth.key_path {
            if !p.is_empty() {
                v.push(AuthMethod::PrivateKey {
                    path: p.into(),
                    passphrase,
                });
            }
        }
        if self.auth.use_agent.unwrap_or(true) {
            v.push(AuthMethod::Agent);
        }
        if let Some(p) = password {
            v.push(AuthMethod::Password(p));
        }
        Ok(v)
    }
}

/// Opaque handle the frontend uses to name a live connection.
pub type ConnectionKey = String;

fn key_of(e: &Endpoint) -> ConnectionKey {
    format!("{}@{}:{}#{}", e.user, e.host, e.port, e.identity)
}

#[derive(Serialize, Clone, Debug)]
pub struct ConnectionInfo {
    pub key: ConnectionKey,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub identity: String,
}

/* ---- Pool ------------------------------------------------------------------ */

#[derive(Default)]
struct Pool {
    live: Mutex<HashMap<ConnectionKey, Arc<Transport>>>,
}

fn pool() -> &'static Pool {
    static POOL: OnceLock<Pool> = OnceLock::new();
    POOL.get_or_init(Pool::default)
}

/// Connect, or return the existing transport for this exact identity.
pub fn connect(req: ConnectRequest) -> Result<ConnectionInfo, String> {
    let endpoint = req.endpoint();
    let key = key_of(&endpoint);

    // §8.1: reuse. A second tab on the same identity opens a channel, not a
    // connection.
    if let Some(existing) = pool().live.lock().map_err(|_| "pool poisoned")?.get(&key) {
        if !existing.is_closed() {
            return Ok(info(&key, &endpoint));
        }
    }

    let methods = req.methods()?;
    if methods.is_empty() {
        return Err("no authentication method supplied (key, SSH agent or password)".into());
    }
    let policy = req.policy();

    let transport = runtime()
        .block_on(Transport::connect(endpoint.clone(), &methods, policy, None))
        .map_err(describe)?;

    pool()
        .live
        .lock()
        .map_err(|_| "pool poisoned")?
        .insert(key.clone(), Arc::new(transport));

    Ok(info(&key, &endpoint))
}

/// What the running SSH agent holds, for the connect dialog.
#[derive(Serialize, Clone, Debug)]
pub struct AgentInfo {
    pub available: bool,
    pub keys: Vec<String>,
    pub error: Option<String>,
}

/// Ask the agent what it holds. A local socket round trip, but on Windows the
/// pipe connect can wait up to its timeout, so callers run it off the UI thread.
pub fn agent_status() -> AgentInfo {
    let s = runtime().block_on(opspilot_core_ssh::agent::status());
    AgentInfo {
        available: s.available,
        keys: s.keys,
        error: s.error,
    }
}

fn info(key: &str, e: &Endpoint) -> ConnectionInfo {
    ConnectionInfo {
        key: key.to_string(),
        host: e.host.clone(),
        port: e.port,
        user: e.user.clone(),
        identity: e.identity.clone(),
    }
}

/// Error text fit for a user.
///
/// A host-key failure is the one case where the message *is* the product: it
/// must say what happened and why the connection was refused, not "connection
/// closed by peer".
fn describe(e: SshError) -> String {
    e.to_string()
}

pub fn get(key: &str) -> Result<Arc<Transport>, String> {
    pool()
        .live
        .lock()
        .map_err(|_| "pool poisoned")?
        .get(key)
        .cloned()
        .ok_or_else(|| format!("not connected: {key}"))
}

pub fn list() -> Vec<ConnectionInfo> {
    let Ok(live) = pool().live.lock() else {
        return Vec::new();
    };
    live.iter()
        .filter(|(_, t)| !t.is_closed())
        .map(|(k, t)| info(k, t.endpoint()))
        .collect()
}

pub fn disconnect(key: &str) -> Result<(), String> {
    let removed = pool().live.lock().map_err(|_| "pool poisoned")?.remove(key);
    if let Some(t) = removed {
        runtime().block_on(t.disconnect());
    }
    Ok(())
}

/* ---- SFTP ------------------------------------------------------------------ */

#[derive(Serialize, Clone, Debug)]
pub struct SftpEntry {
    pub name: String,
    /// "file" | "dir" | "symlink" | "other"
    pub kind: &'static str,
    pub size: u64,
    pub modified: Option<u64>,
    /// "rwxr-xr-x"
    pub permissions: String,
    /// "0755"
    pub octal: Option<String>,
}

fn wire(e: Entry) -> SftpEntry {
    SftpEntry {
        kind: match e.kind {
            EntryKind::Dir => "dir",
            EntryKind::Symlink => "symlink",
            EntryKind::File => "file",
            EntryKind::Other => "other",
        },
        // Both forms, because §Step 10 wants `rwxr-xr-x` and the octal shown
        // together with the octal in --text-lo.
        permissions: e.permission_string(),
        octal: e.permissions.map(|m| format!("{:04o}", m & 0o7777)),
        name: e.name,
        size: e.size,
        modified: e.modified,
    }
}

#[derive(Serialize, Clone, Debug)]
pub struct Listing {
    pub path: String,
    pub entries: Vec<SftpEntry>,
}

/// Run an SFTP operation on a fresh channel over the pooled transport.
///
/// A channel per operation rather than a long-lived session: opening one is
/// cheap (§8.1), and it means a stuck transfer cannot wedge the browser. When
/// the transfer queue lands it will hold its own persistent channel.
fn with_sftp<T>(
    key: &str,
    f: impl FnOnce(
        opspilot_core_ssh::Sftp,
    )
        -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, SshError>> + Send>>,
) -> Result<T, String> {
    let transport = get(key)?;
    runtime().block_on(async move {
        let sftp = transport.open_sftp().await.map_err(describe)?;
        f(sftp).await.map_err(describe)
    })
}

pub fn sftp_list(key: &str, path: &str) -> Result<Listing, String> {
    let path = path.to_string();
    let p2 = path.clone();
    let entries = with_sftp(key, move |s| Box::pin(async move { s.list(&p2).await }))?;
    Ok(Listing {
        path,
        entries: entries.into_iter().map(wire).collect(),
    })
}

pub fn sftp_home(key: &str) -> Result<String, String> {
    with_sftp(key, move |s| {
        Box::pin(async move { s.canonicalize(".").await })
    })
}

pub fn sftp_read(key: &str, path: &str) -> Result<Vec<u8>, String> {
    let p = path.to_string();
    with_sftp(key, move |s| Box::pin(async move { s.read(&p).await }))
}

pub fn sftp_write(key: &str, path: &str, data: Vec<u8>) -> Result<(), String> {
    let p = path.to_string();
    with_sftp(key, move |s| {
        Box::pin(async move { s.create(&p, &data).await })
    })
}

pub fn sftp_mkdir(key: &str, path: &str) -> Result<(), String> {
    let p = path.to_string();
    with_sftp(key, move |s| {
        Box::pin(async move { s.create_dir(&p).await })
    })
}

pub fn sftp_rename(key: &str, from: &str, to: &str) -> Result<(), String> {
    let (a, b) = (from.to_string(), to.to_string());
    with_sftp(key, move |s| {
        Box::pin(async move { s.rename(&a, &b).await })
    })
}

pub fn sftp_remove(key: &str, path: &str, is_dir: bool) -> Result<(), String> {
    let p = path.to_string();
    with_sftp(key, move |s| {
        Box::pin(async move {
            if is_dir {
                s.remove_dir(&p).await
            } else {
                s.remove_file(&p).await
            }
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_keys_separate_identities_on_one_host() {
        // The §8.1 rule: same host, different identity => different transport.
        let a = Endpoint {
            host: "h".into(),
            port: 22,
            user: "u".into(),
            identity: "deploy".into(),
        };
        let b = Endpoint {
            identity: "backup".into(),
            ..a.clone()
        };
        assert_ne!(key_of(&a), key_of(&b));

        // ...but the same identity is the same transport.
        assert_eq!(key_of(&a), key_of(&a.clone()));
    }

    #[test]
    fn pool_keys_separate_users_and_ports() {
        let base = Endpoint {
            host: "h".into(),
            port: 22,
            user: "u".into(),
            identity: "i".into(),
        };
        assert_ne!(
            key_of(&base),
            key_of(&Endpoint {
                user: "other".into(),
                ..base.clone()
            })
        );
        assert_ne!(
            key_of(&base),
            key_of(&Endpoint {
                port: 2222,
                ..base.clone()
            })
        );
    }

    #[test]
    fn key_auth_is_tried_before_password() {
        let req = ConnectRequest {
            host: "h".into(),
            port: 22,
            user: "u".into(),
            identity: "i".into(),
            auth: AuthSpec {
                password: Some("pw".into()),
                key_path: Some("/k".into()),
                passphrase: None,
                password_ref: None,
                passphrase_ref: None,
                use_agent: None,
            },
            host_key_policy: None,
        };
        let m = req.methods().unwrap();
        assert!(matches!(m[0], AuthMethod::PrivateKey { .. }));
        assert!(matches!(m[1], AuthMethod::Agent), "agent goes between key and password");
        assert!(matches!(m[2], AuthMethod::Password(_)));
        assert_eq!(m.len(), 3);
    }

    #[test]
    fn the_agent_is_offered_by_default_and_can_be_turned_off() {
        let mut req = ConnectRequest {
            host: "h".into(),
            port: 22,
            user: "u".into(),
            identity: "i".into(),
            auth: AuthSpec {
                password: None,
                key_path: None,
                passphrase: None,
                password_ref: None,
                passphrase_ref: None,
                use_agent: None,
            },
            host_key_policy: None,
        };
        let m = req.methods().unwrap();
        assert!(matches!(m.as_slice(), [AuthMethod::Agent]), "nothing configured: agent only");

        req.auth.use_agent = Some(false);
        assert!(req.methods().unwrap().is_empty());
    }

    #[test]
    fn empty_credentials_are_not_offered_as_methods() {
        // An empty password field must not become an empty-password attempt.
        let req = ConnectRequest {
            host: "h".into(),
            port: 22,
            user: "u".into(),
            identity: "i".into(),
            auth: AuthSpec {
                password: Some(String::new()),
                key_path: Some(String::new()),
                passphrase: None,
                password_ref: None,
                passphrase_ref: None,
                use_agent: None,
            },
            host_key_policy: None,
        };
        let m = req.methods().unwrap();
        assert!(
            m.iter().all(|m| matches!(m, AuthMethod::Agent)),
            "an empty field became a key or password attempt"
        );
    }

    /// A host configured to use the vault must not silently connect with
    /// whatever stale plaintext the dialog happened to still hold.
    #[test]
    fn a_vault_reference_against_a_locked_vault_fails_the_connect() {
        let req = ConnectRequest {
            host: "h".into(),
            port: 22,
            user: "u".into(),
            identity: "i".into(),
            auth: AuthSpec {
                password: Some("stale-typed-value".into()),
                key_path: None,
                passphrase: None,
                password_ref: Some("vault:prod/root".into()),
                passphrase_ref: None,
                use_agent: None,
            },
            host_key_policy: None,
        };
        // The vault is locked in tests, so this must be an error rather than a
        // Password("stale-typed-value") method.
        let err = req
            .methods()
            .expect_err("a locked vault must stop the connect");
        assert!(err.contains("locked"), "{err}");
    }

    #[test]
    fn policy_defaults_to_accept_new_and_parses_strict() {
        let mk = |p: Option<&str>| ConnectRequest {
            host: "h".into(),
            port: 22,
            user: "u".into(),
            identity: "i".into(),
            auth: AuthSpec {
                password: None,
                key_path: None,
                passphrase: None,
                password_ref: None,
                passphrase_ref: None,
                use_agent: None,
            },
            host_key_policy: p.map(String::from),
        };
        assert_eq!(mk(None).policy(), HostKeyPolicy::AcceptNew);
        assert_eq!(mk(Some("strict")).policy(), HostKeyPolicy::Strict);
        assert_eq!(mk(Some("verify-only")).policy(), HostKeyPolicy::VerifyOnly);
    }
}
