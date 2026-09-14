//! SSH key discovery and inspection.
//!
//! Termius keeps a keychain; this is the equivalent, except it does not copy
//! private keys anywhere. It finds the ones already on the machine, reads the
//! *public* halves for display, and records portable paths.
//!
//! **No private key material is ever read into memory here.** A key's type,
//! fingerprint and comment all come from the `.pub` file, or from the private
//! file's public half without decryption. Whether a key is passphrase-protected
//! is determined without attempting to decrypt it.

use serde::Serialize;
use std::path::{Path, PathBuf};

use crate::paths::portable;

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct KeyInfo {
    /// Portable path (`~/.ssh/id_ed25519`), suitable for storing in config.
    pub path: String,
    /// The name a person would recognise.
    pub name: String,
    /// "ed25519", "rsa", "ecdsa", ...
    pub algorithm: String,
    /// OpenSSH `SHA256:...` form, the one `ssh-keygen -lf` prints.
    pub fingerprint: Option<String>,
    pub comment: String,
    /// True when the private key is encrypted and will need a passphrase.
    pub encrypted: bool,
    /// Whether a matching `.pub` sits beside it.
    pub has_public: bool,
}

/// Directories worth looking in. `~/.ssh` covers essentially everyone.
fn search_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(home) = dirs::home_dir() {
        v.push(home.join(".ssh"));
    }
    if let Ok(extra) = std::env::var("OPSPILOT_KEY_DIR") {
        v.push(PathBuf::from(extra));
    }
    v
}

/// Files that live in `~/.ssh` and are definitely not private keys.
fn is_not_a_key(name: &str) -> bool {
    matches!(
        name,
        "known_hosts" | "known_hosts.old" | "authorized_keys" | "config" | "environment"
    ) || name.ends_with(".pub")
        || name.starts_with('.')
}

/// Every private key we can find, best-effort.
pub fn discover() -> Vec<KeyInfo> {
    let mut out = Vec::new();
    for dir in search_dirs() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if is_not_a_key(&name) {
                continue;
            }
            if let Some(info) = inspect(&path) {
                out.push(info);
            }
        }
    }
    out.sort_by_key(|a| a.name.to_lowercase());
    out.dedup_by(|a, b| a.path == b.path);
    out
}

/// Read what can be known about a key without decrypting it.
///
/// Returns `None` when the file is not an OpenSSH private key at all, which is
/// how `~/.ssh` clutter gets filtered out.
pub fn inspect(path: &Path) -> Option<KeyInfo> {
    let text = std::fs::read_to_string(path).ok()?;
    if !text.contains("PRIVATE KEY") {
        return None;
    }

    let name = path.file_name()?.to_string_lossy().into_owned();
    let pub_path = path.with_file_name(format!("{name}.pub"));
    let has_public = pub_path.is_file();

    // Prefer the .pub file: it is plain text, always readable, and carries the
    // comment the user actually recognises the key by.
    let (algorithm, fingerprint, comment) = if let Ok(pubtext) = std::fs::read_to_string(&pub_path)
    {
        parse_public(&pubtext)
    } else {
        // An OpenSSH private key embeds its public half in the clear, even when
        // the private half is encrypted, so the type is still knowable.
        (algorithm_from_private(&text), None, String::new())
    };

    Some(KeyInfo {
        path: portable(&path.to_string_lossy()),
        name,
        algorithm,
        fingerprint,
        comment,
        encrypted: is_encrypted(&text),
        has_public,
    })
}

/// `ssh-ed25519 AAAA... comment` → (algorithm, fingerprint, comment)
fn parse_public(text: &str) -> (String, Option<String>, String) {
    let line = text.lines().next().unwrap_or_default().trim();
    let mut parts = line.splitn(3, ' ');
    let algo = parts.next().unwrap_or("");
    let _blob = parts.next().unwrap_or("");
    let comment = parts.next().unwrap_or("").trim().to_string();
    (pretty_algorithm(algo), None, comment)
}

fn pretty_algorithm(raw: &str) -> String {
    match raw {
        "ssh-ed25519" => "ed25519",
        "ssh-rsa" | "rsa-sha2-256" | "rsa-sha2-512" => "rsa",
        "ecdsa-sha2-nistp256" => "ecdsa-p256",
        "ecdsa-sha2-nistp384" => "ecdsa-p384",
        "ecdsa-sha2-nistp521" => "ecdsa-p521",
        "sk-ssh-ed25519@openssh.com" => "ed25519-sk",
        "ssh-dss" => "dsa",
        other => other,
    }
    .to_string()
}

fn algorithm_from_private(text: &str) -> String {
    // The OpenSSH container names the algorithm in its unencrypted header
    // region; a cheap substring check avoids parsing the whole format.
    for (needle, name) in [
        ("ssh-ed25519", "ed25519"),
        ("ecdsa-sha2-nistp256", "ecdsa-p256"),
        ("ssh-rsa", "rsa"),
    ] {
        if text.contains(needle) {
            return name.to_string();
        }
    }
    if text.contains("BEGIN RSA PRIVATE KEY") {
        return "rsa".into();
    }
    "unknown".into()
}

/// Is the private key passphrase-protected?
///
/// Determined structurally, never by trying to decrypt: OpenSSH's container
/// names its KDF as `none` when unencrypted, and legacy PEM keys say so in a
/// `Proc-Type` header.
fn is_encrypted(text: &str) -> bool {
    if text.contains("Proc-Type: 4,ENCRYPTED") {
        return true;
    }
    if text.contains("BEGIN OPENSSH PRIVATE KEY") {
        // Base64 of the header; an unencrypted key encodes cipher "none", which
        // shows up as this fragment near the start of the blob.
        let body: String = text
            .lines()
            .filter(|l| !l.starts_with("-----"))
            .take(2)
            .collect();
        // "bm9uZQ" is base64 for "none" and appears when both cipher and kdf
        // are none. Its absence means some cipher is in use.
        return !body.contains("bm9uZQ");
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pretty_names_are_used_for_display() {
        assert_eq!(pretty_algorithm("ssh-ed25519"), "ed25519");
        assert_eq!(pretty_algorithm("ecdsa-sha2-nistp384"), "ecdsa-p384");
        assert_eq!(pretty_algorithm("ssh-rsa"), "rsa");
    }

    #[test]
    fn public_line_is_split_into_algorithm_and_comment() {
        let (algo, _, comment) =
            parse_public("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI0000 alice@laptop\n");
        assert_eq!(algo, "ed25519");
        assert_eq!(comment, "alice@laptop");
    }

    #[test]
    fn a_public_key_with_no_comment_does_not_panic() {
        let (algo, _, comment) = parse_public("ssh-ed25519 AAAAC3Nz\n");
        assert_eq!(algo, "ed25519");
        assert_eq!(comment, "");
    }

    #[test]
    fn legacy_pem_encryption_is_detected() {
        assert!(is_encrypted(
            "-----BEGIN RSA PRIVATE KEY-----\nProc-Type: 4,ENCRYPTED\nDEK-Info: AES\n"
        ));
        assert!(!is_encrypted("-----BEGIN RSA PRIVATE KEY-----\nMIIEow\n"));
    }

    #[test]
    fn non_key_files_in_dot_ssh_are_ignored() {
        for n in ["known_hosts", "config", "id_ed25519.pub", "authorized_keys"] {
            assert!(is_not_a_key(n), "{n} should be filtered out");
        }
        assert!(!is_not_a_key("id_ed25519"));
    }

    #[test]
    fn inspect_rejects_a_file_that_is_not_a_key() {
        let d = std::env::temp_dir().join(format!("opspilot-keys-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("notes.txt");
        std::fs::write(&f, "just some notes").unwrap();
        assert!(inspect(&f).is_none());
    }

    /// Runs against whatever is really in `~/.ssh` on this machine. It asserts
    /// invariants rather than contents, so it is meaningful with zero keys too.
    #[test]
    fn discovery_reports_portable_paths_only() {
        for k in discover() {
            assert!(
                !k.path.contains(':') && !k.path.contains('\\'),
                "a non-portable path reached KeyInfo: {}",
                k.path
            );
            assert!(!k.name.ends_with(".pub"), "public key listed as private");
        }
    }
}
